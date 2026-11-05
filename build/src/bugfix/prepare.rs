//! Everything `fix` finds out before anything is branched: the bug text, the
//! upstream fix and the patch that carries it.

use serde_json::Value;

use crate::allowed_hosts::{AllowedHosts, Asker, Use};
use crate::backport::{self, FixSource, PreparedPatch};
use crate::bugfix::*;
use crate::maintenance::is_cve;

/// The bug fix of one package, as the pipeline gets it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FixJob {
    pub pkg: String,
    /// References in SUSE spelling, each once. Only these are cited.
    pub refs: Vec<String>,
    pub description: Option<String>,
    pub bug_texts: Vec<BugText>,
    /// What was read (hosts and files), for the record.
    pub read: Vec<String>,
    /// What was not, with the reason.
    pub not_read: Vec<String>,
    pub warnings: Vec<String>,
    /// The package's upstream repository, canonical https.
    pub upstream_repo: Option<String>,
    /// The upstream fix, named and with its header; empty when the agent
    /// finds or writes it.
    pub patches: Vec<PreparedPatch>,
    /// Several upstream fixes were found and none chosen: nothing is staged.
    pub candidates: Vec<String>,
    /// How the upstream fix was found.
    pub found: Found,
    /// The staged patches nothing shows are merged upstream: a pull or merge
    /// request, or a commit the user did not give and no release contains.
    pub unmerged: Vec<String>,
    pub fixed_in: FixedIn,
    /// Hosts the fix turn may read besides the fixed ones: the upstream
    /// repository's and those of the `--commit-url`s.
    pub fetch_hosts: Vec<String>,
    /// Ties `fix-end.json` to this run.
    pub nonce: String,
}

/// The fix task of a run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FixTask {
    Bug(FixJob),
    /// The findings of a review of a submit request, fixed in its branch.
    Findings(FindingsJob),
}

impl FixTask {
    /// The job of a bug fix; `None` for the findings of a request.
    pub fn bug(&self) -> Option<&FixJob> {
        match self {
            FixTask::Bug(job) => Some(job),
            FixTask::Findings(_) => None,
        }
    }
}

/// How the upstream fix of a job was found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Found {
    /// The user gave it (`--commit-url`).
    Given,
    /// The bug text or the description names it.
    NamedInBug,
    /// The repository's commit search found it.
    Search,
    /// Nothing was found, or several fixes are candidates.
    NotFound,
}

/// The upstream release that contains the fix.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FixedIn {
    Tag(String),
    /// No release newer than the packaged version has it.
    NoneYet,
    NotChecked(String),
}

/// The hosts a fix turn may read besides the fixed ones; none without a fix task.
pub fn fetch_hosts_of(task: Option<&FixTask>) -> &[String] {
    match task {
        Some(FixTask::Bug(job)) => &job.fetch_hosts,
        Some(FixTask::Findings(_)) | None => &[],
    }
}

/// The risk flags of a fix run: the patches the fix added without an upstream
/// commit, and the staged upstream patches whose code was changed.
pub fn fix_risk_flags(
    task: &FixTask,
    before: &crate::risk_flags::Files,
    after: &crate::risk_flags::Files,
) -> Vec<crate::risk_flags::RiskFlag> {
    // The findings of a request add no upstream patch: the generic flags cover them.
    let Some(job) = task.bug() else {
        return Vec::new();
    };
    let staged: Vec<(String, String)> = job
        .patches
        .iter()
        .map(|p| {
            (
                p.file_name.clone(),
                String::from_utf8_lossy(&p.bytes).into_owned(),
            )
        })
        .collect();
    crate::risk_flags::bug_fix_risks(&staged, &job.unmerged, before, after)
}

/// What `fix` was asked, checked.
pub struct FixRequest<'a> {
    pub pkg: &'a str,
    pub input: &'a BugInput,
    pub commit_urls: &'a [String],
    pub commit_repo: Option<&'a str>,
    pub upstream_repo: Option<String>,
    /// The patch names the spec declares, for the name prefix.
    pub declared_patches: &'a [String],
    /// The packaged version.
    pub version: &'a str,
}

/// The network of a fix lookup, so tests need none.
pub trait FixNet {
    fn get_text(&self, url: &str) -> Result<String, GetError>;
    fn fetch_patch(&self, source: &FixSource) -> Result<Vec<u8>, String>;
    /// A `gh api` answer for `path`.
    fn github_json(&self, path: &str) -> Result<Value, String>;
    /// The tags of the upstream releases, newest first.
    fn release_tags(&self, repo: &str) -> Result<Vec<String>, String>;
}

/// The upstream repository the spec names in `Url:` or `Source:`, when it is a
/// known forge.
pub fn upstream_repo_of_spec(spec: &str) -> Option<String> {
    let h = crate::spec::parse_header(spec)?;
    [h.url, h.expanded_source, h.source]
        .into_iter()
        .flatten()
        .find_map(|s| crate::update::git_repo_url(&s))
}

