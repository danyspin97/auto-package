//! The types of a turn: its request and outcome, the errors and usage it
//! reports, and the actions and tool calls the agent takes.

use super::*;

// ── public types ──────────────────────────────────────────────────────────────

/// Token counts and cost from one API call.
#[derive(Debug, Clone, Default)]
pub struct Usage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_creation_tokens: u64,
    pub cache_read_tokens: u64,
    /// Pre-computed cost in USD from the CLI's own accounting.
    pub cli_cost_usd: Option<f64>,
}

impl Usage {
    pub fn cost_usd(&self) -> f64 {
        if let Some(usd) = self.cli_cost_usd {
            return usd;
        }
        (self.input_tokens as f64 * 5.0
            + self.output_tokens as f64 * 25.0
            + self.cache_creation_tokens as f64 * 6.25
            + self.cache_read_tokens as f64 * 0.50)
            / 1_000_000.0
    }

    pub fn add(&mut self, other: &Usage) {
        self.input_tokens += other.input_tokens;
        self.output_tokens += other.output_tokens;
        self.cache_creation_tokens += other.cache_creation_tokens;
        self.cache_read_tokens += other.cache_read_tokens;
        match (self.cli_cost_usd.as_mut(), other.cli_cost_usd) {
            (Some(a), Some(b)) => *a += b,
            (None, Some(b)) => self.cli_cost_usd = Some(b),
            _ => {}
        }
    }
}

/// Payload of `Error::EndedEarly`, boxed so `Error` stays small.
#[derive(Debug)]
pub struct EndedEarlyError {
    pub subtype: String,
    pub message: String,
    pub stdout: String,
    pub prompt: String,
    /// The model turns the cut-off run took (the CLI's own count).
    pub turns: usize,
    /// What the cut-off turn still cost. The budget cap is only enforced if
    /// the caller charges this, so it is kept rather than dropped.
    pub usage: Usage,
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("could not run claude: {0}")]
    Launch(std::io::Error),

    /// `usage` is what the turn cost before it died (see `turn_usage`), so a
    /// killed or crashed turn is charged rather than read as free.
    #[error("claude exited {code}:\n{stderr}")]
    Failed {
        code: i32,
        stderr: String,
        stdout: String,
        prompt: String,
        usage: Usage,
    },

    /// Claude ended mid-turn with a known structured error: budget cap
    /// reached, rate-limited, context overflow, etc. The `subtype`
    /// originates verbatim from the claude CLI (`error_max_budget_usd`,
    /// `error_rate_limited`, …) so the caller can both show the user a
    /// specific actionable message AND decide whether to retry at the
    /// outer loop (budget: no; rate-limit: maybe later; etc.).
    #[error("claude ended turn with {}: {}", .0.subtype, .0.message)]
    EndedEarly(Box<EndedEarlyError>),

    #[error("unexpected response: {message}")]
    Parse {
        message: String,
        stdout: String,
        prompt: String,
        usage: Usage,
    },

    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

impl Error {
    /// Is this error a budget cap hit (as opposed to a crash or a bad
    /// response shape)? The caller treats this specifically — surfaces
    /// a 'raise --budget-usd' hint and does NOT keep retrying since
    /// every retry will hit the same cap.
    pub fn is_budget_exhausted(&self) -> bool {
        matches!(self, Error::EndedEarly(e) if e.subtype.starts_with("error_max_budget"))
    }

    /// For `EndedEarly`, the human-readable message the claude CLI
    /// supplied verbatim (e.g. "Reached maximum budget ($0.03)") —
    /// use this instead of the `Display` impl when assembling a
    /// user-facing wrapper, so the wrapper's own context (the cap in
    /// --budget-usd) isn't shown alongside the CLI's own dollar figure
    /// in a way that reads like two different budgets.
    pub fn ended_early_message(&self) -> Option<&str> {
        match self {
            Error::EndedEarly(e) => Some(&e.message),
            _ => None,
        }
    }

    /// Did the turn run out of `--max-turns` before it answered?
    pub fn is_max_turns(&self) -> bool {
        matches!(self, Error::EndedEarly(e) if e.subtype == "error_max_turns")
    }

