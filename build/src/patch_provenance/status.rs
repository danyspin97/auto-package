//! The state of an upstream request, read from its forge (plan 3.1 and 3.6),
//! and which release contains its merge commit. Every call is a GET through
//! [`ForgeNet`]; nothing here decides what to do with an answer. The forge's
//! text (title, body, head, diff) is untrusted: only the state, the commit ids
//! (validated) and the title (sanitised, for a reason) are read.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::detect::{is_gitea_host, is_gitlab_host};
use super::reason::{cut, sanitize};
use super::{OriginKind, PatchOrigin};
use crate::bugfix::{ReleaseHas, release_containing, same_project, tag_contains, tag_version};

/// What the tool asks of a forge. A trait, so tests need no network.
pub trait ForgeNet {
    /// The JSON of an https API URL.
    fn api_json(&self, url: &str) -> Result<Value, String>;
    /// The body of a `.patch` URL, size-capped.
    fn patch_text(&self, url: &str) -> Result<String, String>;
    /// The release tags of a repository (its https URL), newest first.
    fn release_tags(&self, repo: &str) -> Result<Vec<String>, String>;
}

/// The state of a request on its forge.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PrState {
    Open,
    Merged,
    Closed,
}

/// The forge's answer, reduced to what is trusted.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PrStatus {
    pub state: PrState,
    /// The merge commit, when the request is merged (or has one).
    pub merge_sha: Option<String>,
    /// The head commit of the request.
    pub head_sha: String,
    /// The title, sanitised and cut: only a fallback for a reason.
    pub title: String,
}

/// Which release contains a commit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Containment {
    /// The release of the packaged version contains it.
    InPackaged { tag: String },
    /// A newer release contains it; `version` is that release's version.
    InNewer { tag: String, version: String },
    /// No release that was asked contains it.
    NotReleased,
    /// It could not be checked; the reason.
    NotChecked(String),
}

/// The API URL of a request's state, for a GitHub, GitLab, Codeberg or
/// Gitea-style forge. A commit has no state call: `None`.
pub fn state_url(origin: &PatchOrigin) -> Option<String> {
    let number = origin.number?;
    let forge = ap_ecosystem::forge::parse(&origin.project)?;
    let (host, path) = (forge.host.as_str(), forge.project_path.as_str());
    match origin.kind {
        OriginKind::Pr if host == "github.com" => Some(format!(
            "https://api.github.com/repos/{path}/pulls/{number}"
        )),
        OriginKind::Pr if is_gitea_host(host) => {
            Some(format!("https://{host}/api/v1/repos/{path}/pulls/{number}"))
        }
        OriginKind::Mr if is_gitlab_host(host) => Some(format!(
            "https://{host}/api/v4/projects/{}/merge_requests/{number}",
            forge.gitlab_encoded_path()
        )),
        _ => None,
    }
}

/// The URL of the `.patch` of a request or a commit: the page with `.patch`.
pub fn diff_url(origin: &PatchOrigin) -> String {
    format!("{}.patch", origin.url)
}

/// Whether `s` is a full commit id (40 or 64 hex digits, as a forge answers).
fn full_sha(s: &str) -> Option<String> {
    (crate::backport::is_commit_id(s) && (s.len() == 40 || s.len() == 64))
        .then(|| s.to_ascii_lowercase())
}

/// The title as a reason: sanitised, leading `[...]` tags dropped, cut at 120.
fn title_of(json: &Value) -> String {
    let mut title = sanitize(
        json.get("title")
            .and_then(Value::as_str)
            .unwrap_or_default(),
    );
    while title.starts_with('[') {
        match title.find(']') {
            Some(i) => title = title[i + 1..].trim_start().to_string(),
            None => break,
        }
    }
    cut(&title, 120)
}

