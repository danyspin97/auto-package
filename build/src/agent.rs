//! AI-assisted spec repair.
//!
//! On a build failure: extract the error context, send the spec and the
//! excerpt to Claude, get a replacement spec back. Repeat until the build
//! passes or the retry limit is reached.
//!
//! Three backends, tried in order:
//! 1. **Vertex AI** — set `AUTO_PACKAGE_GCP_PROJECT` (or configure a default
//!    project with `gcloud config set project`). Token from
//!    `gcloud auth print-access-token`. Override region with
//!    `AUTO_PACKAGE_GCP_REGION` (default `us-east5`).
//! 2. **Direct Anthropic API** — set `AUTO_PACKAGE_API_KEY`.
//! 3. **`claude` CLI** — zero config; uses whatever session `claude` is
//!    already authenticated with. Token counts come from `--output-format json`.

use std::cell::RefCell;
use std::path::Path;
use std::process::Command;

use serde::{Deserialize, Serialize};

use crate::context::{Escalation, Level};
use crate::log::{ExtractedLog, PathMapper};

const MODEL: &str = "claude-opus-5";

thread_local! {
    /// Per-thread output sink for agent diagnostic lines.
    ///
    /// `None`  → write to stderr (CLI mode, spinner enabled).
    /// `Some`  → call the closure (TUI mode, spinner suppressed).
    ///
    /// Each parallel build thread sets its own logger so lines from concurrent
    /// packages don't conflict.
    static AGENT_LOGGER: RefCell<Option<Box<dyn Fn(&str) + 'static>>> =
        const { RefCell::new(None) };
}

/// Route agent diagnostic output through `f` for this thread.
///
/// Setting a logger automatically suppresses the indicatif spinner (which
/// writes directly to stderr and corrupts any TUI or structured output).
pub fn set_agent_logger(f: impl Fn(&str) + 'static) {
    AGENT_LOGGER.with(|cell| *cell.borrow_mut() = Some(Box::new(f)));
}

/// Emit one agent log line — goes to the registered logger or to stderr.
fn alog(msg: &str) {
    AGENT_LOGGER.with(|cell| match cell.borrow().as_ref() {
        Some(f) => f(msg),
        None => eprintln!("{msg}"),
    });
}

/// True when a custom logger is installed (TUI mode).
fn has_custom_logger() -> bool {
    AGENT_LOGGER.with(|cell| cell.borrow().is_some())
}

const CONVENTIONS: &str = "\
openSUSE spec conventions to keep in mind:

1. BuildRequires version constraints go INSIDE the macro with spaces around the operator:
   CORRECT:   BuildRequires:  %{python_module setuptools >= 77}
   WRONG:     BuildRequires:  %{python_module setuptools} >= 77
   WRONG:     BuildRequires:  %{python_module setuptools>=77}

2. Package name case normalization: pip/setuptools install using the normalized
   (lowercase, underscores) name, not the PyPI display name. For example,
   MarkupSafe installs as 'markupsafe'. This affects:
   - %autosetup -n: use the lowercase sdist directory name
   - %files: all installed paths use the lowercase name
   - dist-info: 'markupsafe-%{version}.dist-info', NOT 'MarkupSafe-%{version}.dist-info'

