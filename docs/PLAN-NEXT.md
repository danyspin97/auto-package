# Where things stand (2026-10-27) and what comes next

Written when work was stopped on request. Nothing here is a promise: it is the state
and the order of the remaining work, so it can be picked up cold.

## 1. State of the repository

- Bookmark `fix-pypi-placeholder` holds 43 atomic commits (subjects <= 60 chars) on top of
  `cost-two-decimals`. Its tip is `zxptopnw` ("docs: add AGENTS.md").
- Bookmark `fix-pypi-placeholder-backup` points at the OLD, unsplit history (tip `qquwqsqk`).
  Delete it only when the new history is trusted: `jj bookmark delete fix-pypi-placeholder-backup`.
- **Everything done since then is UNCOMMITTED in the working copy** (~295 files, ~22k insertions,
  148 deleted note files). `cargo fmt` has been run; the full workspace suite passed
  (ap-build 1304, ap-cli 199, ap-core 67, ap-ecosystem 286) apart from one flaky sandbox test
  that was fixed afterwards (it now passes 3 of 3).
- A jj workspace `ap-split2` (`/home/danyspin97/ap-split2`) was created for the commit split and
  holds only an empty commit. Remove it: `jj workspace forget ap-split2 && rm -rf /home/danyspin97/ap-split2`.

### Uncommitted work, by topic (this is the list to turn into atomic commits)

- cargo-fmt-only hunks; notes moved to `../auto-package-notes` (+ `.gitignore /notes/`).
- core: `env_flag`/utf8 env, redaction (`core/src/redact.rs`), 0700/0600 run logs, daily ledger
  and limits (`--max-total-usd` 50, `--max-packages` 25, `--jobs` 4, OBS 5 req/s), reworked `--cleanup`.
- `explain` command + `docs/agents/work-directory.md` + TUI `e` key.
- TUI diff viewer (`cli/src/diffview.rs`, `tui/modal.rs`) and in-TUI submit (S/A), submit pinned to
  the built revision (`rev=<srcmd5>`).
- Quilt patch check (`build/src/patch_check.rs`, `patch_stage`): rebase with quilt, remove a patch only
  when quilt says `can be reverse-applied`, one `.changes` bullet per refreshed/removed patch,
  minimal `series` edits (never published unless OBS tracks it), allowlist for quilt and
  `patch --dry-run`, refused-command warning.
- One version module (`ecosystem/src/version.rs`), `spec_for_package` -> `Result<_, ReadError>`,
  interruptible waits, batched upstream cache, `workspace` min-free setting.
- Security: `osc cat <url>` denied, pipeline-owned files denied to the agent, `.changes` restored
  after agent turns, tag defang, notes blocklist; archive-cache leases, typed forge errors, disk
  reserve + extraction slots + scratch dir under the work dir, retry jitter + host limiter,
  build-log redaction; `osc service` under bwrap with a CONNECT-only allowlist proxy
  (`build/src/service_sandbox.rs`, runs the service helpers directly).
- Correctness fixes: `expand_once`, `Requires(post)`, GitLab token header, `private_dir`, pip cache
  dir, sdist budget, bounded tar scanners, flaky-probe save, agent-jobs parse.
- Maintenance requests for SLE/Leap code streams (`--maintenance`, `--maintenance-mode`,
  `--backport`, `--bug`, `--affected-by`, `StreamCheck` seam), `--revision` / `--to-version`,
  flaky-test retry (`build/src/flaky.rs`), restore of dropped `_constraints`/`_multibuild`,
  noarch-subpackage fix.
- Conventions: now 94. New 84-94 (CUDA trigger, dynamic deps, carried-forward recipe, flaky tests,
  SLE/maintenance, spec hygiene, bundled code, services, signatures, patch rebasing); merges as
  pointer stubs; contradictions fixed; anecdotes removed; CUDA/NVIDIA moved to
  `packaging-skills/nvidia-wheels/SKILL.md` (NOT yet listed in
  `packaging-skills/.claude-plugin/plugin.json`: add `"./nvidia-wheels"` to its `skills`).

## 2. Immediate next step: commit the above atomically

Rules: one concern per commit, subject <= 60 chars (80 only for a real exception), body wrapped at 72
saying why, `Co-Authored-By` trailer, every commit compiles (`cargo check -q --all-targets` with the
shared `target/`), final tree byte-identical to the working copy. Build the commits in a separate
jj workspace (hunk-level `git apply`, see how the 43 existing commits were made), verify, then move
`fix-pypi-placeholder` to the new tip and rebase the working copy. Keep the backup bookmark.

## 3. Decisions the owner made (to implement, in this order)

1. **OBS writes**: keep committing each attempt; when a run ends in failure write the baseline
   revision back (never leave a broken revision in a project, especially with `--in-place`).
2. **Fix loop**: keep the ladder, but FLAG every dropped test, wheel switch and `blocked` result in
   the changelog bullet, the SR text and the end-of-run summary.
3. **Content gate**: flag-only. Detect risky deltas (new scriptlets, new source hosts, `%(`/`%{lua`,
   changed `Requires(pre/post)`/`Provides`, patches touching files outside the existing set) and mark
   them in the plan, the SR description and the summary; `--yes` refuses when any exist.
4. **Remaining code-review findings**: readelf `@file` and `ls`/`readelf` absolute paths in the
   allowlist; typed disk-full (`FailureKind::DiskFull`) instead of substring matching; one XML layer
   (replace the five attribute extractors and two decoders); unify the three `Source` parsers; the
   `osc.rs` tests with real sleeps; remove LLM-prose-driven actions (`parse_turn_response_from_prose`,
   `named_sibling_fix`/`SIBLING_CUES`, keyword-matched `npm_vendor_route_pending`); dead code
   (`AgentBackend`, `POLICY_VERSION`, retired-state migrations, the `osc results` text fallback,
   REDESIGN.md); stale comments and history anecdotes; `cmd_build` (1,560 lines) and `run` splits.
