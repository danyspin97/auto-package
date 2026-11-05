//! The upstream lines a carried patch's header gets, and the spec comment above
//! its `PatchN:` line (plan 2.1 to 2.3). Only lines the header lacks are added;
//! no existing line is edited, moved or removed, and every insertion passes
//! [`checked_insert`] or is not made.

use super::reason::{cut, fold_field, sanitize};
use super::{OriginKind, PatchOrigin};
use crate::bugfix::{code_lines, header_len, spec_safe, subject_end};
use crate::pipeline::{declared_patches, header_problems, is_patch_comment};

/// What a patch's header records about its origin.
pub struct Claim<'a> {
    pub origin: &'a PatchOrigin,
    /// The word of `Upstream:`: `submitted`, `merged`; `None` for a bare URL.
    pub status: Option<&'static str>,
    /// The commit to record in `Git-commit:`.
    pub commit: Option<String>,
    /// What the patch is for, for a `Description:` the header lacks.
    pub reason: Option<String>,
}

/// The lines of `Claim` the header of `text` lacks, in the order of plan 2.1.
/// A header with `Upstream:` or `Patch-Mainline:` is never written (it is
/// left as the maintainer has it).
fn missing_lines(text: &str, claim: &Claim<'_>) -> Vec<String> {
    let lines: Vec<&[u8]> = text.split_inclusive('\n').map(str::as_bytes).collect();
    let head = header_len(&lines);
    let header = &lines[..head];
    if has_key(header, "Upstream") || has_key(header, "Patch-Mainline") {
        return Vec::new();
    }
    let mut out = Vec::new();
    if !has_key(header, "References")
        && let Some(r) = github_reference(claim.origin)
    {
        out.push(format!("References: {r}"));
    }
    out.push(match claim.status {
        Some(word) => format!("Upstream: {word} {}", claim.origin.url),
        None => format!("Upstream: {}", claim.origin.url),
    });
    if let Some(commit) = &claim.commit
        && !has_key(header, "Git-commit")
    {
        out.push(format!("Git-commit: {commit}"));
    }
    if let Some(reason) = &claim.reason
        && !has_key(header, "Description")
        && header_problems(text)
            .iter()
            .any(|p| p.contains("Description"))
    {
        out.extend(fold_field("Description", &sanitize(reason)));
    }
    out
}

/// `gh#owner/repo#N` for a GitHub pull request: the one reference spelling of
/// convention 111 for a request. GitLab and Gitea have the URL alone.
pub fn github_reference(origin: &PatchOrigin) -> Option<String> {
    if origin.kind != OriginKind::Pr {
        return None;
    }
    let forge = ap_ecosystem::forge::parse(&origin.project)?;
    if forge.host != "github.com" {
        return None;
    }
    Some(format!("gh#{}#{}", forge.project_path, origin.number?))
}

/// `text` (the whole patch file) with the header lines of `claim` it lacks.
/// `None` when nothing is missing, or when the insertion is refused.
///
/// A file with a `Subject:` gets the lines after its subject (the place
/// `with_fix_header` uses): `git am` and `quilt` read a mail's header and keep
/// what they do not know. A file with no `Subject:` gets the lines at the top,
/// then one blank line; a file that starts with a `From ` mbox line gets none.
/// Whether the header has the field `key`, with or without a value: a bare
/// `Description:` is a field the writer must not add a second one to.
fn has_key(lines: &[&[u8]], key: &str) -> bool {
    lines.iter().any(|l| {
        String::from_utf8_lossy(l)
            .split_once(':')
            .is_some_and(|(k, _)| k.trim().eq_ignore_ascii_case(key))
    })
}

/// The line ending of a file: CRLF when its first line ends so, else LF.
fn line_ending(text: &str) -> &'static str {
    match text.find('\n') {
        Some(i) if i > 0 && text.as_bytes()[i - 1] == b'\r' => "\r\n",
        _ => "\n",
    }
}

pub fn with_header(text: &str, claim: &Claim<'_>) -> Option<String> {
    let lines = missing_lines(text, claim);
    if lines.is_empty() {
        return None;
    }
    let old: Vec<&[u8]> = text.split_inclusive('\n').map(str::as_bytes).collect();
    let head = header_len(&old);
    let (at, block) = match subject_end(&old, head) {
        Some(after) => (after, lines),
        None => {
            if text.starts_with("From ") {
                return None;
            }
            let mut block = lines;
            block.push(String::new());
            (0, block)
        }
    };
    let mut out = String::with_capacity(text.len() + 256);
    for l in &old[..at] {
        out.push_str(&String::from_utf8_lossy(l));
    }
    let eol = line_ending(text);
    for l in &block {
        out.push_str(l);
        out.push_str(eol);
    }
    for l in &old[at..] {
        out.push_str(&String::from_utf8_lossy(l));
    }
    checked_insert(text, &out, block.len(), head).then_some(out)
}