    /// Did the turn fail because the claude CLI is not logged in (or its
    /// credentials were rejected)? Only claude's own error fields are
    /// scanned: the structured end-early subtype/message, a failed exit's
    /// stderr and its non-JSON stdout lines. Never the result text of a
    /// turn that succeeded, nor a `Parse` error (which quotes it).
    pub fn is_not_logged_in(&self) -> bool {
        match self {
            Error::EndedEarly(e) => {
                e.subtype == "api_error_401"
                    || looks_like_not_logged_in(&e.subtype)
                    || looks_like_not_logged_in(&e.message)
            }
            Error::Failed { stderr, stdout, .. } => {
                looks_like_not_logged_in(stderr)
                    || stdout
                        .lines()
                        .filter(|l| !l.trim_start().starts_with('{'))
                        .any(looks_like_not_logged_in)
            }
            Error::Launch(_) | Error::Parse { .. } | Error::Io(_) => false,
        }
    }

    /// Was this turn rejected by the API for quota/rate limiting (HTTP 429)?
    pub fn is_rate_limited(&self) -> bool {
        matches!(self, Error::EndedEarly(e)
            if e.subtype == "api_error_429" || e.subtype == "error_rate_limited")
    }

    /// For `EndedEarly`, the subtype slug (`error_max_budget_usd`,
    /// `error_rate_limited`, …) exactly as the claude CLI emitted it.
    pub fn ended_early_subtype(&self) -> Option<&str> {
        match self {
            Error::EndedEarly(e) => Some(&e.subtype),
            _ => None,
        }
    }

    /// What a failed turn cost before it died, so the caller can charge it
    /// against the package budget. `None` only for failures that happen before
    /// any model call (`Launch`, a stream-read `Io` error).
    pub fn usage(&self) -> Option<&Usage> {
        match self {
            Error::EndedEarly(e) => Some(&e.usage),
            Error::Failed { usage, .. } | Error::Parse { usage, .. } => Some(usage),
            Error::Launch(_) | Error::Io(_) => None,
        }
    }

    /// The prompt sent for the turn this error aborted, when one had already
    /// been built — `None` only for failures that precede that point
    /// (`Launch`, a stream-read `Io` error), which have nothing turn-specific
    /// to show.
    pub fn prompt(&self) -> Option<&str> {
        match self {
            Error::Failed { prompt, .. } | Error::Parse { prompt, .. } => Some(prompt),
            Error::EndedEarly(e) => Some(&e.prompt),
            Error::Launch(_) | Error::Io(_) => None,
        }
    }

    /// The claude CLI's raw stdout transcript up to the point of failure, when
    /// captured — the single most useful artifact for diagnosing a crash or a
    /// malformed response, since it is otherwise discarded the moment this
    /// process exits (see `turn`'s `stdout_raw` doc comment).
    pub fn stdout(&self) -> Option<&str> {
        match self {
            Error::Failed { stdout, .. } | Error::Parse { stdout, .. } => Some(stdout),
            Error::EndedEarly(e) => Some(&e.stdout),
            Error::Launch(_) | Error::Io(_) => None,
        }
    }

    /// The claude CLI's captured stderr, when any was produced.
    pub fn stderr(&self) -> Option<&str> {
        match self {
            Error::Failed { stderr, .. } => Some(stderr),
            Error::Parse { .. } | Error::EndedEarly(_) | Error::Launch(_) | Error::Io(_) => None,
        }
    }
}

/// Confidence the model reports about its fix.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Confidence {
    High,
    Medium,
    Low,
}

/// Which task the agent should perform this turn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Task {
    /// Inspect the generated spec before the first build.
    Review,
    /// Fix the spec after a failed `osc build`.
    FixAfterBuild,
    /// Fix the spec after a failed `rpmspec --parse`.
    FixAfterValidation,
    /// Translate OBS's RPM-wrapped unresolvable dependency names (e.g.
    /// `libtrantor-devel`) into real upstream project names, read-only — no
    /// spec edit is attempted or possible (see `allowed_tools`).
    TranslateUnresolvableDeps,
    /// Patch a distro package that was branched into the project because a
    /// dependent package needs a change to it (convention 62).
    PatchDependency,
    /// Look once more at a spec that built, after fix attempts changed it.
    PostBuildReview,
    /// Rebase the carried-forward patches that quilt says no longer apply
    /// cleanly to the new release (see `patch_check`), or remove the obsolete ones.
    RefreshPatches,
}

