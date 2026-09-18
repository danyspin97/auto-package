# auto-package — redesign plan

Status: proposal, not yet implemented. Supersedes the parts of `PLAN.md` that
describe orchestration, logging and state. `PLAN.md` stays authoritative for
spec-generation policy and the openSUSE conventions.

Goal of the first milestone: **one package, one build, no TUI, everything
observable.** The user names a PyPI package, auto-package resolves the
dependency closure, picks exactly one package to build, builds it, and with
`--log debug` prints every input and output of every step to stdout/stderr.

---

## 1. Why the current code does not work

These are not style complaints. Each one was observed in the repository or in
`ap-work/` from the last real run.

### 1.1 Confirmed live defects

**a. The work directory is written into the OBS checkout.**
`ap-work/home:dspinella:python/python-nvtx/ap-work/rpms/` exists on disk. `osc
build` runs with `current_dir(pkg_dir)` and is handed `--keep-pkgs ap-work/rpms`
as a *relative* path, so osc creates that tree inside the package working copy.
`osc` then tries to copy it into `SOURCES` and the build log carries
`cp: -r not specified; omitting directory '.../python-nvtx/ap-work'`. Three more
files pollute the same checkout: `spec.generated`, `BUILD_OK`, and the tarball's
sibling artifacts. The checkout is supposed to hold only the spec and the
tarball so `osc status` stays clean.

**b. Agent-authored spec comments break the spec.**
The last nvtx run died with `error: line 3: Unknown tag: set +x`. Line 3 is
`# ap: fix 1: Changed %pytest to %pytest_arch for C extension package`. RPM
expands macros inside comments, `%pytest` expanded to a shell fragment, and the
spec stopped parsing. Convention 11 in the agent prompt tries to teach the model
to work around this with a conditional rule about deleting prior comment lines.
The rule is unlearnable and the failure is silent until the next build.

**c. The log extractor cannot see any source file.**
`PathMapper::with_sdist_root` expects the *versioned* sdist directory name
(`nvtx-0.2.16`). All three call sites pass the bare package name
(`build/src/agent.rs:742`, `cli/src/main.rs:595,639,1186`). `sdist_relative`
searches for the literal `/nvtx/`, never matches, and — critically — returns
`None` with no structural fallback. Consequence: `PathClass::BuildTree` is never
produced, so `primary_source_file()` is always `None`, `source_candidates()` is
always empty, the `BuildTree` escalation rung is a no-op, and
`collapse_traceback` elides the package's own stack frames as if they were
interpreter plumbing. The extractor's unit tests pass because they construct the
mapper with the correct root.

**d. The build log is captured through two independent file descriptors.**
`osc::build` opens `log_path` with `File::create` for stdout and a separate
append handle for stderr. The two offsets advance independently, so ordering is
not chronological and stdout can overwrite stderr bytes.

**e. `rpmspec --parse` is misjudged and its output thrown away.**
`rpmspec_check` fails the spec if stderr contains `warning:` even when the exit
status is 0, and it discards stdout — the fully expanded spec, which is the most
useful artifact in the system for diagnosing a spec problem. It also reports the
failure as `Error::OscFailed { cmd: "rpmspec …" }`, a variant whose `Display`
says "osc".

**f. Attempt records mix data from different runs.**
`ap-work/fixes/nvtx/1/` contains `spec_after.spec` written at 17:18 and
`prompt.txt` written at 17:25 — two different runs in one directory, because the
slot name is a plain counter. `status.json` in that directory still says
`"state": "in_progress"`, `"phase": "starting"`, `"section": "unknown"` for a
run that finished. A stale `failure_reason.txt` from the earlier run sits beside
the newer files. The TUI reads this directory as its data model, which is why
the display is untrustworthy.

**g. The agent backend is chosen behind the user's back.**
`detect_backend()` returns `Backend::Vertex` whenever `gcloud config get-value
project` prints anything, before it ever looks at `AUTO_PACKAGE_API_KEY` or the
`claude` CLI. A developer with an unrelated GCP project configured silently
sends prompts to Vertex. There is no flag to override it.

