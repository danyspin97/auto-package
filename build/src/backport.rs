//! Backports for maintenance runs (`update --maintenance --backport SRC`).
//!
//! A code stream of SLE or Leap normally takes a fix on the version it has, not
//! a version update. The fix is an upstream patch: a file, an https URL of a
//! `.patch`/`.diff` or of a forge commit (whose `.patch` form the forge serves),
//! or a commit id of `--backport-repo`. The run fetches it once ([`prepare`]),
//! puts it into each code stream's branch checkout as `<name>.patch`
//! ([`backport_patches`]) and hands the agent the job of adding it to the spec
//! ([`Job::problem`]); the pipeline's quilt check (`patch_check`) then treats
//! the new `PatchN:` like every other patch of the spec, and refreshes it when
//! it applies with an offset or fuzz. After the build, [`classify`] tells from
//! what the branch holds whether the patch went in as fetched, was refreshed
//! for that code stream's source, or is missing.
//!
//! Everything but [`prepare`], [`fetch`] and [`backport_patches`] is pure.

use std::path::{Path, PathBuf};

use crate::maintenance::is_cve;

/// The largest patch fetched or read: a security fix is a few kilobytes; a
/// body this big is a wrong URL (a release archive, a web page).
pub const MAX_PATCH_BYTES: u64 = 4 * 1024 * 1024;

/// Where a fix comes from, as `--backport` names it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FixSource {
    /// A patch file on this machine.
    File(PathBuf),
    /// The https URL the patch is fetched from (already the `.patch` form).
    Url(String),
    /// A commit of `repo`; fetched from the `.patch` URL its forge serves.
    Commit { repo: String, id: String },
}

impl FixSource {
    /// Where it comes from, for the plan and the request.
    pub fn describe(&self) -> String {
        match self {
            FixSource::File(p) => p.display().to_string(),
            FixSource::Url(u) => u.clone(),
            FixSource::Commit { repo, id } => format!("{repo} commit {id}"),
        }
    }
}

fn is_hex(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| b.is_ascii_hexdigit())
}

/// A git commit id: 7 to 40 hex digits (64 for a sha256 repository).
pub fn is_commit_id(s: &str) -> bool {
    is_hex(s) && ((7..=40).contains(&s.len()) || s.len() == 64)
}

/// The host (lowercase) and the path segments of an https URL, without query
/// and fragment; `None` for anything that is not https.
fn split_https(url: &str) -> Option<(String, Vec<String>)> {
    let rest = url.strip_prefix("https://")?;
    let rest = rest.split(['#', '?']).next().unwrap_or(rest);
    let (host, path) = rest.split_once('/').unwrap_or((rest, ""));
    if host.is_empty() || host.contains('@') {
        return None;
    }
    let segments = path
        .split('/')
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect();
    Some((host.to_lowercase(), segments))
}

fn is_gitlab(host: &str) -> bool {
    host == "gitlab.com" || host.starts_with("gitlab.") || host.contains(".gitlab.")
}

/// The URL a patch is fetched from, for a URL given to `--backport`: a
/// `.patch`/`.diff` URL as it is; a commit on GitHub, GitLab or a Gitea/Forgejo
/// forge (`.../commit/SHA`, `.../-/commit/SHA`) or a GitHub pull request
/// (`.../pull/N`, `.../pull/N/commits/SHA`) with the `.patch` the forge serves
/// for it. Anything else, and anything but https, is refused.
pub fn patch_url(url: &str) -> Result<String, String> {
    let url = url.trim();
    if url.to_lowercase().starts_with("http://") {
        return Err(format!("{url}: only https URLs are fetched"));
    }
    let Some((host, seg)) = split_https(url) else {
        return Err(format!("{url} is not an https URL"));
    };
    let path_only = url.split('#').next().unwrap_or(url);
    let path_no_query = path_only.split('?').next().unwrap_or(path_only);
    if path_no_query.ends_with(".patch") || path_no_query.ends_with(".diff") {
        return Ok(path_only.to_string());
    }
    let base = |n: usize| format!("https://{host}/{}", seg[..n].join("/"));
    // GitLab: GROUP/.../PROJECT/-/commit/SHA, or a merge request.
    if let Some(dash) = seg.iter().position(|s| s == "-")
        && dash >= 2
    {
        match (seg.get(dash + 1).map(String::as_str), seg.get(dash + 2)) {
            (Some("commit"), Some(id)) if is_commit_id(id) => {
                return Ok(format!("{}/{id}.patch", base(dash + 2)));
            }
            (Some("merge_requests"), Some(n)) if n.bytes().all(|b| b.is_ascii_digit()) => {
                return Ok(format!("{}/{n}.patch", base(dash + 2)));
            }
            _ => {}
        }
    }
    if seg.len() >= 4 && !is_gitlab(&host) {
        let (owner, repo) = (&seg[0], &seg[1]);
        match (
            seg[2].as_str(),
            seg.get(3),
            seg.get(4).map(String::as_str),
            seg.get(5),
        ) {
            // GitHub, Gitea, Forgejo, Codeberg: OWNER/REPO/commit/SHA.
            ("commit", Some(id), None, _) if is_commit_id(id) => {
                return Ok(format!("https://{host}/{owner}/{repo}/commit/{id}.patch"));
            }
            ("pull", Some(n), None, _) if n.bytes().all(|b| b.is_ascii_digit()) => {
                return Ok(format!("https://{host}/{owner}/{repo}/pull/{n}.patch"));
            }
            ("pull", Some(_), Some("commits"), Some(id)) if is_commit_id(id) => {
                return Ok(format!("https://{host}/{owner}/{repo}/commit/{id}.patch"));
            }
            _ => {}
        }
    }
    Err(format!(
        "{url} is not a patch: give a .patch or .diff URL, a commit URL of GitHub, GitLab or a \
         Gitea/Forgejo forge, or a GitHub pull request"
    ))
}

