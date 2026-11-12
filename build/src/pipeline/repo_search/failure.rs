//! How a failed repository search is worded.

/// A search error on one line: gh and the HTTP layer can both return several.
pub(super) fn one_line(err: &str) -> String {
    let first = err.lines().map(str::trim).find(|l| !l.is_empty());
    first.unwrap_or("unknown error").to_string()
}

/// The last line of a search that verified nothing. When a search itself
/// failed, "nothing found" is not an answer, so say so and name the way out.
pub(super) fn none_found_line(tried: usize, search_failed: bool) -> String {
    if search_failed {
        format!(
            "  ✗ no verified repositories found after trying {tried} URLs; the GitHub search \
             failed, so this is not conclusive. If you know the repository, run \
             `auto-package build <repo url>`"
        )
    } else {
        format!("  ✗ no verified repositories found after trying {tried} URLs")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_line_keeps_only_the_first_line() {
        let e = "gh api x: To get started, run: gh auth login\nAlternatively, set GH_TOKEN";
        assert_eq!(one_line(e), "gh api x: To get started, run: gh auth login");
        assert_eq!(one_line(""), "unknown error");
    }

    #[test]
    fn failed_search_makes_the_result_non_authoritative() {
        let l = none_found_line(5, true);
        assert!(l.contains("after trying 5 URLs"));
        assert!(l.contains("failed") && l.contains("not conclusive"));
        assert!(l.contains("auto-package build <repo url>"));
        let ok = none_found_line(5, false);
        assert!(!ok.contains("failed") && !ok.contains("auto-package"));
    }
}