5. **Full crate split** (last, one crate at a time, each its own commits with tests green):
   `ap-obs` (client, serde/quick-xml models, `ObsWriter` live/dry-run/recording, caches owned by a
   client instance), `ap-spec` (AST parser/printer, edits, checks, generation), `ap-agent` (driver,
   env scrub, prompts with conventions addressed by slug, typed `Action`), `ap-run` (orchestrator
   from `cmd_build`, dependency frontier, update/lint/maintenance flows). Typed `Outcome` and per-layer
   error enums replace `last_outcome: Option<String>` and `Result<_, String>`, with a `state.json`
   migration.
6. **Conventions**: merge to ~30 topics (one rule per topic), inline the always-applicable ones
   (the review suggests `[27, 40, 61, 62, 63, 64, 86, 89]`), keep numbering stable through pointer stubs.
   Pending `.rs` edits from the conventions review (prompt text in `build/src/agent.rs`):
   changelog rule text and schema description (one bullet per change, patches named by file),
   PatchDependency text (changelog field, no hand-written patch), STATIC_WORKFLOW (`rm`, pg_config
   story), FixAfterBuild (md5sum, anecdotes), PRIORITIES (quoted proof before an import smoke test),
   `changes_message()` must keep one bullet per line, the spec.rs six-`--ignore` message.

## 4. Open items and risks

- Jobs still running when work stopped (they use the already-built binary, not the source tree):
  `lint --in-place` of `home:dspinella:auto-package:branches:devel:libraries:c_c++`,
  `update --only mariadb` (branch `home:dspinella:auto-package:mariadb`), and
  `update --only mariadb --revision 354 --to-version 13.0.2`
  (branch `home:dspinella:auto-package:mariadb-refresh`). Stop one with SIGINT (the tool handles it).
- The `mariadb-refresh` run was made with the OLD behaviour: the agent dropped patches 12 and 14
  without proof. Re-run into a fresh branch project with the new binary once the patch check is in
  the binary (the tool refuses to reuse a branch package when `--revision` is given).
- The `mariadb` branch lost `_constraints` to an old bug (fixed; the restore step brings it back on
  the next run).
- Unverified against a live OBS: `rev=` branching, revision reads, maintenance request creation and
  incident-copy branching, IBS/SLE layout, `permission_denials` field name, sandboxed
  `cargo_vendor`/`go_modules`/`node_modules`/`download_files`.
- Behaviour changes to remember: a patch that does not apply now ends the package as "needs a human"
  (no silent drop); `--jobs` defaults to 4; the daily ledger caps spend at 50 USD; extraction cap
  2 GiB; archives download under `<work-dir>/cache`; project-wide OBS services no longer run locally.
- Disk: `/home` is at 98% (4 GB free). `target/debug/incremental` was deleted once (5.4 GB);
  `ap-work` has old directories (open-webui 24 GB and 17 GB, vllm 13 GB): ask before deleting.
- Ideas not started: `auto-package ask` (a thin wrapper around `claude` over `explain` output, only
  if `explain` is not enough), per-patch refresh by the pipeline for offset-only patches.

## 5. Rules that apply to all of this

- Never touch packages by hand: run auto-package; if it is wrong, fix auto-package (AGENTS.md).
- Never delete anything (tests, patches, files, subpackages) without a proven reason.
- Commit with jj on a branch, never push; subjects <= 60 chars.

## 6. Proposed split of the conventions/skills part (from a planning agent, not applied)

A planning agent mapped the conventions and skills hunks to 25 commits (hunk ids refer to a
diff file that no longer exists, so only the subjects and order are kept). Use them as the
outline when the conventions are committed:

1. conventions: add 87 for flaky tests
2. conventions: add 88 as a pointer to 86 and 10
3. conventions: add 89 for SLE, Leap and maintenance
4. conventions: add 90 for spec hygiene
5. conventions: add 91 for bundled code
6. conventions: add 92: every _service runs in manual mode
7. conventions: add 93 to verify upstream signatures
8. conventions: add 94 for carried-forward patches
9. conventions: move CUDA/NVIDIA material to a skill
10. conventions: make 75 the one rule for version bounds
11. conventions: make 24 the one rule for headers
12. conventions: make 69 the one rule for licence files
13. conventions: merge 72 into 39, fix rpmlint errors first
14. conventions: merge 74 into 40 for the changelog field
15. conventions: use one PATCH-* tag format
16. conventions: name only tools the agent can run
17. conventions: require evidence before dropping a test suite
18. conventions: fix snippets that fail in a real build
19. conventions: write libalternatives for new console scripts
20. conventions: take Obsoletes versions from the distro
21. conventions: fix contradictions in 34 and 85
22. conventions: sharpen the read-when trigger of 9
23. conventions: drop anecdotes and tighten wording (after 5, 6, 9 and 12)
24. skills: drop anecdotes from skills
25. skills: fix wrong macros and defaults in skills

Hunks it could not split (kept whole, ordered after the commit they depend on): 12/44/49/5
(trimming mixed with pointers), 82 (PIP_IGNORE_REQUIRES_PYTHON and mode="manual" share a line),
24 and 68 (CUDA pointer inside one block), 69, 67, 60, 73, 79. 89 and 90 refer to each other, so
the first has one commit of forward reference.
