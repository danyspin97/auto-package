//! A read-only fetch of a public bug from Bugzilla, for the text the agent is
//! told to fix. No login and no API key: a private bug reads as not found to an
//! anonymous client and is reported as not read.

use serde_json::Value;

use crate::allowed_hosts::{AllowedHosts, Asker, Use};
use crate::bugfix::ReportFile;
use crate::patch_check::truncate;

/// bsc#, boo# and bnc# all live on this instance.
pub const BUGZILLA_HOST: &str = "bugzilla.suse.com";

/// The longest single comment kept.
const MAX_COMMENT_BYTES: usize = 4 * 1024;
/// The longest text of one bug kept.
const MAX_BUG_BYTES: usize = 20 * 1024;
/// Comments kept after the report itself.
const LAST_COMMENTS: usize = 5;

/// A bug report: untrusted text written by anyone who can file a bug.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct BugText {
    /// Where it was read from (`bugzilla.suse.com bsc#1234567`, `file x.txt`).
    pub source: String,
    pub summary: String,
    pub status: String,
    pub resolution: String,
    pub product: String,
    pub component: String,
    /// The `url` field and the `see_also` links.
    pub urls: Vec<String>,
    /// The report and the last comments.
    pub comments: Vec<String>,
}

impl BugText {
    /// The text of a `--bug-file`: no network.
    pub fn from_file(file: &ReportFile) -> Self {
        BugText {
            source: format!("file {}", file.path),
            comments: vec![truncate(&file.text, MAX_BUG_BYTES)],
            ..Default::default()
        }
    }

    /// The report as one block of text for the prompt.
    pub fn render(&self) -> String {
        let mut out = format!("Source: {}\n", self.source);
        for (label, v) in [
            ("Summary", &self.summary),
            ("Status", &self.status),
            ("Resolution", &self.resolution),
            ("Product", &self.product),
            ("Component", &self.component),
        ] {
            if !v.is_empty() {
                out.push_str(&format!("{label}: {v}\n"));
            }
        }
        for u in &self.urls {
            out.push_str(&format!("URL: {u}\n"));
        }
        for (i, c) in self.comments.iter().enumerate() {
            out.push_str(&format!("\nComment {}:\n{c}\n", i + 1));
        }
        out
    }
}

/// What reading a bug gave.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BugRead {
    Read(BugText),
    /// Why not: the job goes on without the text.
    NotRead(String),
}

/// Why a GET gave no body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GetError {
    Status(u16),
    Other(String),
}

/// The id of a bsc#, boo# or bnc# reference.
pub fn bug_number(reference: &str) -> Option<u64> {
    let (kind, n) = reference.split_once('#')?;
    matches!(kind, "bsc" | "boo" | "bnc")
        .then(|| n.parse().ok())
        .flatten()
}

/// The two REST paths of a bug: its fields and its comments.
pub fn bug_paths(id: u64) -> [String; 2] {
    [
        format!(
            "https://{BUGZILLA_HOST}/rest/bug/{id}?include_fields=id,summary,status,resolution,product,component,url,see_also,keywords"
        ),
        format!("https://{BUGZILLA_HOST}/rest/bug/{id}/comment"),
    ]
}

/// Why a bug was not parsed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BugError {
    /// A private bug reads as an error or no bug at all.
    NotPublic,
    Bad(String),
}

