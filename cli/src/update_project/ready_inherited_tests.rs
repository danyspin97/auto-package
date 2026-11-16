//! The ready gate's rule for a build failure the origin has too: it does not
//! keep a package from being submitted unless `lint --block-inherited-failures`
//! is given. A failure the branch causes, or one the target has no result for,
//! blocks either way.

use super::ready::{Judging, Verdict, judge, settled};
use crate::{Cli, Commands};
use ap_build::arch_check::{ArchState, Baseline, GateRow};
use ap_build::request_review::build_check::build_findings;
use ap_build::request_review::{Finding, ids};
use clap::Parser;

const DEFAULT: Judging = Judging {
    agent: true,
    block_inherited: false,
};
const FLAGGED: Judging = Judging {
    agent: true,
    block_inherited: true,
};

fn failed() -> ArchState {
    ArchState::Failed {
        detail: Some("boom".into()),
    }
}

/// The findings of a failure on Tumbleweed/x86_64. `origin` is what the
/// origin has for it: `None` when it was not read, `Some(None)` when it has no
/// result, else the result's state.
fn failure_with_origin(origin: Option<Option<ArchState>>) -> Vec<Finding> {
    let rows = [GateRow {
        repo: "Tumbleweed".into(),
        arch: "x86_64".into(),
        state: failed(),
        baseline: origin.map(|state| Baseline {
            project: "Base:System".into(),
            state,
        }),
    }];
    build_findings("pkg", &rows, &[], None)
}

#[test]
fn a_failure_the_origin_has_too_blocks_only_with_the_flag() {
    let found = failure_with_origin(Some(Some(failed())));
    let plain = judge("abc", found.iter(), DEFAULT);
    assert!(plain.ready(), "{:?}", plain.reasons);
    let flagged = judge("abc", found.iter(), FLAGGED);
    assert!(!flagged.ready());
    assert_eq!(flagged.reasons, [ids::BUILD_FAILS_IN_TARGET_TOO]);
}

#[test]
fn a_new_failure_blocks_with_and_without_the_flag() {
    let found = failure_with_origin(Some(Some(ArchState::Succeeded)));
    for judging in [DEFAULT, FLAGGED] {
        let verdict = judge("abc", found.iter(), judging);
        assert!(!verdict.ready(), "{judging:?}");
        assert_eq!(verdict.reasons, [ids::BUILD_FAILED], "{judging:?}");
    }
}

#[test]
fn a_failure_with_no_origin_result_known_blocks_as_before() {
    let not_read = failure_with_origin(None);
    let no_result = failure_with_origin(Some(None));
    for judging in [DEFAULT, FLAGGED] {
        let verdict = judge("abc", not_read.iter(), judging);
        assert_eq!(verdict.reasons, [ids::BUILD_FAILED], "{judging:?}");
        let verdict = judge("abc", no_result.iter(), judging);
        assert_eq!(
            verdict.reasons,
            [ids::BUILD_NO_TARGET_RESULT],
            "{judging:?}"
        );
    }
}

#[test]
fn a_verdict_judged_one_way_is_not_reused_the_other_way() {
    let found = failure_with_origin(Some(Some(failed())));
    let plain = judge("abc", found.iter(), DEFAULT);
    assert!(plain.judged("abc", DEFAULT));
    assert!(!plain.judged("abc", FLAGGED));
    let flagged = judge("abc", found.iter(), FLAGGED);
    assert!(flagged.judged("abc", FLAGGED));
    assert!(!flagged.judged("abc", DEFAULT));
    let (kept, was_kept) = settled(Some(plain), "abc", FLAGGED, || Ok(flagged.clone())).unwrap();
    assert!(!was_kept);
    assert_eq!(kept, flagged);
}

#[test]
fn a_verdict_written_before_the_flag_was_kept_is_judged_again() {
    let old = Verdict {
        srcmd5: "abc".into(),
        reasons: Vec::new(),
        needs_agent: false,
        details: Vec::new(),
        block_inherited: None,
    };
    assert!(!old.judged("abc", DEFAULT));
}

#[test]
fn lint_takes_block_inherited_failures_and_update_does_not() {
    let lint = Cli::try_parse_from([
        "auto-package",
        "lint",
        "--project",
        "Base:System",
        "--block-inherited-failures",
    ]);
    match lint.unwrap().command {
        Commands::Lint(lint) => assert!(lint.block_inherited_failures),
        _ => panic!("not lint"),
    }
    let plain = Cli::try_parse_from(["auto-package", "lint", "--project", "Base:System"]);
    match plain.unwrap().command {
        Commands::Lint(lint) => assert!(!lint.block_inherited_failures),
        _ => panic!("not lint"),
    }
    let update = Cli::try_parse_from([
        "auto-package",
        "update",
        "--project",
        "Base:System",
        "--block-inherited-failures",
    ]);
    assert!(
        update.is_err(),
        "update takes no --block-inherited-failures"
    );
}
