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

use std::path::Path;
use std::process::Command;

use serde::{Deserialize, Serialize};

use crate::log::{ExtractedLog, PathMapper};

const MODEL: &str = "claude-opus-5";

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
    fixed_spec: String,
}

/// Try to fix `spec_path` using the build log at `log_path`.
///
/// `history` is the list of previous fix attempts for this package; the agent
/// sees what it already tried so it doesn't repeat the same mistake.
///
/// Returns the fixed spec, token usage, and an updated history to pass to the
/// next call if the build fails again.
pub fn fix(
    spec_path: &Path,
    log_path: &Path,
    pkg_name: &str,
    history: &[Turn],
) -> Result<(String, Usage, Vec<Turn>), Error> {
    let backend = detect_backend();

    let spec = std::fs::read_to_string(spec_path)?;
    let log_raw = std::fs::read_to_string(log_path)?;

    let mapper = PathMapper::with_sdist_root(pkg_name);
    let extracted = crate::log::extract(&log_raw, &mapper);

    let (response, usage, prompt) = call(&backend, &spec, &extracted, pkg_name, history)?;

    eprintln!(
        "  [agent] {:?} — {}",
        response.confidence, response.reasoning,
    );

    let mut new_history = history.to_vec();
    new_history.push(Turn {
        error_excerpt: extracted.excerpt.clone(),
        prompt,
        reasoning: response.reasoning.clone(),
        fixed_spec: response.fixed_spec.clone(),
    });

    Ok((response.fixed_spec, usage, new_history))
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

fn call(
    backend: &Backend,
    spec: &str,
    extracted: &ExtractedLog,
    pkg_name: &str,
    history: &[Turn],
) -> Result<(FixResponse, Usage, String), Error> {
    match backend {
        Backend::Cli => call_cli(spec, extracted, pkg_name, history),
        _ => call_http(backend, spec, extracted, pkg_name, history),
    }
}

fn call_cli(
    spec: &str,
    extracted: &ExtractedLog,
    pkg_name: &str,
    history: &[Turn],
) -> Result<(FixResponse, Usage, String), Error> {
    let prompt = build_prompt(spec, extracted, pkg_name, history);

    let out = Command::new("claude")
        .args(["--print", "--output-format", "json", "-p", &prompt])
        .output()
        .map_err(Error::Launch)?;

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

    Ok((fix_response, usage, prompt))
}

fn call_http(
    backend: &Backend,
    spec: &str,
    extracted: &ExtractedLog,
    pkg_name: &str,
    history: &[Turn],
) -> Result<(FixResponse, Usage, String), Error> {
    let prompt = build_prompt(spec, extracted, pkg_name, history);

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
                    content: &prompt,
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
                    content: &prompt,
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

    Ok((fix_response, usage, prompt))
}

fn build_prompt(spec: &str, extracted: &ExtractedLog, pkg_name: &str, history: &[Turn]) -> String {
    let section = extracted.failed_section.as_deref().unwrap_or("unknown");
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
        "No error was extracted from the build log. Review the spec carefully for \
         common issues: incorrect %autosetup -n directory, wrong %files paths, \
         missing BuildRequires, or architecture mismatch."
            .to_string()
    } else {
        format!(
            "Current error context:\n```\n{}\n```\n\nError lines:\n{}",
            extracted.excerpt,
            extracted.error_lines.join("\n"),
        )
    };

    format!(
        "You are fixing an openSUSE RPM spec file for a Python package.\n\
         Package: {pkg_name}\n\
         Failed section: {section}\n\
         {prior}\n\
         {error_section}\n\
         \n\
         Current spec:\n\
         ```spec\n\
         {spec}\n\
         ```\n\
         \n\
         openSUSE spec conventions to keep in mind:\n\
         \n\
         1. BuildRequires version constraints go INSIDE the macro with spaces around the operator:\n\
            CORRECT:   BuildRequires:  %{{python_module setuptools >= 77}}\n\
            WRONG:     BuildRequires:  %{{python_module setuptools}} >= 77\n\
            WRONG:     BuildRequires:  %{{python_module setuptools>=77}}\n\
         \n\
         2. Package name case normalization: pip/setuptools install using the normalized\n\
            (lowercase, underscores) name, not the PyPI display name. For example,\n\
            MarkupSafe installs as 'markupsafe'. This affects:\n\
            - %autosetup -n: use the lowercase sdist directory name\n\
            - %files: all installed paths use the lowercase name\n\
            - dist-info: 'markupsafe-%{{version}}.dist-info', NOT 'MarkupSafe-%{{version}}.dist-info'\n\
         \n\
         3. Architecture: if the package installs .so files (C extensions), use\n\
            %{{python_sitearch}} and remove 'BuildArch: noarch'. Pure Python packages\n\
            use %{{python_sitelib}} and keep 'BuildArch: noarch'.\n\
         \n\
         4. Use `%pyproject_wheel` / `%pyproject_install` for PEP 517 packages.\n\
         \n\
         Return a JSON object with:\n\
         - \"confidence\": \"high\", \"medium\", or \"low\"\n\
         - \"reasoning\": one sentence explaining the fix\n\
         - \"fixed_spec\": the complete corrected spec file\n\
         \n\
         Return only the JSON object, no other text.",
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

fn gcloud_token() -> Result<String, Error> {
    let out = Command::new("gcloud")
        .args(["auth", "print-access-token"])
        .output()
        .map_err(|e| Error::GcloudAuth(e.to_string()))?;
    if !out.status.success() {
        return Err(Error::GcloudAuth(
            String::from_utf8_lossy(&out.stderr).trim().to_string(),
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}