/// The `.patch` URL of commit `id` in `repo` (an https clone or web URL).
/// Only forges known to serve that URL are accepted; for another one the
/// patch URL itself has to be given.
pub fn commit_patch_url(repo: &str, id: &str) -> Result<String, String> {
    if !is_commit_id(id) {
        return Err(format!("{id:?} is not a commit id (7 to 40 hex digits)"));
    }
    let Some((host, seg)) = split_https(repo.trim()) else {
        return Err(format!(
            "--backport-repo {repo}: only https repository URLs are used"
        ));
    };
    let mut seg = seg;
    if let Some(last) = seg.last_mut()
        && let Some(stripped) = last.strip_suffix(".git")
    {
        *last = stripped.to_string();
    }
    if seg.len() < 2 {
        return Err(format!(
            "--backport-repo {repo}: name the repository (https://HOST/OWNER/REPO)"
        ));
    }
    let base = format!("https://{host}/{}", seg.join("/"));
    if is_gitlab(&host) {
        return Ok(format!("{base}/-/commit/{id}.patch"));
    }
    let gitea_like = [
        "github.com",
        "codeberg.org",
        "src.opensuse.org",
        "src.suse.de",
    ]
    .contains(&host.as_str())
        || host.starts_with("gitea.")
        || host.starts_with("forgejo.");
    if gitea_like && seg.len() == 2 {
        return Ok(format!("{base}/commit/{id}.patch"));
    }
    Err(format!(
        "--backport-repo {repo}: not a forge known to serve a commit as a patch (GitHub, GitLab, \
         Gitea, Forgejo); pass the commit's .patch URL to --backport instead"
    ))
}

/// What one `--backport` argument names. A bare commit id needs `repo`
/// (`--backport-repo`); `http://` and other schemes are refused.
pub fn parse_fix_source(arg: &str, repo: Option<&str>) -> Result<FixSource, String> {
    let arg = arg.trim();
    if arg.is_empty() {
        return Err("--backport needs a patch file, an https URL or a commit id".to_string());
    }
    let lower = arg.to_lowercase();
    if lower.starts_with("https://") {
        return Ok(FixSource::Url(patch_url(arg)?));
    }
    if lower.contains("://") {
        return Err(format!("--backport {arg}: only https URLs are fetched"));
    }
    if is_commit_id(arg) {
        return match repo {
            Some(r) => {
                commit_patch_url(r, arg)?;
                Ok(FixSource::Commit {
                    repo: r.trim().to_string(),
                    id: arg.to_lowercase(),
                })
            }
            None => Err(format!(
                "--backport {arg} looks like a commit id: name its repository with --backport-repo URL"
            )),
        };
    }
    Ok(FixSource::File(PathBuf::from(arg)))
}

/// The commit id in a forge commit URL (`.../commit/SHA[.patch]`).
pub fn commit_id_in_url(url: &str) -> Option<String> {
    let (_, seg) = split_https(url)?;
    let i = seg.iter().position(|s| s == "commit" || s == "commits")?;
    let id = seg.get(i + 1)?;
    let id = id
        .strip_suffix(".patch")
        .or_else(|| id.strip_suffix(".diff"))
        .unwrap_or(id);
    is_commit_id(id).then(|| id.to_lowercase())
}

/// The commit a `git format-patch` file was made from (`From SHA Mon Sep 17 ...`).
pub fn commit_in_patch(text: &str) -> Option<String> {
    let first = text.lines().next()?;
    let id = first.strip_prefix("From ")?.split_whitespace().next()?;
    (id.len() == 40 && is_hex(id)).then(|| id.to_lowercase())
}

