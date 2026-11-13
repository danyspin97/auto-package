//! Bullets of release notes that are no news for a `.changes` recap: CI,
//! dependency bumps, docs, tests and style changes.

/// Prefixes (lowercase) of the bullets that are routine: a conventional-commit
/// type, a bot's bump, or a pre-commit autoupdate.
const ROUTINE: &[&str] = &[
    "ci:",
    "ci(",
    "chore",
    "build(deps",
    "bump ",
    "update dependabot",
    "[pre-commit.ci]",
    "docs:",
    "test:",
    "tests:",
    "style:",
    "deps:",
    "deps(",
    "pre-commit autoupdate",
];

/// Phrases of the housekeeping of a repository that a package user does not
/// need: its CI, its tooling, its badges, its docs and its release process.
const HOUSEKEEPING: &[&str] = &[
    "github action",
    "github workflow",
    "workflow",
    "dependabot",
    "pre-commit",
    "lint config",
    "linter config",
    "linting config",
    "social preview",
    "release step",
    "release process",
    "release checklist",
    "badge",
    "readme",
    "docs build",
    "documentation build",
    "codecov",
];

/// Words of a note that name what a package user gets: a fix, a removal, a
/// behaviour, an API, a feature or a Python or platform version. A housekeeping
/// note that names one of them stays.
const USER_FACING: &[&str] = &[
    "fix",
    "cve",
    "security",
    "remov",
    "drop",
    "deprecat",
    "break",
    "behav",
    "api",
    "feature",
    "support",
    "python",
    "platform",
    "linux",
    "windows",
    "macos",
    "option",
    "argument",
    "parameter",
    "default",
    "error",
    "exception",
    "crash",
];

/// Whether `item` (a bullet's text, markup removed) is routine and so left out
/// of a recap. A bullet that also names a fix, a CVE or a security issue stays.
/// So does a bullet that names a feature, a removal, a behaviour, an API or a
/// version of Python or of a platform, whatever housekeeping it mentions.
pub(super) fn is_routine(item: &str) -> bool {
    let item = item
        .strip_prefix("- ")
        .or_else(|| item.strip_prefix("* "))
        .unwrap_or(item)
        .trim_start()
        .to_ascii_lowercase();
    let news = ["fix", "cve", "security"].iter().any(|w| item.contains(w));
    if !news && ROUTINE.iter().any(|p| item.starts_with(p)) {
        return true;
    }
    typo_in_docs(&item) || (is_housekeeping(&item) && !USER_FACING.iter().any(|w| item.contains(w)))
}

/// The words of `item` (lowercase), without the punctuation around them.
fn words_of(item: &str) -> Vec<&str> {
    item.split(|c: char| !c.is_alphanumeric() && c != '-')
        .filter(|w| !w.is_empty())
        .collect()
}

/// Whether `item` (lowercase) names one of the [`HOUSEKEEPING`] phrases or a
/// `ci` word.
fn is_housekeeping(item: &str) -> bool {
    HOUSEKEEPING.iter().any(|p| item.contains(p)) || words_of(item).contains(&"ci")
}

/// Whether `item` (lowercase) is a typo fix in the docs: it fixes a typo and
/// names the docs. Such a fix is no news whatever its wording.
fn typo_in_docs(item: &str) -> bool {
    let words = words_of(item);
    words.iter().any(|w| w.starts_with("typo"))
        && words
            .iter()
            .any(|w| ["docs", "documentation", "readme", "doc"].contains(w))
}

#[cfg(test)]
mod tests {
    use super::is_routine;

    #[test]
    fn housekeeping_is_routine_and_user_facing_notes_stay() {
        assert!(is_routine(
            "Add the social preview image of the repository and document its release steps. See PR #113."
        ));
        assert!(is_routine("Update the GitHub Actions workflow to use uv"));
        assert!(is_routine("Add a pre-commit hook for ruff"));
        assert!(is_routine("Show the coverage badge in the README"));
        assert!(is_routine("Fix a typo in the docs"));
        assert!(is_routine("Tidy the ci setup"));
        assert!(!is_routine("Add the Google docstring decorators"));
        assert!(!is_routine(
            "Drop the Python 3.9 support from the docs workflow"
        ));
        assert!(!is_routine("Add a new option to the CI-free build"));
        assert!(!is_routine("Support Python 3.15 in the docs build"));
        assert!(!is_routine("Remove the legacy session class"));
    }
}
