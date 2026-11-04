//! The rules an upstream patch the agent adds must meet (D-20261102-10, the openSUSE
//! patch guidelines): a patch header that says what the patch fixes, why, and where it
//! stands upstream; a `PatchN:` line with a `PATCH-FIX-...` comment above it; a
//! `.changes` bullet that names the file; and a change to source code only. A patch
//! the edit did not add is judged on what it changes: an unchanged one is not judged,
//! and a changed one is judged on the lines its change adds (`patch_rules_content`).
//!
//! A patch is new when its file is not in the pre-turn snapshot with the same bytes
//! under the same name, and no file the pre-turn spec declared holds those bytes (a
//! renumbered or renamed patch of the spec is not new). An unreferenced `.patch` file
//! with the same bytes does not make a new `Patch:` line old. A patch that is a URL
//! cannot be read before the download: it is judged on its `PATCH-*` comment only.
//!
//! A new `Source:` that names a patch, and a patch applied from `%prep` by a shell
//! command (`patch`, `git apply`, or a `sed -i` in an edit that adds a patch), are
//! refused: the patch goes in a `PatchN:` line, which the rules check
//! (`patch_rules_spec`).

use super::*;
use crate::pipeline::patch_rules_content::*;
use crate::pipeline::patch_rules_spec::*;
use crate::pipeline::removal_proof::*;
use crate::pipeline::render::*;

/// Words of a `.changes` bullet that name what was done to a patch file.
const PATCH_VERBS: &[&str] = &["add", "backport", "rebase", "modify", "disable", "drop"];

/// The status words an `Upstream:` value may start with (convention 111).
const STATUS_WORDS: &[&str] = &["submitted", "merged", "never", "no", "tbd", "dead"];

/// A patch the edit declares that the rules judge.
#[derive(Debug)]
pub(super) struct JudgedPatch {
    /// The line index in the edited spec.
    pub(super) index: usize,
    /// The value as written, with the macros unexpanded.
    pub(super) raw: String,
    /// The file name (a URL's last segment for a URL).
    pub(super) name: String,
    /// The bytes of the file now, when it can be read.
    pub(super) bytes: Option<Vec<u8>>,
    /// Whether the patch is new, or is a declared patch whose bytes changed (with
    /// the bytes it had before the turn, when they were kept).
    pub(super) status: PatchStatus,
}

#[derive(Debug, PartialEq)]
pub(super) enum PatchStatus {
    New,
    Changed(Option<Vec<u8>>),
}

/// Whether `value` is a web address with no white space in it.
fn is_web_address(value: &str) -> bool {
    (value.starts_with("https://") || value.starts_with("http://"))
        && !value.contains(char::is_whitespace)
}

/// The identity of a declared patch: its URL, or its file name.
pub(super) fn patch_key(raw: &str, name: &str) -> String {
    if is_web_address(raw) {
        raw.to_string()
    } else {
        name.to_string()
    }
}

/// The `Patch` declarations of `spec`: `(line index, value as written, file name)`.
/// The file name of a URL is its last path segment, and the spec's macros are expanded.
pub(crate) fn declared_patches(spec: &str) -> Vec<(usize, String, String)> {
    spec.lines()
        .enumerate()
        .filter_map(|(index, line)| {
            let patch = crate::spec::parse_source_line(line, &["patch"], str::to_string)?;
            let raw = patch.url.trim().to_string();
            let name = if is_web_address(&raw) {
                let path = raw.split(['?', '#']).next().unwrap_or("");
                path.rsplit('/').next().unwrap_or("").to_string()
            } else {
                expand_patch_macros(&raw, spec)
            };
            Some((index, raw, name))
        })
        .collect()
}

/// Whether the pre-turn snapshot holds `bytes` under the name of one of the patches the
/// pre-turn spec declared (`declared`): a renumbered or renamed patch. Only those names
/// count; an unreferenced `.patch` file with the same bytes does not.
fn declared_before_holds(snap: &CheckoutSnapshot, declared: &[String], bytes: &[u8]) -> bool {
    declared
        .iter()
        .any(|name| snap.content_of(name).as_deref() == Some(bytes))
}