/// The subject of a `git format-patch` file, without the `[PATCH n/m]` tag;
/// a folded header line is joined.
pub fn subject_of_patch(text: &str) -> Option<String> {
    let mut lines = text.lines();
    let mut subject = loop {
        let l = lines.next()?;
        if l.is_empty() {
            return None; // end of the mail header
        }
        if let Some(s) = l.strip_prefix("Subject:") {
            break s.trim().to_string();
        }
    };
    for l in lines {
        if l.starts_with([' ', '\t']) {
            subject.push(' ');
            subject.push_str(l.trim());
        } else {
            break;
        }
    }
    let subject = match subject.strip_prefix('[') {
        Some(rest) if rest.to_uppercase().contains("PATCH") => rest
            .split_once(']')
            .map(|(_, s)| s.trim().to_string())
            .unwrap_or(subject.clone()),
        _ => subject,
    };
    (!subject.is_empty()).then_some(subject)
}

/// Why `bytes` is not a patch: too big, a web page (a login wall or an error
/// page served with 200), or no unified diff hunk in it.
pub fn check_patch(bytes: &[u8]) -> Result<(), String> {
    if bytes.len() as u64 > MAX_PATCH_BYTES {
        return Err(format!(
            "it is larger than {} MiB, which no backport is",
            MAX_PATCH_BYTES / (1024 * 1024)
        ));
    }
    let text = String::from_utf8_lossy(bytes);
    let head = text.trim_start();
    if head.starts_with('<') {
        return Err("it is a web page, not a patch".to_string());
    }
    let has = |marker: &str| text.starts_with(marker) || text.contains(&format!("\n{marker}"));
    if !(has("--- ") && has("+++ ") && has("@@ ")) {
        return Err("it holds no unified diff (no ---/+++/@@ lines)".to_string());
    }
    Ok(())
}

/// Characters a patch file name keeps; others become `-`.
fn sanitize(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '+' | '-') {
                c
            } else {
                '-'
            }
        })
        .collect::<String>()
        .trim_matches(['-', '.'])
        .to_string()
}

/// The file name a fix is stored under in the package: `CVE-2025-1234.patch`
/// for a single fix of a CVE, `fix-<short commit>.patch` without a CVE, and
/// with `total` fixes in the run each name is told apart by its commit
/// (`CVE-2025-1234-abc1234.patch`) or its position (`CVE-2025-1234-2.patch`).
/// Without a CVE and a commit, the first bug reference names it.
pub fn patch_file_name(
    refs: &[String],
    commit: Option<&str>,
    index: usize,
    total: usize,
) -> String {
    let short = commit.map(|c| c.chars().take(7).collect::<String>());
    let cve = refs.iter().find(|r| is_cve(r));
    let base = match (cve, &short) {
        (Some(c), _) => c.clone(),
        (None, Some(s)) if total <= 1 => return format!("fix-{s}.patch"),
        (None, Some(_)) => "fix".to_string(),
        (None, None) => refs
            .first()
            .map(|r| r.replace('#', ""))
            .unwrap_or_else(|| "fix".to_string()),
    };
    let base = sanitize(&base);
    let name = if total <= 1 {
        base
    } else if let Some(s) = short {
        format!("{base}-{s}")
    } else {
        format!("{base}-{}", index + 1)
    };
    format!("{name}.patch")
}

/// One fix, fetched and named, ready to go into every code stream.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreparedPatch {
    /// The file name in the package.
    pub file_name: String,
    pub bytes: Vec<u8>,
    /// Where it came from (the URL, the file, the commit).
    pub origin: String,
    pub commit: Option<String>,
    /// What it fixes, for the spec comment and the changelog: the patch's
    /// subject, or the commit it backports.
    pub reason: String,
}

/// The backports of one run and the references they fix.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Job {
    pub patches: Vec<PreparedPatch>,
    pub refs: Vec<String>,
}

/// The reason a patch is given when it says none of its own.
pub fn reason_for(subject: Option<&str>, commit: Option<&str>) -> String {
    match (subject, commit) {
        (Some(s), _) => s.trim_end_matches('.').to_string(),
        (None, Some(c)) => format!("backport upstream commit {}", &c[..c.len().min(12)]),
        (None, None) => "backport the upstream fix".to_string(),
    }
}

/// Read or download the patch `source` names, refused when it is not one.
/// Network: https only, size-capped, through the SSRF-guarded agent.
pub fn fetch(source: &FixSource) -> Result<Vec<u8>, String> {
    let bytes = match source {
        FixSource::File(path) => {
            let meta = std::fs::metadata(path)
                .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
            if !meta.is_file() {
                return Err(format!("{} is not a file", path.display()));
            }
            if meta.len() > MAX_PATCH_BYTES {
                return Err(format!("{} is too large for a patch", path.display()));
            }
            std::fs::read(path).map_err(|e| format!("cannot read {}: {e}", path.display()))?
        }
        FixSource::Url(url) => download(url)?,
        FixSource::Commit { repo, id } => download(&commit_patch_url(repo, id)?)?,
    };
    check_patch(&bytes).map_err(|e| format!("{}: {e}", source.describe()))?;
    Ok(bytes)
}

