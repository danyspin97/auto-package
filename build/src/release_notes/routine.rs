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

/// Whether `item` (a bullet's text, markup removed) is routine and so left out
/// of a recap. A bullet that also names a fix, a CVE or a security issue stays.
pub(super) fn is_routine(item: &str) -> bool {
    let item = item
        .strip_prefix("- ")
        .or_else(|| item.strip_prefix("* "))
        .unwrap_or(item)
        .trim_start()
        .to_ascii_lowercase();
    let news = ["fix", "cve", "security"].iter().any(|w| item.contains(w));
    !news && ROUTINE.iter().any(|p| item.starts_with(p))
}
