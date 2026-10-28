use super::*;

/// PEP 503 normalisation: runs of `-`, `_`, `.` become one `-`, lowercased.
pub(super) fn normalize_name(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    let mut sep = false;
    for c in name.chars() {
        if matches!(c, '-' | '_' | '.') {
            sep = true;
            continue;
        }
        if sep {
            out.push('-');
            sep = false;
        }
        out.extend(c.to_lowercase());
    }
    if sep {
        out.push('-');
    }
    out
}

/// Replace everything outside `[a-z0-9.+-]` with `_` so untrusted input can
/// never contain a path separator (and `..` is defused by the hash suffix
/// and the fixed `-`-joined shape: the result is only ever a filename part).
pub(super) fn filename_safe(s: &str) -> String {
    let t: String = s
        .chars()
        .map(|c| match c {
            'a'..='z' | '0'..='9' | '.' | '+' | '-' => c,
            'A'..='Z' => c.to_ascii_lowercase(),
            _ => '_',
        })
        .collect();
    t.replace("..", "._")
}

/// Resolve the runtime closure of `name` at `version`.
///
/// `version` may be `None` for "latest". Returns all nodes including the root
/// package itself; callers can filter by `Node::is_requested` if they only
/// want transitive deps.
///
/// Results are cached in `~/.cache/auto-package/pip-resolve/`. Cached results
/// for pinned versions are valid for 7 days, "latest" queries for 1 day.
/// Turn a failed `pip install --dry-run` into the most specific error.
///
/// pip says "No matching distribution found" both when the project does not
/// exist and when every release was skipped for its `Requires-Python`; only
/// the preceding "Ignored the following versions…" line tells them apart.
pub(super) fn classify_failure(name: &str, code: i32, stderr: String, wheels_only: bool) -> Error {
    if let Some(line) = stderr.lines().find(|l| {
        l.contains("Ignored the following versions that require a different python version")
    }) {
        let example = line
            .split("version: ")
            .nth(1)
            .and_then(|l| l.split("; ").next())
            .unwrap_or("all releases")
            .to_string();
        return Error::IncompatiblePython {
            name: name.to_string(),
            example,
        };
    }
    if stderr.contains("Could not find a version that satisfies the requirement")
        || stderr.contains("No matching distribution found for")
    {
        // Without sdists this also covers releases that ship only an sdist.
        return if wheels_only {
            Error::NoWheel(name.to_string())
        } else {
            Error::NotFound(name.to_string())
        };
    }
    Error::Failed { code, stderr }
}

/// Pick the pip whose report is used: the default pip's, or else the first of
/// `alternates` whose pip works. Returns the alternate used (`None` for the
/// default) with the report.
///
/// The default is passed over when its Python is outside every release's
/// `Requires-Python` ([`Error::IncompatiblePython`]) or its prefix may not be
/// sandboxed ([`SandboxError::UnsafePrefix`]): another interpreter may be fine.
/// Any other default failure is final. Alternates that cannot launch or fail
/// either of those ways are skipped; other alternate failures are surfaced.
/// When nothing works, the default's error is returned.
pub(super) fn select_candidate(
    first: Result<Vec<u8>, Error>,
    alternates: &[String],
    mut attempt: impl FnMut(&str) -> Result<Vec<u8>, Error>,
) -> Result<(Option<String>, Vec<u8>), Error> {
    let e = match first {
        Ok(out) => return Ok((None, out)),
        Err(e) if is_candidate_skip(&e) => e,
        Err(e) => return Err(e),
    };
    let mut last_err: Option<Error> = None;
    for py in alternates {
        match attempt(py) {
            Ok(out) => return Ok((Some(py.clone()), out)),
            // A missing interpreter or another incompatibility says
            // nothing new; anything else is worth surfacing.
            Err(Error::Launch(_) | Error::IncompatiblePython { .. }) => {}
            Err(Error::Sandbox(SandboxError::UnsafePrefix { .. })) => {}
            Err(other) => last_err = Some(other),
        }
    }
    match last_err {
        Some(other) => Err(Error::Failed {
            code: -1,
            stderr: format!("{e}; alternate interpreters also failed: {other}"),
        }),
        None => Err(e),
    }
}

/// Errors that mean "this interpreter is not the one to use", not "the
/// project cannot be resolved".
fn is_candidate_skip(e: &Error) -> bool {
    matches!(
        e,
        Error::IncompatiblePython { .. } | Error::Sandbox(SandboxError::UnsafePrefix { .. })
    )
}