fn download(url: &str) -> Result<Vec<u8>, String> {
    if !url.starts_with("https://") {
        return Err(format!("{url}: only https URLs are fetched"));
    }
    let resp = crate::http::get_with_retries(url).map_err(|e| format!("{url}: {e}"))?;
    resp.into_body()
        .with_config()
        .limit(MAX_PATCH_BYTES + 1)
        .read_to_vec()
        .map_err(|e| format!("{url}: {e}"))
}

/// Fetch every `--backport` source and name it. Fails on the first one that
/// cannot be fetched or is not a patch, before anything is branched.
pub fn prepare(sources: &[String], repo: Option<&str>, refs: &[String]) -> Result<Job, String> {
    let parsed: Vec<FixSource> = sources
        .iter()
        .map(|s| parse_fix_source(s, repo))
        .collect::<Result<_, _>>()?;
    let mut patches: Vec<PreparedPatch> = Vec::new();
    for (i, source) in parsed.iter().enumerate() {
        let bytes = fetch(source)?;
        let text = String::from_utf8_lossy(&bytes);
        let commit = match source {
            FixSource::Commit { id, .. } => Some(id.clone()),
            FixSource::Url(u) => commit_id_in_url(u),
            FixSource::File(_) => None,
        }
        .or_else(|| commit_in_patch(&text));
        let file_name = patch_file_name(refs, commit.as_deref(), i, parsed.len());
        if patches.iter().any(|p| p.file_name == file_name) {
            return Err(format!(
                "two backports would both be stored as {file_name}; give each its commit"
            ));
        }
        patches.push(PreparedPatch {
            file_name,
            reason: reason_for(subject_of_patch(&text).as_deref(), commit.as_deref()),
            origin: source.describe(),
            commit,
            bytes,
        });
    }
    Ok(Job {
        patches,
        refs: refs.to_vec(),
    })
}

/// What to do with a backport file in a checkout that may have a file of
/// that name already.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Stage {
    /// Not there: write it.
    Write,
    /// The same bytes are there already.
    Same,
    /// A different file of that name, which the code stream does not have:
    /// an earlier run put it there and refreshed it. It is kept.
    KeepRefreshed,
    /// The code stream itself has a different file of that name: the fix may
    /// be in already, or the name is taken.
    Conflict,
}

pub fn stage_decision(existing: Option<&[u8]>, new: &[u8], in_release: bool) -> Stage {
    match existing {
        None if in_release => Stage::Conflict,
        None => Stage::Write,
        Some(old) if old == new => Stage::Same,
        Some(_) if in_release => Stage::Conflict,
        Some(_) => Stage::KeepRefreshed,
    }
}

/// The pipeline stage that puts the backports into a code stream's checkout,
/// before the agent adds them to the spec. `release_files` are the files the
/// code stream's own package has. Returns a line per patch for the log; a
/// name the code stream already uses for another file fails the stream.
///
/// This is the boundary the quilt patch loop calls: once the files are in the
/// checkout and named by `PatchN:` lines they are checked and refreshed like
/// every other patch of the spec.
pub fn backport_patches(
    checkout_dir: &Path,
    job: &Job,
    release_files: &[String],
) -> Result<Vec<String>, String> {
    let mut notes = Vec::new();
    for p in &job.patches {
        if p.file_name.contains('/') || p.file_name.starts_with('.') {
            return Err(format!("refusing the patch name {:?}", p.file_name));
        }
        let path = checkout_dir.join(&p.file_name);
        let existing = match std::fs::read(&path) {
            Ok(b) => Some(b),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(e) => return Err(format!("cannot read {}: {e}", path.display())),
        };
        let in_release = release_files.iter().any(|f| f == &p.file_name);
        match stage_decision(existing.as_deref(), &p.bytes, in_release) {
            Stage::Write => {
                ap_core::fsx::write_atomic(&path, &p.bytes)
                    .map_err(|e| format!("cannot write {}: {e}", path.display()))?;
                notes.push(format!("added {} ({})", p.file_name, p.origin));
            }
            Stage::Same => notes.push(format!("{} is in the checkout already", p.file_name)),
            Stage::KeepRefreshed => notes.push(format!(
                "kept {}: an earlier run already refreshed it for this code stream",
                p.file_name
            )),
            Stage::Conflict => {
                return Err(format!(
                    "the code stream already has a {} that differs from the backport: the fix may \
                     be in already; check it, or give the fix another reference",
                    p.file_name
                ));
            }
        }
    }
    Ok(notes)
}

