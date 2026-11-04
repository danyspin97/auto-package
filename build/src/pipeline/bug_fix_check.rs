//! What a fix must be once it is in the spec, checked on the files. The staged
//! upstream patch is not judged by the patch rules when the turn leaves it as it
//! is, so these checks cover what the rules would have said about it.

use super::*;
use crate::pipeline::patch_rules::*;
use crate::pipeline::patch_rules_content::*;

/// The patch that carries the fix, or why the checkout does not hold a fix.
/// `expected` is the staged patch's file name, when there is one; without it
/// the fix is the one `PatchN:` line that the spec declares and `baseline` (the
/// patches declared before the stage) does not.
///
/// The fix patch must be declared and applied by the spec, have a header with a
/// subject, a description and an `Upstream:` line, have a `PATCH-FIX-...` comment
/// directly above its `PatchN:` line that names every reference the user gave,
/// and change more than tests.
pub(super) fn check_fix(
    spec: &str,
    checkout: &Path,
    refs: &[String],
    expected: Option<&str>,
    baseline: &[String],
) -> Result<String, String> {
    let declared = declared_patches(spec);
    let (index, name) = match expected {
        Some(name) => declared
            .iter()
            .find(|(_, _, n)| n == name)
            .map(|(i, _, n)| (*i, n.clone()))
            .ok_or_else(|| format!("the spec does not declare {name}"))?,
        None => {
            let new: Vec<&(usize, String, String)> = declared
                .iter()
                .filter(|(_, _, n)| !baseline.contains(n))
                .collect();
            match new.as_slice() {
                [] => return Err("the edit adds no PatchN: line: the fix is a new patch".into()),
                [one] => (one.0, one.2.clone()),
                many => {
                    let names: Vec<&str> = many.iter().map(|p| p.2.as_str()).collect();
                    return Err(format!(
                        "the edit adds {} patches ({}): one fix, one patch",
                        many.len(),
                        names.join(", ")
                    ));
                }
            }
        }
    };
    if !crate::backport::spec_applies(spec, &name) {
        return Err(format!(
            "the spec declares {name} but does not apply it (no %autosetup, %autopatch or %patch line for it)"
        ));
    }
    let bytes = std::fs::read(checkout.join(&name))
        .map_err(|e| format!("the checkout has no {name}: {e}"))?;
    let text = String::from_utf8_lossy(&bytes);
    let problems = header_problems(&text);
    if !problems.is_empty() {
        return Err(format!("{name} {}", problems.join("; ")));
    }
    let comment = index
        .checked_sub(1)
        .and_then(|i| spec.lines().nth(i))
        .filter(|l| is_patch_comment(l))
        .ok_or_else(|| {
            format!(
                "{name} has no `# PATCH-FIX-UPSTREAM {name} <references> -- why` comment (or \
                 PATCH-FIX-OPENSUSE) on the line above its PatchN: line"
            )
        })?;
    let missing = crate::maintenance::missing_refs(comment, refs);
    if !missing.is_empty() {
        return Err(format!(
            "the comment above {name} does not name {}",
            missing.join(", ")
        ));
    }
    let changes = patch_changes(&text);
    let files: Vec<&str> = changes
        .added
        .iter()
        .map(|l| l.file.as_str())
        .chain(changes.removed.iter().map(|(f, _)| f.as_str()))
        .chain(changes.deleted.iter().map(String::as_str))
        .collect();
    if !files.is_empty() && files.iter().all(|f| is_test_path(f)) {
        return Err(format!(
            "{name} changes only tests ({}): it does not fix the code",
            files.join(", ")
        ));
    }
    Ok(name)
}
