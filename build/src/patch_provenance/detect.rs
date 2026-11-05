//! Reading where a carried patch comes from (plan 1.2): the URLs of its header,
//! the comment above its `PatchN:` line, its `PatchN:` value, the `.changes`
//! bullets that name it, and its file name. Only shapes of a request or a
//! commit count; every other URL is not an origin.

use std::sync::OnceLock;

use regex::Regex;

use super::reason::bullets_of;
use super::{OriginEvidence, OriginKind, PatchOrigin};
use crate::bugfix::{upstream_repo_of_spec, urls_in};
use crate::pipeline::{declared_patches, header_field, header_lines, names_in_bullet};

/// Hosts whose URLs are packaging repositories, never an upstream origin.
const PACKAGING_HOSTS: &[&str] = &[
    "src.fedoraproject.org",
    "salsa.debian.org",
    "build.opensuse.org",
];

/// The header fields that may name the upstream request.
const ORIGIN_FIELDS: &[&str] = &[
    "upstream",
    "references",
    "origin",
    "bug",
    "forwarded",
    "link",
];

/// What the origin reading of one patch found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OriginRead {
    /// Nothing names an upstream request or commit.
    None,
    /// One origin.
    Found(PatchOrigin),
    /// Sources name different requests or commits: their URLs. Never a guess.
    Conflict(Vec<String>),
}

/// What [`read_origin`] reads for one patch.
pub struct OriginInputs<'a> {
    /// The package name (for `<pkg>-pr-N.patch` file names).
    pub pkg: &'a str,
    /// The spec text.
    pub spec: &'a str,
    /// The `.changes` text.
    pub changes: &'a str,
    /// The patch's file name as the package has it (`106.patch`).
    pub name: &'a str,
    /// The patch file's text.
    pub text: &'a str,
}

/// The origin of `inputs.name`: every source gives its URLs; each URL of a
/// request or commit shape becomes an origin. One distinct origin is the
/// answer, more than one is a conflict.
pub fn read_origin(inputs: &OriginInputs<'_>) -> OriginRead {
    let mut found: Vec<PatchOrigin> = Vec::new();
    let mut take = |url: &str, evidence: OriginEvidence| {
        if let Some(o) = classify_url(url, evidence)
            && !found.iter().any(|f| f.url == o.url)
        {
            found.push(o);
        }
    };
    for url in header_refs(inputs.text) {
        take(&url, OriginEvidence::PatchHeader);
    }
    let declared = declared_patches(inputs.spec)
        .into_iter()
        .find(|(_, _, name)| name == inputs.name);
    if let Some((index, raw, _)) = &declared {
        for url in spec_comment_refs(inputs.spec, *index) {
            take(&url, OriginEvidence::SpecComment);
        }
        if raw.starts_with("https://") {
            take(raw, OriginEvidence::PatchUrl);
        }
    }
    for url in bullet_refs(inputs.changes, inputs.name) {
        take(&url, OriginEvidence::ChangesBullet);
    }
    if let Some(url) = file_name_url(inputs.pkg, inputs.spec, inputs.name) {
        take(&url, OriginEvidence::FileName);
    }
    // The file name is the weakest source: it only answers when no other
    // source names a request, so a bare `106.patch` never overrules a header.
    let (strong, weak): (Vec<PatchOrigin>, Vec<PatchOrigin>) = found
        .into_iter()
        .partition(|o| o.evidence != OriginEvidence::FileName);
    match (strong.len(), weak.into_iter().next()) {
        (0, None) => OriginRead::None,
        (0, Some(weak)) => OriginRead::Found(weak),
        (1, _) => OriginRead::Found(strong.into_iter().next().expect("one origin")),
        _ => OriginRead::Conflict(strong.into_iter().map(|o| o.url).collect()),
    }
}