/// The spec comment of a backported patch, as SUSE writes them:
/// `# PATCH-FIX-UPSTREAM CVE-2025-1234.patch bsc#123456 CVE-2025-1234 -- reason`.
pub fn spec_comment(p: &PreparedPatch, refs: &[String]) -> String {
    format!(
        "# PATCH-FIX-UPSTREAM {} {} -- {}",
        p.file_name,
        refs.join(" "),
        p.reason
    )
}

impl Job {
    /// The agent's task for `pkg` at `version` in one code stream.
    pub fn problem(&self, pkg: &str, version: &str) -> String {
        let mut text = format!(
            "This is a maintenance backport for a released code stream: {pkg} stays at version \
             {version}. Do not change `Version:` or the source archive. The upstream fix is in \
             the checkout as the patch file(s) below; add each to the spec:\n"
        );
        for p in &self.patches {
            text.push_str(&format!(
                "\n- `{}` (from {}): add a `PatchN:` line with the next free number, preceded by \
                 the comment line\n  `{}`\n  and make sure it is applied: covered by `%autosetup \
                 -p1`/`%autopatch`, or by its own `%patch -P N -p1` line beside the others.",
                p.file_name,
                p.origin,
                spec_comment(p, &self.refs)
            ));
        }
        text.push_str(
            "\n\nThe patch was written for a newer upstream version. Make it apply to THIS \
             version's source with `--fuzz=0`: a hunk that fails (tests and `.result` files \
             included) is REBASED with quilt (`quilt push -f`, fix the rejected hunk by hand, \
             `quilt refresh`), never dropped. Only a hunk whose target file does not exist in this \
             version's source may be left out, and the patch header then says which and why. Keep \
             the patch's header. If the vulnerable code does not exist in this version at all, \
             change nothing and say so, quoting the code you checked (block with the reason): the \
             code stream is then left out of the maintenance request.",
        );
        text
    }
}

/// The changelog bullet of a backported patch:
/// `- Add CVE-2025-1234.patch: reason (bsc#123456, CVE-2025-1234)`.
pub fn changes_bullet(p: &PreparedPatch, refs: &[String]) -> String {
    let refs = if refs.is_empty() {
        String::new()
    } else {
        format!(" ({})", refs.join(", "))
    };
    format!("- Add {}: {}{refs}", p.file_name, p.reason)
}

fn is_separator(l: &str) -> bool {
    let l = l.trim();
    l.len() >= 20 && l.bytes().all(|b| b == b'-')
}

/// `changes` with the bullet of each backport in its newest entry: a bullet
/// that already names the patch file (the agent's own) is replaced, else the
/// bullet goes first. `None` when nothing changes or there is no entry.
pub fn with_backport_bullets(changes: &str, bullets: &[(String, String)]) -> Option<String> {
    let mut lines: Vec<String> = changes.lines().map(str::to_string).collect();
    let first = lines.iter().position(|l| is_separator(l))?;
    let mut changed = false;
    for (file, bullet) in bullets {
        let end = lines[first + 1..]
            .iter()
            .position(|l| is_separator(l))
            .map_or(lines.len(), |i| first + 1 + i);
        let header = (first + 1..end).find(|&i| !lines[i].trim().is_empty())?;
        // Bullets of the entry, each with its continuation lines.
        let mut spans: Vec<(usize, usize)> = Vec::new();
        let mut i = header + 1;
        while i < end {
            if lines[i].starts_with("- ") {
                let start = i;
                i += 1;
                while i < end
                    && lines[i].starts_with(' ')
                    && !lines[i].trim().is_empty()
                    && !lines[i].trim_start().starts_with("- ")
                {
                    i += 1;
                }
                spans.push((start, i));
            } else {
                i += 1;
            }
        }
        let naming = spans
            .iter()
            .find(|(s, e)| lines[*s..*e].iter().any(|l| l.contains(file.as_str())));
        match naming {
            Some(&(s, e)) => {
                let current = lines[s..e].join("\n");
                if current != *bullet {
                    lines.splice(s..e, [bullet.clone()]);
                    changed = true;
                }
            }
            None => {
                let at = spans.first().map_or_else(
                    || {
                        // After the header and its blank line.
                        let mut at = header + 1;
                        if at < end && lines[at].trim().is_empty() {
                            at += 1;
                        }
                        at
                    },
                    |(s, _)| *s,
                );
                lines.insert(at, bullet.clone());
                changed = true;
            }
        }
    }
    if !changed {
        return None;
    }
    let mut text = lines.join("\n");
    if changes.ends_with('\n') {
        text.push('\n');
    }
    Some(text)
}

/// How a backport went into one code stream.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Applied {
    /// The branch has the patch as it was fetched, applied by the spec.
    Clean,
    /// The branch has a changed patch (refreshed or adapted for this code
    /// stream's source), applied by the spec.
    Refreshed,
    /// It is not in the branch, or the spec does not apply it.
    Missing(String),
}