/// What a [`Task::RefreshPatches`] turn is told to do. The quilt report (each
/// patch, quilt's output and the rejected hunks) comes in the Context.
pub(super) fn refresh_patches_task() -> &'static str {
    "The package was bumped to a new version and quilt says some of its patches no \
     longer apply cleanly; the report is under \"Context\". Rebase ONLY the patches, \
     with quilt. In `source/` (the new sources) quilt is ready: `QUILT_PATCHES` is the \
     checkout and `QUILT_SERIES` is `series-rebase`, which lists the patches in the \
     spec's order with the level each is applied with. These exact commands work:\n\
     - `cd source && quilt pop -a` (start from the pristine tree)\n\
     - `cd source && quilt push` (the next patch; quilt has no dry run: push, read its \
       output, `quilt pop` to undo)\n\
     - `cd source && quilt push -f` (force the failing patch; it leaves `*.rej` files)\n\
     - edit the file to port each rejected hunk, then \
       `cd source && quilt refresh -p ab --no-timestamps --no-index` (`-p 0` instead of \
       `-p ab` for a `-p0` patch); quilt writes the patch back into the checkout\n\
     - `cd source && quilt diff`, `quilt files`, `quilt series`, `quilt top` to look.\n\
     Each quilt command is one Bash call of the form `cd source && quilt <sub> ...`: the \
     working directory is the workspace, not `source/`. Do not chain several commands \
     with `&&`, `;` or `|` (the sandbox refuses them), and do not put environment \
     variables on the command line: QUILT_PATCHES and QUILT_SERIES are already set. \
     Do one patch per cycle: `quilt pop -a`, `quilt push` (the next patch), the edit, \
     `quilt refresh`; push the next patch only after that. Do not read the series file \
     (`quilt series` lists it), and do not create directories: quilt keeps its state in \
     `source/.pc`. Edit and Write refuse a file that is not UTF-8, and no other tool \
     here may edit it: answer that patch as `blocked` and name the file in its reason.\n\
     The pipeline refreshed every patch that applies with an offset or fuzz before you \
     started: each patch you are given has rejected hunks. Keep each patch's comment \
     header (PATCH-FIX-UPSTREAM / PATCH-FIX-OPENSUSE and the \
     reference) as it is, and never write a .patch file by hand. `patch --dry-run` \
     is a read-only fallback when quilt cannot be used, never the main tool and \
     never used to apply. The checkout's patch files are outside the workspace, and \
     a path to them needs `..`, which the sandbox refuses by design: a dry run cannot \
     read them, so check each patch with quilt.\n\
     \n\
     A patch is NEVER removed because it does not apply, nor because it only touches \
     tests: a failing hunk in a test file or a `.result` file is rebased like any \
     other. The one proof that a patch may be removed is quilt's own message `Patch \
     <name> can be reverse-applied` when you `quilt push` the unchanged patch: its \
     change is already in the new sources. Only then remove it: its `PatchN:` line, \
     its `%patchN` line if any, and its file (`cd source && quilt delete -r <name>`). \
     The pipeline checks that proof and rolls back a removal without it.\n\
     \n\
     A command the sandbox refuses is a bug in the tool: name the exact command in \
     `reasoning`, and never skip a check because of it. Change nothing else in the \
     spec. Answer with `patches`, which this turn must give: a non-empty list with one \
     entry per patch, its `name`, and a `status` of \
     `rebased` (ported with quilt), `removed` (quilt proved it applied upstream) or \
     `blocked` (not ported; say which hunk and why) with a `reason`. Only the patches \
     you list are kept, so a patch you leave out is rolled back. `action` is `edited` \
     when an entry is `rebased` or `removed`, else `no_change`."
}

