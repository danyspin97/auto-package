//! Fetches the dependencies of a package, following each `build.zig.zon` down
//! through the dependencies of each dependency, and checks every archive
//! against the hash its manifest declares.
//!
//! Each URL, and each redirect hop, goes through the service host allowlist
//! ([`check_service_url`]). The HTTP client is set not to follow redirects, so
//! each hop is checked before it is requested. A `git+https` URL becomes the
//! forge's archive of its commit, for four fixed forges only.

use std::collections::{BTreeSet, VecDeque};
use std::io::Read;
use std::path::{Path, PathBuf};

use super::hash::{Declared, parse_declared};
use super::tree::{Item, included_items, legacy_hash, package_hash};
use super::unpack::{Format, Limits, unpack};
use super::zon::{self, Dependency, Manifest};
use crate::allowed_hosts::Asker;
use crate::service_sandbox::check_service_url;

/// Redirects followed for one archive.
pub(super) const MAX_REDIRECTS: usize = 5;
/// Dependencies fetched for one package, transitive ones included.
pub(super) const MAX_DEPS: usize = 256;

/// The answer to one request: a redirect to follow, or the archive's body
/// with the `Content-Type` it was served as.
pub(super) enum Response {
    Redirect(String),
    Body {
        content_type: Option<String>,
        body: Box<dyn Read>,
    },
}

/// Where archives come from. The real one is [`HttpFetch`]; tests use a fake.
pub(super) trait Fetch {
    /// Request `url` without following redirects.
    fn get(&self, url: &str) -> Result<Response, String>;
}

/// The network: the shared HTTP agent with a timeout and no redirects.
pub(super) struct HttpFetch;

impl Fetch for HttpFetch {
    fn get(&self, url: &str) -> Result<Response, String> {
        let resp = crate::http::agent(crate::http::DOWNLOAD_TIMEOUT, 0)
            .get(url)
            .call()
            .map_err(|e| format!("fetching {url}: {e}"))?;
        if resp.status().is_redirection() {
            let location = resp
                .headers()
                .get(ureq::http::header::LOCATION)
                .and_then(|v| v.to_str().ok())
                .ok_or_else(|| format!("{url} redirects without a Location"))?;
            return Ok(Response::Redirect(location.to_string()));
        }
        let content_type = resp
            .headers()
            .get(ureq::http::header::CONTENT_TYPE)
            .map(|v| v.to_str().unwrap_or("").to_string());
        let body = resp
            .into_body()
            .into_with_config()
            .limit(crate::http::MAX_DOWNLOAD)
            .reader();
        Ok(Response::Body {
            content_type,
            body: Box::new(body),
        })
    }
}

/// One fetched and verified dependency.
#[derive(Debug)]
pub(super) struct Dep {
    /// The dependency's name in the manifest that first asked for it.
    pub(super) name: String,
    /// The declared hash, `name-version-<44 chars>`. Also the name zig gives
    /// the package in its global cache.
    pub(super) declared: String,
    /// The URL as the manifest wrote it.
    pub(super) url: String,
    /// The archive URL that was requested, after the git mapping.
    pub(super) archive_url: String,
    /// The unpacked tree, the top-level directory stripped.
    pub(super) root: PathBuf,
    /// The dependency's own manifest.
    pub(super) manifest: Manifest,
    /// The files and links its `.paths` keeps, which are what zig hashes.
    pub(super) items: Vec<Item>,
}

/// What one fetch run works with.
pub(super) struct Request<'a> {
    /// The package being vendored, for the allowlist refusal record.
    pub(super) package: &'a str,
    pub(super) fetch: &'a dyn Fetch,
    /// A fresh directory: each archive unpacks into its own subdirectory.
    pub(super) work: &'a Path,
    pub(super) limits: Limits,
}