**h. Three `to_str().unwrap()` panics** on user-controlled paths
(`osc.rs:263,265`, `spec.rs:372`), and `Workspace::open` joins an unsanitized
package name onto the state root.

### 1.2 Structural problems

**Two build loops.** `build_one` (legacy, prompt-and-replace-whole-spec) and
`build_one_v2` (tool-using turns) are 600 lines of near-duplicate orchestration
behind a `--agent-v2` flag. Every fix has to be made twice, and in practice the
flag defaults off, so the path that is actually exercised is the weaker one.

**No event model.** Output is `Logger = Arc<dyn Fn(&str)>` receiving
pre-formatted strings, plus `eprintln!` from `agent::alog`, plus `indicatif`
writing to stderr, plus ten `tracing` macros in the whole workspace (zero in the
`build` crate). `--verbose` therefore controls almost nothing. There is no level
at which the prompt, the response, or the extract are printed. The TUI has to
suppress tracing entirely because it corrupts the screen.

**The on-disk record is a pile of single-value text files.** `AttemptLogger`
writes `kind.txt`, `section.txt`, `confidence.txt`, `action.txt`,
`reasoning.txt`, `escalation.txt`, `response.txt`, `status.txt` *and*
`status.json` containing the same fields again. Nothing is atomic, nothing is
versioned, and the two status files disagree.

**Failure classification is substring matching on English tool output** in six
places: `"already exists"`, `"404"`, `"Not Found"`, `"no repositories defined"`,
`"is not a working copy"`, `"nothing provides "`, `"No patches in series"`,
`"Could not find a version that satisfies the requirement"`. Anything
unrecognized degrades quietly to a generic build failure.

**The escalation ladder is obsolete and half-broken.** `context.rs` is the
cleanest file in the tree, but with a tool-using agent the model reads what it
needs; the ladder's job is gone. It is also unable to update `build_failed`
after construction, so the CLI throws each `Escalation` away on a new error
signature and loses the `history`/`skipped` telemetry the module exists to
collect.

**Extraction is un-scoped and tail-biased.** `parse_python_frames`,
`parse_errors` and `rank_paths` scan the whole log ignoring the located failure
and the phase spans, so frames from a *recovered* traceback rank equally with
the real one. `collapse_traceback` carefully preserves source frames wherever
they appear, then `finish` keeps only the last 60 lines and can delete exactly
those. The `error:` verdict line usually falls outside the excerpt because the
region is clipped at the section boundary. There is no genre for compiler,
linker, make or pytest output — `SHELL_ERROR` is `^\S+: line \d+: `, which
matches none of them.

**Dependency data is wrong in both directions.** `pip::resolve` returns a
*runtime wheel* closure evaluated against the host interpreter. Build-backend
dependencies (hatchling → pathspec, pluggy, trove-classifiers) are never in the
graph, so the `ModuleNotFoundError` scraper is the de-facto build-dep resolver.
`dep_name` drops every `extra ==` requirement but keeps `sys_platform ==
'win32'` ones, so Windows-only packages become build edges. `spec.rs` emits
`BuildRequires: %{python_module setuptools>=42}` — exactly the form the agent
prompt documents as WRONG — so the model spends a turn fixing what the generator
just produced.

---

## 2. Target architecture

Four changes carry the whole redesign. Everything else follows from them.

### 2.1 One event stream

A single `Event` enum in `ap-core`, emitted by every layer, consumed by sinks.
Nothing below the CLI formats a human string ever again.