impl std::fmt::Display for Applied {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Applied::Clean => write!(f, "applied cleanly"),
            Applied::Refreshed => write!(f, "applied after a refresh for this code stream"),
            Applied::Missing(why) => write!(f, "not applied: {why}"),
        }
    }
}

/// Whether `spec` applies the patch `name` (a `PatchN:` line that `%autosetup`,
/// `%autopatch` or a `%patch` line applies).
pub fn spec_applies(spec: &str, name: &str) -> bool {
    crate::patch_check::plan_series(spec)
        .iter()
        .any(|p| matches!(p, crate::patch_check::Planned::Check(e) if e.name == name))
}

/// [`Applied`] for a patch from what the branch holds: its file (`None` when
/// absent) and its spec.
pub fn classify(p: &PreparedPatch, committed: Option<&str>, spec: &str) -> Applied {
    let Some(committed) = committed else {
        return Applied::Missing(format!("the branch has no {}", p.file_name));
    };
    if !spec_applies(spec, &p.file_name) {
        return Applied::Missing(format!("the spec does not apply {}", p.file_name));
    }
    let original = String::from_utf8_lossy(&p.bytes);
    if original.trim_end() == committed.trim_end() {
        Applied::Clean
    } else {
        Applied::Refreshed
    }
}

/// What a dry run says it would do with the backports in one code stream.
pub fn dry_run_lines(job: &Job, pkg: &str, version: &str, branch: &str) -> Vec<String> {
    let mut out = vec![format!(
        "backport into {pkg} {version} in {branch} (the version stays {version}):"
    )];
    for p in &job.patches {
        out.push(format!("  {} from {}", p.file_name, p.origin));
        out.push(format!("    spec: {}", spec_comment(p, &job.refs)));
        out.push(format!("    .changes: {}", changes_bullet(p, &job.refs)));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const SHA: &str = "0123456789abcdef0123456789abcdef01234567";

    #[test]
    fn a_forge_commit_url_becomes_the_patch_the_forge_serves() {
        assert_eq!(
            patch_url(&format!("https://github.com/curl/curl/commit/{SHA}")).unwrap(),
            format!("https://github.com/curl/curl/commit/{SHA}.patch")
        );
        assert_eq!(
            patch_url("https://github.com/curl/curl/pull/1234").unwrap(),
            "https://github.com/curl/curl/pull/1234.patch"
        );
        assert_eq!(
            patch_url(&format!("https://github.com/o/r/pull/9/commits/{SHA}#diff")).unwrap(),
            format!("https://github.com/o/r/commit/{SHA}.patch")
        );
        assert_eq!(
            patch_url(&format!(
                "https://gitlab.gnome.org/GNOME/libxml2/-/commit/{SHA}"
            ))
            .unwrap(),
            format!("https://gitlab.gnome.org/GNOME/libxml2/-/commit/{SHA}.patch")
        );
        assert_eq!(
            patch_url("https://gitlab.com/a/b/c/-/merge_requests/7").unwrap(),
            "https://gitlab.com/a/b/c/-/merge_requests/7.patch"
        );
        assert_eq!(
            patch_url("https://codeberg.org/o/r/commit/abc1234").unwrap(),
            "https://codeberg.org/o/r/commit/abc1234.patch"
        );
        // A patch URL is kept, query and all, without its fragment.
        assert_eq!(
            patch_url("https://example.org/fix.diff?raw=1#top").unwrap(),
            "https://example.org/fix.diff?raw=1"
        );
        for bad in [
            "http://github.com/o/r/commit/abc1234.patch",
            "https://github.com/o/r",
            "https://github.com/o/r/commit/not-a-sha",
            "https://example.org/page.html",
            "ftp://x/y.patch",
        ] {
            assert!(patch_url(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn a_commit_id_needs_a_known_forge_repository() {
        assert_eq!(
            commit_patch_url("https://github.com/curl/curl.git", "abc1234").unwrap(),
            "https://github.com/curl/curl/commit/abc1234.patch"
        );
        assert_eq!(
            commit_patch_url("https://gitlab.gnome.org/GNOME/glib/", "abc1234").unwrap(),
            "https://gitlab.gnome.org/GNOME/glib/-/commit/abc1234.patch"
        );
        assert!(commit_patch_url("https://git.kernel.org/pub/scm/x.git", "abc1234").is_err());
        assert!(commit_patch_url("http://github.com/o/r", "abc1234").is_err());
        assert!(commit_patch_url("https://github.com/o/r", "xyz").is_err());
        assert!(commit_patch_url("https://github.com/o", "abc1234").is_err());
    }

    #[test]
    fn backport_arguments_are_a_file_an_https_url_or_a_commit_with_its_repo() {
        assert_eq!(
            parse_fix_source("fix.patch", None).unwrap(),
            FixSource::File("fix.patch".into())
        );
        assert_eq!(
            parse_fix_source(&format!("https://github.com/o/r/commit/{SHA}"), None).unwrap(),
            FixSource::Url(format!("https://github.com/o/r/commit/{SHA}.patch"))
        );
        assert_eq!(
            parse_fix_source("ABC1234", Some("https://github.com/o/r")).unwrap(),
            FixSource::Commit {
                repo: "https://github.com/o/r".into(),
                id: "abc1234".into()
            }
        );
        assert!(
            parse_fix_source("abc1234", None)
                .unwrap_err()
                .contains("--backport-repo")
        );
        assert!(parse_fix_source("http://x/y.patch", None).is_err());
        assert!(parse_fix_source("file:///etc/passwd", None).is_err());
        assert!(parse_fix_source("  ", None).is_err());
    }

    #[test]
    fn patch_names_come_from_the_cve_or_the_commit() {
        let refs = vec!["bsc#1234567".to_string(), "CVE-2025-1234".to_string()];
        assert_eq!(
            patch_file_name(&refs, Some(SHA), 0, 1),
            "CVE-2025-1234.patch"
        );
        assert_eq!(
            patch_file_name(&refs, Some(SHA), 1, 2),
            "CVE-2025-1234-0123456.patch"
        );
        assert_eq!(patch_file_name(&refs, None, 1, 2), "CVE-2025-1234-2.patch");
        let bug = vec!["bsc#1234567".to_string()];
        assert_eq!(patch_file_name(&bug, Some(SHA), 0, 1), "fix-0123456.patch");
        assert_eq!(patch_file_name(&bug, Some(SHA), 0, 2), "fix-0123456.patch");
        assert_eq!(patch_file_name(&bug, None, 0, 1), "bsc1234567.patch");
        assert_eq!(
            patch_file_name(&["jsc#SLE-12".to_string()], None, 0, 1),
            "jscSLE-12.patch"
        );
    }

    const PATCH: &str = "From 0123456789abcdef0123456789abcdef01234567 Mon Sep 17 00:00:00 2001
From: Dev <dev@example.org>
Subject: [PATCH 1/2] http: reject a header line that is too
 long
---
 lib/http.c | 1 +
--- a/lib/http.c
+++ b/lib/http.c
@@ -1,1 +1,2 @@
 a
+b
";

    #[test]
    fn a_format_patch_names_its_commit_and_subject() {
        assert_eq!(commit_in_patch(PATCH).as_deref(), Some(SHA));
        assert_eq!(
            subject_of_patch(PATCH).as_deref(),
            Some("http: reject a header line that is too long")
        );
        assert_eq!(subject_of_patch("--- a\n+++ b\n@@ -1 +1 @@\n"), None);
        assert_eq!(
            commit_id_in_url(&format!("https://github.com/o/r/commit/{SHA}.patch")).as_deref(),
            Some(SHA)
        );
        assert_eq!(commit_id_in_url("https://example.org/fix.patch"), None);
        assert_eq!(
            reason_for(None, Some(SHA)),
            "backport upstream commit 0123456789ab"
        );
        assert_eq!(reason_for(Some("Fix it."), None), "Fix it");
    }

    #[test]
    fn only_a_unified_diff_is_a_patch() {
        assert!(check_patch(PATCH.as_bytes()).is_ok());
        assert!(check_patch(b"<!DOCTYPE html><html>sign in</html>").is_err());
        assert!(check_patch(b"just text").is_err());
        let big = vec![b'a'; MAX_PATCH_BYTES as usize + 1];
        assert!(check_patch(&big).unwrap_err().contains("larger"));
    }

    #[test]
    fn a_staged_file_is_written_kept_or_refused() {
        assert_eq!(stage_decision(None, b"a", false), Stage::Write);
        assert_eq!(stage_decision(Some(b"a"), b"a", true), Stage::Same);
        assert_eq!(
            stage_decision(Some(b"b"), b"a", false),
            Stage::KeepRefreshed
        );
        assert_eq!(stage_decision(Some(b"b"), b"a", true), Stage::Conflict);
        assert_eq!(stage_decision(None, b"a", true), Stage::Conflict);
    }

    fn prepared() -> PreparedPatch {
        PreparedPatch {
            file_name: "CVE-2025-1234.patch".into(),
            bytes: PATCH.as_bytes().to_vec(),
            origin: "https://github.com/o/r/commit/x.patch".into(),
            commit: Some(SHA.into()),
            reason: "reject overlong header lines".into(),
        }
    }

    #[test]
    fn the_spec_comment_the_bullet_and_the_task_name_patch_and_references() {
        let refs = vec!["bsc#123456".to_string(), "CVE-2025-1234".to_string()];
        let p = prepared();
        assert_eq!(
            spec_comment(&p, &refs),
            "# PATCH-FIX-UPSTREAM CVE-2025-1234.patch bsc#123456 CVE-2025-1234 -- reject overlong header lines"
        );
        assert_eq!(
            changes_bullet(&p, &refs),
            "- Add CVE-2025-1234.patch: reject overlong header lines (bsc#123456, CVE-2025-1234)"
        );
        let job = Job {
            patches: vec![p],
            refs,
        };
        let task = job.problem("curl", "8.6.0");
        assert!(task.contains("stays at version 8.6.0"), "{task}");
        assert!(task.contains("Do not change `Version:`"), "{task}");
        assert!(
            task.contains("# PATCH-FIX-UPSTREAM CVE-2025-1234.patch"),
            "{task}"
        );
        assert!(task.contains("%autosetup"), "{task}");
        let dry = dry_run_lines(&job, "curl", "8.6.0", "home:u:branches:X:Update");
        assert!(dry[0].contains("the version stays 8.6.0"), "{dry:?}");
    }

    const CHANGES: &str = "-------------------------------------------------------------------
Mon Oct 26 10:00:00 UTC 2026 - Dev <dev@suse.com>

- Add CVE-2025-1234.patch to fix the header parser
  that crashed
- Add BuildRequires: zlib

-------------------------------------------------------------------
Mon Sep  1 10:00:00 UTC 2026 - Dev <dev@suse.com>

- Update to 8.6.0
";

    #[test]
    fn the_pipeline_writes_the_backport_bullet_in_the_newest_entry() {
        let b = (
            "CVE-2025-1234.patch".to_string(),
            "- Add CVE-2025-1234.patch: reject overlong header lines (bsc#123456, CVE-2025-1234)"
                .to_string(),
        );
        let got = with_backport_bullets(CHANGES, std::slice::from_ref(&b)).unwrap();
        assert!(
            got.contains(&format!(
                "<dev@suse.com>\n\n{}\n- Add BuildRequires: zlib\n",
                b.1
            )),
            "{got}"
        );
        assert!(!got.contains("that crashed"), "{got}");
        assert!(got.ends_with("- Update to 8.6.0\n"));
        assert_eq!(
            with_backport_bullets(&got, &[b.clone()]),
            None,
            "idempotent"
        );
        // An entry that does not name the file gets the bullet first.
        let other = CHANGES.replace("CVE-2025-1234.patch to fix", "the fix of");
        let got = with_backport_bullets(&other, &[b.clone()]).unwrap();
        assert!(
            got.contains(&format!("<dev@suse.com>\n\n{}\n- Add the fix of", b.1)),
            "{got}"
        );
        assert_eq!(with_backport_bullets("no entry", &[b]), None);
    }

    #[test]
    fn a_backport_is_clean_refreshed_or_missing() {
        let p = prepared();
        let spec = "Name: curl\nVersion: 8.6.0\nSource: curl.tar.xz\n\
                    # PATCH-FIX-UPSTREAM CVE-2025-1234.patch bsc#1 -- x\n\
                    Patch1: CVE-2025-1234.patch\n%prep\n%autosetup -p1\n";
        assert_eq!(classify(&p, Some(PATCH), spec), Applied::Clean);
        let refreshed = PATCH.replace("@@ -1,1 +1,2 @@", "@@ -3,1 +3,2 @@");
        assert_eq!(classify(&p, Some(&refreshed), spec), Applied::Refreshed);
        assert!(matches!(classify(&p, None, spec), Applied::Missing(_)));
        let unapplied = "Name: curl\nVersion: 8.6.0\n%prep\n%setup -q\n";
        assert!(matches!(
            classify(&p, Some(PATCH), unapplied),
            Applied::Missing(w) if w.contains("does not apply")
        ));
    }

    #[test]
    fn staging_writes_the_patch_into_the_checkout_and_refuses_a_taken_name() {
        // Under the workspace's target directory, not /tmp.
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../target")
            .join(format!("backport-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let job = Job {
            patches: vec![prepared()],
            refs: vec!["CVE-2025-1234".into()],
        };
        let notes = backport_patches(&dir, &job, &[]).unwrap();
        assert!(
            notes[0].starts_with("added CVE-2025-1234.patch"),
            "{notes:?}"
        );
        assert_eq!(
            std::fs::read(dir.join("CVE-2025-1234.patch")).unwrap(),
            PATCH.as_bytes()
        );
        let again = backport_patches(&dir, &job, &[]).unwrap();
        assert!(again[0].contains("already"), "{again:?}");
        std::fs::write(dir.join("CVE-2025-1234.patch"), "other").unwrap();
        assert!(backport_patches(&dir, &job, &["CVE-2025-1234.patch".into()]).is_err());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