pub fn resolve(name: &str, version: Option<&str>) -> Result<Vec<Node>, Error> {
    // The mode is part of the cache key, so it is decided before the cache
    // is read.
    let mode = resolve_mode();
    let dir = cache_dir();
    // The default pip runs whatever `pip` is on PATH, which may not belong to
    // the host's `python3`: the key names the interpreter it reports.
    let path_var = std::env::var_os("PATH").unwrap_or_default();
    let default_py = default_python();
    let default_prefix = interpreter_prefix_of("pip", &path_var);

    // Try to load from cache first. Only entries recorded under the default
    // pip's interpreter are read: an entry produced by an alternate interpreter
    // is stored under that interpreter's key, so it is never served to the
    // default pip.
    if let Some(cached) = load_cached_resolve(
        &dir,
        name,
        version,
        &default_py,
        default_prefix.as_deref(),
        mode,
    ) {
        return Ok(cached);
    }

    ap_core::diag!("  resolving dependencies with pip (not in cache)...");

    let spec = match version {
        Some(v) => format!("{name}=={v}"),
        None => name.to_string(),
    };

    // The default pip's interpreter may be outside every release's
    // Requires-Python (open-webui needs <3.13 on a 3.13 system), or the pip on
    // PATH may sit where it cannot be sandboxed: the project is fine, so try
    // another installed Python.
    let alternates: Vec<String> = ALTERNATE_PYTHON_MINORS
        .iter()
        .map(|minor| format!("python3.{minor}"))
        .collect();
    let (alternate, stdout) = select_candidate(
        run_pip_report(&["pip"], name, &spec, mode),
        &alternates,
        |py| run_pip_report(&[py, "-m", "pip"], name, &spec, mode),
    )?;
    // The interpreter whose pip produced `stdout`: the default pip's, or the
    // alternate that was used. The cache entry is keyed by it.
    let (produced_by, produced_prefix) = match &alternate {
        Some(py) => (probe_python(py), interpreter_prefix_of(py, &path_var)),
        None => (default_py, default_prefix),
    };
    if let Some(py) = alternate {
        ap_core::diag!("  default pip was not usable for {name}; resolved with {py}");
    }

    let report: Report = serde_json::from_slice(&stdout)?;
    let nodes: Vec<Node> = report
        .install
        .into_iter()
        .map(|e| {
            let sha256 = e
                .download_info
                .archive_info
                .hashes
                .sha256
                .ok_or_else(|| Error::NoDigest(e.metadata.name.clone()))?;
            Ok::<Node, Error>(Node {
                name: e.metadata.name,
                version: e.metadata.version,
                sha256,
                requires_dist: e.metadata.requires_dist,
                requires_python: e.metadata.requires_python,
                is_requested: e.requested,
                is_yanked: e.is_yanked,
            })
        })
        .collect::<Result<Vec<_>, Error>>()?;

    // Save to cache for future use. A result whose interpreter version cannot
    // be probed is not cached: there is no key that would describe it.
    if !produced_by.is_empty() {
        save_cached_resolve(
            &dir,
            name,
            version,
            &produced_by,
            produced_prefix.as_deref(),
            mode,
            &nodes,
        );
    }

    Ok(nodes)
}

// --- wire format -----------------------------------------------------------

#[derive(Deserialize)]
pub(super) struct Report {
    pub(super) install: Vec<RawEntry>,
}

#[derive(Deserialize)]
pub(super) struct RawEntry {
    pub(super) download_info: RawDownloadInfo,
    #[serde(default)]
    pub(super) is_yanked: bool,
    #[serde(default)]
    pub(super) requested: bool,
    pub(super) metadata: RawMetadata,
}

#[derive(Deserialize)]
pub(super) struct RawDownloadInfo {
    pub(super) archive_info: RawArchiveInfo,
}

#[derive(Deserialize)]
pub(super) struct RawArchiveInfo {
    pub(super) hashes: RawHashes,
}

#[derive(Deserialize, Default)]
pub(super) struct RawHashes {
    #[serde(default)]
    pub(super) sha256: Option<String>,
}

#[derive(Deserialize)]
pub(super) struct RawMetadata {
    pub(super) name: String,
    pub(super) version: String,
    #[serde(default)]
    pub(super) requires_dist: Vec<String>,
    #[serde(default)]
    pub(super) requires_python: Option<String>,
}
