use super::patch_block_quilt::drop_blocks_quilt_clears;
use crate::agent::{PatchAnswer, PatchAnswerStatus};
use crate::patch_check::{CheckReport, PatchResult, PatchStatus};

fn blocked(name: &str) -> PatchAnswer {
    PatchAnswer {
        name: name.to_string(),
        status: PatchAnswerStatus::Blocked,
        reason: "Applies cleanly ... listed for completeness".to_string(),
    }
}

fn result(name: &str, status: PatchStatus) -> PatchResult {
    PatchResult {
        name: name.to_string(),
        level: 1,
        status,
        refreshed: None,
    }
}

#[test]
fn a_block_on_a_patch_quilt_applies_cleanly_is_dropped() {
    let report = CheckReport {
        patches: vec![
            result(
                "move-pytest-configuration-to-conftest.patch",
                PatchStatus::Applies,
            ),
            result(
                "other.patch",
                PatchStatus::Fails {
                    output: "failed".to_string(),
                    rejects: String::new(),
                },
            ),
        ],
        not_run: None,
    };
    let mut answers = vec![
        blocked("move-pytest-configuration-to-conftest.patch"),
        blocked("other.patch"),
        blocked("absent.patch"),
    ];
    let mut lines = Vec::new();
    drop_blocks_quilt_clears(&mut answers, &report, &mut lines);
    let names: Vec<&str> = answers.iter().map(|a| a.name.as_str()).collect();
    assert_eq!(names, ["other.patch", "absent.patch"]);
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert!(lines[0].starts_with("- move-pytest-configuration-to-conftest.patch"));
}

#[test]
fn a_block_on_a_patch_with_fuzz_or_an_offset_is_kept() {
    let report = CheckReport {
        patches: vec![result(
            "fuzzy.patch",
            PatchStatus::AppliesWithOffsetOrFuzz {
                fuzz: true,
                output: String::new(),
            },
        )],
        not_run: None,
    };
    let mut answers = vec![blocked("fuzzy.patch")];
    let mut lines = Vec::new();
    drop_blocks_quilt_clears(&mut answers, &report, &mut lines);
    assert_eq!(answers.len(), 1);
    assert!(lines.is_empty());
}