/// The patches `after` declares that the edit adds or changes, judged against
/// `before` and the pre-turn snapshot. Without a snapshot a declared patch is not
/// judged (there is nothing to compare its bytes with).
pub(super) fn judge_patches(
    spec_path: &Path,
    before: &str,
    after: &str,
    snapshot: Option<&CheckoutSnapshot>,
) -> Vec<JudgedPatch> {
    let dir = spec_path.parent().unwrap_or(Path::new("."));
    let before_declared = declared_patches(before);
    let declared_before: std::collections::HashSet<String> = before_declared
        .iter()
        .map(|(_, raw, name)| patch_key(raw, name))
        .collect();
    let declared_names: Vec<String> = before_declared.into_iter().map(|(_, _, n)| n).collect();
    let mut out = Vec::new();
    for (index, raw, name) in declared_patches(after) {
        let declared = declared_before.contains(&patch_key(&raw, &name));
        let plain = !is_web_address(&raw)
            && !name.is_empty()
            && !name.contains('%')
            && Path::new(&name).file_name() == Some(std::ffi::OsStr::new(&name));
        let bytes = if plain {
            std::fs::read(dir.join(&name)).ok()
        } else {
            None
        };
        if let (Some(bytes), Some(snap)) = (&bytes, snapshot) {
            let same_file = snap.content_of(&name).as_deref() == Some(bytes.as_slice());
            if same_file || declared_before_holds(snap, &declared_names, bytes) {
                continue;
            }
        }
        let status = if !declared {
            PatchStatus::New
        } else {
            match (snapshot, &bytes) {
                (Some(snap), Some(_)) if !is_web_address(&raw) => {
                    PatchStatus::Changed(snap.content_of(&name))
                }
                _ => continue,
            }
        };
        out.push(JudgedPatch {
            index,
            raw,
            name,
            bytes,
            status,
        });
    }
    out
}

/// The patches of an edit that are new: the rules judge their header, comment and
/// content. `(line index in after, value as written, file name with the macros
/// expanded)`.
pub(super) fn added_patches(
    spec_path: &Path,
    before: &str,
    after: &str,
    snapshot: Option<&CheckoutSnapshot>,
) -> Vec<(usize, String, String)> {
    judge_patches(spec_path, before, after, snapshot)
        .into_iter()
        .filter(|j| j.status == PatchStatus::New)
        .map(|j| (j.index, j.raw, j.name))
        .collect()
}

/// Whether `line` is the type 1 markup of the guidelines: a comment that starts with
/// `PATCH-FIX-` or `PATCH-FEATURE-` and names its kind (`OPENSUSE`, `SLE`, `UPSTREAM`).
pub(crate) fn is_patch_comment(line: &str) -> bool {
    let Some(rest) = line.trim().strip_prefix('#') else {
        return false;
    };
    let tag = rest.split_whitespace().next().unwrap_or("");
    let Some(kind) = tag
        .strip_prefix("PATCH-FIX-")
        .or_else(|| tag.strip_prefix("PATCH-FEATURE-"))
    else {
        return false;
    };
    ["OPENSUSE", "SLE", "UPSTREAM"].contains(&kind)
}

/// The lines of a patch file before its diff: up to a `---` line (the end of an
/// mbox commit message), an `Index:` or `diff --git` line, or a `---`/`+++` pair.
fn header_lines(text: &str) -> Vec<&str> {
    let lines: Vec<&str> = text.lines().collect();
    let end = lines
        .iter()
        .enumerate()
        .position(|(i, l)| {
            l.trim_end() == "---"
                || l.starts_with("Index: ")
                || l.starts_with("diff --git ")
                || (l.starts_with("--- ")
                    && lines.get(i + 1).is_some_and(|n| n.starts_with("+++ ")))
        })
        .unwrap_or(lines.len());
    lines[..end].to_vec()
}

/// The value of the header field `key` (`Subject`, `Upstream`).
fn header_field<'a>(header: &[&'a str], key: &str) -> Option<&'a str> {
    header.iter().find_map(|line| {
        let (k, v) = line.split_once(':')?;
        k.trim().eq_ignore_ascii_case(key).then(|| v.trim())
    })
}

/// Whether a header line is a `Key: value` field.
fn is_field_line(line: &str) -> bool {
    !line.starts_with(char::is_whitespace)
        && line.split_once(':').is_some_and(|(k, _)| {
            !k.is_empty() && k.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
        })
}

/// Whether the header describes the patch: a `Description:` with text after it, or
/// free text in the header (the commit message of a `git format-patch` mail, which
/// follows its `From:` and `Subject:` lines). A bare `Description:` does not count.
fn has_description(header: &[&str]) -> bool {
    let inline = header_field(header, "Description").is_some_and(|v| !v.is_empty());
    inline
        || header.iter().any(|line| {
            let t = line.trim();
            !t.is_empty() && !t.starts_with("From ") && !is_field_line(line)
        })
}

/// Without a parenthesised note at the end (`https://... (fixed in 2.0)`).
fn without_note(value: &str) -> &str {
    if value.ends_with(')')
        && let Some(at) = value.rfind('(')
    {
        return value[..at].trim_end();
    }
    value
}

