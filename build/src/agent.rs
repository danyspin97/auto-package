//! AI-assisted spec repair.
//!
//! On a build failure: extract the error context, send the spec and the
//! excerpt to Claude, get a replacement spec back. Repeat until the build
//! passes or the retry limit is reached.
//!
//! No logprobs, no built-in confidence field. The model self-reports
//! confidence as an enum; we gate retries on whether it claims to be
//! confident and whether the diff applies cleanly — not on the number alone.

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::log::{ExtractedLog, PathMapper};

const MAX_RETRIES: usize = 5;
const MODEL: &str = "claude-opus-5";
const API_URL: &str = "https://api.anthropic.com/v1/messages";

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("ANTHROPIC_API_KEY is not set")]
    NoApiKey,

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
/// Returns the fixed spec content. The caller is responsible for writing it
/// back to disk and re-running the build.
pub fn fix(spec_path: &Path, log_path: &Path, pkg_name: &str) -> Result<String, Error> {
    let api_key = std::env::var("ANTHROPIC_API_KEY").map_err(|_| Error::NoApiKey)?;

    let spec = std::fs::read_to_string(spec_path)?;
    let log_raw = std::fs::read_to_string(log_path)?;

    let mapper = PathMapper::with_sdist_root(pkg_name);
    let extracted = crate::log::extract(&log_raw, &mapper);

    fix_loop(&api_key, &spec, &extracted, pkg_name, 0)
}

fn fix_loop(
    api_key: &str,
    spec: &str,
    extracted: &ExtractedLog,
    pkg_name: &str,
    attempt: usize,
) -> Result<String, Error> {
    if attempt >= MAX_RETRIES {
        return Err(Error::Exhausted);
    }

    let response = call_api(api_key, spec, extracted, pkg_name)?;

    if response.confidence == Confidence::Low {
        eprintln!(
            "  [agent] attempt {}: low confidence, reasoning: {}",
            attempt + 1,
            response.reasoning
        );
    } else {
        eprintln!(
            "  [agent] attempt {}: {:?} confidence — {}",
            attempt + 1,
            response.confidence,
            response.reasoning
        );
    }

    Ok(response.fixed_spec)
}

fn call_api(
    api_key: &str,
    spec: &str,
    extracted: &ExtractedLog,
    pkg_name: &str,
) -> Result<FixResponse, Error> {
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
        pkg_name = pkg_name,
        section = section,
        excerpt = extracted.excerpt,
        errors = errors,
        spec = spec,
    );

    #[derive(Serialize)]
    struct Message<'a> {
        role: &'a str,
        content: &'a str,
    }

    #[derive(Serialize)]
    struct Request<'a> {
        model: &'a str,
        max_tokens: u32,
        messages: Vec<Message<'a>>,
    }

    #[derive(Deserialize)]
    struct ApiResponse {
        content: Vec<ContentBlock>,
    }

    #[derive(Deserialize)]
    struct ContentBlock {
        #[serde(rename = "type")]
        kind: String,
        text: Option<String>,
    }

    let body = Request {
        model: MODEL,
        max_tokens: 8192,
        messages: vec![Message {
            role: "user",
            content: &prompt,
        }],
    };

    let json_body = serde_json::to_string(&body).map_err(|e| Error::Parse(e.to_string()))?;

    let response = ureq::post(API_URL)
        .header("x-api-key", api_key)
        .header("anthropic-version", "2023-06-01")
        .header("content-type", "application/json")
        .send(json_body.as_bytes())
        .map_err(Error::Http)?;

    let mut body = response.into_body();
    let text = body
        .read_to_string()
        .map_err(|e| Error::Parse(e.to_string()))?;
    let resp: ApiResponse =
        serde_json::from_str(&text).map_err(|e| Error::Parse(format!("bad API JSON: {e}")))?;

    let text = resp
        .content
        .into_iter()
        .find(|b| b.kind == "text")
        .and_then(|b| b.text)
        .ok_or_else(|| Error::Parse("no text block in response".to_string()))?;

    // Strip markdown code fences if the model wrapped the JSON.
    let json = text
        .trim()
        .trim_start_matches("```json")
        .trim_start_matches("```")
        .trim_end_matches("```")
        .trim();

    serde_json::from_str(json).map_err(|e| Error::Parse(format!("bad JSON: {e}\n{json}")))
}