/// Input to one agent turn.
pub struct TurnRequest<'a> {
    pub workspace: &'a std::path::Path,
    pub pkg_name: &'a str,
    pub version: &'a str,
    /// Whether this package is a Python project (per the `python-` RPM-name
    /// prefix set by `Ecosystem::rpm_name`). Controls which Review checklist
    /// the agent gets: a plain-C/C++ or Rust package has no pyproject.toml,
    /// no `%{python_module ...}` macros, no `%python_subpackages`, so handing
    /// it the Python-centric checklist (import name, console scripts, pytest
    /// config, %fdupes sitelib vs. sitearch) is a wall of irrelevant work.
    pub is_python: bool,
    pub task: Task,
    pub attempt: usize,
    pub max_attempts: usize,
    pub spent_usd: f64,
    /// Total USD budget for the whole package, so the turn can be capped at
    /// whatever is left rather than being allowed to overrun it in one call.
    pub budget_usd: f64,
    pub hint: Option<&'a ExtractedLog>,
    pub notes: &'a str,
    /// Durable per-package memory (`package-notes.md`): what the package needs
    /// to build and test, and the workarounds already in place. Inlined into
    /// the prompt so a new attempt does not rediscover them.
    pub package_notes: &'a str,
    /// The tarball is a repository archive rather than a PyPI sdist.
    pub from_repo_archive: bool,
    /// Sibling projects found next to the one being built, as
    /// `(directory, declared name)`. Non-empty only for a monorepo.
    pub subprojects: &'a [(String, String)],
    /// Packages OBS reported as unresolvable during the last build attempt.
    pub unresolvable_deps: &'a [String],
    /// The raw `nothing provides <pkg> <op> <version>` / `(got version
    /// <installed>)` lines behind `unresolvable_deps`, verbatim — `..._deps`
    /// itself keeps only the bare package name (needed to queue it for
    /// building), which reads identically whether the package is genuinely
    /// missing or, as in the `setuptools<70.0.0` case that prompted this,
    /// already present at a version this spec's own upper-bound constraint
    /// rules out. Empty when OBS gave no version detail to show.
    pub unresolvable_detail: &'a [String],
    /// Packages this run already tried to build, and failed to. These are the
    /// only ones it is right to work around rather than report again.
    pub failed_deps: &'a [String],
    /// Packages confirmed to have no PyPI project and no discoverable public
    /// source repository at all — this pipeline can never build them, no
    /// matter how many times they're reported (almost always a proprietary
    /// vendor SDK/compiler like NVIDIA's `cuda-devel`). See convention 28
    /// Exception 4.
    pub unbuildable_deps: &'a [String],
    /// Session ID from the previous turn, used to resume the claude session.
    pub session_id: Option<&'a str>,
    /// OBS checkout directory — when set, QUILT_PATCHES is pointed here so the
    /// agent can create patches that land directly in the OBS package directory.
    pub checkout_dir: Option<&'a std::path::Path>,
    /// Optional short note injected into the prompt just before the hint section.
    pub context: Option<&'a str>,
    /// Where the RPMs this project built are kept. When set, the turn may list
    /// one's files with `rpm -qlp` (see [`rpm_inspection_tools`]).
    pub rpms_dir: Option<&'a std::path::Path>,
    /// Checkouts of sibling packages (the ones this package depends on): the
    /// turn may read them, never write them. Changes to a sibling go through
    /// `patch_dependencies`.
    pub sibling_dirs: &'a [std::path::PathBuf],
    /// How many models to move along [`AGENT_MODELS`] from the one the attempt
    /// number picks. Quota is per model, so the pipeline bumps this after a 429
    /// to retry the same turn on a different model.
    pub model_rotation: usize,
    /// Called in real-time as each tool-use block is parsed, with `(tool_name,
    /// input_summary)`. Used by the pipeline to stream AgentToolUse events
    /// while the turn is still running so the TUI stays alive.
    pub on_tool_use: Option<&'a ToolUseCallback<'a>>,
}

