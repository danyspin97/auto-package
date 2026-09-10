//! AI-assisted spec repair.
//!
//! On a build failure: extract the error context, send the spec and the
//! excerpt to Claude, get a replacement spec back. Repeat until the build
//! passes or the retry limit is reached.
//!
//! Two backends are supported:
//! - Direct Anthropic API: set `AUTO_PACKAGE_API_KEY`.
//! - Google Cloud Vertex AI: set `AUTO_PACKAGE_GCP_PROJECT` (and optionally
//!   `AUTO_PACKAGE_GCP_REGION`, default `us-east5`). Auth is via `gcloud auth
//!   print-access-token`; no Anthropic key needed.
//!
//! If both are set, Vertex takes precedence.

use std::path::Path;
use std::process::Command;

use serde::{Deserialize, Serialize};

use crate::log::{ExtractedLog, PathMapper};

const MAX_RETRIES: usize = 5;
const MODEL: &str = "claude-opus-5";

/// Token counts from one API call.
#[derive(Debug, Clone, Default)]
pub struct Usage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_creation_tokens: u64,
    pub cache_read_tokens: u64,
}

impl Usage {
    /// Cost in US cents (multiply by 0.01 for dollars).
    ///
    /// Direct API rates for claude-opus-5 as of 2026:
    /// $5/MTok input, $25/MTok output.
    /// Vertex AI is billed separately by Google at different rates.
    pub fn cost_cents(&self) -> f64 {
        (self.input_tokens as f64 * 5.0
            + self.output_tokens as f64 * 25.0
            + self.cache_creation_tokens as f64 * 6.25
            + self.cache_read_tokens as f64 * 0.50)
            / 1_000_000.0
            * 100.0
    }

    pub fn add(&mut self, other: &Usage) {
        self.input_tokens += other.input_tokens;
        self.output_tokens += other.output_tokens;
        self.cache_creation_tokens += other.cache_creation_tokens;
        self.cache_read_tokens += other.cache_read_tokens;
    }
}

enum Backend {
    Direct { api_key: String },
    Vertex { project: String, region: String },
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("set AUTO_PACKAGE_API_KEY (direct) or AUTO_PACKAGE_GCP_PROJECT (Vertex)")]
    NoCredentials,

    #[error("gcloud auth print-access-token failed: {0}")]
    GcloudAuth(String),

    #[error("API request failed: {0}")]
    Http(#[from] ureq::Error),

    #[error("unexpected API response: {0}")]
    Parse(String),

    #[error("build still failing after {MAX_RETRIES} attempts")]
    Exhausted,

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
/// Returns the fixed spec content and total API usage for this fix session.
pub fn fix(spec_path: &Path, log_path: &Path, pkg_name: &str) -> Result<(String, Usage), Error> {
    let backend = detect_backend()?;

    let spec = std::fs::read_to_string(spec_path)?;
    let log_raw = std::fs::read_to_string(log_path)?;

    let mapper = PathMapper::with_sdist_root(pkg_name);
    let extracted = crate::log::extract(&log_raw, &mapper);

    let mut total = Usage::default();
    let fixed = fix_loop(&backend, &spec, &extracted, pkg_name, 0, &mut total)?;
    Ok((fixed, total))
}

fn detect_backend() -> Result<Backend, Error> {
    if let Ok(project) = std::env::var("AUTO_PACKAGE_GCP_PROJECT") {
        let region =
            std::env::var("AUTO_PACKAGE_GCP_REGION").unwrap_or_else(|_| "us-east5".to_string());
        return Ok(Backend::Vertex { project, region });
    }
    if let Ok(api_key) = std::env::var("AUTO_PACKAGE_API_KEY") {
        return Ok(Backend::Direct { api_key });
    }
    Err(Error::NoCredentials)
}

fn fix_loop(
    backend: &Backend,
    spec: &str,
    extracted: &ExtractedLog,
    pkg_name: &str,
    attempt: usize,
    total: &mut Usage,
) -> Result<String, Error> {
    if attempt >= MAX_RETRIES {
        return Err(Error::Exhausted);
    }

    let (response, usage) = call_api(backend, spec, extracted, pkg_name)?;
    total.add(&usage);

    eprintln!(
        "  [agent] attempt {}: {:?} — {} ({} in / {} out, ${:.4})",
        attempt + 1,
        response.confidence,
        response.reasoning,
        usage.input_tokens,
        usage.output_tokens,
        usage.cost_cents() / 100.0,
    );

    Ok(response.fixed_spec)
}

fn call_api(
    backend: &Backend,
    spec: &str,
    extracted: &ExtractedLog,
    pkg_name: &str,
) -> Result<(FixResponse, Usage), Error> {
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

    let json_body = match backend {
        Backend::Direct { .. } => {
            #[derive(Serialize)]
            struct DirectRequest<'a> {
                model: &'a str,
                max_tokens: u32,
                messages: Vec<DirectMessage<'a>>,
            }
            #[derive(Serialize)]
            struct DirectMessage<'a> {
                role: &'a str,
                content: &'a str,
            }
            serde_json::to_string(&DirectRequest {
                model: MODEL,
                max_tokens: 8192,
                messages: vec![DirectMessage {
                    role: "user",
                    content: &prompt,
                }],
            })
            .map_err(|e| Error::Parse(e.to_string()))?
        }
        Backend::Vertex { .. } => {
            #[derive(Serialize)]
            struct VertexRequest<'a> {
                anthropic_version: &'a str,
                max_tokens: u32,
                messages: Vec<VertexMessage<'a>>,
            }
            #[derive(Serialize)]
            struct VertexMessage<'a> {
                role: &'a str,
                content: &'a str,
            }
            serde_json::to_string(&VertexRequest {
                anthropic_version: "vertex-2023-10-16",
                max_tokens: 8192,
                messages: vec![VertexMessage {
                    role: "user",
                    content: &prompt,
                }],
            })
            .map_err(|e| Error::Parse(e.to_string()))?
        }
    };

    let response = match backend {
        Backend::Direct { api_key } => ureq::post("https://api.anthropic.com/v1/messages")
            .header("x-api-key", api_key)
            .header("anthropic-version", "2023-06-01")
            .header("content-type", "application/json")
            .send(json_body.as_bytes())
            .map_err(Error::Http)?,

        Backend::Vertex { project, region } => {
            let token = gcloud_token()?;
            let url = format!(
                "https://{region}-aiplatform.googleapis.com/v1/projects/{project}\
                 /locations/{region}/publishers/anthropic/models/{MODEL}:rawPredict"
            );
            ureq::post(&url)
                .header("Authorization", &format!("Bearer {token}"))
                .header("content-type", "application/json")
                .send(json_body.as_bytes())
                .map_err(Error::Http)?
        }
    };

    let mut body = response.into_body();
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

    let json = content_text
        .trim()
        .trim_start_matches("```json")
        .trim_start_matches("```")
        .trim_end_matches("```")
        .trim();

    let fix_response: FixResponse =
        serde_json::from_str(json).map_err(|e| Error::Parse(format!("bad JSON: {e}\n{json}")))?;

    let usage = Usage {
        input_tokens: resp.usage.input_tokens,
        output_tokens: resp.usage.output_tokens,
        cache_creation_tokens: resp.usage.cache_creation_input_tokens,
        cache_read_tokens: resp.usage.cache_read_input_tokens,
    };

    Ok((fix_response, usage))
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
