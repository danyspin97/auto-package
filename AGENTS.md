# Instructions for agents working in this repository

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

## Working on auto-package itself

- Commit each change on a branch with `jj` (not raw `git`); never push.
- Run `cargo fmt` before committing and commit its result.
- Do not build cargo under `/tmp`; `/home` fills up during runs, so check `df`
  and clean `target` after large builds. Ask before deleting anything in `ap-work`.

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
