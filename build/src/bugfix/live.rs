//! The real network of a fix lookup.

use serde_json::Value;

use crate::backport::{self, FixSource};
use crate::bugfix::{FixNet, GetError, http_get_text};

/// [`FixNet`] over the shared HTTP agent and `gh`.
pub struct LiveNet;

impl FixNet for LiveNet {
    fn get_text(&self, url: &str) -> Result<String, GetError> {
        http_get_text(url)
    }

    fn fetch_patch(&self, source: &FixSource) -> Result<Vec<u8>, String> {
        backport::fetch(source)
    }

    fn github_json(&self, path: &str) -> Result<Value, String> {
        crate::sources::get_json_for_search(&format!("https://api.github.com/{path}"))
    }

    fn release_tags(&self, repo: &str) -> Result<Vec<String>, String> {
        Ok(crate::sources::list_releases(repo)?
            .into_iter()
            .map(|r| r.tag)
            .collect())
    }
}