/// An `Upstream:` value that names where the fix stands (convention 111): a URL, or
/// `not yet reported`, optionally after a status word (`submitted`, `merged`,
/// `never`, `no`, `tbd`, `dead`), and optionally followed by a note in parentheses.
fn is_upstream_ref(value: &str) -> bool {
    let value = without_note(value.trim());
    if value.eq_ignore_ascii_case("not yet reported") {
        return true;
    }
    let is_status = |w: &str| STATUS_WORDS.iter().any(|s| w.eq_ignore_ascii_case(s));
    let words: Vec<&str> = value.split_whitespace().collect();
    match words.as_slice() {
        [one] => is_status(one) || is_web_address(one),
        [status, url] => is_status(status) && is_web_address(url),
        _ => false,
    }
}

/// What the header of a patch file lacks. An X11 header (`Patch-Mainline:` with
/// `Git-commit:`) stands for the `Upstream:` line.
pub(crate) fn header_problems(text: &str) -> Vec<String> {
    let header = header_lines(text);
    let mut out = Vec::new();
    if header_field(&header, "Subject").is_none_or(|v| v.is_empty()) {
        out.push("has no Subject: line saying what it fixes".to_string());
    }
    if !has_description(&header) {
        out.push("has no Description: saying why the fix is needed".to_string());
    }
    let upstream = header_field(&header, "Upstream").is_some_and(is_upstream_ref)
        || header_field(&header, "Patch-Mainline").is_some_and(|v| !v.is_empty());
    if !upstream {
        out.push(
            "has no Upstream: line with an issue, PR or commit URL, or `not yet reported` \
             (or Patch-Mainline: for an X11 header)"
                .to_string(),
        );
    }
    out
}

/// Whether the `.changes` bullet words name the patch `name`: the name, as a whole
/// word or in backticks, after a verb (`Add foo.patch`, `Add patch foo.patch`) or
/// in backticks by itself.
pub(crate) fn names_in_bullet(words: &[&str], name: &str) -> bool {
    let clean = |w: &str| {
        w.trim_matches(|c: char| "`'\"()[]:,;".contains(c))
            .to_string()
    };
    words.iter().enumerate().any(|(i, w)| {
        clean(w) == name
            && (w.contains('`')
                || i > 0 && {
                    let prev = clean(words[i - 1]).to_ascii_lowercase();
                    prev == "patch" || PATCH_VERBS.contains(&prev.as_str())
                })
    })
}

/// The new patches of an edit that the turn's `.changes` bullet does not name.
pub(super) fn unnamed_new_patches(
    spec_path: &Path,
    before: &str,
    changelog: &str,
    snapshot: Option<&CheckoutSnapshot>,
) -> Vec<String> {
    let after = std::fs::read_to_string(spec_path).unwrap_or_default();
    let words: Vec<&str> = changelog.split_whitespace().collect();
    added_patches(spec_path, before, &after, snapshot)
        .into_iter()
        .map(|(_, _, name)| name)
        .filter(|name| !names_in_bullet(&words, name))
        .collect()
}

/// The problems the rules find in the edit: the spec lines, the patches it adds (the
/// header, the comment and the content) and the patches it changes (their content).
/// Missing patch files are left to the missing-source check.
pub(super) fn patch_rule_problems(
    spec_path: &Path,
    before: &str,
    after: &str,
    snapshot: Option<&CheckoutSnapshot>,
) -> Vec<String> {
    let lines: Vec<&str> = after.lines().collect();
    let mut out = spec_route_problems(before, after);
    for judged in judge_patches(spec_path, before, after, snapshot) {
        let raw = judged.raw.as_str();
        let name = judged.name.as_str();
        match judged.status {
            PatchStatus::New => {
                if raw.contains("%{version}") || raw.contains("%version") {
                    out.push(format!(
                        "Patch: {raw} has %{{version}} in its name: a patch keeps its file name \
                         across releases"
                    ));
                }
                let commented = judged
                    .index
                    .checked_sub(1)
                    .and_then(|i| lines.get(i))
                    .is_some_and(|l| is_patch_comment(l));
                if !commented {
                    out.push(format!(
                        "Patch: {raw} has no comment above it of the form `# PATCH-FIX-UPSTREAM \
                         {raw} <ref> -- why` (or PATCH-FIX-OPENSUSE, PATCH-FEATURE-...)"
                    ));
                }
                // A URL, or a name with a macro, is not read: only its comment is checked.
                if let Some(bytes) = &judged.bytes {
                    let text = String::from_utf8_lossy(bytes);
                    for problem in header_problems(&text)
                        .into_iter()
                        .chain(source_only_problems(&patch_delta(None, &text)))
                    {
                        out.push(format!("{name} {problem}"));
                    }
                }
            }
            PatchStatus::Changed(old) => {
                let Some(bytes) = &judged.bytes else {
                    continue;
                };
                let old = old.map(|b| String::from_utf8_lossy(&b).into_owned());
                let text = String::from_utf8_lossy(bytes);
                // A header requirement the old bytes met must still be met.
                if let Some(old) = &old {
                    let had = header_problems(old);
                    for problem in header_problems(&text) {
                        if !had.contains(&problem) {
                            out.push(format!("{name} (changed) {problem}"));
                        }
                    }
                }
                for problem in source_only_problems(&patch_delta(old.as_deref(), &text)) {
                    out.push(format!("{name} (changed) {problem}"));
                }
            }
        }
    }
    out
}

