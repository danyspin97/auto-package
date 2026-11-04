//! The summary rows of the other-architecture check: the section lines and
//! the submit rule they follow.

use super::*;

fn row(pkg: &str, repo: &str, arch: &str, state: ArchState, baseline_fails: bool) -> ArchRow {
    ArchRow {
        pkg: pkg.to_string(),
        repo: repo.to_string(),
        arch: arch.to_string(),
        state,
        baseline: if baseline_fails {
            BaselineState::Fails
        } else {
            BaselineState::Builds
        },
        log: None,
    }
}

fn failed(detail: &str) -> ArchState {
    ArchState::Failed {
        detail: Some(detail.to_string()),
    }
}

#[test]
fn a_failure_the_origin_shares_does_not_block_submit() {
    let rows = vec![
        row("p", "ports", "s390x", failed("x"), true),
        row("p", "ports", "ppc64le", failed("y"), false),
        row("p", "ports", "riscv64", ArchState::Ignored, false),
    ];
    let blocking: Vec<&ArchRow> = rows.iter().filter(|r| r.blocks_submit()).collect();
    assert_eq!(blocking.len(), 1);
    assert_eq!(blocking[0].arch, "ppc64le");
}

#[test]
fn the_section_lists_each_failing_result_once() {
    let mut dup = row(
        "p",
        "ports",
        "s390x",
        failed("ld: cannot find -lfoo"),
        false,
    );
    dup.log = Some("/w/logs/p-s390x.log".to_string());
    let rows = vec![
        row("p", "standard", "i586", ArchState::Succeeded, false),
        dup.clone(),
        dup,
        row(
            "q",
            "ports",
            "armv7l",
            ArchState::Pending("building".into()),
            false,
        ),
        row("q", "ports", "riscv64", ArchState::Ignored, false),
        row(
            "p",
            "ports",
            "aarch64",
            ArchState::Unresolvable {
                detail: Some("missing foo-devel".into()),
            },
            true,
        ),
    ];
    assert_eq!(
        section_lines(&rows),
        vec![
            "p ports/s390x: failed: ld: cannot find -lfoo [log: /w/logs/p-s390x.log]",
            "q ports/armv7l: still building",
            "p ports/aarch64: unresolvable: missing foo-devel (also fails in the origin)",
        ]
    );
}

#[test]
fn an_all_green_check_has_no_section() {
    let rows = vec![row("p", "ports", "s390x", ArchState::Succeeded, false)];
    assert!(section_lines(&rows).is_empty());
}
