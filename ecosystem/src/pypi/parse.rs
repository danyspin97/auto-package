use super::*;

/// Parse a `/pypi/…/json` response body.
pub fn parse_release(json: &str) -> Result<Release, Error> {
    let raw: Envelope = serde_json::from_str(json)?;
    let info = raw.info;
    let sdist = pick_sdist(&raw.urls).ok_or_else(|| Error::NoSdist {
        name: info.name.clone(),
        version: info.version.clone(),
    })?;

    let sha256 = sdist
        .digests
        .sha256
        .clone()
        .ok_or_else(|| Error::NoDigest(sdist.filename.clone()))?;

    Ok(Release {
        homepage: homepage(&info),
        license: license(&info),
        // `yanked` sits on both the release and the individual file; either
        // being set is disqualifying.
        yanked: info.yanked || sdist.yanked,
        sdist: Sdist {
            filename: sdist.filename.clone(),
            url: sdist.url.clone(),
            sha256,
            size: sdist.size,
        },
        name: info.name,
        version: info.version,
        summary: info.summary,
        requires_python: info.requires_python,
        requires_dist: info.requires_dist,
    })
}

/// The best upstream URL, preferring the project's own page over its host.
///
/// `home_page` is null on anything packaged in the last few years, and
/// `project_urls` has no required keys — MarkupSafe publishes `Source` and
/// `Documentation` but no `Homepage`. The last resort is the PyPI page, which
/// is a poor `URL:` tag but better than an empty one.
pub(super) fn homepage(info: &RawInfo) -> Option<String> {
    // An empty or whitespace-only `project_urls` value is "unset" too; if it
    // were returned as `Some("")` it would stop the fallbacks below.
    let from_urls = |key: &str| non_empty(info.project_urls.get(key).cloned());
    non_empty(info.home_page.clone())
        .or_else(|| from_urls("Homepage"))
        .or_else(|| from_urls("Home"))
        .or_else(|| from_urls("Source"))
        .or_else(|| from_urls("Repository"))
        .or_else(|| from_urls("Documentation"))
        .or_else(|| non_empty(info.project_url.clone()))
}

fn license(info: &RawInfo) -> License {
    if let Some(expr) = non_empty(info.license_expression.clone()) {
        return License::Spdx(expr);
    }
    if let Some(text) = non_empty(info.license.clone()) {
        return License::FreeText(text);
    }

    let classifiers: Vec<String> = info
        .classifiers
        .iter()
        .filter(|c| c.starts_with("License ::"))
        .cloned()
        .collect();
    if classifiers.is_empty() {
        License::Unknown
    } else {
        License::Classifiers(classifiers)
    }
}

/// PyPI uses `null` and `""` interchangeably for "unset", and `"UNKNOWN"` for
/// the same thing on records old enough to predate the convention.
fn non_empty(s: Option<String>) -> Option<String> {
    let s = s?;
    let t = s.trim();
    if t.is_empty() || t == "UNKNOWN" {
        None
    } else {
        Some(t.to_string())
    }
}

// --- wire format -----------------------------------------------------------
//
// Deliberately private and deliberately lenient: every optional field really is
// absent somewhere in PyPI's back catalogue, and unknown fields are ignored so
// that a new key upstream cannot break parsing.

/// Treat an explicit JSON `null` the same as an absent field: produce `T::default()`.
///
/// serde's `#[serde(default)]` only activates when a key is missing entirely.
/// PyPI frequently sends `"project_urls": null` or `"classifiers": null`, which
/// a bare `default` attribute cannot handle — the deserialiser still tries to
/// deserialise the `null` token as the concrete type and fails.
pub(super) fn deserialize_null_default<'de, D, T>(d: D) -> Result<T, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Default + Deserialize<'de>,
{
    Ok(Option::<T>::deserialize(d)?.unwrap_or_default())
}

#[derive(Deserialize)]
pub(super) struct Envelope {
    pub(super) info: RawInfo,
    #[serde(default, deserialize_with = "deserialize_null_default")]
    pub(super) urls: Vec<RawFile>,
}

#[derive(Deserialize)]
pub(super) struct RawInfo {
    pub(super) name: String,
    pub(super) version: String,
    #[serde(default)]
    pub(super) summary: Option<String>,
    #[serde(default)]
    pub(super) license: Option<String>,
    #[serde(default)]
    pub(super) license_expression: Option<String>,
    #[serde(default, deserialize_with = "deserialize_null_default")]
    pub(super) classifiers: Vec<String>,
    #[serde(default)]
    pub(super) requires_python: Option<String>,
    #[serde(default, deserialize_with = "deserialize_null_default")]
    pub(super) requires_dist: Vec<String>,
    #[serde(default)]
    pub(super) home_page: Option<String>,
    #[serde(default)]
    pub(super) project_url: Option<String>,
    #[serde(default, deserialize_with = "deserialize_null_default")]
    pub(super) project_urls: std::collections::BTreeMap<String, String>,
    #[serde(default, deserialize_with = "deserialize_null_default")]
    pub(super) yanked: bool,
}

#[derive(Deserialize)]
pub(super) struct RawFile {
    pub(super) filename: String,
    pub(super) url: String,
    pub(super) packagetype: String,
    #[serde(default, deserialize_with = "deserialize_null_default")]
    pub(super) size: u64,
    #[serde(default, deserialize_with = "deserialize_null_default")]
    pub(super) digests: RawDigests,
    #[serde(default, deserialize_with = "deserialize_null_default")]
    pub(super) yanked: bool,
}

#[derive(Deserialize, Default)]
pub(super) struct RawDigests {
    #[serde(default)]
    pub(super) sha256: Option<String>,
}

// Forge hosts and URL parsing live in `crate::forge` — the same code backs
// `source_url_from_json` below, `build::sources::owner_repo`, and
// `build::update::git_repo_url`.

/// Parse a `/pypi/…/json` response into a `Release` using a synthetic sdist
/// entry for the given `tarball_filename`.
///
/// Used for wheel-only packages where we source the tarball from GitHub
/// (or another forge) rather than PyPI. The `sdist` field is populated with
/// placeholder values — callers must have already downloaded the tarball.
pub fn parse_release_with_tarball(json: &str, tarball_filename: &str) -> Result<Release, Error> {
    let raw: Envelope = serde_json::from_str(json)?;
    let info = raw.info;
    // The tarball is identified by filename, so its file entry (if PyPI lists
    // it) carries the same per-file `yanked` flag `parse_release` consults.
    let file_yanked = raw
        .urls
        .iter()
        .any(|f| f.filename == tarball_filename && f.yanked);

    Ok(Release {
        homepage: homepage(&info),
        license: license(&info),
        yanked: info.yanked || file_yanked,
        sdist: Sdist {
            filename: tarball_filename.to_string(),
            url: String::new(),
            sha256: String::new(),
            size: 0,
        },
        name: info.name,
        version: info.version,
        summary: info.summary,
        requires_python: info.requires_python,
        requires_dist: info.requires_dist,
    })
}