/// Fetch every URL dependency of `root`, and of each one, checking each hash.
/// Local `.path` dependencies are skipped. The result is sorted by declared
/// hash, one entry per distinct hash.
pub(super) fn fetch_vendor(req: &Request<'_>, root: &Manifest) -> Result<Vec<Dep>, String> {
    let mut queue: VecDeque<Dependency> = root.dependencies.iter().cloned().collect();
    let mut seen: BTreeSet<String> = BTreeSet::new();
    let mut out: Vec<Dep> = Vec::new();
    while let Some(dep) = queue.pop_front() {
        let Some(url) = dep.url.clone() else {
            continue;
        };
        let declared = dep
            .hash
            .clone()
            .ok_or_else(|| format!("zig dep {}: the manifest has no .hash", dep.name))?;
        if !seen.insert(declared.clone()) {
            continue;
        }
        if out.len() >= MAX_DEPS {
            return Err(format!("more than {MAX_DEPS} dependencies"));
        }
        let fetched = fetch_one(req, &dep.name, &url, &declared, seen.len())?;
        queue.extend(fetched.manifest.dependencies.iter().cloned());
        out.push(fetched);
    }
    out.sort_by(|a, b| a.declared.cmp(&b.declared));
    Ok(out)
}

fn fetch_one(
    req: &Request<'_>,
    name: &str,
    url: &str,
    declared: &str,
    index: usize,
) -> Result<Dep, String> {
    let form = parse_declared(declared).map_err(|e| format!("zig dep {name}: {e}"))?;
    let (archive, by_url) = archive_url(url).map_err(|e| format!("zig dep {name}: {e}"))?;
    let root = req.work.join(format!("dep-{index}"));
    let mut current = archive.clone();
    let mut hops = 0;
    let links = loop {
        check_url(req, name, &current)?;
        match req
            .fetch
            .get(&current)
            .map_err(|e| format!("zig dep {name}: {e}"))?
        {
            Response::Body { content_type, body } => {
                let format = Format::resolve(content_type.as_deref(), by_url)
                    .map_err(|e| format!("zig dep {name}: {current}: {e}"))?;
                break unpack(body, format, &root, req.limits)
                    .map_err(|e| format!("zig dep {name}: {e}"))?;
            }
            Response::Redirect(location) => {
                hops += 1;
                if hops > MAX_REDIRECTS {
                    return Err(format!(
                        "zig dep {name}: more than {MAX_REDIRECTS} redirects"
                    ));
                }
                let target = redirect_target(&current, &location)
                    .map_err(|e| format!("zig dep {name}: {e}"))?;
                current = without_fragment(&target).to_string();
            }
        }
    };
    let text = std::fs::read_to_string(root.join("build.zig.zon"))
        .map_err(|_| format!("zig dep {name}: the archive has no build.zig.zon"))?;
    let manifest = zon::parse(&text).map_err(|e| format!("zig dep {name}: {e}"))?;
    let items = included_items(&root, &links, manifest.paths.as_deref())
        .map_err(|e| format!("zig dep {name}: {e}"))?;
    let computed = match form {
        Declared::Current { name_version, .. } => {
            let fingerprint = manifest
                .fingerprint
                .ok_or_else(|| format!("zig dep {name}: the manifest has no .fingerprint"))?;
            // Zig's id is the low 32 bits of the fingerprint.
            let id = (fingerprint & 0xffff_ffff) as u32;
            let digest = package_hash(&items, id).map_err(|e| format!("zig dep {name}: {e}"))?;
            format!("{name_version}-{digest}")
        }
        Declared::Legacy => legacy_hash(&items).map_err(|e| format!("zig dep {name}: {e}"))?,
    };
    if computed != declared {
        return Err(format!(
            "zig dep {name}: archive hash {computed} ≠ build.zig.zon {declared}"
        ));
    }
    Ok(Dep {
        name: name.to_string(),
        declared: declared.to_string(),
        url: url.to_string(),
        archive_url: archive,
        root,
        manifest,
        items,
    })
}

fn check_url(req: &Request<'_>, name: &str, url: &str) -> Result<(), String> {
    check_service_url(url, &Asker::service(req.package, "zig_vendor"))
        .map_err(|e| format!("zig dep {name}: {e}"))
}