```rust
// core/src/event.rs
pub enum Event {
    RunStarted   { run_id: RunId, argv: Vec<String>, work_dir: PathBuf },
    RunFinished  { outcome: RunOutcome, elapsed: Duration, usage: Usage },

    ResolveStarted  { root: String },
    ResolveFinished { total: usize, built: usize, remaining: usize },
    TargetSelected  { pkg: String, version: String, reason: SelectionReason,
                      blocked_by: Vec<String> },

    StepStarted  { step: StepId, kind: StepKind },
    StepFinished { step: StepId, outcome: StepOutcome, elapsed: Duration },

    Command      { step: StepId, program: String, argv: Vec<String>,
                   cwd: PathBuf, log: Option<PathBuf> },
    CommandDone  { step: StepId, code: i32, elapsed: Duration,
                   stdout_bytes: u64, stderr_bytes: u64 },

    BuildFailed   { step: StepId, classification: BuildFailure, log: PathBuf },
    ExtractResult { step: StepId, extract: Extract, artifact: PathBuf },

    AgentPrompt   { step: StepId, task: Task, prompt: String, artifact: PathBuf },
    AgentResponse { step: StepId, response: AgentResponse, raw: String,
                    usage: Usage, artifact: PathBuf },
    AgentToolUse  { step: StepId, tool: String, input: String },

    SpecChanged   { step: StepId, diff: String, lines_added: usize,
                    lines_removed: usize, artifact: PathBuf },
    SpecRejected  { step: StepId, reason: SpecRejection, restored_from: PathBuf },

    MissingPackages { step: StepId, names: Vec<String>, source: MissingSource },
    Warning         { step: Option<StepId>, message: String },
    Artifact        { step: StepId, name: &'static str, path: PathBuf },
}
```

Every variant carries the artifact path it corresponds to. That is what makes
"the location of the full build log" a property of the model rather than
something a formatter has to remember to print.

`Reporter` is the sink trait; the runner holds `Arc<dyn Reporter>` and fans out:

- `HumanSink` — stderr, level-filtered, the only place formatting lives.
- `JsonlSink` — appends one JSON object per event to `run.jsonl`.
- `MemorySink` — for tests and, later, the TUI.

This replaces `Logger`, `AGENT_LOGGER`/`alog`, `AttemptLogger`, the `indicatif`
spinner, and the `tracing` subscriber. `tracing` stays available for
library-internal trace-level detail but is no longer the user-facing channel.

### 2.2 `--log <level>` drives the human sink

```
--log error | warn | info | debug | trace     (default: info)
-v / -vv                                       aliases for info / debug
```

| level | what the user sees |
|---|---|
| `error` | only the final outcome and hard failures |
| `warn` | + degraded-mode warnings (no buildroot, extractor found nothing) |
| `info` | + one line per step with timing, outcome, and artifact path; the classified error; the spec diff summary; cost |
| `debug` | + **full agent prompt**, **full agent response JSON**, **full extracted error with provenance**, every subprocess argv and cwd, every tool call the agent made, the full spec diff |
| `trace` | + raw subprocess stdout/stderr streamed inline, raw CLI envelope JSON |

`debug` is the level the first milestone is built around. Everything the user
listed — prompt, response, extracted error, full build log location — is printed
there, and each dump is delimited so it can be grepped:

```
── agent prompt ── step 6 · fix-after-build · 3184 tokens ─────────────────────
...
── end agent prompt ── also at runs/20260914-172518/python-nvtx/06-agent/prompt.md
```

### 2.3 The run directory is the record

One directory per run, immutable, never reused. This kills defect (f) outright.

```
ap-work/runs/<utc-timestamp>-<short-id>/
  run.jsonl                     every event, append-only
  run.json                      final summary, written once at the end
  python-nvtx/
    01-resolve/     closure.json
    02-checkout/    cmd.json  output.log
    03-sdist/       inspect.json
    04-spec/        py2pack.log  spec.generated  spec.after-postprocess
    05-validate/    cmd.json  rpmspec.stdout  rpmspec.stderr
    06-build/       cmd.json  build.log  extract.json  buildroot -> …
    07-agent/       prompt.md  response.json  raw.json  tools.jsonl
                    spec.before  spec.after  spec.diff
    08-build/       …
ap-work/state/<rpm-name>/       durable, survives runs
  spec -> …/checkout/python-nvtx.spec
  sdist/                        extracted tarball
  notes.md                      attempt journal the agent reads
  state.json                    last built version, last outcome
  latest -> ../../runs/<id>/python-nvtx
ap-work/<project>/<rpm-name>/   the osc checkout — spec + tarball ONLY
```