fn new_nonce(pkg: &str) -> String {
    use std::hash::{Hash, Hasher};
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    let mut h = std::collections::hash_map::DefaultHasher::new();
    (pkg, nanos, std::process::id()).hash(&mut h);
    format!("{nanos:x}-{:x}", h.finish())
}

/// The page of a commit or pull request a `.patch` URL is made from.
fn upstream_page(url: &str) -> String {
    let keeps_patch =
        !(url.contains("/commit/") || url.contains("/pull/") || url.contains("/merge_requests/"));
    match url.strip_suffix(".patch") {
        Some(page) if !keeps_patch => page.to_string(),
        _ => url.to_string(),
    }
}

fn is_request(url: &str) -> bool {
    url.contains("/pull/") || url.contains("/merge_requests/")
}

fn host_allowed(hosts: &AllowedHosts, url: &str, pkg: &str) -> Result<(), String> {
    let host = crate::http::host_of(url).ok_or_else(|| format!("{url} has no host"))?;
    let asker = Asker {
        package: pkg.to_string(),
        what: "upstream fix lookup".to_string(),
    };
    hosts.check_host(&host, Use::Source, &asker).map(|_| ())
}

fn fixed_release(repo: &str, sha: &str, version: &str, net: &dyn FixNet) -> FixedIn {
    let Some(forge) = ap_ecosystem::forge::parse(repo).filter(|f| f.host == "github.com") else {
        return FixedIn::NotChecked("only GitHub repositories are compared".to_string());
    };
    let tags = match net.release_tags(repo) {
        Ok(t) => t,
        Err(e) => return FixedIn::NotChecked(e),
    };
    let mut contains = |tag: &str| {
        let path = compare_path(&forge.project_path, tag, sha);
        net.github_json(&path).map(|j| tag_contains(&j))
    };
    match release_containing(&tags, version, 8, false, &mut contains) {
        Ok(ReleaseHas::Newer(tag)) => FixedIn::Tag(tag),
        Ok(_) => FixedIn::NoneYet,
        Err(e) => FixedIn::NotChecked(e),
    }
}

fn read_texts(req: &FixRequest<'_>, hosts: &AllowedHosts, net: &dyn FixNet, job: &mut FixJob) {
    let get = |u: &str| net.get_text(u);
    if let Some(f) = &req.input.report_file {
        job.read.push(format!("file {}", f.path));
        job.bug_texts.push(BugText::from_file(f));
    }
    for r in &job.refs.clone() {
        if is_cve(r) {
            for read in read_cve(req.pkg, r, hosts, &get) {
                match read {
                    CveRead::Read(t) => {
                        job.read.push(t.source.clone());
                        job.bug_texts.push(t);
                    }
                    CveRead::NotRead { host, why } => {
                        job.not_read.push(format!("{host}: {why}"));
                    }
                }
            }
        } else {
            match read_bug(req.pkg, r, hosts, &get) {
                BugRead::Read(t) => {
                    job.read.push(t.source.clone());
                    job.bug_texts.push(t);
                }
                BugRead::NotRead(why) => job.not_read.push(format!("{r}: {why}")),
            }
        }
    }
}

/// The fix sources to fetch: the `--commit-url`s, or the one fix the bug
/// names or the repository's commit search finds. Several are `candidates`.
fn choose_sources(
    req: &FixRequest<'_>,
    hosts: &AllowedHosts,
    net: &dyn FixNet,
    job: &mut FixJob,
) -> Vec<String> {
    if !req.commit_urls.is_empty() {
        job.found = Found::Given;
        return req.commit_urls.to_vec();
    }
    let Some(repo) = req.upstream_repo.as_deref() else {
        return Vec::new();
    };
    let mut found = candidate_fixes(&job.bug_texts, job.description.as_deref(), repo);
    // A URL from a bug text is untrusted: its host must be allowed.
    found.retain(|u| match host_allowed(hosts, u, req.pkg) {
        Ok(()) => true,
        Err(why) => {
            job.not_read.push(format!("{u}: {why}"));
            false
        }
    });
    let github = ap_ecosystem::forge::parse(repo).filter(|f| f.host == "github.com");
    let mut searched = false;
    if found.is_empty()
        && let Some(forge) = github
    {
        searched = true;
        for term in &job.refs {
            match net.github_json(&search_commits_path(&forge.project_path, term)) {
                Ok(j) => {
                    for u in parse_commit_search(&j, repo) {
                        if !found.contains(&u) {
                            found.push(u);
                        }
                    }
                }
                Err(e) => job.not_read.push(format!("commit search for {term}: {e}")),
            }
        }
    }
    if found.len() > 1 {
        job.candidates = found;
        return Vec::new();
    }
    if !found.is_empty() {
        job.found = if searched {
            Found::Search
        } else {
            Found::NamedInBug
        };
    }
    found
}

