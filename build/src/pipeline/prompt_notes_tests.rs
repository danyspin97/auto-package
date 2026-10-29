use super::*;

#[test]
fn short_notes_pass_through_and_long_notes_keep_the_recent_end() {
    assert_eq!(tail_for_prompt("short\n"), "short\n");
    let long: String = (0..4000).map(|i| format!("line {i}\n")).collect();
    let cut = tail_for_prompt(&long);
    assert!(cut.len() <= MAX_PROMPT_NOTES_BYTES + 64);
    assert!(cut.starts_with("(earlier entries omitted)\n"));
    assert!(cut.trim_end().ends_with("line 3999"));
    assert!(!cut.contains("line 0\n"));
}

#[test]
fn long_package_notes_drop_failure_history_before_workarounds() {
    let filler = "- a fact about the build\n".repeat(1000);
    let text = format!(
        "# Notes\n## Build requirements\n- gcc\n## Workarounds\n- patch X\n\
             ## Failure history\n{filler}## Test changes\n- skip Y\n"
    );
    assert!(text.len() > MAX_PROMPT_NOTES_BYTES);
    let cut = package_notes_for_prompt(&text);
    assert!(cut.contains("## Build requirements\n- gcc"));
    assert!(cut.contains("## Workarounds\n- patch X"));
    assert!(!cut.contains("## Failure history"));
    assert!(cut.contains("## Test changes"));
}

#[test]
fn a_bold_line_stays_with_the_section_it_is_in() {
    let filler = "- a fact about the build\n".repeat(1000);
    let text = format!(
        "## Build requirements\n**gcc**: needed for the C extension\n- gcc\n\
             ## Failure history\n{filler}"
    );
    assert!(text.len() > MAX_PROMPT_NOTES_BYTES);
    let cut = package_notes_for_prompt(&text);
    // The bold line is body text of the kept section, not a rank-2 section.
    assert!(cut.contains("**gcc**: needed for the C extension"));
    assert!(!cut.contains("## Failure history"));
}

#[test]
fn a_hash_line_inside_a_fence_is_not_a_section_start() {
    let filler = "- a fact about the build\n".repeat(1000);
    // The fenced `# export CFLAGS` is code inside `## Workarounds`: it must
    // stay with that section, which is kept.
    let text = format!(
        "## Build requirements\n- gcc\n## Workarounds\n```sh\n# export CFLAGS=-O2\n```\n\
             - the flags above\n## Failure history\n{filler}"
    );
    assert!(text.len() > MAX_PROMPT_NOTES_BYTES);
    let cut = package_notes_for_prompt(&text);
    assert!(cut.contains("## Workarounds\n```sh\n# export CFLAGS=-O2\n```"));
    assert!(cut.contains("- the flags above"));
    assert!(!cut.contains("## Failure history"));
    // A tilde fence closes only on a tilde, so a ``` inside it does not end it.
    let tilde =
        format!("## Workarounds\n~~~\n```\n# inner\n~~~\n- kept\n## Failure history\n{filler}");
    let cut = package_notes_for_prompt(&tilde);
    assert!(cut.contains("# inner"));
    assert!(cut.contains("- kept"));
    assert!(!cut.contains("## Failure history"));
}
