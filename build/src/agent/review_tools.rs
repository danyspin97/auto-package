//! The tools of a PatchReview turn: quilt to look at and fix one patch, Edit and
//! Write only in `source/` and on the named patch files of the checkout.

use super::*;

/// The quilt subcommands a PatchReview turn may run. Not granted: `new`,
/// `delete`, `import`, `rename`, `fold`, `header`, `snapshot` and `revert`, so
/// the turn cannot create, delete or rename a patch, or rewrite a header.
pub(super) const REVIEW_QUILT_SUBCOMMANDS: [&str; 15] = [
    "push",
    "pop",
    "refresh",
    "add",
    "remove",
    "diff",
    "files",
    "series",
    "top",
    "applied",
    "unapplied",
    "patches",
    "next",
    "previous",
    "grep",
];

/// Whether `name` may reach a tool grant or a command line as a patch file name:
/// plain characters only, not starting with `-` or `.`. A name from the spec can
/// hold a glob, a comma, a space or a parenthesis, which would widen a grant.
pub(crate) fn safe_review_patch_name(name: &str) -> bool {
    !name.is_empty()
        && !name.starts_with(['-', '.'])
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "._+-".contains(c))
}

/// The allow-list of a PatchReview turn. Reads the workspace and the checkout;
/// writes `<workspace>/source/**` and, in the checkout, the files of `patches`
/// that pass [`safe_review_patch_name`] and nothing else. There is no byte-edit
/// helper: a file that is not UTF-8 is not edited by a review.
pub(super) fn review_allowed_tools(
    workspace: &std::path::Path,
    checkout: Option<&std::path::Path>,
    patches: &[String],
) -> String {
    let mut tools = vec!["Grep".to_string(), "Skill".into(), "Bash(ls *)".into()];
    for prefix in source_prefixes(workspace) {
        for sub in REVIEW_QUILT_SUBCOMMANDS {
            tools.push(format!("Bash({prefix}quilt {sub} *)"));
        }
    }
    let ws = permission_path(workspace);
    tools.push(format!("Read({ws}/**)"));
    tools.push(format!("Edit({ws}/source/**)"));
    tools.push(format!("Write({ws}/source/**)"));
    if let Some(checkout) = checkout {
        let co = permission_path(checkout);
        tools.push(format!("Read({co}/**)"));
        for name in patches.iter().filter(|n| safe_review_patch_name(n)) {
            tools.push(format!("Edit({co}/{name})"));
            tools.push(format!("Write({co}/{name})"));
        }
    }
    tools.join(",")
}

/// What a PatchReview turn may not write in the checkout: the spec, the
/// `.changes` and the `_service`. They add to [`turn_denied_tools`].
pub(super) fn review_denied_tools(checkout: Option<&std::path::Path>) -> Vec<String> {
    let Some(checkout) = checkout else {
        return Vec::new();
    };
    let p = permission_path(checkout);
    let mut out = Vec::new();
    for tool in ["Edit", "Write", "NotebookEdit"] {
        for name in ["*.spec", "*.changes", "_service"] {
            out.push(format!("{tool}({p}/{name})"));
        }
    }
    out
}