fn prepare_patches(
    req: &FixRequest<'_>,
    sources: &[String],
    net: &dyn FixNet,
    refs: &[String],
) -> Result<Vec<PreparedPatch>, String> {
    let parsed: Vec<FixSource> = sources
        .iter()
        .map(|s| backport::parse_fix_source(s, req.commit_repo))
        .collect::<Result<_, _>>()?;
    let prefix = patch_prefix(req.pkg, req.declared_patches);
    let mut patches: Vec<PreparedPatch> = Vec::new();
    for (i, source) in parsed.iter().enumerate() {
        let bytes = net.fetch_patch(source)?;
        let text = String::from_utf8_lossy(&bytes).into_owned();
        let patch_url = match source {
            FixSource::Url(u) => Some(u.clone()),
            FixSource::Commit { repo, id } => backport::commit_patch_url(repo, id).ok(),
            FixSource::File(_) => None,
        };
        let commit = match source {
            FixSource::Commit { id, .. } => Some(id.clone()),
            FixSource::Url(u) => backport::commit_id_in_url(u),
            FixSource::File(_) => None,
        }
        .or_else(|| backport::commit_in_patch(&text));
        let reason = backport::reason_for(
            backport::subject_of_patch(&text).as_deref(),
            commit.as_deref(),
        );
        let file_name = fix_patch_name(&prefix, refs, commit.as_deref(), i, parsed.len());
        if patches.iter().any(|p| p.file_name == file_name) {
            return Err(format!(
                "two fixes would both be stored as {file_name}; give each its commit"
            ));
        }
        let facts = HeaderFacts {
            refs: refs.to_vec(),
            upstream: patch_url.as_deref().map(upstream_page),
            status: if patch_url.as_deref().is_some_and(is_request) {
                UpstreamStatus::Submitted
            } else {
                UpstreamStatus::Merged
            },
            reason: reason.clone(),
        };
        patches.push(PreparedPatch {
            file_name,
            bytes: with_fix_header(&bytes, &facts),
            origin: source.describe(),
            commit,
            // The subject is the patch author's: it goes into a spec comment and a
            // changelog bullet, where `%` and a line break are not text.
            reason: spec_safe(&reason),
        });
    }
    Ok(patches)
}

/// Read the bug, find and fetch the upstream fix and name it, before anything
/// is branched. An unreadable bug or a lookup that finds nothing is no error;
/// a `--commit-url` that is not a patch is.
pub fn prepare_fix(
    req: &FixRequest<'_>,
    hosts: &AllowedHosts,
    net: &dyn FixNet,
) -> Result<FixJob, String> {
    let mut job = FixJob {
        pkg: req.pkg.to_string(),
        refs: req.input.refs.clone(),
        description: req.input.description.clone(),
        bug_texts: Vec::new(),
        read: Vec::new(),
        not_read: Vec::new(),
        warnings: req.input.warnings.clone(),
        upstream_repo: req.upstream_repo.clone(),
        patches: Vec::new(),
        candidates: Vec::new(),
        unmerged: Vec::new(),
        found: Found::NotFound,
        fixed_in: FixedIn::NotChecked("no upstream fix found".to_string()),
        fetch_hosts: Vec::new(),
        nonce: new_nonce(req.pkg),
    };
    read_texts(req, hosts, net, &mut job);
    let sources = choose_sources(req, hosts, net, &mut job);
    job.patches = prepare_patches(req, &sources, net, &job.refs)?;
    let mut fetch_hosts: Vec<String> = Vec::new();
    for url in req.upstream_repo.iter().chain(req.commit_urls.iter()) {
        if let Some(h) = crate::http::host_of(url)
            && !fetch_hosts.contains(&h)
        {
            fetch_hosts.push(h);
        }
    }
    job.fetch_hosts = fetch_hosts;
    if let (Some(repo), [first, ..]) = (req.upstream_repo.as_deref(), job.patches.as_slice()) {
        job.fixed_in = match &first.commit {
            Some(sha) => fixed_release(repo, sha, req.version, net),
            None => FixedIn::NotChecked("the fix has no commit id".to_string()),
        };
    } else if !job.patches.is_empty() {
        job.fixed_in = FixedIn::NotChecked("the upstream repository is unknown".to_string());
    }
    // A commit is vouched for by the user (`--commit-url`), or by a release
    // that contains it.
    let vouched = job.found == Found::Given || matches!(job.fixed_in, FixedIn::Tag(_));
    job.unmerged = job
        .patches
        .iter()
        .filter(|p| is_request(&p.origin) || !vouched)
        .map(|p| p.file_name.clone())
        .collect();
    Ok(job)
}
