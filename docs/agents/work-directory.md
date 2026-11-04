# Reading an auto-package work directory

For agents that answer questions about what auto-package did ("why did
aws-c-http fail?", "what did the agent change?", "what did it cost?"). This is
for reading. Never fix a package by hand: see AGENTS.md. (`packaging-skills/`
is a different thing: it is what the packaging agent itself is taught.)

## Run this first

    auto-package explain <pkg> --work-dir ~/Music/ap-work [--project <OBS project>] [--run <id>] [--format json]

It reads the files below and prints, with no model and no network: how the
version was chosen, the outcome and the last build-log errors, every attempt
(step, agent turn, the agent's reasoning and evidence, spec +N -M, OBS result,
cost, time), flaky-test verdicts, cost against the budget, the warnings that
mattered, the package notes, and a list of what it cannot tell. `--run` picks
an older run (default: the latest run that touched the package); `--json` is
the same data for `jq`. In the TUI, `e` on a row shows the same report.
Use the recipes below only for what it does not answer.

## Layout

`--work-dir` (default `ap-work`) holds one directory per OBS project and arch,
named `<project with : and / as _>-<arch>`, plus `upstream-latest.json` (a
cache of upstream versions). Inside one (call it `$P`):

- `$P/state/<pkg>/` the durable per-package state:
  - `state.json`: `last_outcome` (`success`, `failed`, `missing_deps`,
    `interrupted`, `running`), `last_failure_reason`, `last_built_version`,
    `cumulative_spent_usd` and `cumulative_spent_run_id` (the spend of that one
    run), `ladder_reason`, `obs_name`, hashes the tool uses to not repeat work.
    `running` with no live process means the run died.
  - `notes.md`: one `## Attempt N - <task>` entry per agent turn
    (Reasoning, Evidence, Confidence). It accumulates across runs.
  - `build.log` (latest OBS log), `spec`, `sdist/` (unpacked sources),
    `conventions/` (the rules the agent was given).
- `$P/runs/<run-id>/` one directory per invocation (`YYYYMMDD-HHMMSS-...`):
  - `run.jsonl` the event log, one JSON object per line. Read it with `jq`.
  - `<pkg>/NNN-<kind>/` the artifacts of step `NNN-1` (steps count from 0,
    directories from 1): `prompt.md` (what the agent was told),
    `response.json` (its structured reply), `output.md` (its tool calls),
    `spec.diff`, `build.log`, `rpmspec.stdout`, `extract.json`.
- `$P/<OBS project>/<pkg>/` the checkout: `<pkg>.spec`, `<pkg>.changes`.
- `$P/rpms/` RPMs built so far (used to satisfy dependencies).
- `../auto-package-notes/<pkg>.md` (relative to the directory the tool runs
  in; `--notes DIR` changes it): the package notes the tool keeps across runs
  and projects (build requirements, test skips, workarounds). It is created on
  the first write and is outside both `ap-work` and the repository. A missing
  directory just means no notes yet.
- `ap-work/ledger.json` (work directory root, with `ledger.json.lock`): USD
  spent by agent turns per UTC day, across runs; `--max-total-usd` (default 50)
  is checked against it. `--cleanup` never deletes it, `runs/`, `state/` or an
  osc checkout; it only removes `rpms/` and `state/<pkg>/sdist`.

## Reviews of submit requests

`auto-package review <request-id>` saves its report under
`<work-dir>/reviews/<request-id>/<run-id>/`, at the root of the work directory,
beside the project directories. It is not a package run: there is no
`run.jsonl`, no `state/` and no notes (a review's opinions are never build
input), and `explain` does not read it.

- `review.json` the report (`schema` 1): `request`, `actions[]` with `source`,
  `target`, the two versions, `changed`/`added`/`removed`, `findings[]` (`id`,
  `severity`, `origin`, `file`, `line`, `message`) and `skipped[]` (what was not
  checked, and why), `agent` (`null` without `--agent`; its findings are the
  model's opinion, with `origin: agent`), `fail_on` and `exit`. `review.txt` is
  the same as text.
- `request.xml`, `source.diff` what was read.
- `agent/` with `--agent`: the turn's workspace (`request.xml`, `source.diff`,
  `old/`, `new/`, `findings.json`), `prompt.md`, `response.json`, `output.md`.
  Its text is the model's own summary, not ground truth (see below).

    jq -r '.actions[].findings[] | [.severity, .id, .message] | @tsv' review.json

`--cleanup` never deletes it.

## run.jsonl

Every line has `event`; most have `pkg` (compact JSON, so `"pkg":"foo"` greps
cleanly). Events of several packages are interleaved. Events seen:
`run_started` (`argv`, `build`: the auto-package revision that wrote
the run, `unknown` when none was embedded; absent in older logs),
`run_finished` (`outcome`, `elapsed` in ms),
`target_selected` (`version`, `from_version`, `reason`: ExplicitOverride,
FirstUnbuiltDependency, RequestedRoot), `step_started` / `step_finished`
(`step`, `kind`, `attempt`, `round`, `detail`; `outcome` Ok/Failed/Skipped,
`elapsed` ms), `step_skipped`, `agent_prompt`, `agent_reply` (`response` with
`action`, `reasoning`, `evidence`, `confidence`; `usage.cost_usd`),
`spec_changed` (`lines_added`, `lines_removed`, `artifact`), `spec_rejected`
(the spec was invalid and the previous one restored), `build_failed`
(`classification`, `log`), `missing_packages`, `extract_result`, `notes_usage`
(cost of the notes turn), `warning`, `package_finished` (`outcome`: `Built`,
`AlreadyBuilt`, `{"Failed":{"reason"}}`, `{"Blocked":{"reason"}}`,
`{"MissingDeps":{"deps"}}`), `host_allowed` (`host`, `scope`: `run` or
`persistent`, `source`: `flag` for `--allow-host`, `tui` for the TUI's prompt; a run-level event with no `pkg`, written when the host is allowed, so a
log can be read for every host a run was allowed to contact; absent in older
logs), plus noisy `command`, `command_done`, `progress`,
`agent_tool_use`. There are no timestamps: use `elapsed` and the run id.
`arch_check` (`package`, `results`: one `{repo, arch, state}` per checked
result of an updated package, with `detail` (the first error line, the
missing names, or `nothing provides ...` for a port), `origin_fails` and `log`
when set; `origin_fails` means the baseline fails too: the origin, or Factory
for a port repository (`openSUSE_Factory_zSystems`, `openSUSE_Factory_PowerPC`,
`openSUSE_Factory_RISCV`); `state` is `succeeded`, `failed`, `failed (no Factory
baseline)` (a port failure Factory has no result of, which blocks), `unresolvable`,
`broken`, `excluded`, `not testable` (a port that cannot resolve its build
dependencies, never a failure), `not final (...)` (a port still going at the
bound, never blocking) or `still ...` for any other result still scheduled,
blocked or building at the bound; written by `update` after the package built,
absent with `--no-arch-check` or `--local`, and absent from older logs). A
result's state is a label: a log that predates a label reads as before. Its
failures are also in the summary's `Other architectures` section and in a
`warning`.
`hosts allow` and `hosts remove` run outside any run and write no event: the
user's `allowed-hosts` file (its `# reason` comments) is their record.
`auto-package explain` lists the `host_allowed` hosts of the run it reads.
Recipe: `jq -c 'select(.event=="host_allowed")' "$R"`.
Recipe: `jq -c 'select(.event=="arch_check")' "$R"`.
`zig_vendor` (`pkg`, `file`, `changed`, `deps`: `name`, `url`, `hash`) is the
refresh of a zig vendor archive from the new source's `build.zig.zon`, written
on an update; absent when the package has no such archive.
`auto-package explain` lists the dependencies it names.
Recipe: `jq -c 'select(.event=="zig_vendor")' "$R"`.
`patch_reviewed` (`step`, `pkg`, `patch`, `round`, `verdict`, `reason`) is one
round of the review of a patch an agent rewrote: `verdict` is `accepted`,
`rewritten`, `rejected`, `gave_up`, `rolled_back` (a rewrite or an edit the gate
refused), `slip` or `not_reviewed` (no turn could start; `reason` says why).
Recipe: `jq -c 'select(.pkg==env.K and .event=="patch_reviewed")' "$R"`.
`bug_fix` (`step`, `pkg`, `stage`, `refs`, `read`, `not_read`, `patch`,
`patch_sha256`, `origin`, `verdict`, `applied`, `reason`) is a record of
`auto-package fix`: `stage` `input` says what the tool read (hosts and files),
what it could not (`not_read`, with the reason) and which upstream patch it
staged; `end` says how the fix ended (`verdict`: `fixed`, `already_fixed`,
`not_affected`, `no_fix`, `needs_maintainer` or `does_not_apply`; `applied`:
`clean`, `refreshed`, `adapted` or `written`). The turn itself is `agent_prompt`
with task `fix bug`.
Recipe: `jq -c 'select(.pkg==env.K and .event=="bug_fix")' "$R"`.
`finding_fix` (`step`, `pkg`, `request`, `finding`, `route`, `outcome`, `reason`)
is a record of `auto-package fix --request`, one for each finding of the review
of request `request`: `route` is `tool`, `agent`, `pipeline`, `wait`,
`maintainer` or `not_acted_on`; `outcome` is `fixed`, `not_fixed`, `maintainer`
or `not_acted_on`. The turn itself is `agent_prompt` with task `fix findings`.
Recipe: `jq -c 'select(.pkg==env.K and .event=="finding_fix")' "$R"`.
The workspace keeps three review files under `patch-review/`: `<name>.orig` (the
patch before the rewrite, for the reviewer, removed after each round), `rewritten`
(patches a reviewer rewrote itself, each with a hash of its bytes; the risk flag
reads it, a patch whose bytes changed since is not flagged, and a run clears it
at its start) and `pending` (rewritten patches a stop left unreviewed, with a
hash; a rerun refuses to go on while a patch still has those bytes, so read the
patch, edit it or remove the file).
A run with no `run_finished` was interrupted or is still going.

Recipes (`R=$P/runs/<run>/run.jsonl`, `K=<pkg>`):

    # how it ended
    jq -c 'select(.pkg==env.K and .event=="package_finished")' $R
    # why did it fail: failed steps, then the log tail
    jq -c 'select(.pkg==env.K and .event=="step_finished" and .outcome=="Failed")' $R
    jq -r 'select(.pkg==env.K and .event=="build_failed")|.log' $R | tail -1 | xargs tail -60
    # what did the agent change and why
    jq -r 'select(.pkg==env.K and .event=="agent_reply")|[.step,.response.action,.response.reasoning]|@tsv' $R
    jq -r 'select(.pkg==env.K and .event=="spec_changed")|[.step,.lines_added,.lines_removed,.artifact]|@tsv' $R
    cat $P/runs/<run>/$K/*/spec.diff
    # what did it cost
    jq -s 'map(select(.pkg==env.K)|(.usage.cost_usd // 0))|add' $R
    jq -r 'select(.pkg==env.K and .event=="agent_reply")|.usage.cost_usd' $R
    # warnings, minus status polling
    jq -r 'select(.pkg==env.K and .event=="warning")|.message' $R | grep -v '^OBS build status'
    # which tests were flaky: the pipeline writes the verdict into the prompt
    # of the next turn and into the package notes (convention 87)
    grep -h -A4 'Flaky tests (read by the pipeline)' $P/runs/<run>/$K/*/prompt.md
    grep -h 'flaky tests' ../auto-package-notes/$K.md $P/state/$K/notes.md

(Run them with `K=<pkg> jq ...` so `env.K` is set.)

## Rules for answering

- `reasoning` and `evidence` are the model's own summary of its turn, not
  ground truth: they can be wrong or self-serving. Cite the log, the diff and
  the outcome for facts and say "the agent says" for the rest.
- Do not guess a cause the files do not show. Say what is not recorded: no
  per-attempt changelog text, no OBS timing beyond step `elapsed`, no cost of
  OBS builds, nothing about a run that wrote no events.
- `state.json` describes the latest run; an older run's outcome is in its
  own `package_finished`. `cumulative_spent_*` belongs to
  `cumulative_spent_run_id` only. The budget is `--budget-usd` in the run's
  `argv` (default 10.00 USD per package per run).
- Reading anything here is fine. Writing is not: no editing of specs,
  `.changes`, `state.json`, notes or `run.jsonl`, and no `osc` writes. To
  change what auto-package does, fix auto-package and run it again.
