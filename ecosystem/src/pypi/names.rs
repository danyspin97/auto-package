use super::*;

/// The `project_urls` keys that name a source repository, in the order
/// `source_url_from_json` prefers them. Other keys (`Homepage`, `Documentation`,
/// `Changelog`, `Issues`, `Download`, ...) do not name the source.
pub const SOURCE_KEYS: &[&str] = &[
    "Source",
    "Source Code",
    "Repository",
    "Code",
    "GitHub",
    "GitLab",
];

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
    let mut candidates: Vec<String> = SOURCE_KEYS
        .iter()
        .filter_map(|k| info.project_urls.get(*k).cloned())
        .collect();
    // Then add all other project_urls values and home_page as fallbacks.
    for (key, val) in &info.project_urls {
        let k = key.as_str();
        if !SOURCE_KEYS.contains(&k) {
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

/// Whether the `/pypi/…/json` body names a source URL, under one of
/// [`SOURCE_KEYS`], on a host that is not a recognised forge: a custom domain,
/// or a self-hosted site that is not a `gitlab.` one. `source_url_from_json`
/// returns `None` for such a project, so the upstream PyPI names cannot be
/// compared with a repository.
///
/// Only the source keys count. A `Homepage`, `Documentation` or `home_page`
/// value on a custom domain is a project's website, not its upstream, and does
/// not make a same-named repository incomparable.
pub fn names_non_forge_url(json: &str) -> bool {
    #[derive(Deserialize)]
    struct Outer {
        info: SrcInfo,
    }
    #[derive(Deserialize)]
    struct SrcInfo {
        #[serde(default, deserialize_with = "deserialize_null_default")]
        project_urls: std::collections::BTreeMap<String, String>,
    }

    let Ok(outer) = serde_json::from_str::<Outer>(json) else {
        return false;
    };
    SOURCE_KEYS
        .iter()
        .filter_map(|k| outer.info.project_urls.get(*k))
        .any(|url| is_non_forge_url(url))
}

/// Whether `url` is an `http(s)` URL, or a forge spelling, whose host is not a
/// recognised forge.
fn is_non_forge_url(url: &str) -> bool {
    let url = url.trim();
    let host = match crate::forge::parse(url) {
        Some(forge) => forge.host,
        None => match url.parse::<ureq::http::Uri>() {
            Ok(uri) if matches!(uri.scheme_str(), Some("http" | "https")) => match uri.host() {
                Some(host) if !host.is_empty() => host.to_string(),
                _ => return false,
            },
            _ => return false,
        },
    };
    !crate::forge::is_known_host(&host)
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

#[cfg(test)]
mod tests {
    use super::*;

    /// A PyPI body with the given `project_urls` object and `home_page` value.
    fn body(project_urls: &str, home_page: &str) -> String {
        format!(
            r#"{{"info": {{"name": "jay", "project_urls": {project_urls}, "home_page": {home_page}}}, "urls": []}}"#
        )
    }

    #[test]
    fn a_custom_domain_source_key_is_a_non_forge_url() {
        let json = body(r#"{"Source": "https://code.example.org/jay"}"#, "null");
        assert!(names_non_forge_url(&json));
        // The same JSON has no source URL that `source_url_from_json` can use.
        assert_eq!(source_url_from_json(&json), None);
    }

    #[test]
    fn every_source_key_counts() {
        for key in SOURCE_KEYS {
            let json = body(
                &format!(r#"{{"{key}": "https://code.example.org/jay"}}"#),
                "null",
            );
            assert!(names_non_forge_url(&json), "{key}");
        }
    }

    #[test]
    fn a_homepage_or_documentation_on_a_custom_domain_is_not_a_source() {
        let homepage = body(r#"{"Homepage": "https://jay.example.org/"}"#, "null");
        assert!(!names_non_forge_url(&homepage));
        let docs = body(
            r#"{"Documentation": "https://docs.example.org/jay"}"#,
            "null",
        );
        assert!(!names_non_forge_url(&docs));
        let legacy = body("{}", r#""https://jay.example.org/""#);
        assert!(!names_non_forge_url(&legacy));
        let others = body(
            r#"{"Changelog": "https://c.example.org", "Issues": "https://i.example.org", "Download": "https://d.example.org"}"#,
            "null",
        );
        assert!(!names_non_forge_url(&others));
    }

    #[test]
    fn a_forge_source_with_a_custom_homepage_is_the_forge_source() {
        let json = body(
            r#"{"Source": "https://github.com/mahkoh/jay", "Homepage": "https://jay.example.org/"}"#,
            r#""https://jay.example.org/""#,
        );
        assert_eq!(
            source_url_from_json(&json).as_deref(),
            Some("https://github.com/mahkoh/jay")
        );
        // Not a non-forge source: the caller compares against the forge URL.
        assert!(!names_non_forge_url(&json));
    }

    #[test]
    fn a_forge_source_key_is_not_a_non_forge_url() {
        let json = body(r#"{"Repository": "https://gitlab.com/x/jay"}"#, "null");
        assert!(!names_non_forge_url(&json));
    }

    #[test]
    fn a_self_hosted_gitlab_is_a_known_forge() {
        let json = body(r#"{"Source": "https://gitlab.example.org/x/jay"}"#, "null");
        assert!(!names_non_forge_url(&json));
    }

    #[test]
    fn no_url_or_a_non_url_value_is_not_a_non_forge_url() {
        assert!(!names_non_forge_url(&body("{}", "null")));
        let json = body(
            r#"{"Source": "mailto:jay@example.org", "Notes": "none"}"#,
            "null",
        );
        assert!(!names_non_forge_url(&json));
    }

    #[test]
    fn a_body_that_is_not_pypi_json_is_not_a_non_forge_url() {
        assert!(!names_non_forge_url("not json"));
        assert!(!names_non_forge_url(r#"{"info": 3}"#));
    }
}