/// The origin a URL names, when it has the shape of a request or a commit
/// (plan 1.2); `None` for anything else: an issue, a bug tracker, a packaging
/// repository, a tarball.
pub fn classify_url(url: &str, evidence: OriginEvidence) -> Option<PatchOrigin> {
    let rest = url.trim().strip_prefix("https://")?;
    let rest = rest.split(['#', '?']).next()?;
    let (host, path) = rest.split_once('/')?;
    let host = host.to_ascii_lowercase();
    let arch_packaging = host == "gitlab.archlinux.org" && path.starts_with("archlinux/packaging/");
    if host.is_empty()
        || host.contains('@')
        || PACKAGING_HOSTS.contains(&host.as_str())
        || arch_packaging
    {
        return None;
    }
    let mut seg: Vec<String> = path
        .split('/')
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect();
    if let Some(last) = seg.last_mut() {
        for suffix in [".patch", ".diff"] {
            if let Some(stem) = last.strip_suffix(suffix) {
                *last = stem.to_string();
                break;
            }
        }
    }
    if seg.iter().any(|s| !valid_segment(s)) {
        return None;
    }
    let origin = |kind, project: String, number, commit: Option<String>, url: String| {
        Some(PatchOrigin {
            url,
            kind,
            project,
            number,
            commit,
            evidence,
        })
    };
    if is_gitlab_host(&host) {
        // GROUP/.../PROJECT/-/merge_requests/N and GROUP/.../PROJECT/-/commit/SHA.
        let dash = seg.iter().position(|s| s == "-")?;
        if dash < 2 {
            return None;
        }
        let project = format!("https://{host}/{}", seg[..dash].join("/"));
        return match seg[dash + 1..]
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>()[..]
        {
            ["merge_requests", n] if is_number(n) => {
                let n = number_of(n)?;
                origin(
                    OriginKind::Mr,
                    project.clone(),
                    Some(n),
                    None,
                    format!("{project}/-/merge_requests/{n}"),
                )
            }
            ["commit", id] if crate::backport::is_commit_id(id) => {
                let id = id.to_ascii_lowercase();
                origin(
                    OriginKind::Commit,
                    project.clone(),
                    None,
                    Some(id.clone()),
                    format!("{project}/-/commit/{id}"),
                )
            }
            _ => None,
        };
    }
    let github = host == "github.com";
    if !(github || is_gitea_host(&host)) || seg.len() < 3 {
        return None;
    }
    let project = format!("https://{host}/{}/{}", seg[0], seg[1]);
    let parts: Vec<&str> = seg[2..].iter().map(String::as_str).collect();
    match parts[..] {
        ["commit", id] if crate::backport::is_commit_id(id) => {
            let id = id.to_ascii_lowercase();
            origin(
                OriginKind::Commit,
                project.clone(),
                None,
                Some(id.clone()),
                format!("{project}/commit/{id}"),
            )
        }
        ["pull", n] | ["pull", n, "files" | "commits"] if github && is_number(n) => {
            let n = number_of(n)?;
            origin(
                OriginKind::Pr,
                project.clone(),
                Some(n),
                None,
                format!("{project}/pull/{n}"),
            )
        }
        ["pull", n, "commits", id]
            if github && is_number(n) && crate::backport::is_commit_id(id) =>
        {
            let id = id.to_ascii_lowercase();
            origin(
                OriginKind::Commit,
                project.clone(),
                None,
                Some(id.clone()),
                format!("{project}/commit/{id}"),
            )
        }
        ["pulls", n] if !github && is_number(n) => {
            let n = number_of(n)?;
            origin(
                OriginKind::Pr,
                project.clone(),
                Some(n),
                None,
                format!("{project}/pulls/{n}"),
            )
        }
        _ => None,
    }
}

/// Whether `host` is a GitLab instance (gitlab.com, or `gitlab.<x>`, `<x>.gitlab.<y>`).
fn is_gitlab_host(host: &str) -> bool {
    host == "gitlab.com" || host.starts_with("gitlab.") || host.contains(".gitlab.")
}

/// Whether `host` is a Gitea-style forge: Codeberg, Gitea or Forgejo.
fn is_gitea_host(host: &str) -> bool {
    host == "codeberg.org" || host.starts_with("gitea.") || host.starts_with("forgejo.")
}

/// A path segment a forge allows: letters, digits, `_`, `.` and `-`, and not
/// a dot segment.
fn valid_segment(s: &str) -> bool {
    !s.is_empty()
        && s != "."
        && s != ".."
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_.-".contains(&b))
}

fn is_number(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit())
}

fn number_of(s: &str) -> Option<u64> {
    s.parse().ok().filter(|n| *n > 0)
}