Steps are numbered in execution order, so the directory listing *is* the
timeline. `state.json` replaces the `BUILD_OK` marker and the `.reviewed` marker
file, both of which currently live in the checkout.

`Workspace` becomes the owner of this layout: every path is an accessor, no
caller writes `ws.dir().join("literal")`. Package names are sanitized before
being joined.

### 2.4 One build pipeline, explicit steps

Delete `build_one`, `build_one_v2`, `--agent-v2`, `agent::fix`, `context.rs` and
the whole escalation ladder. One state machine in `build/src/pipeline.rs`:

```rust
enum Step {
    Resolve, SelectTarget, Checkout, FetchSdist, InspectSdist,
    GenerateSpec, ValidateSpec, Build, ExtractFailure, AgentTurn, ApplyAndValidate,
}
```

The loop is: `Build → (ok ? done : Extract → AgentTurn → ApplyAndValidate →
Build)`, bounded by attempts and by a USD budget. `Review` becomes an
`AgentTurn` with `Task::Review` before the first build, not a separate function
with its own prompt and its own response schema.

Standardize on the tool-using turn (`agent::turn`). It is strictly better: the
agent reads `build.log` itself when the heuristic extract is wrong, and it edits
the spec in place instead of round-tripping the whole file through JSON. The
extract stops being a filter and becomes what its prompt already calls it — a
hint.

**Consequence to accept:** tool use requires the `claude` CLI. The Vertex and
direct-API backends only support the legacy whole-spec flow. Recommendation:
make the CLI the only backend for now, add an explicit `--agent-backend
cli|vertex|api` flag, and never auto-select from ambient `gcloud` config.

---

## 3. Milestone 1 — single package, fully observable

Scope: `auto-package build <pkg>` resolves, picks one target, builds it, and
reports. No TUI, no parallelism, no automatic dependency chaining. Missing
dependencies are reported as the suggested next target.

### 3.1 CLI surface

```
auto-package build <package> [--version <v>]
    --project <obs-project>          required
    --work-dir <dir>                 default ./ap-work
    --distro <name>                  default openSUSE_Tumbleweed
    --arch <arch>                    default x86_64
    --vm-type <type>                 default chroot
    --log <level>                    default info
    --max-attempts <n>               default 6
    --budget-usd <f>                 default 2.00
    --target <pkg>                   build this node instead of the auto-picked one
    --force                          rebuild even if state.json says it is built
    --regen                          regenerate the spec from scratch
    --agent-backend cli|vertex|api   default cli
    --no-agent                       build and report, never call the model
```

`--builds`, `--interactive`, `--allow-full-log`, `--agent-v2`, `--fixes-dir` are
removed. `-i` returns in milestone 4 as a reader of `run.jsonl`.

**All path arguments are canonicalized before use.** This alone fixes defect
(a): no relative path can ever reach a subprocess whose cwd is the checkout.

### 3.2 Target selection, and saying why

```
1. resolve(<pkg>) → closure, cached
2. topo-sort the closure
3. for each node in order: built according to state.json? skip
4. first unbuilt node = target
5. emit TargetSelected { pkg, reason, blocked_by }
```

`SelectionReason` is one of `RequestedRoot`, `FirstUnbuiltDependency`,
`ExplicitOverride`. `blocked_by` lists the unbuilt dependencies of the requested
root, so the user understands why they got `python-cffi` when they asked for
`python-nvtx`.

### 3.3 Default output (`--log info`)