fn text_of(v: &Value, key: &str) -> String {
    v.get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

/// The text of one bug from the answers of the two REST calls.
pub fn parse_bug(bug_json: &str, comments_json: &str, id: u64) -> Result<BugText, BugError> {
    let bad = |what: &str, e: serde_json::Error| BugError::Bad(format!("{what}: {e}"));
    let bug: Value = serde_json::from_str(bug_json).map_err(|e| bad("the bug", e))?;
    if bug.get("error").and_then(Value::as_bool) == Some(true) {
        return Err(BugError::NotPublic);
    }
    let fields = bug
        .get("bugs")
        .and_then(Value::as_array)
        .and_then(|b| b.first())
        .ok_or(BugError::NotPublic)?;
    let comments: Value =
        serde_json::from_str(comments_json).map_err(|e| bad("the comments", e))?;
    let all: Vec<String> = comments
        .pointer(&format!("/bugs/{id}/comments"))
        .and_then(Value::as_array)
        .map(|list| {
            list.iter()
                .filter(|c| c.get("is_private").and_then(Value::as_bool) != Some(true))
                .filter_map(|c| c.get("text").and_then(Value::as_str))
                .map(|t| truncate(t, MAX_COMMENT_BYTES))
                .collect()
        })
        .unwrap_or_default();
    let skip = all.len().saturating_sub(LAST_COMMENTS + 1);
    let mut kept: Vec<String> = all.iter().take(1).cloned().collect();
    kept.extend(all.iter().skip(1 + skip).cloned());
    let mut urls: Vec<String> = Vec::new();
    let url = text_of(fields, "url");
    if !url.is_empty() {
        urls.push(url);
    }
    for u in fields
        .get("see_also")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
    {
        if !urls.iter().any(|x| x == u) {
            urls.push(u.to_string());
        }
    }
    let mut text = BugText {
        source: String::new(),
        summary: text_of(fields, "summary"),
        status: text_of(fields, "status"),
        resolution: text_of(fields, "resolution"),
        product: text_of(fields, "product"),
        component: text_of(fields, "component"),
        urls,
        comments: kept,
    };
    // The whole text is cut, the newest comments first.
    let mut room = MAX_BUG_BYTES;
    for c in &mut text.comments {
        if c.len() > room {
            *c = truncate(c, room);
        }
        room = room.saturating_sub(c.len());
    }
    Ok(text)
}

/// Read `reference` from Bugzilla through `get`. The host must be allowed: a
/// refusal is recorded (`pkg` asked) and the job goes on without the text.
pub fn read_bug(
    pkg: &str,
    reference: &str,
    hosts: &AllowedHosts,
    get: &dyn Fn(&str) -> Result<String, GetError>,
) -> BugRead {
    let Some(id) = bug_number(reference) else {
        return BugRead::NotRead(format!(
            "{reference} is not read from a tracker: pass --description or --bug-file"
        ));
    };
    let asker = Asker {
        package: pkg.to_string(),
        what: "bug lookup".to_string(),
    };
    if hosts
        .check_host(BUGZILLA_HOST, Use::Source, &asker)
        .is_err()
    {
        return BugRead::NotRead(format!(
            "{BUGZILLA_HOST} is not an allowed host (auto-package hosts allow {BUGZILLA_HOST}, \
             --allow-host {BUGZILLA_HOST}, or pass --bug-file)"
        ));
    }
    let not_public =
        || BugRead::NotRead("the bug is not public: pass --description or --bug-file".to_string());
    let [bug_path, comment_path] = bug_paths(id);
    let fetch = |path: &str| match get(path) {
        Ok(body) => Ok(body),
        Err(GetError::Status(401 | 403)) => Err(not_public()),
        Err(GetError::Status(code)) => Err(BugRead::NotRead(format!(
            "{BUGZILLA_HOST} answered {code} for {reference}"
        ))),
        Err(GetError::Other(e)) => Err(BugRead::NotRead(format!(
            "{BUGZILLA_HOST} could not be read for {reference}: {e}"
        ))),
    };
    let bug = match fetch(&bug_path) {
        Ok(b) => b,
        Err(r) => return r,
    };
    let comments = match fetch(&comment_path) {
        Ok(b) => b,
        Err(r) => return r,
    };
    match parse_bug(&bug, &comments, id) {
        Ok(mut text) => {
            text.source = format!("{BUGZILLA_HOST} {reference}");
            BugRead::Read(text)
        }
        Err(BugError::NotPublic) => not_public(),
        Err(BugError::Bad(e)) => BugRead::NotRead(format!("{reference}: unreadable answer ({e})")),
    }
}

/// GET `url` as text through the shared HTTP agent (timeout, size cap, SSRF guard).
pub fn http_get_text(url: &str) -> Result<String, GetError> {
    let resp = crate::http::get_with_retries(url).map_err(|e| match e {
        ureq::Error::StatusCode(code) => GetError::Status(code),
        e => GetError::Other(e.to_string()),
    })?;
    crate::http::read_text(resp).map_err(|e| GetError::Other(e.to_string()))
}
