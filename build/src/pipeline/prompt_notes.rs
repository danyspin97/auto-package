//! The package notes and attempts that go into a prompt, and their file writes.

use super::*;

/// Most notes text inlined into one prompt. Older entries are dropped first, so
/// a long history costs context but never blows up every turn.
pub(super) const MAX_PROMPT_NOTES_BYTES: usize = 16 * 1024;

/// The last `MAX_PROMPT_NOTES_BYTES` of `text`, cut at a line boundary.
pub(crate) fn tail_for_prompt(text: &str) -> String {
    if text.len() <= MAX_PROMPT_NOTES_BYTES {
        return text.to_string();
    }
    let mut start = text.len() - MAX_PROMPT_NOTES_BYTES;
    while !text.is_char_boundary(start) {
        start += 1;
    }
    let rest = match text[start..].find('\n') {
        Some(i) => &text[start + i + 1..],
        None => "",
    };
    format!("(earlier entries omitted)\n{rest}")
}

/// Package notes for a prompt. When they are too long, whole sections are
/// dropped rather than the top of the file: build requirements and workarounds
/// are kept, then test changes, then anything else (failure history). Only if
/// the kept sections still do not fit does the plain tail cut apply.
pub(super) fn package_notes_for_prompt(text: &str) -> String {
    if text.len() <= MAX_PROMPT_NOTES_BYTES {
        return text.to_string();
    }
    // A section starts at a markdown heading (`#`, `##`, ...) only: a bold line
    // is text inside the section it is in, so a `**Label**` under `## Build
    // requirements` keeps that section's rank. Text before the first heading is
    // kept with the highest priority.
    let mut sections: Vec<(u8, String)> = Vec::new();
    // Inside a fenced code block a `#` line is code (a shell comment, say), not
    // a heading: the fence's marker (``` or ~~~) is open until the same marker.
    let mut fence: Option<&str> = None;
    for line in text.lines() {
        let trimmed = line.trim_start();
        let heading = fence.is_none() && line.starts_with('#');
        match fence {
            None => {
                if trimmed.starts_with("```") {
                    fence = Some("```");
                } else if trimmed.starts_with("~~~") {
                    fence = Some("~~~");
                }
            }
            Some(marker) => {
                if trimmed.starts_with(marker) {
                    fence = None;
                }
            }
        }
        if heading || sections.is_empty() {
            let rank = if heading { notes_section_rank(line) } else { 0 };
            sections.push((rank, String::new()));
        }
        if let Some((_, body)) = sections.last_mut() {
            body.push_str(line);
            body.push('\n');
        }
    }
    let mut used = 0;
    let mut keep = vec![false; sections.len()];
    for (i, (rank, body)) in sections.iter().enumerate() {
        if *rank == 0 {
            keep[i] = true;
            used += body.len();
        }
    }
    for want in [1, 2] {
        for (i, (rank, body)) in sections.iter().enumerate() {
            if *rank == want && used + body.len() <= MAX_PROMPT_NOTES_BYTES {
                keep[i] = true;
                used += body.len();
            }
        }
    }
    let kept: String = sections
        .iter()
        .zip(&keep)
        .filter(|(_, keep)| **keep)
        .map(|((_, body), _)| body.as_str())
        .collect();
    tail_for_prompt(&kept)
}

/// How much a notes section matters to the next build: 0 keeps it, 1 and 2 drop
/// it first (test changes, then everything else).
fn notes_section_rank(heading: &str) -> u8 {
    let h = heading.to_ascii_lowercase();
    if h.contains("build requirements") || h.contains("workaround") {
        0
    } else if h.contains("test changes") {
        1
    } else {
        2
    }
}

/// Persist the fix-attempt count after every increment, the same way
/// `agent_turn` persists `spent_usd` after every turn — so a crash or a
/// `MissingDeps` round trip mid-pipeline resumes with the right count
/// instead of restarting `attempts` at 0 (see `PkgState::cumulative_attempts`).
pub(super) fn persist_attempts(ws: &Workspace, run_id: &str, attempts: usize) {
    let saved = PkgState::update(ws, |st| {
        claim_run(st, run_id);
        st.cumulative_attempts = attempts;
    });
    if let Err(e) = saved {
        ap_core::diag!("  cannot save the attempt count: {e}");
    }
}

/// Write an artifact or spec copy, warning with the path when it fails so the
/// run does not go on as if the file had been written.
pub(super) fn write_or_warn(path: impl AsRef<Path>, contents: impl AsRef<[u8]>) {
    let path = path.as_ref();
    if let Err(e) = ap_core::fsx::write_atomic(path, contents) {
        ap_core::diag!("  cannot write {}: {e}", path.display());
    }
}

/// Report a failed bookkeeping step (state file, convenience symlinks the
/// agent reads) as a warning instead of dropping it.
pub(super) fn warn_if_err<T, E: std::fmt::Display>(
    reporter: &dyn Reporter,
    what: &str,
    result: Result<T, E>,
) {
    if let Err(e) = result {
        reporter.report(&Event::Warning {
            step: None,
            message: format!("{what} failed: {e}"),
        });
    }
}

pub(super) fn safe_file_part(s: &str) -> String {
    let cleaned: String = s
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '+' | '-') {
                c
            } else {
                '_'
            }
        })
        .collect();
    cleaned.replace("..", "_")
}
