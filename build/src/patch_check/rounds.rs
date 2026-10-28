use super::*;

/// What one `quilt push` came to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PushVerdict {
    Clean,
    OffsetOrFuzz {
        fuzz: bool,
    },
    Failed,
    /// It failed, and quilt says the change is already in the tree.
    ReverseApplied,
}

/// Read a `quilt push` of one patch: its exit status and output.
pub fn classify_push(success: bool, output: &str) -> PushVerdict {
    if !success {
        return if output.lines().any(|l| l.contains(REVERSE_APPLIED)) {
            PushVerdict::ReverseApplied
        } else {
            PushVerdict::Failed
        };
    }
    let fuzz = output.lines().any(|l| l.contains(" with fuzz "));
    let offset = output.lines().any(|l| l.contains("(offset "));
    if fuzz || offset {
        PushVerdict::OffsetOrFuzz { fuzz }
    } else {
        PushVerdict::Clean
    }
}

/// The reject files a `quilt push` (or `quilt push -f`) names, relative to the
/// tree: `... -- rejects in file foo.c` and `... -- saving rejects to file
/// foo.c.rej`.
pub fn reject_files(output: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for line in output.lines() {
        let file = if let Some((_, rest)) = line.split_once("saving rejects to file ") {
            rest.trim().to_string()
        } else if let Some((_, rest)) = line.split_once("rejects in file ") {
            format!("{}.rej", rest.trim())
        } else {
            continue;
        };
        if !file.is_empty() && !out.contains(&file) {
            out.push(file);
        }
    }
    out
}

/// `text` cut to about `limit` bytes on a character boundary, saying so.
pub fn truncate(text: &str, limit: usize) -> String {
    let text = text.trim_end();
    if text.len() <= limit {
        return text.to_string();
    }
    let mut end = limit;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    format!(
        "{}\n[... {} more bytes cut]",
        &text[..end],
        text.len() - end
    )
}

/// The report the agent turn is given: every patch with quilt's verdict, the
/// output and rejected hunks of a failing one, the hints, and where things are.
/// `hints` are `(patch, directory)` for patches whose paths name another
/// release's tree (`stale_patches`).
pub fn agent_report(
    report: &CheckReport,
    version: &str,
    hints: &[(String, String)],
    checkout: &Path,
) -> String {
    let mut text = format!(
        "quilt was run on a pristine copy of the {version} sources, the patches in \
         series order:\n"
    );
    for p in &report.patches {
        let level = if p.level == 1 {
            String::new()
        } else {
            format!(" (-p{})", p.level)
        };
        match &p.status {
            PatchStatus::Applies => text.push_str(&format!(
                "\n- {}{level}: applies cleanly; leave it.\n",
                p.name
            )),
            PatchStatus::AppliesWithOffsetOrFuzz { fuzz, output } => text.push_str(&format!(
                "\n- {}{level}: applies with {}: refresh it.\n{}",
                p.name,
                if *fuzz {
                    "fuzz (rpm applies patches with --fuzz=0, so it FAILS the build)"
                } else {
                    "an offset"
                },
                crate::agent::fenced_data(output)
            )),
            PatchStatus::Fails { output, rejects } => {
                text.push_str(&format!(
                    "\n- {}{level}: FAILS to apply. quilt said:\n{}",
                    p.name,
                    crate::agent::fenced_data(output)
                ));
                if !rejects.is_empty() {
                    text.push_str(&format!(
                        "Rejected hunks:\n{}",
                        crate::agent::fenced_data(rejects)
                    ));
                }
                if let Some((_, dir)) = hints.iter().find(|(n, _)| *n == p.name) {
                    text.push_str(&format!(
                        "Its paths name `{dir}`, another release's tree: rebase it onto the \
                         {version} tree like the others.\n"
                    ));
                }
            }
            PatchStatus::AlreadyApplied { .. } => text.push_str(&format!(
                "\n- {}{level}: already applied upstream (quilt: `{REVERSE_APPLIED}`); the \
                 pipeline removes it.\n",
                p.name
            )),
            PatchStatus::NotChecked { reason } => text.push_str(&format!(
                "\n- {}{level}: not checked ({reason}); leave it.\n",
                p.name
            )),
        }
    }
    text.push_str(&format!(
        "\nThe new sources are `source/` in the workspace. The patches are files in the \
         checkout {} under their own names (QUILT_PATCHES points there, and its `series` \
         lists them in the order above), so `quilt refresh` in `source/` writes the patch \
         back in place.\n\
         A patch is never removed because it does not apply, nor because it only touches \
         tests (a failing hunk in a `.result` or test file is rebased like any other). The \
         only accepted proof is quilt's own: `quilt push` of the unchanged patch on the \
         pristine sources prints `Patch <name> {REVERSE_APPLIED}`. A removal without it is \
         rejected and rolled back.\n",
        checkout.display()
    ));
    text
}

/// What the loop does after a round of agent turns.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NextRound {
    /// Every patch applies cleanly.
    Clean,
    /// Something still fails and the round made progress: ask again.
    Again,
    /// The round failed nothing fewer and changed no patch.
    NoProgress,
    /// The rounds are spent.
    OutOfRounds,
}

/// The loop's stopping rule. `rounds_done` counts the rounds run so far
/// (this one included); `before` and `after` are the quilt tallies around the
/// round; `patches_changed` says whether the round changed or removed a patch.
pub fn next_round(
    rounds_done: usize,
    max_rounds: usize,
    before: Tally,
    after: Tally,
    patches_changed: bool,
) -> NextRound {
    if after == Tally::default() {
        return NextRound::Clean;
    }
    if !(after.failing < before.failing || patches_changed) {
        return NextRound::NoProgress;
    }
    if rounds_done >= max_rounds {
        return NextRound::OutOfRounds;
    }
    NextRound::Again
}
