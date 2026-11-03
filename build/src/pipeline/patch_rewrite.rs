//! Telling a patch whose code changed from one that was only refreshed: a
//! header, an offset, a context line or a path prefix is not a rewrite.

use super::*;
use crate::pipeline::patch_rules_content::patch_changes;
use crate::pipeline::removal_proof::is_checkout_file;
use std::collections::BTreeMap;

/// `path` without its first `level` parts.
fn strip(path: &str, level: u32) -> String {
    path.split('/')
        .filter(|p| !p.is_empty())
        .skip(level as usize)
        .collect::<Vec<_>>()
        .join("/")
}

/// The code a patch changes: `(file, '+' | '-' | 'd', line)`, sorted, with the
/// file names stripped of `level` leading parts. The header, the offsets and the
/// context are not in it.
pub(super) fn patch_code(text: &str, level: u32) -> Vec<(String, char, String)> {
    let changes = patch_changes(text);
    let mut out: Vec<(String, char, String)> = Vec::new();
    for a in changes.added {
        out.push((strip(&a.file, level), '+', a.text));
    }
    for (file, text) in changes.removed {
        out.push((strip(&file, level), '-', text));
    }
    for file in changes.deleted {
        out.push((strip(&file, level), 'd', String::new()));
    }
    out.sort();
    out
}

/// Whether the code changed between `old` (`None`: the patch is new) and `new`.
/// Equal bytes are unchanged without parsing.
pub(super) fn code_changed(old: Option<&[u8]>, new: &[u8], level: u32) -> bool {
    let Some(old) = old else {
        return true;
    };
    if old == new {
        return false;
    }
    patch_code(&String::from_utf8_lossy(old), level)
        != patch_code(&String::from_utf8_lossy(new), level)
}

/// The patches `spec` declares now whose code differs from `before` (from
/// `patch_files`), in spec order. A patch new since `before` is in it, a removed
/// one is not. `accepted` holds the bytes a review already accepted: a patch
/// still at those bytes is left out.
pub(super) fn rewritten_patches(
    spec: &str,
    checkout: &Path,
    before: &BTreeMap<String, Option<Vec<u8>>>,
    accepted: &BTreeMap<String, Vec<u8>>,
) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for (number, name) in crate::patch_check::declared_patches(spec) {
        if !is_checkout_file(&name) || out.contains(&name) {
            continue;
        }
        let Ok(now) = std::fs::read(checkout.join(&name)) else {
            continue;
        };
        if accepted.get(&name) == Some(&now) {
            continue;
        }
        let old = before.get(&name).and_then(|o| o.as_deref());
        let level = spec_patch_level(spec, &number);
        if code_changed(old, &now, level) {
            out.push(name);
        }
    }
    out
}