3. Architecture and C dependencies: if the package has C extensions (.so files):
   - Use %{python_sitearch} instead of %{python_sitelib}
   - Remove 'BuildArch: noarch'
   - Add BuildRequires for any C libraries the extension links against,
     e.g. 'BuildRequires: openssl-devel' for SSL, 'BuildRequires: libffi-devel'
     for cffi. Check the build errors or setup.py/pyproject.toml for hints.
   - Remove installed C source files in %install:
       %python_expand rm -f %{buildroot}%{$python_sitearch}/<pkg>/*.c

4. Use %pyproject_wheel / %pyproject_install for PEP 517 packages.

5. Always include a %check section with %pytest_arch (C extensions) or %pytest
   (pure Python) and add 'BuildRequires: %{python_module pytest}' for it.

6. For multi-flavor builds (standard Factory pattern), the spec should use
   '%global flavor @BUILD_FLAVOR@%{nil}' with a 'test' flavor that only runs
   %check. The %build/%install/%files sections should be inside
   '%if !%{with test}', and %check inside '%if %{with test}'.
   This lets OBS build and test in separate subpackages.

7. Build resources: there are no constraints on RAM or CPU. Always use the
   maximum available parallelism — never add '-j1' or any other job limit to
   make, cmake, or pytest invocations.

8. Test failures: attempt to fix a failing test up to twice before giving up
   (e.g. by adding a patch, adjusting the %check invocation, or adding a
   missing test dependency). These two attempts do not count against the
   build budget. Only skip the specific failing tests (via pytest's -k or
   --ignore) after two unsuccessful fix attempts, and only as a last resort
   — never skip the entire %check section.

9. File reading: when you receive an \"Available source files\" manifest, you
   may request the content of any listed file by adding its path to
   `files_needed`. The build system will read those files and call you again
   with their contents before you must produce the final fix. Request only
   the files you actually need — each costs tokens.

10. Missing module in %check (e.g. `ModuleNotFoundError: No module named
   'conftest'` or any other import error): a test dependency is absent.
   Read the packaging file (pyproject.toml [project.optional-dependencies] /
   setup.cfg [options.extras_require]) to identify all test deps and add
   them as `BuildRequires: %{python_module <dep>}`. If a required package
   is not yet available in the OBS project and must be built first, include
   its PyPI name in `missing_packages` so the build system can queue it
   automatically. Only list packages that genuinely need to be built; omit
   ones already available (e.g. in Factory).

11. Spec comments: the spec may begin with `# ap: gen:` lines that document
   generation decisions (detected backend, test status, architecture). These
   are ground truth — do NOT remove or contradict them. For example, if you
   see `# ap: gen: tests: none in sdist — %%check omitted`, do not add a
   %%check section. When you make a fix, add ONE new line immediately before
   the first line you changed:
       # ap: fix N: <one sentence: what changed and why>
   where N is the current attempt number (provided in the prompt). Preserve
   all existing `# ap:` lines unchanged.

12. %files paths must be exact — never use shell globs (e.g. never write
   'outlines_core*' or 'Package-*'). The standard entries for a Python package:
   - Module directory:    %{python_sitelib}/package_name/
   - dist-info:          %{python_sitelib}/package_name-%{version}.dist-info/
   - C extension module: %{python_sitearch}/package_name/
   A trailing '*' risks claiming unrelated files and causes 'file listed twice'
   errors. Always use the exact normalized (lowercase) path. If you are unsure
   of the exact path, consult the BUILDROOT listing in the context.

13. RPM macros use single '%', never '%%'. For example: %check, %install,
   %{version}, %{python_module foo}. Only use %% when you need a literal
   percent character in the spec file output (which is rare).";

/// Token counts and cost from one API call.
#[derive(Debug, Clone, Default)]
pub struct Usage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_creation_tokens: u64,
    pub cache_read_tokens: u64,
    /// Pre-computed cost in USD from the CLI's own accounting. When set,
    /// `cost_usd()` returns this directly instead of computing from token
    /// counts (which include CLI session overhead and are misleading).
    pub cli_cost_usd: Option<f64>,
}

impl Usage {
    /// Cost in US dollars.
    ///
    /// Uses the CLI's `total_cost_usd` when available (CLI backend). For
    /// HTTP backends, computes from token counts at direct API rates for
    /// claude-opus-5: $5/MTok input, $25/MTok output, $6.25/MTok cache
    /// write, $0.50/MTok cache read.
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

enum Backend {
    Vertex { project: String, region: String },
    Direct { api_key: String },
    Cli,
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("could not run claude: {0}")]
    Launch(std::io::Error),

    #[error("claude exited {code}:\n{stderr}")]
    Failed { code: i32, stderr: String },

    #[error("gcloud auth print-access-token failed: {0}")]
    GcloudAuth(String),

    #[error("API request failed: {0}")]
    Http(#[from] ureq::Error),

    #[error("unexpected response: {0}")]
    Parse(String),

    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

/// Confidence the model reports about its fix.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Confidence {
    High,
    Medium,
    Low,
}

// ── agent-v2 public types ─────────────────────────────────────────────────────

/// Which task the agent should perform this turn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Task {
    /// No build has run yet — inspect the generated spec before the first build.
    Review,
    /// `build.log` in the workspace holds a failed `osc build`.
    FixAfterBuild,
    /// `rpmspec.txt` in the workspace holds a failed `rpmspec --parse`.
    FixAfterValidation,
}

/// Per-turn tool-call and token budget.
#[derive(Debug, Clone)]
pub struct TurnBudget {
    /// Maximum number of tool calls the agent may make before it must `finish`.
    pub max_tool_calls: usize,
    /// Maximum input+output tokens for this turn.
    pub max_tokens: u64,
}

impl Default for TurnBudget {
    fn default() -> Self {
        Self { max_tool_calls: 15, max_tokens: 200_000 }
    }
}

/// Input to one agent turn.
pub struct TurnRequest<'a> {
    /// The workspace root (REDESIGN §2.2).
    pub workspace: &'a std::path::Path,
    /// PyPI package name (e.g. `markupsafe`).
    pub pkg_name: &'a str,
    pub version: &'a str,
    /// What kind of turn this is.
    pub task: Task,
    /// 1-based attempt counter covering all turns for this package.
    pub attempt: usize,
    pub max_attempts: usize,
    /// Total USD spent so far across all prior turns for this package.
    pub spent_usd: f64,
    /// Heuristic extract from the log — a hint, not a filter. `None` for Review.
    pub hint: Option<&'a crate::log::ExtractedLog>,
    /// Contents of `notes.md` from the workspace.
    pub notes: &'a str,
    pub budget: TurnBudget,
}

/// What the agent did this turn.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// The agent edited `spec`.
    Edited,
    /// The agent found nothing to change.
    NoChange,
    /// The agent believes no spec change can fix this.
    Blocked { why: String },
}

/// Result of one agent turn.
pub struct TurnOutcome {
    pub action: Action,
    pub reasoning: String,
    pub missing_packages: Vec<String>,
    pub confidence: Confidence,
    pub usage: Usage,
    pub tool_calls: usize,
    /// True when the agent read beyond the supplied hint (grepped or read
    /// build.log directly). Recorded as a quality metric for the extractor.
    pub went_past_hint: bool,
    /// The full prompt sent to the agent (for TUI display).
    pub prompt: String,
}

/// JSON schema for the v2 turn final answer (returned via `--json-schema`).
const TURN_RESPONSE_SCHEMA: &str = r#"{
  "type": "object",
  "properties": {
    "action": {"type": "string", "enum": ["edited", "no_change", "blocked"]},
    "why": {"type": "string"},
    "reasoning": {"type": "string"},
    "confidence": {"type": "string", "enum": ["high", "medium", "low"]},
    "missing_packages": {"type": "array", "items": {"type": "string"}},
    "went_past_hint": {"type": "boolean"}
  },
  "required": ["action", "reasoning", "confidence", "went_past_hint"],
  "additionalProperties": false
}"#;

#[derive(Deserialize)]
struct TurnResponse {
    action: String,
    #[serde(default)]
    why: String,
    reasoning: String,
    confidence: Confidence,
    #[serde(default)]
    missing_packages: Vec<String>,
    #[serde(default)]
    went_past_hint: bool,
}

/// Run one agent turn against the workspace (agent-v2).
///
/// The agent has full tool access to the workspace and edits `spec` in place.
/// The caller is responsible for:
/// - snapshotting `spec` before the call
/// - running `rpmspec --parse` after and rolling back on failure
/// - appending the result to `notes.md`
pub fn turn(req: TurnRequest<'_>) -> Result<TurnOutcome, Error> {
    let prompt = build_turn_prompt(&req);

    let schema_path = req.workspace.join(".turn_schema.json");
    std::fs::write(&schema_path, TURN_RESPONSE_SCHEMA).map_err(Error::Io)?;

    if has_custom_logger() {
        alog("  ⋯  waiting for Claude… (v2)");
    }

    let pb = if has_custom_logger() {
        None
    } else {
        use indicatif::{ProgressBar, ProgressStyle};
        use std::time::Duration;
        let pb = ProgressBar::new_spinner();
        pb.set_style(
            ProgressStyle::default_spinner()
                .template("{spinner} {msg}")
                .expect("static template"),
        );
        pb.set_message("waiting for Claude… (v2)");
        pb.enable_steady_tick(Duration::from_millis(100));
        Some(pb)
    };

    let out = Command::new("claude")
        .args([
            "--print",
            "--output-format",
            "json",
            "--json-schema",
            schema_path.to_str().unwrap_or_default(),
            "--permission-mode",
            "acceptEdits",
            "--add-dir",
            req.workspace.to_str().unwrap_or_default(),
            "--allowed-tools",
            "Read,Grep,Edit,Bash(rpmspec *)",
            "--bare",
            "-p",
            &prompt,
        ])
        .current_dir(req.workspace)
        .stdin(std::process::Stdio::null())
        .output()
        .map_err(Error::Launch)?;

    if let Some(pb) = pb {
        pb.finish_and_clear();
    }

    // Clean up temporary schema file.
    let _ = std::fs::remove_file(&schema_path);

    if !out.status.success() {
        return Err(Error::Failed {
            code: out.status.code().unwrap_or(-1),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        });
    }

    #[derive(Deserialize)]
    struct CliOutput {
        result: String,
        #[serde(default)]
        total_cost_usd: Option<f64>,
        #[serde(default)]
        usage: CliUsage2,
        #[serde(default)]
        num_turns: usize,
    }
    #[derive(Deserialize, Default)]
    struct CliUsage2 {
        #[serde(default)]
        input_tokens: u64,
        #[serde(default)]
        output_tokens: u64,
    }

    let envelope: CliOutput = serde_json::from_slice(&out.stdout)
        .map_err(|e| Error::Parse(format!("bad CLI JSON: {e}")))?;

    let resp: TurnResponse = serde_json::from_str(envelope.result.trim())
        .map_err(|e| Error::Parse(format!("bad turn JSON: {e}")))?;

    let usage = Usage {
        input_tokens: envelope.usage.input_tokens,
        output_tokens: envelope.usage.output_tokens,
        cli_cost_usd: envelope.total_cost_usd,
        ..Usage::default()
    };

    let action = match resp.action.as_str() {
        "edited" => Action::Edited,
        "no_change" => Action::NoChange,
        _ => Action::Blocked { why: resp.why },
    };

    alog(&format!("  turn-v2  {:?}: {}", resp.confidence, resp.reasoning));

    Ok(TurnOutcome {
        action,
        reasoning: resp.reasoning,
        missing_packages: resp.missing_packages,
        confidence: resp.confidence,
        usage,
        tool_calls: envelope.num_turns,
        went_past_hint: resp.went_past_hint,
        prompt,
    })
}

fn build_turn_prompt(req: &TurnRequest<'_>) -> String {
    let task_desc = match req.task {
        Task::Review => "Review the generated spec before the first build.",
        Task::FixAfterBuild => "Fix the spec after a failed `osc build`.",
        Task::FixAfterValidation => "Fix the spec after a failed `rpmspec --parse`.",
    };

    let budget_line = format!(
        "Attempt {attempt} of {max}. ${spent:.2} spent so far. You may make up to {calls} tool calls this turn.",
        attempt = req.attempt,
        max = req.max_attempts,
        spent = req.spent_usd,
        calls = req.budget.max_tool_calls,
    );

    // Workspace map: list every path that actually exists.
    let ws = req.workspace;
    let mut map_lines = Vec::new();
    let entries = [
        ("spec", "the spec file (writable — use Edit to change it)", true),
        ("spec.generated", "pristine generated spec for reference", false),
        ("build.log", "full osc build log from the last attempt", false),
        ("rpmspec.txt", "rpmspec --parse stderr from the last failure", false),
        ("sdist/", "extracted source tarball (read-only)", false),
        ("source/", "source root directory (symlink to sdist top-level, read-only)", false),
        ("buildroot/", "BUILDROOT from the last build (read-only)", false),
        ("notes.md", "attempt journal — read this first", false),
    ];
    for (name, desc, always) in &entries {
        let path = ws.join(name);
        if *always || path.exists() {
            map_lines.push(format!("  {name:<20} {desc}"));
        }
    }
    let workspace_map = map_lines.join("\n");

    // Hint section (labelled as heuristic).
    let hint_section = if let Some(hint) = req.hint {
        let mut s = String::from(
            "The following is a heuristic extract of the build log. \
             It may be incomplete or wrong. The full log is at build.log.\n\n",
        );
        if !hint.excerpt.is_empty() {
            s.push_str("Excerpt:\n```\n");
            s.push_str(&hint.excerpt);
            s.push_str("\n```\n");
        }
        if !hint.error_lines.is_empty() {
            s.push_str("Error lines:\n");
            for l in &hint.error_lines {
                s.push_str("  ");
                s.push_str(l);
                s.push('\n');
            }
        }
        s
    } else {
        String::new()
    };

    // CONVENTIONS (v2 edits per REDESIGN §4).
    let conventions_v2 = CONVENTIONS
        // Rewrite item 8: replace hard two-attempt rule with agent judgement.
        .replace(
            "8. Test failures: attempt to fix a failing test up to twice before giving up \
             (e.g. by adding a patch, adjusting the %check invocation, or adding a \
             missing test dependency). These two attempts do not count against the \
             build budget. Only skip the specific failing tests (via pytest's -k or \
             --ignore) after two unsuccessful fix attempts, and only as a last resort \
             — never skip the entire %check section.",
            "8. Test failures: prefer fixing the root cause. First look for a missing \
             test dependency in pyproject.toml [project.optional-dependencies] or \
             setup.cfg [options.extras_require] and add it as BuildRequires. Skipping \
             specific tests (via pytest -k or --ignore) is a last resort. Never \
             disable %check wholesale without explaining why in your reasoning.",
        )
        // Rewrite item 9: replace files_needed with tool description.
        .replace(
            "9. File reading: when you receive an \"Available source files\" manifest, you \
             may request the content of any listed file by adding its path to \
             `files_needed`. The build system will read those files and call you again \
             with their contents before you must produce the final fix. Request only \
             the files you actually need — each costs tokens.",
            "9. Tools available — USE THEM:\n\
             - Read: get line-numbered file content (use for build.log, pyproject.toml, setup.py)\n\
             - Grep: search for patterns (use to find test dependencies, entry points, etc.)\n\
             - Edit: modify the spec file (the ONLY writable file)\n\
             - Bash: run `rpmspec --parse spec` to validate syntax\n\
             \n\
             If you need information, READ IT. Don't say \"I need to check\" without calling Read/Grep.\n\
             Examples:\n\
             - Missing test dep? → Grep pyproject.toml for 'test'\n\
             - Entry point error? → Read setup.py or Grep pyproject.toml for 'entry'\n\
             - Unclear error? → Read build.log\n\
             - Before editing? → Read spec to see current state",
        );

    let notes_section = if req.notes.is_empty() {
        String::new()
    } else {
        format!("## Prior attempt journal (notes.md)\n\n{}\n\n", req.notes)
    };

    format!(
        "You are fixing an openSUSE RPM spec for the Python package {pkg} {version}.\n\
         Task: {task}\n\
         \n\
         {budget}\n\
         \n\
         ## Workspace\n\
         {map}\n\
         \n\
         {conventions}\n\
         \n\
         CRITICAL: NEVER delete the spec file completely. You can edit it, but it must \
         remain present. Deleting it will break the build system.\n\
         \n\
         {notes}\
         {hint}\
         \n\
         ## Workflow\n\
         \n\
         1. **Investigate FIRST**: Use your tools to gather information BEFORE editing.\n\
            - If you say \"I need to check X\" — DO IT. Use Read/Grep/Bash immediately.\n\
            - Never say \"I should verify\" or \"I need to check\" without making the tool call.\n\
            - If the hint is unclear, Read build.log. If you need test deps, Grep pyproject.toml.\n\
            - Set went_past_hint=true if you read/grep beyond the provided hint.\n\
         \n\
         2. **Edit the spec**: Use Edit to change `spec` (never return full spec as text).\n\
         \n\
         3. **Validate**: Run `rpmspec --parse spec` to verify syntax.\n\
         \n\
         4. **Finish**: Return a JSON object with:\n\
         - \"action\": \"edited\" (you changed the spec), \"no_change\" (spec is already correct), \
           or \"blocked\" (no spec change can fix this)\n\
         - \"why\": required when action is \"blocked\" — one sentence explaining why\n\
         - \"reasoning\": one sentence summarising what you did (not what you planned to do)\n\
         - \"confidence\": \"high\" (verified the fix works), \"medium\" (logical fix but unverified), \
           or \"low\" (guessing)\n\
         - \"missing_packages\": PyPI names of packages that must be built first (if any)\n\
         - \"went_past_hint\": true if you read build.log or grepped files beyond the hint\n\
         \n\
         CRITICAL: If you say you need information, USE YOUR TOOLS to get it. Don't guess.",
        pkg = req.pkg_name,
        version = req.version,
        task = task_desc,
        budget = budget_line,
        map = workspace_map,
        conventions = conventions_v2,
        notes = notes_section,
        hint = hint_section,
    )
}

/// One round of the fix loop, kept so the next attempt can learn from it.
#[derive(Debug, Clone)]
pub struct Turn {
    /// The error excerpt that prompted this fix.
    pub error_excerpt: String,
    /// The full prompt that was sent.
    pub prompt: String,
    /// The reasoning the agent gave.
    pub reasoning: String,
    /// The spec the agent produced (which then failed to build).
    pub fixed_spec: String,
}

#[derive(Deserialize)]
struct FixResponse {
    confidence: Confidence,
    reasoning: String,
    #[serde(default)]
    fixed_spec: String,
    #[serde(default)]
    missing_packages: Vec<String>,
    /// Paths (relative to sdist root) whose contents the agent needs before
    /// it can produce a confident fix. Only used at the FailingFiles escalation
    /// level; the orchestrator will read them and make a second call.
    #[serde(default)]
    files_needed: Vec<String>,
    /// True when the agent found no issues — the spec requires no changes.
    /// Replaces prose-sniffing on `reasoning.contains("No issues")`.
    #[serde(default)]
    no_change: bool,
}

/// Review a newly generated spec before the first build.
///
/// Returns `Some((fixed_spec, usage))` when the agent finds a medium- or
/// high-confidence issue to fix. Returns `None` when the spec looks fine or
/// the agent is only guessing — no fix is applied in that case.
pub fn review(
    spec_path: &Path,
    pkg_name: &str,
    tarball_files: &[String],
) -> Result<Option<(String, Usage, String, String)>, Error> {
    let backend = detect_backend();
    let spec = std::fs::read_to_string(spec_path)?;

    let files_section = if tarball_files.is_empty() {
        String::new()
    } else {
        format!(
            "\nFiles in the sdist tarball (first 60):\n{}\n",
            tarball_files
                .iter()
                .take(60)
                .map(|f| format!("  {f}"))
                .collect::<Vec<_>>()
                .join("\n")
        )
    };

    let prompt = format!(
        "You are reviewing an openSUSE RPM spec file for a Python package BEFORE building it.\n\
         Package: {pkg_name}\n\
         {files_section}\n\
         Spec:\n\
         ```spec\n\
         {spec}\n\
         ```\n\
         \n\
         {conventions}\n\
         Return a JSON object with:\n\
         - \"confidence\": \"high\", \"medium\", or \"low\"\n\
         - \"reasoning\": one sentence describing any issue found, or 'No issues found'\n\
         - \"no_change\": true if the spec is correct and requires no edits, false otherwise\n\
         - \"fixed_spec\": the corrected spec (ONLY include if changes are needed; omit if no_change is true)\n\
         \n\
         Return only the JSON object, no other text.",
        conventions = CONVENTIONS,
    );

    let (response, usage, prompt) = call_prompt(&backend, &prompt)?;

    alog(&format!("  review  {:?}: {}", response.confidence, response.reasoning));

    // Only apply the fix if the agent is reasonably confident and found something.
    if response.confidence == Confidence::Low || response.no_change {
        return Ok(None);
    }

    // If changes are needed but no fixed_spec was provided, that's an error
    if response.fixed_spec.is_empty() {
        return Err(Error::Parse(
            "agent indicated changes needed but did not provide fixed_spec".to_string()
        ));
    }

    Ok(Some((response.fixed_spec, usage, prompt, response.reasoning)))
}

/// Try to fix `spec_path` using the build log at `log_path`.
///
/// `escalation` tracks how far up the context ladder this error signature has
/// climbed. When the agent returns low confidence, the caller should call
/// `escalation.escalate()` and retry with more context.
///
/// `sdist_root` is the top-level directory from the extracted sdist tarball.
/// Used to read source files at `Level::BuildTree` and `Level::FailingFiles`.
///
/// `allow_full_log`: when `true` and the escalation has reached
/// `Level::FullLog`, the entire raw log is appended to the prompt. This can
/// exceed 34k tokens for a trivial package — only enable it when lower rungs
/// have failed repeatedly.
///
/// `force_test_skip`: when `true`, the prompt instructs the agent to skip
/// the failing tests (up to and including the entire %check section) rather
/// than attempt another fix.
///
/// `section_hint`: overrides the failed-section label in the prompt when the
/// log format doesn't produce one (e.g. rpmspec errors).
///
/// Returns the fixed spec, token usage, updated history, confidence, and any
/// PyPI packages the agent identified as missing from the OBS project.
pub fn fix(
    spec_path: &Path,
    log_path: &Path,
    pkg_name: &str,
    history: &[Turn],
    escalation: &Escalation,
    sdist_root: &Path,
    allow_full_log: bool,
    force_test_skip: bool,
    section_hint: Option<&str>,
) -> Result<(String, Usage, Vec<Turn>, Confidence, Vec<String>), Error> {
    let backend = detect_backend();

    let spec = std::fs::read_to_string(spec_path)?;
    let log_raw = std::fs::read_to_string(log_path)?;

    let mapper = PathMapper::with_sdist_root(pkg_name);
    let extracted = crate::log::extract(&log_raw, &mapper);
    let section = extracted.failed_section.as_deref().unwrap_or("unknown");

    // Gather additional context based on the current escalation level.
    let (extra_context, context_summary) = gather_context(
        escalation.level(),
        &extracted,
        &log_raw,
        sdist_root,
        allow_full_log,
    );

    // Log what we're sending so the operator can see the context ladder in action.
    let excerpt_lines = extracted.excerpt.lines().count();
    let using_tail = excerpt_lines == 0 && extracted.error_lines.is_empty();
    let excerpt_desc = if using_tail {
        format!("excerpt=0 lines (using log tail — extractor found no signal)")
    } else {
        format!("excerpt={excerpt_lines} lines")
    };
    if context_summary.is_empty() {
        alog(&format!("  agent  sending: {excerpt_desc}, section={section}"));
    } else {
        alog(&format!(
            "  agent  sending: {excerpt_desc}, section={section}, extra={context_summary}"
        ));
    }

    let (mut response, mut usage, mut prompt) = call_with_context(
        &backend,
        &spec,
        &extracted,
        pkg_name,
        history,
        &extra_context,
        force_test_skip,
        section_hint,
    )?;

    // Two-turn file reading at FailingFiles: if the agent listed files it needs,
    // read them and make a second call with their contents.
    if escalation.level() == Level::FailingFiles && !response.files_needed.is_empty() {
        let mut file_contents = String::new();

        for requested in &response.files_needed {
            // Reject absolute paths and directory traversal.
            let rel = std::path::Path::new(requested);
            if rel.is_absolute()
                || rel
                    .components()
                    .any(|c| c == std::path::Component::ParentDir)
            {
                continue;
            }
            let full = sdist_root.join(rel);
            if let Ok(content) = std::fs::read_to_string(&full) {
                let content = middle_truncate(&content, 32_768);
                file_contents.push_str(&format!("\n--- {requested} ---\n{content}\n"));
            }
        }

        if !file_contents.is_empty() {
            alog(&format!(
                "  agent  requested {} file(s) — reading and calling again",
                response.files_needed.len()
            ));
            let extra_with_files =
                format!("{extra_context}\nRequested file contents:{file_contents}");
            let (r2, u2, p2) = call_with_context(
                &backend,
                &spec,
                &extracted,
                pkg_name,
                history,
                &extra_with_files,
                force_test_skip,
                section_hint,
            )?;
            usage.add(&u2);
            response = r2;
            prompt = p2;
        }
    }

    alog(&format!("  agent  {:?}: {}", response.confidence, response.reasoning));

    let mut new_history = history.to_vec();
    new_history.push(Turn {
        error_excerpt: extracted.excerpt.clone(),
        prompt,
        reasoning: response.reasoning.clone(),
        fixed_spec: response.fixed_spec.clone(),
    });

    let confidence = response.confidence.clone();
    let missing = response.missing_packages.clone();
    Ok((response.fixed_spec, usage, new_history, confidence, missing))
}

/// Scan `log_raw` for paths that were installed to BUILDROOT.
///
/// Looks at every whitespace-delimited token that contains "/BUILDROOT/" and
/// classifies it via [`PathMapper`]. Returns deduplicated module-relative paths,
/// capped at 40 entries. Used when `%files` fails, because Python tracebacks
/// (the normal source of BUILDROOT candidates) don't exist for that error class.
fn extract_buildroot_paths(log_raw: &str) -> Vec<String> {
    use crate::log::{strip_timestamp, PathClass, PathMapper};

    let mapper = PathMapper::default();
    let mut paths: Vec<String> = Vec::new();
    let mut seen = std::collections::HashSet::new();

    for raw_line in log_raw.lines() {
        let line = strip_timestamp(raw_line);
        if !line.contains("/BUILDROOT/") {
            continue;
        }
        for token in line.split_whitespace() {
            if !token.contains("/BUILDROOT/") {
                continue;
            }
            // Strip common trailing punctuation that isn't part of the path.
            let path = token.trim_end_matches([',', ':', ';', '\'', '"']);
            if let PathClass::BuildRoot { module_path } = mapper.classify(path) {
                if !module_path.is_empty() && seen.insert(module_path.clone()) {
                    paths.push(module_path);
                    if paths.len() >= 40 {
                        return paths;
                    }
                }
            }
        }
    }

    paths
}

/// Read additional context for the given escalation level.
///
/// `sdist_root` is the top-level directory extracted from the sdist tarball
/// (e.g. `/tmp/work/markupsafe-3.0.3/`). All relative paths from the log
/// are resolved against this root.
///
/// Returns `(context_string, summary)` where `summary` is a short human-readable
/// description of what extra material is being sent (e.g. "2 source files").
/// Callers print the summary so the operator knows what the agent is seeing.
fn gather_context(
    level: Level,
    extracted: &ExtractedLog,
    log_raw: &str,
    sdist_root: &Path,
    allow_full_log: bool,
) -> (String, String) {
    // %files failures don't generate Python tracebacks, so installed_candidates()
    // is always empty for them. Detect by failed_section OR by recognising the
    // rpm error patterns in error_lines (since %files isn't a shell scriptlet,
    // it often doesn't produce a "Bad exit status" with a section name).
    let is_files_failure = extracted.failed_section.as_deref() == Some("%files")
        || extracted.error_lines.iter().any(|l| {
            l.contains("File not found") || l.contains("unpackaged") || l.contains("file(s) found")
        });
    let mut files_prefix = String::new();
    let mut files_summary = String::new();
    if is_files_failure {
        let paths = extract_buildroot_paths(log_raw);
        if !paths.is_empty() {
            files_prefix.push_str("\nInstalled paths in BUILDROOT (from build log):\n");
            for p in &paths {
                files_prefix.push_str(&format!("  {p}\n"));
            }
            files_summary = format!("{} BUILDROOT path(s)", paths.len());
        }
    }

    let (level_ctx, level_summary) = match level {
        Level::ErrorContext => (String::new(), String::new()),
        Level::BuildTree => {
            // Read the source files the extractor flagged as relevant.
            let candidates = extracted.source_candidates(3);
            if candidates.is_empty() {
                ("".to_string(), "no source candidates".to_string())
            } else {
                let mut out = String::from("\nRelevant source files from the sdist:\n");
                let mut read = Vec::new();
                for rel in &candidates {
                    let full = sdist_root.join(rel);
                    if let Ok(src) = std::fs::read_to_string(&full) {
                        out.push_str(&format!("\n--- {rel} ---\n{src}\n"));
                        read.push(*rel);
                    }
                }
                let summary = if read.is_empty() {
                    "source candidates found but unreadable".to_string()
                } else {
                    format!("source files: {}", read.join(", "))
                };
                (out, summary)
            }
        }
        Level::BuildRoot => {
            // List installed paths from the extracted log.
            let paths = extracted.installed_candidates(10);
            if paths.is_empty() {
                ("".to_string(), "no BUILDROOT candidates".to_string())
            } else {
                let mut out = String::from("\nInstalled paths in BUILDROOT:\n");
                for p in &paths {
                    out.push_str(&format!("  {p}\n"));
                }
                let summary = format!("{} BUILDROOT path(s)", paths.len());
                (out, summary)
            }
        }
        Level::FailingFiles => {
            // Send a manifest of available files rather than their contents.
            // The agent requests specific files via `files_needed`; the caller
            // reads them and makes a second call with their actual text.
            let mut available: Vec<String> =
                extracted.source_candidates(20).into_iter().map(str::to_owned).collect();

            const BUILD_CONFIGS: &[&str] = &[
                "pyproject.toml",
                "setup.py",
                "setup.cfg",
                "CMakeLists.txt",
                "meson.build",
                "configure.ac",
                "Makefile.am",
            ];
            for name in BUILD_CONFIGS {
                if sdist_root.join(name).exists()
                    && !available.iter().any(|f| f == name)
                {
                    available.push((*name).to_owned());
                }
            }

            if available.is_empty() {
                ("".to_string(), "no files available to offer".to_string())
            } else {
                let listing = available
                    .iter()
                    .map(|f| format!("  {f}"))
                    .collect::<Vec<_>>()
                    .join("\n");
                let summary = format!("manifest: {} file(s) offered", available.len());
                (
                    format!(
                        "\nAvailable source files — request their contents via `files_needed`:\n{listing}\n"
                    ),
                    summary,
                )
            }
        }
        Level::FullLog => {
            if allow_full_log {
                let lines = log_raw.lines().count();
                (
                    format!("\nFull build log (unextracted):\n```\n{log_raw}\n```\n"),
                    format!("full log ({lines} lines)"),
                )
            } else {
                ("".to_string(), "full log suppressed (use --allow-full-log)".to_string())
            }
        }
    };

    // Combine the %files BUILDROOT prefix (if any) with the level-specific context.
    let combined_ctx = files_prefix + &level_ctx;
    let combined_summary = match (files_summary.is_empty(), level_summary.is_empty()) {
        (true, _) => level_summary,
        (_, true) => files_summary,
        _ => format!("{files_summary}, {level_summary}"),
    };
    (combined_ctx, combined_summary)
}

fn detect_backend() -> Backend {
    let gcp_project = std::env::var("AUTO_PACKAGE_GCP_PROJECT")
        .ok()
        .or_else(gcloud_default_project);

    if let Some(project) = gcp_project {
        let region =
            std::env::var("AUTO_PACKAGE_GCP_REGION").unwrap_or_else(|_| "us-east5".to_string());
        return Backend::Vertex { project, region };
    }

    if let Ok(api_key) = std::env::var("AUTO_PACKAGE_API_KEY") {
        return Backend::Direct { api_key };
    }

    Backend::Cli
}

fn gcloud_default_project() -> Option<String> {
    let out = Command::new("gcloud")
        .args(["config", "get-value", "project"])
        .stdin(std::process::Stdio::null())
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let project = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if project.is_empty() || project == "(unset)" {
        return None;
    }
    Some(project)
}

/// Send a ready-made prompt to whichever backend is configured.
///
/// In TUI mode (custom logger installed) emits a "⋯ waiting for Claude…" line
/// so the operator can see when the agent is blocked on the API — the spinner
/// that would normally appear on stderr is suppressed there.
fn call_prompt(backend: &Backend, prompt: &str) -> Result<(FixResponse, Usage, String), Error> {
    if has_custom_logger() {
        alog("  ⋯  waiting for Claude…");
    }
    match backend {
        Backend::Cli => call_cli_prompt(prompt),
        _ => call_http_prompt(backend, prompt),
    }
}

/// Build the prompt from spec + extracted log + history + extra context, then send it.
fn call_with_context(
    backend: &Backend,
    spec: &str,
    extracted: &ExtractedLog,
    pkg_name: &str,
    history: &[Turn],
    extra_context: &str,
    force_test_skip: bool,
    section_hint: Option<&str>,
) -> Result<(FixResponse, Usage, String), Error> {
    let prompt = build_prompt(spec, extracted, pkg_name, history, extra_context, force_test_skip, section_hint);
    call_prompt(backend, &prompt)
}

/// JSON Schema passed to `claude --json-schema` to enforce structured output.
/// This prevents the model from returning prose instead of JSON.
const RESPONSE_SCHEMA: &str = r#"{
  "type": "object",
  "properties": {
    "confidence": {"type": "string", "enum": ["high", "medium", "low"]},
    "reasoning": {"type": "string"},
    "no_change": {"type": "boolean"},
    "fixed_spec": {"type": "string"},
    "missing_packages": {"type": "array", "items": {"type": "string"}},
    "files_needed": {"type": "array", "items": {"type": "string"}}
  },
  "required": ["confidence", "reasoning", "no_change", "fixed_spec"],
  "additionalProperties": false
}"#;

fn call_cli_prompt(prompt: &str) -> Result<(FixResponse, Usage, String), Error> {
    use std::time::Duration;

    let pb = if has_custom_logger() {
        None
    } else {
        use indicatif::{ProgressBar, ProgressStyle};
        let pb = ProgressBar::new_spinner();
        pb.set_style(
            ProgressStyle::default_spinner()
                .template("{spinner} {msg}")
                .expect("static template"),
        );
        pb.set_message("waiting for Claude…");
        pb.enable_steady_tick(Duration::from_millis(100));
        Some(pb)
    };

    let out = Command::new("claude")
        .args([
            "--print",
            "--output-format",
            "json",
            "--json-schema",
            RESPONSE_SCHEMA,
            "-p",
            prompt,
        ])
        .stdin(std::process::Stdio::null())
        .output()
        .map_err(Error::Launch)?;

    if let Some(pb) = pb {
        pb.finish_and_clear();
    }

    if !out.status.success() {
        return Err(Error::Failed {
            code: out.status.code().unwrap_or(-1),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        });
    }

    #[derive(Deserialize)]
    struct CliOutput {
        result: String,
        #[serde(default)]
        total_cost_usd: Option<f64>,
        #[serde(default)]
        usage: CliUsage,
    }
    #[derive(Deserialize, Default)]
    struct CliUsage {
        #[serde(default)]
        input_tokens: u64,
        #[serde(default)]
        output_tokens: u64,
    }

    let envelope: CliOutput = serde_json::from_slice(&out.stdout)
        .map_err(|e| Error::Parse(format!("bad CLI JSON: {e}")))?;

    let fix_response = parse_response(&envelope.result)?;
    let usage = Usage {
        input_tokens: envelope.usage.input_tokens,
        output_tokens: envelope.usage.output_tokens,
        // CLI session tokens include project context overhead — use the
        // pre-computed cost from the CLI instead.
        cli_cost_usd: envelope.total_cost_usd,
        ..Usage::default()
    };

    Ok((fix_response, usage, prompt.to_string()))
}

fn call_http_prompt(
    backend: &Backend,
    prompt: &str,
) -> Result<(FixResponse, Usage, String), Error> {
    #[derive(Deserialize)]
    struct ApiResponse {
        content: Vec<ContentBlock>,
        usage: RawUsage,
    }
    #[derive(Deserialize)]
    struct ContentBlock {
        #[serde(rename = "type")]
        kind: String,
        text: Option<String>,
    }
    #[derive(Deserialize, Default)]
    struct RawUsage {
        #[serde(default)]
        input_tokens: u64,
        #[serde(default)]
        output_tokens: u64,
        #[serde(default)]
        cache_creation_input_tokens: u64,
        #[serde(default)]
        cache_read_input_tokens: u64,
    }

    let (json_body, request) = match backend {
        Backend::Direct { api_key } => {
            #[derive(Serialize)]
            struct Req<'a> {
                model: &'a str,
                max_tokens: u32,
                messages: Vec<Msg<'a>>,
            }
            #[derive(Serialize)]
            struct Msg<'a> {
                role: &'a str,
                content: &'a str,
            }
            let body = serde_json::to_string(&Req {
                model: MODEL,
                max_tokens: 8192,
                messages: vec![Msg {
                    role: "user",
                    content: prompt,
                }],
            })
            .map_err(|e| Error::Parse(e.to_string()))?;

            let req = ureq::post("https://api.anthropic.com/v1/messages")
                .header("x-api-key", api_key)
                .header("anthropic-version", "2023-06-01")
                .header("content-type", "application/json");
            (body, req)
        }
        Backend::Vertex { project, region } => {
            #[derive(Serialize)]
            struct Req<'a> {
                anthropic_version: &'a str,
                max_tokens: u32,
                messages: Vec<Msg<'a>>,
            }
            #[derive(Serialize)]
            struct Msg<'a> {
                role: &'a str,
                content: &'a str,
            }
            let body = serde_json::to_string(&Req {
                anthropic_version: "vertex-2023-10-16",
                max_tokens: 8192,
                messages: vec![Msg {
                    role: "user",
                    content: prompt,
                }],
            })
            .map_err(|e| Error::Parse(e.to_string()))?;

            let token = gcloud_token()?;
            let url = format!(
                "https://{region}-aiplatform.googleapis.com/v1/projects/{project}\
                 /locations/{region}/publishers/anthropic/models/{MODEL}:rawPredict"
            );
            let req = ureq::post(&url)
                .header("Authorization", &format!("Bearer {token}"))
                .header("content-type", "application/json");
            (body, req)
        }
        Backend::Cli => unreachable!(),
    };

    let mut body = request
        .send(json_body.as_bytes())
        .map_err(Error::Http)?
        .into_body();
    let text = body
        .read_to_string()
        .map_err(|e| Error::Parse(e.to_string()))?;
    let resp: ApiResponse =
        serde_json::from_str(&text).map_err(|e| Error::Parse(format!("bad API JSON: {e}")))?;

    let content_text = resp
        .content
        .into_iter()
        .find(|b| b.kind == "text")
        .and_then(|b| b.text)
        .ok_or_else(|| Error::Parse("no text block in response".to_string()))?;

    let fix_response = parse_response(&content_text)?;

    let usage = Usage {
        input_tokens: resp.usage.input_tokens,
        output_tokens: resp.usage.output_tokens,
        cache_creation_tokens: resp.usage.cache_creation_input_tokens,
        cache_read_tokens: resp.usage.cache_read_input_tokens,
        cli_cost_usd: None,
    };

    Ok((fix_response, usage, prompt.to_string()))
}

fn build_prompt(
    spec: &str,
    extracted: &ExtractedLog,
    pkg_name: &str,
    history: &[Turn],
    extra_context: &str,
    force_test_skip: bool,
    section_hint: Option<&str>,
) -> String {
    let section = extracted
        .failed_section
        .as_deref()
        .or(section_hint)
        .unwrap_or("unknown");
    let attempt_number = history.len() + 1;
    let mut prior = String::new();
    if !history.is_empty() {
        prior.push_str("\nPrevious fix attempts that did not work:\n");
        for (i, turn) in history.iter().enumerate() {
            prior.push_str(&format!(
                "\nAttempt {}:\n  Reasoning: {}\n  The build then failed with:\n  {}\n",
                i + 1,
                turn.reasoning,
                turn.error_excerpt
                    .lines()
                    .take(10)
                    .collect::<Vec<_>>()
                    .join("\n  "),
            ));
        }
        prior.push_str("\nDo not repeat those fixes. Try a different approach.\n");
    }

    let error_section = if extracted.excerpt.is_empty() && extracted.error_lines.is_empty() {
        // The extractor found no recognisable failure signal. Send the raw log
        // tail so the agent has something concrete rather than generic advice
        // — a blank excerpt produces low-confidence guesses and wastes a build.
        format!(
            "The build log extractor found no recognisable failure pattern. \
             Here are the last lines of the log:\n```\n{}\n```",
            extracted.log_tail,
        )
    } else {
        format!(
            "Current error context:\n```\n{}\n```\n\nError lines:\n{}",
            extracted.excerpt,
            extracted.error_lines.join("\n"),
        )
    };

    let skip_directive = if force_test_skip {
        "\nIMPORTANT: Two fix attempts on this test failure have already been made \
         without success. You MUST now skip the failing tests. First try to skip \
         only the specific failing tests via pytest's -k or --ignore. If that is \
         not feasible (e.g. the failures are too broad or the test names are \
         unclear), skip the entire %check section by removing its body. Do NOT \
         attempt another fix to the underlying test logic.\n"
    } else {
        ""
    };

    format!(
        "You are fixing an openSUSE RPM spec file for a Python package.\n\
         Package: {pkg_name}\n\
         Fix attempt: {attempt_number}\n\
         Failed section: {section}\n\
         {prior}\n\
         {error_section}\n\
         {extra_context}\n\
         {skip_directive}\
         Current spec:\n\
         ```spec\n\
         {spec}\n\
         ```\n\
         \n\
         {conventions}\n\
         Return a JSON object with:\n\
         - \"confidence\": \"high\", \"medium\", or \"low\"\n\
         - \"reasoning\": one sentence explaining the fix\n\
         - \"fixed_spec\": the complete corrected spec file\n\
         \n\
         Return only the JSON object, no other text.",
        conventions = CONVENTIONS,
    )
}

fn parse_response(text: &str) -> Result<FixResponse, Error> {
    // Try the whole text first (ideal: model returned only JSON).
    let trimmed = text.trim();
    if let Ok(r) = serde_json::from_str::<FixResponse>(trimmed) {
        return Ok(r);
    }

    // Strip a single ```json … ``` fence if present.
    let fenced = trimmed
        .trim_start_matches("```json")
        .trim_start_matches("```")
        .trim_end_matches("```")
        .trim();
    if let Ok(r) = serde_json::from_str::<FixResponse>(fenced) {
        return Ok(r);
    }

    // Last resort: find the first `{` and last `}` and try that substring.
    // This handles models that prepend an explanation sentence.
    if let (Some(start), Some(end)) = (trimmed.find('{'), trimmed.rfind('}'))
        && start < end
        && let Ok(r) = serde_json::from_str::<FixResponse>(&trimmed[start..=end])
    {
        return Ok(r);
    }

    Err(Error::Parse(format!(
        "could not find JSON in response:\n{trimmed}"
    )))
}

/// Keep the head and tail of `s` within `max_bytes`, replacing the middle with
/// a truncation marker. Returns the original string if it fits.
fn middle_truncate(s: &str, max_bytes: usize) -> String {
    if s.len() <= max_bytes {
        return s.to_owned();
    }
    let half = max_bytes / 2;
    let head = &s[..half];
    let tail = &s[s.len() - half..];
    format!(
        "{head}\n\n[… {} bytes omitted …]\n\n{tail}",
        s.len() - max_bytes
    )
}

fn gcloud_token() -> Result<String, Error> {
    let out = Command::new("gcloud")
        .args(["auth", "print-access-token"])
        .stdin(std::process::Stdio::null())
        .output()
        .map_err(|e| Error::GcloudAuth(e.to_string()))?;
    if !out.status.success() {
        return Err(Error::GcloudAuth(
            String::from_utf8_lossy(&out.stderr).trim().to_string(),
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}
