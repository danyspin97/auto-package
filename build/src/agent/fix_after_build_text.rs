//! The task text of a fix turn after a failed `osc build`. Kept apart from
//! `build_prompt.rs`, which is at its size limit.

/// The task of a `Task::FixAfterBuild` turn: the %check reading order, the
/// stale-log rule, and the whole-list rule for `%files` errors.
pub(super) fn fix_after_build_task() -> &'static str {
    concat!(
        "Fix the spec after a failed `osc build`.\n\
             \n\
             For a %check failure, Read `conventions/10.md` and `conventions/11.md` first and \
             follow them IN ORDER: diagnose and \
             add missing test dependencies first (convention 10 — a BuildRequires or \
             fixing an uninstalled test helper is almost always the right fix for an \
             `ImportError`/`ModuleNotFoundError`/`fixture not found`), and only then \
             consider convention 11's test-skip ladder for tests that genuinely \
             cannot run in this environment (no network, no GPU, no display beyond \
             what xvfb-run supplies, no shipped fixture). Skipping before you have \
             checked what the test actually needs throws away coverage for a problem \
             a BuildRequires would have fixed.\n\
             \n\
             The hint below is a heuristic extract of the failure; read build.log \
             yourself whenever the extract looks like it caught the wrong thing.\n\
             \n\
             Before concluding a prior fix did not work, rule out a stale build.log: \
             compare this attempt's build.log with the previous one (Read or Grep the \
             VM boot timestamp and the line count: you have no md5sum). If they match, \
             OBS has not actually rebuilt against your last commit yet, and this log says \
             nothing about whether that fix worked. You cannot re-run the build: verify the \
             fix statically (by reading the spec), say in your reasoning that the log looks \
             stale, and return `no_change` rather than reverting a plausible fix on the \
             strength of a stale log.",
        "\n\n",
        "Fix every `%files` error of the log in one edit. A failure that lists several ",
        "`Installed (but unpackaged) file(s)`, `File listed twice` or `File not found` lines ",
        "is one set of paths, not one error: add each unpackaged path to `%files`, drop each ",
        "duplicate and correct each missing one, all in the same edit. ",
        "Before you answer, check the whole list in the log against the spec again, line by ",
        "line, and say in `reasoning` which paths you covered. A fix that answers one line ",
        "costs a build per line.",
    )
}
