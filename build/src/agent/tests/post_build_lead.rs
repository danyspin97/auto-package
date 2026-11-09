//! The turn's prompt leads a post-build check failure with the lead that says
//! the tests passed and the check is the failure, so "no change" is not a
//! plausible answer (python-kivy, run 20261109-173516).

use super::prompt::request;
use crate::agent::build_prompt::build_turn_prompt;
use crate::log::{PathMapper, extract};

const PACKAGED_TWICE: &str = "\
[  465s] = 813 passed, 27 skipped, 84 deselected, 1 xfailed in 45.20s =
[  491s] ... running 50-check-packaged-twice
[  491s] found conflict of python313-Kivy-devel-2.3.2 with python314-Kivy-devel-2.3.2:
[  491s]   - /usr/lib64/python3.13/site-packages/kivy/include/config.h
";

const BAD_SECTION: &str = "\
[   1s] Executing(%check): /usr/bin/bash -e /var/tmp/rpm-tmp.a
[   2s] 1 failed, 12 passed
[   3s] error: Bad exit status from /var/tmp/rpm-tmp.a (%check)
";

fn prompt_for(log: &str) -> String {
    let ws = tempfile::tempdir().unwrap();
    let extracted = extract(log, &PathMapper::with_sdist_root("Kivy-2.3.2"));
    let req = crate::agent::TurnRequest {
        task: crate::agent::Task::FixAfterBuild,
        hint: Some(&extracted),
        ..request(ws.path(), &[])
    };
    build_turn_prompt(&req)
}

#[test]
fn a_post_build_check_failure_leads_the_hint_and_names_the_check() {
    let p = prompt_for(PACKAGED_TWICE);
    let lead = p
        .find("## The failure is a post-build check")
        .expect("the lead is in the prompt");
    let excerpt = p.find("Excerpt:").expect("the excerpt is in the prompt");
    assert!(lead < excerpt, "the lead must come before the excerpt");
    assert!(p.contains("`50-check-packaged-twice`"));
    assert!(p.contains("found conflict of python313-Kivy-devel-2.3.2"));
}

#[test]
fn a_failed_section_gets_no_post_build_lead() {
    let p = prompt_for(BAD_SECTION);
    assert!(!p.contains("## The failure is a post-build check"));
}