/// The acceptance check for the patches of an edit: an edit that breaks a patch rule
/// is rolled back the way a rejected spec is, and the rejection is noted. Returns the
/// reason when the edit was rolled back.
pub(super) fn reject_patch_rules(
    spec_path: &Path,
    before: &str,
    snapshot: Option<&CheckoutSnapshot>,
    step: &StepDir,
    reporter: &dyn Reporter,
    notes_path: PathBuf,
    keep: &[String],
    judge_removals: bool,
) -> Option<String> {
    let after = std::fs::read_to_string(spec_path).unwrap_or_else(|_| before.to_string());
    let problems = patch_rule_problems(spec_path, before, &after, snapshot);
    // Backstop for every accept path: a removed patch needs quilt's proof whatever
    // turn removed it. Empty (and no quilt run) when nothing was removed, and not
    // asked when `judge_removals` is false (a switch to the upstream wheel).
    let version = crate::spec::parse_header(&after)
        .map(|h| h.version)
        .unwrap_or_default();
    let checkout = spec_path.parent().unwrap_or(Path::new("."));
    let unproven = if judge_removals {
        unproven_patch_removals(&version, checkout, spec_path, before, snapshot)
    } else {
        Vec::new()
    };
    let mut reasons = Vec::new();
    if !problems.is_empty() {
        reasons.push(format!(
            "a patch the edit adds does not meet the patch rules: {}",
            problems.join("; ")
        ));
    }
    if !unproven.is_empty() {
        reasons.push(unproven_removal_problem(&unproven));
    }
    if reasons.is_empty() {
        return None;
    }
    let problem = reasons.join("; ");
    note_spec_rejection(spec_path, &problem);
    reporter.report(&Event::SpecRejected {
        step: step.id,
        reason: ap_core::event::SpecRejection::RpmspecParseFailed {
            stderr: problem.clone(),
        },
        restored_from: spec_path.to_path_buf(),
    });
    restore_spec(spec_path, before, Some(step), reporter);
    if let Some(snap) = snapshot {
        snap.restore_keeping(reporter, Some(step), keep);
    }
    append_notes(
        notes_path,
        &format!(
            "\n## Edit rejected — a patch rule or a patch removal or disable\n{problem}\n\
             A patch the edit adds needs a header with Subject:, Description: and \
             Upstream: (a URL, a status word, or `not yet reported`); a `Patch:` line with a \
             `# PATCH-FIX-...` comment above it; and changes to source code only (no removed \
             test, no `|| true`, no unexplained -Wno-error, no build-system change that skips \
             the broken part). A patch that is shell-applied in %prep or added as a Source: is \
             refused: use a `PatchN:` line. Fix those, keep the rest, and try again. The edit \
             was rolled back.\n"
        ),
    );
    Some(problem)
}

/// The `.changes` check of an edit: a new patch the bullet does not name is rolled
/// back and journaled. Returns the reason when the edit was rolled back.
#[allow(clippy::too_many_arguments)]
pub(super) fn reject_unnamed_patches(
    spec_path: &Path,
    before: &str,
    snapshot: Option<&CheckoutSnapshot>,
    step: &StepDir,
    reporter: &dyn Reporter,
    notes_path: PathBuf,
    keep: &[String],
    changelog: &str,
    label: &str,
) -> Option<String> {
    let unnamed = unnamed_new_patches(spec_path, before, changelog, snapshot);
    if unnamed.is_empty() {
        return None;
    }
    let problem = format!(
        "the .changes bullet does not name the new patch {}: write `Add|Backport|Modify <file>` \
         for each patch the edit adds",
        unnamed.join(", ")
    );
    restore_spec(spec_path, before, Some(step), reporter);
    if let Some(snap) = snapshot {
        snap.restore_keeping(reporter, Some(step), keep);
    }
    note_spec_rejection(spec_path, &problem);
    reporter.report(&Event::Warning {
        step: Some(step.id),
        message: format!("{label}: edit rejected: {problem}"),
    });
    append_notes(
        notes_path,
        &format!(
            "\n## {label} — edit rejected: a new patch is not named in .changes\n{problem}\n\
             The edit was rolled back.\n"
        ),
    );
    Some(problem)
}
