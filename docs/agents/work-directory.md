# Reading an auto-package work directory

For agents that answer questions about what auto-package did ("why did
aws-c-http fail?", "what did the agent change?", "what did it cost?"). This is
for reading. Never fix a package by hand: see AGENTS.md. (`packaging-skills/`
is a different thing: it is what the packaging agent itself is taught.)

## Run this first

    auto-package explain <pkg> --work-dir ~/Music/ap-work [--project <OBS project>] [--run <id>] [--json]

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

## run.jsonl

Every line has `event`; most have `pkg` (compact JSON, so `"pkg":"foo"` greps
cleanly). Events of several packages are interleaved. Events seen:
`run_started` (`argv`), `run_finished` (`outcome`, `elapsed` in ms),
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
`{"MissingDeps":{"deps"}}`), plus noisy `command`, `command_done`, `progress`,
`agent_tool_use`. There are no timestamps: use `elapsed` and the run id.
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