```
auto-package · run 20260914-172518 · ap-work/runs/20260914-172518

resolve nvtx
  4 packages in closure · 3 already built · 1 remaining
  target: nvtx 0.2.16  (the requested package; all dependencies are built)

nvtx 0.2.16 → python-nvtx
  01 checkout    home:dspinella:python/python-nvtx              0.4s  ok
  02 sdist       nvtx-0.2.16.tar.gz · 48 files · C extensions   1.2s  ok
  03 spec        py2pack + 5 post-processing rules              0.9s  ok
  04 validate    rpmspec --parse                                0.2s  ok
  05 build       openSUSE_Tumbleweed/x86_64 · chroot            4.1s  FAILED

     %prep failed — rpm parse error
       error: line 3: Unknown tag: set +x
     full log: runs/20260914-172518/python-nvtx/05-build/build.log

  06 agent       fix-after-build · attempt 1/6                 18.3s  $0.04
     edited spec · 2 lines changed · confidence high
       removed the generated comment on line 3; RPM expanded %pytest inside it
  07 validate    rpmspec --parse                                0.2s  ok
  08 build       openSUSE_Tumbleweed/x86_64 · chroot           41.7s  ok

built python-nvtx 0.2.16 in 1m07s · $0.04 · 1 agent turn
  rpms: ap-work/rpms/python313-nvtx-0.2.16-0.x86_64.rpm  (+2 more)
  run:  ap-work/runs/20260914-172518
```

Rules that make this readable and are currently violated: one line per step, no
`[pkg]` prefix when only one package is building, artifact paths relative to the
work dir, the classified error stated in words before the raw excerpt, and the
excerpt indented so it cannot be confused with auto-package's own output.

### 3.4 `--log debug` additions

Between step lines, in full, with delimiters and artifact paths:

- the resolved closure with versions and edges,
- the argv and cwd of every subprocess before it runs,
- the complete extract: genre, phase, which strategy located it, the excerpt,
  the error lines, the elided count, the candidate source files, and an explicit
  `extractor found no signal` line when that is what happened,
- the complete agent prompt,
- every tool call the agent made, with its input,
- the complete agent response JSON,
- the complete unified spec diff,
- on rejection, the rpmspec stderr and the path the spec was restored from.

### 3.5 Work items

Ordered so each lands independently and the tree keeps building.

| # | Work | Files |
|---|---|---|
| 1 | `Event`, `Reporter`, `HumanSink`, `JsonlSink`, `LogLevel` | new `core/src/event.rs`, `core/src/sink.rs` |
| 2 | `RunDir`/`StepDir` allocator; rewrite `Workspace` to own the layout; sanitize names | `core/src/run.rs`, `build/src/workspace.rs` |
| 3 | `proc::run()` — argv, cwd, env, timeout, single-fd capture, emits `Command`/`CommandDone`, returns `ProcessOutput { code, stdout, stderr, log_path, elapsed }` | new `build/src/proc.rs` |
| 4 | Port `osc`, `spec`, `pip` onto `proc::run`; one `ProcessError`; replace substring classification with a matcher table that reports `Unclassified` loudly; fix the dual-fd capture; fix `rpmspec_check` to judge on exit status and keep stdout; remove the three `unwrap()`s | `build/src/osc.rs`, `build/src/spec.rs`, `ecosystem/src/pip.rs` |
| 5 | Canonicalize all CLI paths; move `BUILD_OK`, `spec.generated`, `.reviewed` out of the checkout into `state.json` | `cli/src/main.rs`, `build/src/workspace.rs` |
| 6 | Fix `PathMapper`: take the real sdist directory name, add the structural fallback, and emit a `Warning` event when the root never matches any path | `build/src/log/paths.rs`, call sites |
| 7 | `Extract` becomes serializable with provenance (`located_by`, `genre`, `phase`, `has_signal()`); move the "no signal → use tail" and "%files failure" rules out of the consumers into one place | `build/src/log/mod.rs`, `build/src/log/failure.rs` |
| 8 | Ban model-authored spec comments: drop convention 11, strip `# ap: fix` lines on load, and reject any spec edit that introduces `%` inside a comment. Rationale goes in `run.jsonl` and `notes.md`, never in the spec | `build/src/agent.rs`, `build/src/spec.rs` |
| 9 | `pipeline.rs`: the single state machine. Delete `build_one`, `build_one_v2`, `agent::fix`, `agent::review`, `context.rs`, `attempt_logger.rs` | new `build/src/pipeline.rs`, deletions |
| 10 | Target selection with `SelectionReason`/`blocked_by` | `build/src/pipeline.rs` |
| 11 | `--agent-backend`; remove ambient gcloud detection; stream and record tool calls | `build/src/agent.rs` |
| 12 | New CLI: flags above, `HumanSink` formatting, final summary | `cli/src/main.rs` |
| 13 | Point the TUI at `run.jsonl` or feature-gate it off until milestone 4 | `cli/src/tui.rs` |

