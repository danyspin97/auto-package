//! The auto-package build behind a run: the revision `cli/build.rs` embedded
//! in the binary. It is shown in the run banner, in `--version` and recorded
//! in the run's `run_started` event. `cli/build.rs` includes this file, so it
//! must stay free of crate imports.

/// What is shown when no revision was embedded (no jj or git, or the call
/// failed).
pub const UNKNOWN: &str = "unknown";

/// The revision to show: the embedded id, or `unknown` when there is none.
pub fn revision_label(revision: &str) -> &str {
    let revision = revision.trim();
    if revision.is_empty() {
        UNKNOWN
    } else {
        revision
    }
}

/// The `--version` line: the crate version and the build revision.
pub fn version_line(package_version: &str, revision: &str) -> String {
    format!("{package_version} (build {})", revision_label(revision))
}

/// The commit id with a `-dirty` suffix when the tree has changes. `None`
/// unless the commit is a hex id.
pub fn stamp(commit: &str, dirty: bool) -> Option<String> {
    let commit = commit.trim();
    let hex = !commit.is_empty() && commit.chars().all(|c| c.is_ascii_hexdigit());
    hex.then(|| {
        if dirty {
            format!("{commit}-dirty")
        } else {
            commit.to_string()
        }
    })
}

/// The answer of a `if(empty, "clean", "dirty")` jj template; anything else
/// is `None`.
pub fn dirty_state(output: &str) -> Option<bool> {
    match output.trim() {
        "clean" => Some(false),
        "dirty" => Some(true),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_line_names_the_revision_or_unknown() {
        assert_eq!(
            version_line("0.1.0", "3f2a9c1d8e7b"),
            "0.1.0 (build 3f2a9c1d8e7b)"
        );
        assert_eq!(version_line("0.1.0", UNKNOWN), "0.1.0 (build unknown)");
        assert_eq!(version_line("0.1.0", ""), "0.1.0 (build unknown)");
        assert_eq!(version_line("0.1.0", " \n"), "0.1.0 (build unknown)");
    }

    #[test]
    fn a_clean_commit_is_stamped_bare() {
        assert_eq!(
            stamp("3f2a9c1d8e7b\n", false).as_deref(),
            Some("3f2a9c1d8e7b")
        );
    }

    #[test]
    fn uncommitted_edits_add_a_dirty_suffix() {
        assert_eq!(
            stamp("3f2a9c1d8e7b", true).as_deref(),
            Some("3f2a9c1d8e7b-dirty")
        );
    }

    #[test]
    fn only_a_hex_commit_id_is_stamped() {
        assert_eq!(stamp("", false), None);
        assert_eq!(stamp("3f2a\n9c1d", false), None);
        assert_eq!(stamp("error: no repo", true), None);
    }

    #[test]
    fn the_jj_template_answer_is_read_strictly() {
        assert_eq!(dirty_state("clean\n"), Some(false));
        assert_eq!(dirty_state("dirty"), Some(true));
        assert_eq!(dirty_state(""), None);
        assert_eq!(dirty_state("Error: stale"), None);
    }
}
