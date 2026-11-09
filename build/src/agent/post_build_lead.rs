//! The lead of a failure hint whose failure is a post-build check.
//!
//! Such a build has compiled, passed its tests and written its RPMs; the OBS
//! `50-check-*` script or rpm's file-list check then marked it failed. Without
//! a lead the hint is a log excerpt, and a turn that reads "N passed" answers
//! that the build succeeded (python-kivy, run 20261109-173516, twice).

use crate::log::ExtractedLog;

/// The lead for `hint`, `None` when its failure is not a post-build check.
pub(super) fn post_build_lead(hint: &ExtractedLog) -> Option<String> {
    let failure = hint.failure.as_ref().filter(|f| f.is_post_build_check())?;
    let check = failure
        .excerpt
        .lines()
        .find_map(|l| l.strip_prefix("... running "))
        .map(|name| format!(" `{}`", name.trim()));

    let mut s = String::from("## The failure is a post-build check\n\n");
    s.push_str(&format!(
        "The build compiled, its tests passed and it wrote its RPMs. It is marked failed by \
         the post-build check{} on those RPMs, and the excerpt below is that check's complaint. \
         It is not a compile, test or scriptlet error: output above the complaint that looks \
         like an error (test summaries, logging at exit) is not the failure.\n\n",
        check.unwrap_or_default(),
    ));
    s.push_str(
        "A `no_change` answer has to name this check and show why its complaint does not \
         hold. That the tests passed is not an answer: the build failed because of the check.\n\n",
    );
    if failure.excerpt.contains("found conflict of") {
        s.push_str(
            "Two subpackages install the same file. Convention 118 says which one ships it and \
             how the install list is written.\n\n",
        );
    }
    Some(s)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::log::{PathMapper, extract};

    const TAIL: &str = "\
[  490s] ... running 50-check-packaged-twice
[  491s] found conflict of python313-Kivy-devel-2.3.2 with python314-Kivy-devel-2.3.2:
[  491s]   - /usr/lib64/python3.13/site-packages/kivy/include/config.h
";

    #[test]
    fn a_packaged_twice_failure_is_named_and_routed_to_its_convention() {
        let lead = post_build_lead(&extract(TAIL, &PathMapper::default())).expect("a lead");
        assert!(lead.starts_with("## The failure is a post-build check"));
        assert!(lead.contains("`50-check-packaged-twice`"));
        assert!(lead.contains("Convention 118"));
        assert!(lead.contains("`no_change` answer has to name this check"));
    }

    #[test]
    fn a_failed_section_gets_no_post_build_lead() {
        let log = "\
[   1s] Executing(%check): /usr/bin/bash -e /var/tmp/rpm-tmp.a
[   2s] 1 failed, 12 passed
[   3s] error: Bad exit status from /var/tmp/rpm-tmp.a (%check)
";
        assert!(post_build_lead(&extract(log, &PathMapper::default())).is_none());
    }
}