Items 1-3 are the foundation and should land together. 4-8 are independent bug
fixes that can be reviewed separately. 9-12 are the visible change.

### 3.6 Verification

- **Golden-log tests.** Check the two real logs in `ap-work/state/*/build.log`
  into `build/tests/logs/` and assert the extract: nvtx must yield genre
  `rpm-parse-error`, phase `%prep`, and the `Unknown tag` line; lark must yield
  no failure. Add a C-extension compile log and a pytest-failure log.
- **Path-mapper regression test** asserting that a versioned sdist root produces
  `BuildTree`, and that a bare package name produces a warning rather than
  silence.
- **Spec-comment test**: a spec edit adding `# fix: use %pytest` must be
  rejected before `rpmspec` ever runs.
- **Checkout-cleanliness test**: after a full run, the only files in
  `<work-dir>/<project>/<pkg>/` are the spec, the tarball and `.osc/`.
- **Event-schema test**: replay `run.jsonl` through `HumanSink` and diff against
  a golden transcript. This is what keeps the output from rotting again.
- **End-to-end smoke**: `auto-package build nvtx --log debug` against the real
  OBS project, checked by hand once per milestone.

---

## 4. Later milestones

**M2 — dependency correctness.** Move graph construction into `ecosystem`.
Evaluate environment markers instead of string-matching them. Resolve
`[build-system] requires` transitively and merge it into the same graph, so
build backends stop being discovered by failing the build. Emit
`BuildRequires: %{python_module foo >= 1.2}` correctly from the generator so the
agent never has to fix it.

**M3 — multi-package runs.** Restore the closure loop and `--builds N` on top of
the pipeline, with per-package run directories already in place. Concurrency
gets a lock per workspace, which today does not exist.

**M4 — TUI as a reader.** The TUI tails `run.jsonl` and renders the same model
the CLI prints. It stops scanning a directory of loose text files, and it stops
needing tracing to be switched off.

**M5 — extractor genres.** Compiler (`file.c:12:5: error:`), linker, make,
cmake, meson and pytest-summary genres, registered as matchers rather than
bolted into `quote`'s `rposition` chain. Budget the excerpt in tokens, not
lines, and apply the budget before the semantic collapse rather than after.

---

## 5. Open decisions

1. **Drop the Vertex and direct-API backends?** They cannot do tool use, so
   keeping them means keeping the legacy whole-spec flow alive. Recommendation:
   drop them in M1, keep the flag so they can return if the CLI becomes a
   bottleneck.
2. **Does `build <pkg>` ever build more than one package?** M1 says no and
   reports the next target instead. If the intent is unattended runs, M3 should
   add `--chain` rather than making chaining the default again.
3. **Delete `ap-work/fixes/` and the old run data on upgrade?** The layout
   change is not backward compatible. Recommendation: leave it in place, ignore
   it, and let the user delete it.
4. **Keep `notes.md`?** It is the only cross-attempt memory the agent has, and
   it works. Recommendation: keep it, generate it from `run.jsonl` so there is
   one writer.
