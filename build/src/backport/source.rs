use super::*;

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

pub(super) fn is_hex(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| b.is_ascii_hexdigit())
}

/// A git commit id: 7 to 40 hex digits (64 for a sha256 repository).
pub fn is_commit_id(s: &str) -> bool {
    is_hex(s) && ((7..=40).contains(&s.len()) || s.len() == 64)
}

/// The host (lowercase) and the path segments of an https URL, without query
/// and fragment; `None` for anything that is not https.
pub(super) fn split_https(url: &str) -> Option<(String, Vec<String>)> {
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

pub(super) fn is_gitlab(host: &str) -> bool {
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
pub(super) fn sanitize(name: &str) -> String {
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
