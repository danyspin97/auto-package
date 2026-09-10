//! AI-assisted spec repair via the `claude` CLI.
//!
//! On a build failure: extract the error context, send the spec and the
//! excerpt to `claude -p`, get a replacement spec back. Repeat until the
//! build passes or the retry limit is reached.
//!
//! Auth is whatever `claude` is already configured to use — no extra keys or
//! environment variables needed.

use std::path::Path;
use std::process::Command;

use serde::Deserialize;

use crate::log::{ExtractedLog, PathMapper};

/// Token counts — not available from the CLI, kept as a zero stub so the
/// caller's cost summary still compiles and prints nothing when unused.
#[derive(Debug, Clone, Default)]
pub struct Usage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_creation_tokens: u64,
    pub cache_read_tokens: u64,
}

impl Usage {
    pub fn cost_cents(&self) -> f64 {
        0.0
    }

    pub fn add(&mut self, other: &Usage) {
        self.input_tokens += other.input_tokens;
        self.output_tokens += other.output_tokens;
        self.cache_creation_tokens += other.cache_creation_tokens;
        self.cache_read_tokens += other.cache_read_tokens;
    }
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("could not run claude: {0}")]
    Launch(std::io::Error),

    #[error("claude exited {code}:\n{stderr}")]
    Failed { code: i32, stderr: String },

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

#[derive(Deserialize)]
struct FixResponse {
    confidence: Confidence,
    reasoning: String,
    fixed_spec: String,
}

/// Try to fix `spec_path` using the build log at `log_path`.
///
/// Returns the fixed spec content. Usage is always zero — the CLI doesn't
/// expose token counts.
pub fn fix(spec_path: &Path, log_path: &Path, pkg_name: &str) -> Result<(String, Usage), Error> {
    let spec = std::fs::read_to_string(spec_path)?;
    let log_raw = std::fs::read_to_string(log_path)?;

    let mapper = PathMapper::with_sdist_root(pkg_name);
    let extracted = crate::log::extract(&log_raw, &mapper);

    let response = call_claude(&spec, &extracted, pkg_name)?;

    eprintln!(
        "  [agent] {:?} — {}",
        response.confidence, response.reasoning,
    );

    Ok((response.fixed_spec, Usage::default()))
}

fn call_claude(spec: &str, extracted: &ExtractedLog, pkg_name: &str) -> Result<FixResponse, Error> {
    let section = extracted.failed_section.as_deref().unwrap_or("unknown");
    let errors = extracted.error_lines.join("\n");

    let prompt = format!(
        "You are fixing an openSUSE RPM spec file for a Python package.\n\
         Package: {pkg_name}\n\
         Failed section: {section}\n\
         \n\
         Error context:\n\
         ```\n\
         {excerpt}\n\
         ```\n\
         \n\
         Error lines:\n\
         {errors}\n\
         \n\
         Current spec:\n\
         ```spec\n\
         {spec}\n\
         ```\n\
         \n\
         Return a JSON object with:\n\
         - \"confidence\": \"high\", \"medium\", or \"low\"\n\
         - \"reasoning\": one sentence explaining the fix\n\
         - \"fixed_spec\": the complete corrected spec file\n\
         \n\
         Return only the JSON object, no other text.",
        excerpt = extracted.excerpt,
    );

    let out = Command::new("claude")
        .args(["--print", "--output-format", "text", "-p", &prompt])
        .output()
        .map_err(Error::Launch)?;

    if !out.status.success() {
        return Err(Error::Failed {
            code: out.status.code().unwrap_or(-1),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        });
    }

    let text = String::from_utf8_lossy(&out.stdout);
    let json = text
        .trim()
        .trim_start_matches("```json")
        .trim_start_matches("```")
        .trim_end_matches("```")
        .trim();

    serde_json::from_str(json).map_err(|e| Error::Parse(format!("bad JSON: {e}\n{json}")))
}
