# Handoff (2026-11-09, late evening)

Written so the work can continue on another machine with nothing but this
repository. `docs/PLAN-NEXT.md` (2026-10-27) is older; where they disagree this
file wins. Nothing here is a promise: it is the state, what is unverified, and
the order of the remaining work.

## 1. Repository state

- Everything from this session is committed and contained in `main`. The
  working copy is an empty change on top of `main`. No bookmark holds a commit
  that `main` lacks. **Nothing was pushed** (rule: never push); push `main`
  yourself (`jj git push --bookmark main`) before leaving this machine.
- Old side heads not in `main` (about 13, from before this session: `vvuypopo`
  conflicted wip, `rlzryqox`, `wyozrnwp`, `ryyqqrpl` hold generated notes that
  belong in `../auto-package-notes`). A read-only review found nothing in them
  worth landing; most subjects exist in `main` under new change ids. Abandon
  only on the owner's say (`qwxzmpqt` "match only the HTTP 404 text" was not
  diffed).
- About 190 `~/ap-*` workspace directories and logs, and `~/Music/ap-work`
  (65 GB of run data: open-webui 24G, open-webui_nvidia 17G, vllm 13G are old),
  are the owner's to remove. Workers cannot `rm -rf`.

## 2. Goal and numbers

Goal: 95% of packages build in a couple of tries (cap 10 per package).
Tries = OBS builds (`Build` step_started) a package needed. Measure with
`auto-package explain --run <id> --work-dir ~/Music/ap-work --project <branch project>`
(read-only, no network); the run summary prints the same block.

- Baseline over 232 package-runs of 32 runs (history): 1 try 55.6%, 2 tries 12.9%,
  3-5 9.9%, 6-10 3.9%, not built 17.7%: built in <= 2 tries 68.5%, built at all 82.3%.
  python 66%, c/c++ 79%, X11_Wayland 82%, Base_System 35%.
- Last partial run of `devel:languages:python` (20261109-193719, 110 packages):
  18 in 1 try, 2 in 2, 4 in 3+, 15 not built (9 died before any build: tool
  defects, since fixed). 75% of the packages that reached a build did it in one try.
- Prompt fixes alone were estimated at 75-80%, not 95%: the rest is dependency
  waits, OBS environment and hard packages (mariadb, duckdb, pcp).

## 3. What the next machine should do first

1. `cargo build` (see section 6 for the disk rules), then restart the whole
   project update in a BRANCH project, never `--in-place`:
   `auto-package update --project devel:languages:python --branch-project home:dspinella:auto-package:branches:devel:languages:python --work-dir ~/Music/ap-work --max-packages 0`
   (`--max-packages 0` matters: the default cap is 25). Add `--tui` in a real
   terminal. The last attempt was stopped at planning on 2026-11-09 22:40 only
   because the owner had to leave; package state persists in `ap-work`.
2. Read `explain --run` after the first hour and compare with section 2.
3. Verify, on real runs, everything listed in section 4 ("unverified").

## 4. Landed this session but NEVER run end to end

Only unit tests and read-only replays exist for these; the first real run is
their test. Look at each in the run log / TUI:

- Parallel build of all found updates (`schedule.rs`, `--jobs`), the arch check
  after the slot (`POST_JOBS` 8), one shared arch deadline, a submit is not held
  for pending arches, `rebuild="local"` on branch repositories (its OBS meaning
  is UNCONFIRMED: measure the branch queue before and after), lazy branching
  (cap `2 x jobs`), origin binaries by default, `--test-against-updates`.
- `--update-deps`: PyPI `Requires-Dist` prediction, `%{python_module}` lookup,
  same-project ordering with parked packages (cap 25). Known limit: a parked
  package is already branched on OBS.
- TUI redesign (state machine, header `checked N/T`, queue rows, order): only
  tested on ratatui's test backend, never on a terminal.
