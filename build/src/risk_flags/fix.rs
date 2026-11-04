//! The risks of a bug fix: a patch no upstream commit stands behind, and an
//! upstream patch that was changed to apply here.

use super::*;

pub use crate::bugfix::code_lines;

/// Whether the header of `patch` says its `Upstream:` is `not yet reported`.
pub fn says_not_yet_reported(patch: &str) -> bool {
    patch
        .lines()
        .take_while(|l| !l.starts_with("@@"))
        .filter_map(|l| l.split_once(':'))
        .any(|(k, v)| {
            k.trim().eq_ignore_ascii_case("upstream")
                && v.trim()
                    .to_ascii_lowercase()
                    .starts_with("not yet reported")
        })
}

/// The flags of a bug fix. A patch the fix added (in `after`, not in `before`)
/// that was not staged from upstream was written by the agent: whatever its
/// `Upstream:` line says, the tool did not verify it, so it is a fix without an
/// upstream commit. A staged patch (name and the bytes as fetched) whose
/// `Upstream:` is `not yet reported` is one too, and one whose code differs
/// from what is committed was adapted; a patch refreshed only by offset or
/// context is not. A staged patch in `unmerged` (a pull or merge request, or a
/// commit that nothing shows is on the default branch) is not upstream yet.
pub fn bug_fix_risks(
    staged: &[(String, String)],
    unmerged: &[String],
    before: &Files,
    after: &Files,
) -> Vec<RiskFlag> {
    let mut out = Vec::new();
    for (name, text) in &after.patches {
        if before.patches.contains_key(name) {
            continue;
        }
        let is_staged = staged.iter().any(|(n, _)| n == name);
        if !is_staged || says_not_yet_reported(text) {
            out.push(RiskFlag::FixWithoutUpstream {
                patch: name.clone(),
            });
        }
    }
    for (name, fetched) in staged {
        let Some(committed) = after.patches.get(name) else {
            continue;
        };
        let (mut a, mut b) = (code_lines(fetched), code_lines(committed));
        a.sort();
        b.sort();
        if a != b {
            out.push(RiskFlag::AdaptedUpstreamFix {
                patch: name.clone(),
            });
        }
        if unmerged.contains(name) {
            out.push(RiskFlag::UnmergedUpstreamFix {
                patch: name.clone(),
            });
        }
    }
    out
}