/// The guard of every header insertion (plan 2.3): the code is the same, the
/// file still passes `check_patch`, no header problem is new, and the inserted
/// lines lie before the header boundary.
pub(super) fn checked_insert(old: &str, new: &str, inserted: usize, old_head: usize) -> bool {
    let new_bytes = new.as_bytes();
    if code_lines(old) != code_lines(new) {
        return false;
    }
    if crate::backport::check_patch(new_bytes).is_err() {
        return false;
    }
    let new_lines: Vec<&[u8]> = new_bytes.split_inclusive(|b| *b == b'\n').collect();
    if header_len(&new_lines) < old_head + inserted {
        return false;
    }
    let before = header_problems(old);
    if header_problems(new).iter().any(|p| !before.contains(p)) {
        return false;
    }
    true
}

/// The `# PATCH-FIX-UPSTREAM` comment above the `PatchN:` line of `name`
/// (plan 2.1): the reference, and the reason cut at 80 characters. Nothing is
/// done when the line above is a type 1 comment already, or the file is not
/// declared.
pub fn spec_with_comment(
    spec: &str,
    name: &str,
    reference: &str,
    reason: Option<&str>,
) -> Option<String> {
    let (index, _, _) = declared_patches(spec)
        .into_iter()
        .find(|(_, _, declared)| declared == name)?;
    let lines: Vec<&str> = spec.split_inclusive('\n').collect();
    if index > 0 && is_patch_comment(lines.get(index - 1)?) {
        return None;
    }
    let mut comment = format!("# PATCH-FIX-UPSTREAM {name} {reference}");
    if let Some(reason) = reason {
        let reason = spec_safe(&cut(&sanitize(reason), 80));
        comment.push_str(" -- ");
        comment.push_str(&reason);
    }
    let mut out = String::with_capacity(spec.len() + comment.len() + 1);
    for l in &lines[..index] {
        out.push_str(l);
    }
    out.push_str(&comment);
    out.push_str(line_ending(spec));
    for l in &lines[index..] {
        out.push_str(l);
    }
    Some(out)
}

#[test]
fn a_crlf_file_gets_crlf_lines() {
    let text =
        "Subject: [PATCH] x\r\n\r\nmsg\r\n---\r\n--- a/f\r\n+++ b/f\r\n@@ -1 +1 @@\r\n-a\r\n+b\r\n";
    let o = PatchOrigin {
        url: "https://github.com/o/r/pull/7".into(),
        kind: crate::patch_provenance::OriginKind::Pr,
        project: "https://github.com/o/r".into(),
        number: Some(7),
        commit: None,
        evidence: crate::patch_provenance::OriginEvidence::PatchHeader,
    };
    let c = Claim {
        origin: &o,
        status: Some("submitted"),
        commit: None,
        reason: None,
    };
    let out = with_header(text, &c).unwrap();
    assert!(
        out.contains("Upstream: submitted https://github.com/o/r/pull/7\r\n"),
        "{out:?}"
    );
    assert!(
        !out.replace("\r\n", "").contains('\n'),
        "a bare LF: {out:?}"
    );
}

#[test]
fn a_bare_description_field_is_not_doubled() {
    let text = "Subject: [PATCH] x\nDescription:\n\n---\n--- a/f\n+++ b/f\n@@ -1 +1 @@\n-a\n+b\n";
    let o = PatchOrigin {
        url: "https://github.com/o/r/pull/7".into(),
        kind: crate::patch_provenance::OriginKind::Pr,
        project: "https://github.com/o/r".into(),
        number: Some(7),
        commit: None,
        evidence: crate::patch_provenance::OriginEvidence::PatchHeader,
    };
    let c = Claim {
        origin: &o,
        status: Some("submitted"),
        commit: None,
        reason: Some("the fix".into()),
    };
    let out = with_header(text, &c).unwrap_or_else(|| text.to_string());
    assert_eq!(out.matches("Description:").count(), 1, "{out}");
}