/// The URLs and `gh#owner/repo#N` references in `text`. A `gh#` reference
/// does not say issue or pull request; it is taken as a pull request, and a
/// 404 later makes it no origin.
fn refs_in(text: &str) -> Vec<String> {
    static GH: OnceLock<Regex> = OnceLock::new();
    let gh = GH.get_or_init(|| {
        Regex::new(r"\bgh#([A-Za-z0-9_.-]+)/([A-Za-z0-9_.-]+)#([0-9]+)\b").unwrap()
    });
    let mut out = urls_in(text);
    for c in gh.captures_iter(text) {
        out.push(format!(
            "https://github.com/{}/{}/pull/{}",
            &c[1], &c[2], &c[3]
        ));
    }
    out
}

/// The request URLs of the patch header: the values of the origin fields, and
/// the `Git-repo:` with `Git-commit:` pair.
fn header_refs(text: &str) -> Vec<String> {
    let lines = header_lines(text);
    let mut out = Vec::new();
    for line in &lines {
        if let Some((key, value)) = line.split_once(':')
            && ORIGIN_FIELDS.contains(&key.trim().to_ascii_lowercase().as_str())
        {
            out.extend(refs_in(value));
        }
    }
    let repo = header_field(&lines, "Git-repo");
    let commit = header_field(&lines, "Git-commit");
    if let (Some(repo), Some(commit)) = (repo, commit)
        && crate::backport::is_commit_id(commit)
    {
        let repo = repo.trim_end_matches('/').trim_end_matches(".git");
        out.push(format!("{repo}/commit/{commit}"));
    }
    out
}

/// The URLs in the comment block directly above the `PatchN:` line `index`:
/// the contiguous `#` lines, stopping at a blank or non-comment line.
fn spec_comment_refs(spec: &str, index: usize) -> Vec<String> {
    let lines: Vec<&str> = spec.lines().collect();
    let mut block = Vec::new();
    let mut i = index;
    while i > 0 && lines[i - 1].trim_start().starts_with('#') {
        block.push(lines[i - 1]);
        i -= 1;
    }
    block.iter().flat_map(|l| refs_in(l)).collect()
}

/// The URLs and references of the `.changes` bullets that name `name`.
fn bullet_refs(changes: &str, name: &str) -> Vec<String> {
    bullets_of(changes)
        .iter()
        .filter(|b| {
            let words: Vec<&str> = b.split_whitespace().collect();
            names_in_bullet(&words, name)
        })
        .flat_map(|b| refs_in(b))
        .collect()
}

/// The request a file name alone names (`106.patch`, `pr-106.patch`,
/// `<pkg>-pr-106.patch`), in the project of the spec. A bare number is
/// refused on a GitLab project and `mr` on a GitHub or Gitea one; a leading
/// zero, a number of more than six digits or text after the digits is no
/// request (`0106.patch`, `<pkg>-3.patch`, which numbers the package's own
/// patches).
fn file_name_url(pkg: &str, spec: &str, name: &str) -> Option<String> {
    let repo = upstream_repo_of_spec(spec)?;
    let forge = ap_ecosystem::forge::parse(&repo)?;
    let (host, path) = (forge.host.as_str(), forge.project_path.as_str());
    let (kind, number) = file_name_parts(pkg, name)?;
    let url = match kind.as_deref() {
        Some("mr") if is_gitlab_host(host) => {
            format!("https://{host}/{path}/-/merge_requests/{number}")
        }
        Some("mr") => return None,
        _ if host == "github.com" => format!("https://{host}/{path}/pull/{number}"),
        _ if is_gitea_host(host) => format!("https://{host}/{path}/pulls/{number}"),
        _ => return None,
    };
    Some(url)
}

/// The kind word (`pr`, `pull`, `gh` or `mr`, lowercase; `None` for a bare
/// number) and the number of a file name, when it has one of the shapes.
fn file_name_parts(pkg: &str, name: &str) -> Option<(Option<String>, String)> {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| {
        Regex::new(
            r"(?i)^(?:(?P<k1>pr|pull|gh|mr)[-_]?|(?P<pkg>[A-Za-z0-9_.+-]+)-(?P<k2>pr|pull|gh|mr)-?)?(?P<n>[1-9][0-9]{0,5})\.patch$",
        )
        .unwrap()
    });
    let c = re.captures(name)?;
    if let Some(p) = c.name("pkg")
        && p.as_str() != pkg
    {
        return None;
    }
    let kind = c
        .name("k1")
        .or(c.name("k2"))
        .map(|m| m.as_str().to_ascii_lowercase());
    Some((kind, c.name("n")?.as_str().to_string()))
}