/// The archive URL a dependency's `url` field names, and the format its URL
/// names, if any. `https://` archives are used as written, without a fragment,
/// as zig requests them. `git+https://` becomes the forge's archive of the
/// commit after `#`, for codeberg.org, github.com, gitlab.com and git.sr.ht
/// only, and those archives are gzipped tarballs.
pub(super) fn archive_url(url: &str) -> Result<(String, Option<Format>), String> {
    if let Some(rest) = url.strip_prefix("git+https://") {
        return forge_archive(rest, url).map(|a| (a, Some(Format::TarGz)));
    }
    if !url.starts_with("https://") {
        return Err(format!("`{url}` is not an https:// or git+https:// URL"));
    }
    Ok((without_fragment(url).to_string(), Format::of_extension(url)))
}

/// The URL without its fragment, which is never sent.
fn without_fragment(url: &str) -> &str {
    url.split('#').next().unwrap_or(url)
}

/// `rest` is `host/owner/repo[?ref=…]#commit`, the part after `git+https://`.
fn forge_archive(rest: &str, url: &str) -> Result<String, String> {
    let (location, commit) = rest
        .split_once('#')
        .ok_or_else(|| format!("git dependency `{url}` has no #commit"))?;
    if !is_commit(commit) {
        return Err(format!(
            "git dependency `{url}`: #{commit} is not a full commit id"
        ));
    }
    let location = match location.split_once('?') {
        Some((loc, query)) => {
            if !query.starts_with("ref=") || query.contains(['&', '#']) {
                return Err(format!("git dependency `{url}`: only ?ref= is allowed"));
            }
            loc
        }
        None => location,
    };
    let (host, path) = location
        .split_once('/')
        .ok_or_else(|| format!("git dependency `{url}` has no owner and repository"))?;
    let parts: Vec<&str> = path.split('/').collect();
    let [owner, repo] = parts[..] else {
        return Err(format!("git dependency `{url}` must be host/owner/repo"));
    };
    if !is_name(owner) || !is_name(repo) {
        return Err(format!(
            "git dependency `{url}` has a bad owner or repository"
        ));
    }
    match host {
        "codeberg.org" => Ok(format!(
            "https://codeberg.org/{owner}/{repo}/archive/{commit}.tar.gz"
        )),
        "github.com" => Ok(format!(
            "https://codeload.github.com/{owner}/{repo}/tar.gz/{commit}"
        )),
        "gitlab.com" => Ok(format!(
            "https://gitlab.com/{owner}/{repo}/-/archive/{commit}/{repo}-{commit}.tar.gz"
        )),
        "git.sr.ht" if owner.starts_with('~') => Ok(format!(
            "https://git.sr.ht/{owner}/{repo}/archive/{commit}.tar.gz"
        )),
        _ => Err(format!(
            "git dependency `{url}`: only codeberg.org, github.com, gitlab.com and git.sr.ht are mapped to archives"
        )),
    }
}

/// A full commit id: 40 (SHA-1) or 64 (SHA-256) hex digits.
fn is_commit(s: &str) -> bool {
    (s.len() == 40 || s.len() == 64) && s.bytes().all(|c| c.is_ascii_hexdigit())
}

/// An owner or repository name: no path tricks, no odd characters.
fn is_name(s: &str) -> bool {
    !s.is_empty()
        && s != "."
        && s != ".."
        && s.bytes()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'-' | b'_' | b'.' | b'~'))
}

/// The URL a redirect points to. Only an absolute `https://` URL, or a path on
/// the same host, is followed; anything else ends the fetch.
fn redirect_target(base: &str, location: &str) -> Result<String, String> {
    if location.starts_with("https://") {
        return Ok(location.to_string());
    }
    if location.starts_with('/') && !location.starts_with("//") {
        let rest = &base["https://".len()..];
        let host = rest.split('/').next().unwrap_or(rest);
        return Ok(format!("https://{host}{location}"));
    }
    Err(format!(
        "a redirect to `{location}` is not an https:// URL or a path"
    ))
}
