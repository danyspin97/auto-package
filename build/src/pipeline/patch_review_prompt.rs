//! The Context of a PatchReview turn: the one patch, what changed it and what
//! quilt says, built by the pipeline from files, never from the writer's own
//! account.

use crate::pipeline::patch_check::expand_patch_name;

/// The patch text capped in the Context; the turn can read the full copy.
const PATCH_CAP: usize = 40_000;
/// A short quoted text (a name list, a quilt line, a reason) capped in the Context.
const SMALL_CAP: usize = 4_000;
/// The diff of the patch file capped in the Context.
const DIFF_CAP: usize = 20_000;

/// What one review round is told.
pub(super) struct ReviewInput<'a> {
    pub(super) round: usize,
    pub(super) name: &'a str,
    /// The strip level of the spec's `%patch` line.
    pub(super) level: u32,
    /// The `# PATCH-...` comment above the `PatchN:` line, when it has one.
    pub(super) spec_comment: Option<&'a str>,
    /// What changed the patch, e.g. `rebased onto 2.0 by a refresh turn`.
    pub(super) cause: &'a str,
    /// The files the patch (before and now) touches.
    pub(super) targets: &'a [String],
    /// Quilt's verdict on the new sources, in a few words.
    pub(super) quilt: &'a str,
    /// The patch before the rewrite: `None` when the patch is new.
    pub(super) original: Option<&'a str>,
    pub(super) current: &'a str,
    /// Why the last round's edit was rolled back.
    pub(super) rollback: Option<&'a str>,
}

/// The Context text of a review round.
pub(super) fn patch_review_context(input: &ReviewInput) -> String {
    use crate::agent::untrusted_data;
    use crate::patch_check::truncate;
    let max = crate::pipeline::patch_review::MAX_REVIEW_ROUNDS;
    let mut out = format!("Review round {} of at most {max}.\n", input.round);
    if input.round >= max {
        out.push_str(
            "This is the last round: answer accept, reject or blocked; a rewrite is rolled back.\n",
        );
    }
    // Everything below that quotes a patch, a spec or a tool's output is data.
    let fence = |label: &str, text: &str| untrusted_data(label, &truncate(text, SMALL_CAP));
    if let Some(why) = input.rollback {
        out.push_str(&format!(
            "Your last rewrite was rolled back:\n{}\n",
            fence("rollback reason", why)
        ));
    }
    out.push_str(&format!(
        "Patch: {}, applied with -p{}; spec comment:\n{}\n",
        input.name,
        input.level,
        fence("spec comment", input.spec_comment.unwrap_or("none"))
    ));
    out.push_str(&format!("What changed it: {}\n", input.cause));
    out.push_str(&format!(
        "Files it touches (read them in the new sources under source/):\n{}\n",
        fence("patch file names", &input.targets.join(", "))
    ));
    out.push_str(&format!(
        "Quilt on the new sources:\n{}\n",
        fence("quilt output", input.quilt)
    ));
    out.push_str(&format!(
        "The patch before (as OBS ships it; full copy: patch-review/{}.orig):\n",
        input.name
    ));
    match input.original {
        Some(text) => out.push_str(&untrusted_data("patch before", &truncate(text, PATCH_CAP))),
        None => out.push_str("none: the patch is new"),
    }
    out.push_str(&format!(
        "\nThe patch now (the checkout's {}):\n",
        input.name
    ));
    out.push_str(&untrusted_data(
        "patch now",
        &truncate(input.current, PATCH_CAP),
    ));
    out.push_str("\nWhat changed in the patch file:\n");
    let diff = unified_diff_text(input.original.unwrap_or(""), input.current);
    out.push_str(&untrusted_data(
        "patch file diff",
        &truncate(&diff, DIFF_CAP),
    ));
    out.push('\n');
    out
}

fn unified_diff_text(before: &str, after: &str) -> String {
    crate::pipeline::render::unified_diff(before, after)
}

/// The `# PATCH-...` comment line above the `PatchN:` line that names `name`.
pub(super) fn spec_patch_comment(spec: &str, name: &str) -> Option<String> {
    let lines: Vec<&str> = spec.lines().collect();
    let at = lines.iter().position(|l| {
        let t = l.trim_start();
        t.starts_with("Patch")
            && t.split_once(':')
                .is_some_and(|(_, raw)| expand_patch_name(raw, spec) == name)
    })?;
    lines[..at]
        .iter()
        .rev()
        .map(|l| l.trim())
        .take_while(|l| l.starts_with('#'))
        .find(|l| l.contains("PATCH-"))
        .map(str::to_string)
}
