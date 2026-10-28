use super::*;

/// Extract a source repository URL from a `/pypi/…/json` response body.
///
/// Searches `project_urls` and `home_page` for any URL on a known Git hosting
/// platform, then normalises it via [`crate::forge::parse`]: SSH URLs become
/// HTTPS, nested GitLab groups are preserved in full, and sub-paths like
/// `/tree/main`, `/issues`, or `/-/blob/...` are trimmed off so the result is
/// a clean project URL.
///
/// Returns `None` when no recognisable source URL is found in the metadata.
pub fn source_url_from_json(json: &str) -> Option<String> {
    #[derive(Deserialize)]
    struct Outer {
        info: SrcInfo,
    }
    #[derive(Deserialize)]
    struct SrcInfo {
        #[serde(default, deserialize_with = "deserialize_null_default")]
        project_urls: std::collections::BTreeMap<String, String>,
        #[serde(default)]
        home_page: Option<String>,
    }

    let outer: Outer = serde_json::from_str(json).ok()?;
    let info = outer.info;

    // Prefer URLs whose key hints at source over generic homepage links.
    let source_keys = [
        "Source",
        "Source Code",
        "Repository",
        "Code",
        "GitHub",
        "GitLab",
    ];
    let mut candidates: Vec<String> = source_keys
        .iter()
        .filter_map(|k| info.project_urls.get(*k).cloned())
        .collect();
    // Then add all other project_urls values and home_page as fallbacks.
    for (key, val) in &info.project_urls {
        let k = key.as_str();
        if !source_keys.contains(&k) {
            candidates.push(val.clone());
        }
    }
    if let Some(hp) = info.home_page {
        candidates.push(hp);
    }

    for url in candidates {
        if let Some(forge) = crate::forge::parse(url.trim())
            && crate::forge::is_known_host(&forge.host)
        {
            return Some(forge.canonical_https());
        }
    }
    None
}

/// Convenience alias kept for call sites that specifically want a GitHub URL.
#[inline]
pub fn github_url_from_json(json: &str) -> Option<String> {
    source_url_from_json(json)
}

/// Extract the PyPI project name from a spec's `Source:` URL.
///
/// Matches the `files.pythonhosted.org/packages/source/<letter>/<name>/...`
/// layout openSUSE's generator writes. Returns `None` for non-PyPI hosts and
/// for malformed paths — the update sweep uses that `None` to skip packages
/// it doesn't know how to resolve a latest version for.
pub fn pypi_name_from_source_url(url: &str) -> Option<String> {
    // Trim `%{...}` RPM macros (the usual one is `%{version}` in the filename)
    // so the host/path check doesn't trip over them; nothing before the
    // filename segment should contain a macro in practice.
    let host_marker = "files.pythonhosted.org/packages/source/";
    let idx = url.find(host_marker)?;
    let tail = &url[idx + host_marker.len()..];
    // Expect `<letter>/<name>/<name>-<version>.tar.gz`. We just need `<name>`
    // (the second segment); the first-letter segment is PyPI's bucket shard.
    let mut parts = tail.splitn(3, '/');
    let _letter = parts.next()?;
    let name = parts.next()?;
    if name.is_empty() {
        None
    } else {
        Some(name.to_string())
    }
}

/// Extract the PyPI project name and version from a PEP 427 wheel filename.
///
/// Wheel filenames are `{distribution}-{version}(-{build})?-{py}-{abi}-{plat}.whl`,
/// so splitting on `-` from the left gives the first two components. The
/// `{distribution}` segment is the PyPI name with hyphens normalised to
/// underscores; it is returned verbatim — the caller can pass either spelling
/// to PyPI's JSON API, which treats them interchangeably.
///
/// Returns `None` for a filename that isn't a `.whl` or doesn't have at least
/// the five mandatory hyphen-separated segments.
pub fn pypi_name_and_version_from_wheel_filename(filename: &str) -> Option<(String, String)> {
    let stem = filename.strip_suffix(".whl")?;
    let parts: Vec<&str> = stem.split('-').collect();
    if parts.len() < 5 {
        return None;
    }
    Some((parts[0].to_string(), parts[1].to_string()))
}

/// Extract the PyPI project name from a bare tarball-filename source like
/// `torchaudio-%{version}.tar.gz` — one openSUSE's generator sometimes writes
/// instead of a full PyPI URL for packages fetched via `_service`.
///
/// `spec_version` lets this also handle filenames that embed a literal
/// version (`cuda-core-1.2.0.tar.gz`) rather than the `%{version}` macro,
/// by using the spec's own `Version:` to split the tail from the name — a
/// `<name>-<version>` split from the right is otherwise ambiguous (plenty
/// of PyPI names contain dashes).
///
/// Returns `None` when neither template nor literal-version form matches.
/// The returned name is a *candidate* PyPI project name — the caller still has
/// to query PyPI and handle a 404 (the package genuinely isn't on PyPI).
pub fn pypi_name_from_versioned_tarball(filename: &str, spec_version: &str) -> Option<String> {
    // Accept .tar.gz, .tar.xz, .tar.bz2, .tgz.
    let stem = filename
        .strip_suffix(".tar.gz")
        .or_else(|| filename.strip_suffix(".tar.xz"))
        .or_else(|| filename.strip_suffix(".tar.bz2"))
        .or_else(|| filename.strip_suffix(".tgz"))?;
    // A bare filename only: a URL (forge release asset, archive link) or any
    // other macro would yield a bogus project name and shadow the forge check.
    if stem.contains('/') || stem.replace("%{version}", "").contains('%') {
        return None;
    }
    // Macro form first — the generator's usual output.
    if let Some(name) = stem.strip_suffix("-%{version}")
        && !name.is_empty()
    {
        return Some(name.to_string());
    }
    // Literal-version form, resolved against the spec's own Version:.
    let suffix = format!("-{spec_version}");
    if let Some(name) = stem.strip_suffix(&suffix)
        && !name.is_empty()
    {
        return Some(name.to_string());
    }
    None
}
