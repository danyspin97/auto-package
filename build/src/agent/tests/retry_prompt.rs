//! A package whose failure repeated: its next turn is told to try something
//! else, and the instruction is in the prompt the turn is sent.

use super::prompt::request;
use super::*;
use crate::agent::build_prompt::*;
use crate::pipeline::RetryRecord;

/// A record of one failure repeated `repeats` times, instructed.
fn repeated(repeats: usize) -> RetryRecord {
    let mut rec = RetryRecord::default();
    rec.note_failure("13.0.2", "needs a human: 2 patch(es) do not apply", false);
    for _ in 0..repeats {
        rec.note_failure("13.0.2", "needs a human: 2 patch(es) do not apply", true);
    }
    rec
}

#[test]
fn the_next_fix_turn_prompt_carries_the_retry_instruction() {
    let rec = repeated(1);
    let instruction = rec
        .instruction_for("13.0.2")
        .expect("an instruction is due");
    let ws = std::path::Path::new("/nonexistent-workspace");
    let mut req = request(ws, &[]);
    req.task = Task::FixAfterBuild;
    req.context = Some(&instruction);
    let prompt = build_turn_prompt(&req);
    assert!(
        prompt.contains("## Retry: try a different approach"),
        "{prompt}"
    );
    assert!(
        prompt.contains("The same failure happened 2 times"),
        "{prompt}"
    );
    assert!(prompt.contains("Do NOT repeat them"), "{prompt}");
    assert!(prompt.contains("answer `blocked`"), "{prompt}");
}

#[test]
fn no_instruction_is_in_the_prompt_of_another_version() {
    let rec = repeated(1);
    assert_eq!(rec.instruction_for("13.0.3"), None);
    let ws = std::path::Path::new("/nonexistent-workspace");
    let prompt = build_turn_prompt(&request(ws, &[]));
    assert!(!prompt.contains("## Retry: try a different approach"));
}