/// The state of a request from its forge's JSON (`state_url`'s answer).
/// Errors name what was wrong; a merged request needs a valid merge commit,
/// and the head must be a valid commit id.
pub fn parse_state(origin: &PatchOrigin, json: &Value) -> Result<PrStatus, String> {
    let (state, merge) = if origin.kind == OriginKind::Mr {
        let state = match json.get("state").and_then(Value::as_str) {
            Some("merged") => PrState::Merged,
            Some("opened" | "locked") => PrState::Open,
            Some("closed") => PrState::Closed,
            other => return Err(format!("unknown state {other:?}")),
        };
        let merge = ["merge_commit_sha", "squash_commit_sha"]
            .iter()
            .find_map(|k| json.get(*k).and_then(Value::as_str).and_then(full_sha));
        (state, merge)
    } else {
        let merged = json.get("merged").and_then(Value::as_bool).unwrap_or(false);
        let state = match json.get("state").and_then(Value::as_str) {
            Some("open") => PrState::Open,
            Some("closed") if merged => PrState::Merged,
            Some("closed") => PrState::Closed,
            other => return Err(format!("unknown state {other:?}")),
        };
        let merge = json
            .get("merge_commit_sha")
            .and_then(Value::as_str)
            .and_then(full_sha);
        (state, merge)
    };
    let head_sha = json
        .pointer("/head/sha")
        .or_else(|| json.get("sha"))
        .and_then(Value::as_str)
        .and_then(full_sha)
        .ok_or("the head is not a commit id")?;
    if state == PrState::Merged && merge.is_none() {
        return Err("the request is merged but its merge commit is not a commit id".into());
    }
    Ok(PrStatus {
        state,
        merge_sha: merge,
        head_sha,
        title: title_of(json),
    })
}

/// Whether the release of `repo` at `tag` contains `sha` through the forge's
/// compare call. Gitea-style forges have none this tool uses.
fn compare_contains(net: &dyn ForgeNet, repo: &str, tag: &str, sha: &str) -> Result<bool, String> {
    if !plain_tag(tag) {
        return Err(format!("the tag {tag:?} is not a plain release name"));
    }
    let forge = ap_ecosystem::forge::parse(repo).ok_or("the project is not a forge URL")?;
    let (host, path) = (forge.host.as_str(), forge.project_path.as_str());
    if host == "github.com" {
        let url = format!("https://api.github.com/repos/{path}/compare/{tag}...{sha}");
        return net.api_json(&url).map(|j| tag_contains(&j));
    }
    if is_gitlab_host(host) {
        let url = format!(
            "https://{host}/api/v4/projects/{}/repository/compare?from={}&to={sha}",
            forge.gitlab_encoded_path(),
            urlencoding::encode(tag)
        );
        let json = net.api_json(&url)?;
        return Ok(json
            .get("commits")
            .and_then(Value::as_array)
            .is_some_and(Vec::is_empty));
    }
    Err("the forge has no compare call this tool uses".to_string())
}

/// Whether a release tag may be put in an API path: letters, digits and `._+-/`,
/// no `..` and no leading `-`.
pub(super) fn plain_tag(tag: &str) -> bool {
    !tag.is_empty()
        && !tag.contains("..")
        && !tag.starts_with('-')
        && tag
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._+-/".contains(&b))
}

/// Which release of `repo` contains the commit `sha`: the packaged version's
/// tag first, then at most four newer tags, oldest first. Only a request of
/// the package's own project is checked (another project's tags say nothing
/// about the packaged version).
pub fn containment_of(
    net: &dyn ForgeNet,
    origin: &PatchOrigin,
    spec_repo: &str,
    version: &str,
    sha: &str,
) -> Containment {
    if !same_project(&origin.project, spec_repo) {
        return Containment::NotChecked(
            "the request is of another project; its tags say nothing about this version".into(),
        );
    }
    let tags = match net.release_tags(&origin.project) {
        Ok(t) => t,
        Err(e) => return Containment::NotChecked(e),
    };
    let mut contains = |tag: &str| compare_contains(net, &origin.project, tag, sha);
    match release_containing(&tags, version, 4, true, &mut contains) {
        Ok(ReleaseHas::Packaged) => Containment::InPackaged {
            tag: tags
                .iter()
                .find(|t| tag_version(t) == version)
                .cloned()
                .unwrap_or_default(),
        },
        Ok(ReleaseHas::Newer(tag)) => Containment::InNewer {
            version: tag_version(&tag).to_string(),
            tag,
        },
        // The search reads at most four newer releases: when there were more, a
        // `None` is not a fact about the newer ones.
        Ok(ReleaseHas::None)
            if tags
                .iter()
                .filter(|t| crate::update::is_newer(tag_version(t), version))
                .count()
                > 4 =>
        {
            Containment::NotChecked(
                "more than four newer releases; the rest are not checked".into(),
            )
        }
        Ok(ReleaseHas::None) => Containment::NotReleased,
        Err(e) => Containment::NotChecked(e),
    }
}
