//! The name and the header of the patch that carries a fix.

use crate::backport;

/// Prefixes that name a kind of patch, not the package.
const KIND_PREFIXES: &[&str] = &["fix", "bsc", "boo", "bnc", "jsc", "cve", "patch"];

/// The prefix a new patch of `pkg` takes: the package name, or the one the
/// package's patches already share (at least two, after the same words up to a
/// `-`), the convention of the package (convention 111).
pub fn patch_prefix(pkg: &str, declared: &[String]) -> String {
    if declared.len() < 2 || declared.iter().all(|n| n.starts_with(&format!("{pkg}-"))) {
        return pkg.to_string();
    }
    let first = &declared[0];
    let mut common = first.len();
    for n in &declared[1..] {
        common = common.min(
            first
                .bytes()
                .zip(n.bytes())
                .take_while(|(a, b)| a == b)
                .count(),
        );
    }
    let shared = &first[..common];
    let Some(end) = shared.rfind('-') else {
        return pkg.to_string();
    };
    let prefix = &shared[..end];
    let usable = prefix.len() >= 3
        && prefix.chars().any(|c| c.is_ascii_lowercase())
        && !KIND_PREFIXES.contains(&prefix.to_ascii_lowercase().as_str());
    if usable {
        prefix.to_string()
    } else {
        pkg.to_string()
    }
}

/// The file name of a fix in the package: `<prefix>-CVE-2025-1234.patch`,
/// `<prefix>-CVE-2025-1234-abc1234.patch` with two commits,
/// `<prefix>-fix-abc1234.patch` without a CVE. `prefix` is [`patch_prefix`].
pub fn fix_patch_name(
    prefix: &str,
    refs: &[String],
    commit: Option<&str>,
    index: usize,
    total: usize,
) -> String {
    format!(
        "{prefix}-{}",
        backport::patch_file_name(refs, commit, index, total)
    )
}

/// Where the fix stands upstream.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UpstreamStatus {
    /// A commit.
    Merged,
    /// A pull or merge request.
    Submitted,
}

impl UpstreamStatus {
    fn word(self) -> &'static str {
        match self {
            UpstreamStatus::Merged => "merged",
            UpstreamStatus::Submitted => "submitted",
        }
    }
}

/// What the header of a fix patch says.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HeaderFacts {
    pub refs: Vec<String>,
    /// The commit or pull request URL; `None` when the patch has no known
    /// upstream (a file the user gave).
    pub upstream: Option<String>,
    pub status: UpstreamStatus,
    /// What it fixes, for the subject of a plain diff.
    pub reason: String,
}

impl HeaderFacts {
    fn references(&self) -> Option<String> {
        (!self.refs.is_empty()).then(|| format!("References: {}", self.refs.join(" ")))
    }

    fn upstream_line(&self) -> String {
        match &self.upstream {
            Some(url) => format!("Upstream: {} {url}", self.status.word()),
            None => "Upstream: not yet reported".to_string(),
        }
    }

    fn description(&self) -> String {
        match &self.upstream {
            Some(url) => format!("Description: backport of {url}"),
            None => format!("Description: {}", self.reason),
        }
    }
}

/// The lines of `bytes`, each with its newline.
fn lines_of(bytes: &[u8]) -> Vec<&[u8]> {
    bytes.split_inclusive(|b| *b == b'\n').collect()
}

fn text(line: &[u8]) -> String {
    String::from_utf8_lossy(line).trim_end().to_string()
}

/// The number of lines before the diff or the `---` that ends a commit message.
fn header_len(lines: &[&[u8]]) -> usize {
    (0..lines.len())
        .find(|&i| {
            let l = text(lines[i]);
            l == "---"
                || l.starts_with("Index: ")
                || l.starts_with("diff --git ")
                || (l.starts_with("--- ")
                    && lines.get(i + 1).is_some_and(|n| n.starts_with(b"+++ ")))
        })
        .unwrap_or(lines.len())
}

