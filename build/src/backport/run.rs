use super::*;

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

pub(super) fn download(url: &str) -> Result<Vec<u8>, String> {
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

pub(super) fn is_separator(l: &str) -> bool {
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
