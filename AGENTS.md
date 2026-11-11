# Instructions for agents working in this repository

If you delegate work to other agents, read ORCHESTRATOR.md first.

## Never touch packages yourself: always use auto-package

auto-package is the only thing that changes packages.

- Do not edit a spec, a `.changes` file, a `_service`, a patch or any other
  package file by hand, in a checkout, in `ap-work`, or on OBS.
- Do not run `osc` yourself to commit, branch, rebuild, submit or delete anything
  in a package. Reading (`osc cat`, `osc ls`, `osc rdiff`) is fine for a review.
- Do not work out a package's upstream version by hand (`curl`, `git ls-remote`)
  to decide what to do with it. Ask auto-package (`update`, `lint`, `build`).
- To update, lint or fix a package, run the matching auto-package command
  (`auto-package update --project <P> --only <pkg> --branch-project <B>`,
  `auto-package lint --project <P> ...`).
- When auto-package gets a package wrong (a wrong version, a bad spec, a missing
  rule), fix auto-package (the code, the conventions, the skills), rebuild it and
  run it again. Never fix the package instead.

## Bare package names

`auto-package build <name>` stops when PyPI and a same-named repository both
claim the name. Pick one: `build pypi:<name>` builds the PyPI project, and
`build <repo url>` builds the repository. The TUI `n` prompt takes the same
forms (`pypi:<name>` or a repository URL). Never work around the refusal by
hand.

## Working on auto-package itself

- Commit each change on a branch with `jj` (not raw `git`); never push.
- Run `cargo fmt` before committing and commit its result.
- Do not build cargo under `/tmp`; `/home` fills up during runs, so check `df`
  and clean `target` after large builds. Ask before deleting anything in `ap-work`.
- Source files stay at most 600 lines (enforced by a test).

## Rules for every worker agent

These add to the rules above and to the brief. On conflict, the stricter rule wins.

Workspace
- One jj workspace per task: `jj workspace add <workspace-root>/ap-<task> -r <base>`.
  `<workspace-root>`: the parent directory of the repository checkout, where the
  jj workspaces live (siblings of the checkout, never inside it).
  Work only there. Never touch the main checkout or another agent's workspace.
- Set only the bookmark named in the brief (`jj bookmark set <task> -r @-`).
  Do not move, create or delete other bookmarks, and do not rewrite commits
  below `<base>` or another agent's commits. Your own unpublished commits in
  your workspace may be rewritten.
- You cannot `rm -rf`. Run `jj workspace forget ap-<task>` only if the brief
  says so; leave the directory for the user.

Cargo and disk
- Every cargo call: `CARGO_TARGET_DIR=<repo>/target cargo ...`.
  `<repo>`: the main checkout; `<repo>/target` is the shared target directory.
  Never create `target/` inside a workspace or under `/tmp`.
- Run `df -h /home` before building. Below 2.5 GB free: stop and report.

Commits
- One concern per commit. Subject <= 60 chars, imperative, `area: what` (see `jj log`).
- Body wrapped at 72, stating only causes the diff shows. No history, no
  "as discussed", no claims the diff does not support.
- Trailer, exactly: `Co-Authored-By: Claude Sonnet 5.5 <noreply@anthropic.com>`
- Keep the author and committer that jj already has configured; never set them
  (no `--author`, no `jj config set user.*`, no `GIT_AUTHOR_*` or `GIT_COMMITTER_*`).
- Every commit compiles and passes on its own: `cargo fmt`,
  `cargo check -q --all-targets`, `cargo test -q -p <crate>` for the crates it
  touches and their dependents. Crates: `ap-build`, `ap-cli`, `ap-core`, `ap-ecosystem`.
- Split a hunk that mixes two concerns at hunk or line level. Every
  intermediate tree is a prefix of the final diff: no commit adds what a later
  one removes. If the brief gives a target, `jj diff --from <expected> --to @-`
  must be empty.

Review findings
- Verify each claim in the code first. If it is wrong, do not "fix" it; report
  it with file:line evidence.
- Squash each fix into the commit it repairs (`jj squash --into <rev>`), then
  re-run the checks from there to the tip.

Report
- Report once, at the end, with nothing before it: commits (change id and
  subject, in order); tests (commands and results); items skipped and why;
  limits (unverified, guesses, known risks). A short true report beats a
  complete-looking one.
- If asked again for the report, answer only the specific question.

## Answering questions about a run

When asked what happened to a package (why it failed, what the agent changed,
what it cost, which tests were flaky), read; do not change anything. Run
`auto-package explain <pkg> --work-dir <ap-work> [--project <P>] [--run <id>]`
first: it reads the work directory without a model or the network and says what
it cannot tell. `docs/agents/work-directory.md` describes the `ap-work` layout,
`run.jsonl` and the `jq` recipes for what `explain` does not answer, and why the
agent's `reasoning` is its own summary and not ground truth.

## Package notes live outside the repository

The per-package notes the tool writes (LLM output) are kept in
`../auto-package-notes` (relative to where the tool runs; `--notes DIR`
changes it), not in this git tree, and `notes/` is ignored here. Read them
there; do not recreate `notes/packages/`.