fn has_field(lines: &[&[u8]], key: &str) -> bool {
    lines.iter().any(|l| {
        text(l)
            .split_once(':')
            .is_some_and(|(k, v)| k.trim().eq_ignore_ascii_case(key) && !v.trim().is_empty())
    })
}

/// `bytes` with the `References:` and `Upstream:` lines convention 111 asks
/// for. A `git format-patch` mail (or any patch with a `Subject:` line in its
/// header) keeps every line and gets them after its subject, with its folded
/// continuation lines; a plain diff gets a header block. A line the header has
/// already is kept, not doubled.
pub fn with_fix_header(bytes: &[u8], facts: &HeaderFacts) -> Vec<u8> {
    let lines = lines_of(bytes);
    let head = header_len(&lines);
    let subject = lines[..head].iter().position(|l| {
        text(l)
            .split_once(':')
            .is_some_and(|(k, _)| k.eq_ignore_ascii_case("Subject"))
    });
    let mut add: Vec<String> = Vec::new();
    if !has_field(&lines[..head], "References")
        && let Some(r) = facts.references()
    {
        add.push(r);
    }
    if !has_field(&lines[..head], "Upstream") && !has_field(&lines[..head], "Patch-Mainline") {
        add.push(facts.upstream_line());
    }
    let Some(s) = subject else {
        let mut out: Vec<u8> = Vec::with_capacity(bytes.len() + 256);
        out.extend(format!("Subject: {}\n", facts.reason).bytes());
        for l in &add {
            out.extend(format!("{l}\n").bytes());
        }
        out.extend(format!("\n{}\n---\n", facts.description()).bytes());
        out.extend_from_slice(bytes);
        return out;
    };
    let mut after = s + 1;
    while after < head
        && lines[after]
            .first()
            .is_some_and(|b| *b == b' ' || *b == b'\t')
    {
        after += 1;
    }
    let assemble = |add: &[String]| -> Vec<u8> {
        let mut out: Vec<u8> = Vec::with_capacity(bytes.len() + 256);
        for l in &lines[..after] {
            out.extend_from_slice(l);
        }
        for l in add {
            out.extend(format!("{l}\n").bytes());
        }
        for l in &lines[after..] {
            out.extend_from_slice(l);
        }
        out
    };
    let out = assemble(&add);
    // A commit message may be empty: say what the patch is.
    if crate::pipeline::header_problems(&String::from_utf8_lossy(&out))
        .iter()
        .any(|p| p.contains("Description"))
    {
        add.push(facts.description());
        return assemble(&add);
    }
    out
}

/// `text` made safe to put in a spec comment or a `.changes` bullet: rpm expands
/// `%` in a comment (`%(cmd)` runs a command), so each `%` is doubled, and a
/// control character (a newline would end the comment) is a space.
pub fn spec_safe(text: &str) -> String {
    text.chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect::<String>()
        .replace('%', "%%")
}

/// The `+` and `-` lines inside the hunks of a patch: its code, without the
/// header, the file names, the hunk headers and the context. A hunk's lines
/// are read by its counts, so a removed `-- x` is not a file header.
pub fn code_lines(patch: &str) -> Vec<String> {
    let mut out = Vec::new();
    let (mut old_left, mut new_left) = (0usize, 0usize);
    for line in patch.lines() {
        if old_left > 0 || new_left > 0 {
            if line.starts_with('+') {
                new_left = new_left.saturating_sub(1);
                out.push(line.to_string());
            } else if line.starts_with('-') {
                old_left = old_left.saturating_sub(1);
                out.push(line.to_string());
            } else if !line.starts_with('\\') {
                old_left = old_left.saturating_sub(1);
                new_left = new_left.saturating_sub(1);
            }
        } else if line.starts_with("@@") {
            let mut counts = line.split_whitespace().skip(1).map(|p| {
                p.trim_start_matches(['-', '+'])
                    .split(',')
                    .nth(1)
                    .and_then(|d| d.parse().ok())
                    .unwrap_or(1)
            });
            old_left = counts.next().unwrap_or(0);
            new_left = counts.next().unwrap_or(0);
        }
    }
    out
}