/// Streaming per-tool-use callback — clippy's `type_complexity` floor
/// hits the inline `dyn Fn(&str, &str) + Send + Sync` on `on_tool_use`.
pub type ToolUseCallback<'a> = dyn Fn(&str, &str) + Send + Sync + 'a;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    Edited,
    NoChange,
    Blocked {
        why: String,
    },
    /// The package's own source cannot be built in this environment (e.g. it
    /// requires a proprietary compiler/toolchain OBS cannot provide, per
    /// convention 28 Exception 4's "environment" case) and a prebuilt wheel
    /// exists on PyPI for this exact version. `why` is the agent's cited
    /// evidence — the pipeline still independently verifies a wheel exists
    /// before acting on this.
    UseUpstreamWheel {
        why: String,
    },
}

/// One tool invocation the agent made during a turn.
pub struct ToolCall {
    pub name: String,
    /// The tool's input, JSON-encoded and truncated to a readable length.
    pub input: String,
}

pub struct TurnOutcome {
    pub action: Action,
    pub reasoning: String,
    /// The `.changes` bullet the turn asked for; empty when it gave none.
    pub changelog: String,
    pub evidence: Vec<String>,
    pub unresolvable_packages: Vec<String>,
    /// Dependencies to branch and patch rather than build (convention 62).
    pub patch_dependencies: Vec<PatchRequest>,
    /// Files the pipeline copies into the package checkout for the turn.
    pub stage_files: Vec<crate::stage::StageFile>,
    /// A RefreshPatches turn's answer for each patch it touched or could not
    /// port. Empty for every other task.
    pub patch_answers: Vec<PatchAnswer>,
    pub confidence: Confidence,
    pub usage: Usage,
    pub tool_calls: Vec<ToolCall>,
    pub prompt: String,
    pub output: String,
    /// The raw `result` payload from the CLI, before parsing.
    pub raw: String,
    pub session_id: Option<String>,
    /// Set when the claude CLI's final `result` event carried
    /// `is_error: true` with a known `subtype` (budget cap, rate limit,
    /// context overflow) — EVEN IF a usable `StructuredOutput` was
    /// rescued from the stream and the turn therefore returned `Ok`.
    /// The pipeline surfaces this as a warning so a budget-cut-but-
    /// rescued turn doesn't look identical to a clean one in the
    /// event stream, which would otherwise let the per-package budget
    /// state silently drift under the user.
    pub ended_early: Option<EndedEarlySignal>,
    /// The Bash commands the permission rules refused in this turn: each is a
    /// gap the pipeline reports, so a refused check is seen.
    pub denied_commands: Vec<String>,
}

/// Structured end-of-turn signal from the claude CLI. Same fields as
/// `Error::EndedEarly`, carried on the `Ok` path when the model emitted
/// a usable `StructuredOutput` before being cut off — see
/// `TurnOutcome::ended_early`.
#[derive(Debug, Clone)]
pub struct EndedEarlySignal {
    pub subtype: String,
    pub message: String,
}

/// The end-early signal in a `result` event. `error_max_turns` (and any other
/// `error_*` subtype) counts even with `is_error: false` and an empty result,
/// which is how the CLI can report running out of turns.
pub(super) fn ended_early_signal(result_event: &ResultEvent) -> Option<EndedEarlySignal> {
    let failed = result_event.is_error && !result_event.subtype.is_empty()
        || result_event.subtype.starts_with("error_");
    if !failed {
        return None;
    }
    // `subtype: "success"` with `is_error: true` is how the CLI reports a
    // failure whose reason only lives in the `result` text (API errors,
    // a refused tool call); the subtype alone says nothing there.
    let message = if !result_event.errors.is_empty() {
        result_event.errors.join("; ")
    } else if !result_event.result.trim().is_empty() {
        let t = result_event.result.trim();
        t.chars().take(600).collect::<String>()
    } else {
        result_event.subtype.clone()
    };
    let subtype = match result_event.api_error_status {
        Some(status) => format!("api_error_{status}"),
        None => result_event.subtype.clone(),
    };
    Some(EndedEarlySignal { subtype, message })
}
