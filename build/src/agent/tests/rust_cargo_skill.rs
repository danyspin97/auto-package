//! The `rust-cargo-packaging` skill covers the rust flags, the private
//! features and the vendor tarball, and a Rust crate's review prompt points to it.

use super::prompt::request;
use super::*;
use crate::agent::build_prompt::*;

const SKILL: &str = include_str!("../../../../packaging-skills/rust-cargo-packaging/SKILL.md");

/// The section of the skill from `start` to `end`, its line breaks folded.
fn section(text: &str, start: &str, end: &str) -> String {
    let at = text
        .find(start)
        .unwrap_or_else(|| panic!("the skill has no {start:?} section"));
    let rest = &text[at..];
    rest[..rest.find(end).unwrap_or(rest.len())]
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

#[test]
fn the_skill_says_how_to_pass_rust_flags_without_the_encoded_variable() {
    let s = section(SKILL, "R4c. Rust flags.", "R4d.");
    assert!(s.contains("export RUSTFLAGS=\"--cfg <flag>\""), "{s}");
    assert!(s.contains("`%define __rustflags`"), "{s}");
    assert!(s.contains("Do not write `CARGO_ENCODED_RUSTFLAGS`"), "{s}");
    assert!(s.contains("Plain quotes are enough."), "{s}");
}

#[test]
fn the_skill_points_a_private_dependency_feature_at_the_crate_features() {
    let s = section(SKILL, "R4d. Features.", "R5.");
    for want in [
        "`--features` names only features the crate declares",
        "`rustix`",
        "`--no-default-features`",
        "vendored tree",
    ] {
        assert!(s.contains(want), "R4d lacks {want:?}");
    }
}

#[test]
fn the_skill_regenerates_the_vendor_tarball_through_the_service_and_never_blocks_on_the_sandbox() {
    let s = section(SKILL, "R4b. `vendor.tar.zst`", "R4c.");
    for want in [
        "Do not edit `Cargo.lock` by hand",
        "`mode=\"manual\"` (convention 77 (section 92))",
        "`Source1: vendor.tar.zst`",
        "`update-crate`",
        "not a reason to answer `blocked` or `no_change`",
    ] {
        assert!(s.contains(want), "R4b lacks {want:?}");
    }
}

#[test]
fn a_rust_crate_review_prompt_names_the_skill() {
    let ws = std::path::Path::new("/nonexistent-workspace");
    let mut req = request(ws, &[]);
    req.is_python = false;
    req.task = Task::Review;
    let prompt = build_turn_prompt(&req);
    assert!(prompt.contains("`rust-cargo-packaging`"), "{prompt}");
}