- Retry with an instruction instead of giving up ("The same failure happened N
  times", 3 instructed repeats then needs-a-human); wait record flavour match
  and bound; checkout version race after a baseline restore; sdist top
  directory; carried-recipe inherited description problems; post-image patch
  proof (comment 90% match), TargetsAbsent; adopted own commit (cramjam);
  reconcile heals stale state at start-up; osc write lock per project dir.
- Prompt/convention changes: conventions 117 (stale `_service` revision), 118
  (shared flavour file), bounds on `-devel` names, test dependencies in one
  edit, whole check-files list in one edit, installed-tree rule, skip-comment
  rule, braced `%python_expand` example, Rust vendor/RUSTFLAGS skill.
  `INDEX.md` is 11,438 of 12,500 bytes; conventions 40 and 86 are inline in the
  system prompt (about +900 bytes). Nothing was run against a model.

## 5. Open defects and decisions

Tool, ordered by likely value:
- `update_project/items.rs:416-419` existence lookup still compares with
  `fuzzy_key` (the separator collision `python-whois` / `python-pythonwhois`
  was fixed everywhere else; `fuzzy_key` is not an identity key).
- `append_landed` never records commits of a package with a `_service`; every
  attempt commit of such a package depends on the adoption rule
  (`osc/baseline_adopt.rs`).
- A waited dependency can wait forever if its commit never reaches the branch
  (no "built in this run" check; the precheck has no run-event access).
- The package's own `Epoch:` is not read; capability bounds are compared with
  the provide's version where known, else the package version.
- Copyright header rewritten "2024 -> 2026 SUSE LLC and contributors" on a
  built package: not our code; comes from `spec-cleaner` or the OBS
  `format_spec_file` service. Stage not identified.
- `python-hypothesis` switched Source to a prebuilt wheel (convention 32 says
  build from source): needs a human decision.
- `config.update` stays false for a dependency by design; two review gates
  (`build_round.rs:310`, `review.rs:123`) still use it.
- Product-owner recommendations NOT implemented: `--jobs` throttle when free
  disk < 10 GiB, `--fix-preexisting` for backport/maintenance runs, a
  `source-switched` TUI state, `waiting-upstream` status, "never stop the run on
  one package's network error".
- Claim not implemented because unsupported by the logs: a stale OBS "failed"
  state counted as a try (the miner said 28 builds, the check found 17, most not
  stale).

Package-specific, need a human (as of the last run): pyenv-like patches that do
not apply (click-plugins, checkdmarc, celery before the fix), python-chess
(origin `.changes` has `00:00:00 PM`: now fails once with a clear reason),
python-hypothesis wheel, mariadb/duckdb/pcp class.

Owner decisions pending: abandon the old side heads; whether a branch with port
repositories from an old `--ports` run is still waited for (now: no, unless
`--ports`/`--arches` names a port); INDEX guard moved 12,000 -> 12,500 (accepted).

## 6. Working rules that cost time this session

- Disk: `/home` is small and filled repeatedly. Shared `target/` grows 10G+ with
  parallel agents (duplicate artifact sets from different flags; incremental
  caches 4-8G). Always `CARGO_INCREMENTAL=0`, never the
  `--config profile.dev.package.ap-core.codegen-units=255` workaround, never
  delete `target/debug/incremental` while an agent builds (it breaks their
  build); `cargo clean` between agents is allowed by AGENTS.md. Never use `/tmp`
  (shared tmpfs, fills up); use `TMPDIR=~/Music/ap-tmp-<task>`, one per worker,
  deleted by its owner only. Run at most 2-3 building agents at once.
- Workflow: `ORCHESTRATOR.md`: haiku medium implements, sonnet reviews, opus only
  for design; every worker in its own jj workspace and bookmark, lands with
  `jj bookmark set main`, then forgets its workspace. Review findings were often
  half wrong: verify each claim in the code.
- Known environmental test failure: `service_sandbox::tests::the_sandbox_isolates_home_bus_network_and_osc_metadata`
  (with `TMPDIR` under `$HOME`). Load-sensitive: `pipeline::tests::concurrent_updates_do_not_lose_writes`,
  `update_project::schedule_tests::a_post_step_does_not_hold_a_job_slot`,
  `upstream_cache::..asked_again..`: rerun alone before believing a failure.
- Rules from AGENTS.md that matter: never touch packages by hand; fix the tool
  and run it again; lint/update only in a branch project; commit with `jj`,
  `cargo fmt` first, subjects <= 60 chars, trailer
  `Co-Authored-By: Claude Sonnet 5.5 <noreply@anthropic.com>`; files <= 600
  lines (a test enforces it).
