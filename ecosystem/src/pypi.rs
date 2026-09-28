//! The PyPI JSON API, and the two things it will not tell us.
//!
//! `GET /pypi/<name>/json` describes the latest release,
//! `GET /pypi/<name>/<version>/json` a pinned one. Both return the same
//! envelope: an `info` object, and a `urls` array of the files uploaded for
//! that release.
//!
//! - **`requires_dist` is frequently `null`.** It is `null` for MarkupSafe
//!   3.0.3, a package that plainly has requirements. Runtime dependencies have
//!   to come from the sdist.
//! - **`[build-system] requires` is not in the response at all.** Whether a
//!   package needs `hatchling`, `flit-core`, `poetry-core` or `setuptools` is
//!   recorded only in the sdist's `pyproject.toml`.
//!
//! So this module's job stops at the sdist: identify the release, and say
//! exactly which tarball to fetch and what it should hash to. Everything else
//! is decided after unpacking.

use std::fs;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

use serde::Deserialize;
use sha2::{Digest, Sha256};

/// A release, reduced to what a spec needs plus the tarball to fetch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Release {
    /// PyPI's display spelling — `MarkupSafe`, not `markupsafe`. Kept verbatim
    /// because `Source` URLs are case-sensitive; the RPM name is derived
    /// separately.
    pub name: String,
    pub version: String,
    pub summary: Option<String>,
    /// Best upstream URL, by the order in [`homepage`].
    pub homepage: Option<String>,
    pub license: License,
    /// PEP 440 specifier such as `>=3.9`, verbatim. Translating it to
    /// `%{?sle15_python_module_pythons}` conditionals is the generator's job.
    pub requires_python: Option<String>,
    /// A yanked release is one upstream withdrew. Packaging one is almost
    /// always wrong, so it is surfaced rather than silently accepted.
    pub yanked: bool,
    pub sdist: Sdist,
}

/// The source tarball, and enough to verify it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sdist {
    pub filename: String,
    pub url: String,
    /// Lowercase hex. Verify after download — this is the only integrity check
    /// available, and it is free while we are already parsing the record.
    pub sha256: String,
    pub size: u64,
}

/// A prebuilt wheel, picked as a last resort when a release has no sdist and
/// no buildable source repository — a closed-source binary redistribution
/// (e.g. NVIDIA's individual CUDA toolkit component wheels). Not something to
/// build from; something to unpack directly into an RPM.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WheelFile {
    pub filename: String,
    pub url: String,
    /// Lowercase hex.
    pub sha256: String,
    pub size: u64,
}

impl WheelFile {
    /// Download to `dest_dir/<filename>` and verify sha256. Same contract as
    /// [`Sdist::fetch`] — the two share nothing but a filename/url/digest, so
    /// this borrows that implementation via a throwaway `Sdist` value.
    pub fn fetch(&self, dest_dir: &Path) -> Result<PathBuf, FetchError> {
        Sdist {
            filename: self.filename.clone(),
            url: self.url.clone(),
            sha256: self.sha256.clone(),
            size: self.size,
        }
        .fetch(dest_dir)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum FetchError {
    #[error("download failed for {url}: {source}")]
    Http {
        url: String,
        #[source]
        source: Box<ureq::Error>,
    },

    #[error("io error on {path}")]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },

    #[error("sha256 mismatch for {filename}: expected {expected}, got {actual}")]
    BadDigest {
        filename: String,
        expected: String,
        actual: String,
    },
}

impl Sdist {
    /// Download to `dest_dir/<filename>` and verify sha256.
    ///
    /// Idempotent: if the file already exists and its hash matches, the
    /// download is skipped. A corrupted file (wrong hash) is overwritten.
    pub fn fetch(&self, dest_dir: &Path) -> Result<PathBuf, FetchError> {
        let dest = dest_dir.join(&self.filename);

        if dest.exists() && sha256_of(&dest)? == self.sha256 {
            return Ok(dest);
        }

        let resp = ureq::get(&self.url).call().map_err(|e| FetchError::Http {
            url: self.url.clone(),
            source: Box::new(e),
        })?;

        let mut hasher = Sha256::new();
        let mut buf = [0u8; 65536];
        let mut file = fs::File::create(&dest).map_err(|e| FetchError::Io {
            path: dest.clone(),
            source: e,
        })?;
        let mut body = resp.into_body().into_reader();

        loop {
            let n = body.read(&mut buf).map_err(|e| FetchError::Io {
                path: dest.clone(),
                source: e,
            })?;
            if n == 0 {
                break;
            }
            hasher.update(&buf[..n]);
            file.write_all(&buf[..n]).map_err(|e| FetchError::Io {
                path: dest.clone(),
                source: e,
            })?;
        }

        let actual = hex::encode(hasher.finalize());
        if actual != self.sha256 {
            let _ = fs::remove_file(&dest);
            return Err(FetchError::BadDigest {
                filename: self.filename.clone(),
                expected: self.sha256.clone(),
                actual,
            });
        }

        Ok(dest)
    }
}

fn sha256_of(path: &Path) -> Result<String, FetchError> {
    let mut file = fs::File::open(path).map_err(|e| FetchError::Io {
        path: path.to_path_buf(),
        source: e,
    })?;
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 65536];
    loop {
        let n = file.read(&mut buf).map_err(|e| FetchError::Io {
            path: path.to_path_buf(),
            source: e,
        })?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hex::encode(hasher.finalize()))
}

/// What PyPI said about licensing, in descending order of trust.
///
/// Only the first variant can go into a spec unexamined. The rest are inputs
/// to normalisation, which is the generator's problem, not this module's.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum License {
    /// PEP 639 `license_expression`: already a valid SPDX expression.
    Spdx(String),
    /// The legacy free-text `license` field. Anything from `BSD-3-Clause` to a
    /// pasted copy of the license itself.
    FreeText(String),
    /// Neither field was set, but there are `License ::` classifiers to map.
    Classifiers(Vec<String>),
    /// Nothing at all. A spec cannot be finished without a human deciding.
    Unknown,
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("malformed PyPI response")]
    Malformed(#[from] serde_json::Error),

    /// Wheel-only uploads. Not a defect to fix and not worth a model call: RPM
    /// packaging builds from source, so the release is simply unusable.
    #[error("{name} {version} has no sdist on PyPI, only wheels")]
    NoSdist { name: String, version: String },

    /// No sdist AND no usable wheel either — the release has nothing to
    /// package at all (an sdist-less, wheel-less upload is legacy/malformed).
    #[error("{name} {version} has no sdist and no usable wheel on PyPI")]
    NoWheel { name: String, version: String },

    /// PyPI has always published sha256; a record without one is a signal that
    /// the response is not what we think it is.
    #[error("no sha256 digest for {0}")]
    NoDigest(String),

}

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
    })
}

/// The source tarball among the release's files.
///
/// A release usually has one sdist and many wheels, but the format is not
/// guaranteed: older uploads are `.zip`, and a few have both. Prefer
/// `.tar.gz`, which is what every openSUSE Python spec expects.
fn pick_sdist(urls: &[RawFile]) -> Option<&RawFile> {
    let sdists = || urls.iter().filter(|f| f.packagetype == "sdist");
    sdists()
        .find(|f| f.filename.ends_with(".tar.gz"))
        .or_else(|| sdists().next())
}

/// Pick a wheel to repackage as a prebuilt binary, for a release that has
/// neither an sdist nor a discoverable source repository. Prefers a
/// universal `py3-none-any` wheel (installable regardless of platform), then
/// a wheel whose platform tag names `arch`, then whatever wheel exists.
pub fn pick_wheel_from_json(json: &str, arch: &str) -> Result<WheelFile, Error> {
    let raw: Envelope = serde_json::from_str(json)?;
    let info = &raw.info;
    let wheel = pick_wheel(&raw.urls, arch).ok_or_else(|| Error::NoWheel {
        name: info.name.clone(),
        version: info.version.clone(),
    })?;
    let sha256 = wheel.digests.sha256.clone().ok_or_else(|| Error::NoDigest(wheel.filename.clone()))?;
    Ok(WheelFile {
        filename: wheel.filename.clone(),
        url: wheel.url.clone(),
        sha256,
        size: wheel.size,
    })
}

fn pick_wheel<'a>(urls: &'a [RawFile], arch: &str) -> Option<&'a RawFile> {
    let wheels = || {
        urls.iter()
            .filter(|f| f.packagetype == "bdist_wheel" && !f.yanked && f.filename.ends_with(".whl"))
    };
    wheels()
        .find(|f| f.filename.ends_with("-none-any.whl"))
        .or_else(|| wheels().find(|f| f.filename.contains(arch)))
        .or_else(|| wheels().next())
}

/// The best upstream URL, preferring the project's own page over its host.
///
/// `home_page` is null on anything packaged in the last few years, and
/// `project_urls` has no required keys — MarkupSafe publishes `Source` and
/// `Documentation` but no `Homepage`. The last resort is the PyPI page, which
/// is a poor `URL:` tag but better than an empty one.
fn homepage(info: &RawInfo) -> Option<String> {
    let from_urls = |key: &str| info.project_urls.get(key).cloned();
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
fn deserialize_null_default<'de, D, T>(d: D) -> Result<T, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Default + Deserialize<'de>,
{
    Ok(Option::<T>::deserialize(d)?.unwrap_or_default())
}

#[derive(Deserialize)]
struct Envelope {
    info: RawInfo,
    #[serde(default, deserialize_with = "deserialize_null_default")]
    urls: Vec<RawFile>,
}

#[derive(Deserialize)]
struct RawInfo {
    name: String,
    version: String,
    #[serde(default)]
    summary: Option<String>,
    #[serde(default)]
    license: Option<String>,
    #[serde(default)]
    license_expression: Option<String>,
    #[serde(default, deserialize_with = "deserialize_null_default")]
    classifiers: Vec<String>,
    #[serde(default)]
    requires_python: Option<String>,
    #[serde(default)]
    home_page: Option<String>,
    #[serde(default)]
    project_url: Option<String>,
    #[serde(default, deserialize_with = "deserialize_null_default")]
    project_urls: std::collections::BTreeMap<String, String>,
    #[serde(default, deserialize_with = "deserialize_null_default")]
    yanked: bool,
}

#[derive(Deserialize)]
struct RawFile {
    filename: String,
    url: String,
    packagetype: String,
    #[serde(default, deserialize_with = "deserialize_null_default")]
    size: u64,
    #[serde(default, deserialize_with = "deserialize_null_default")]
    digests: RawDigests,
    #[serde(default, deserialize_with = "deserialize_null_default")]
    yanked: bool,
}

#[derive(Deserialize, Default)]
struct RawDigests {
    #[serde(default)]
    sha256: Option<String>,
}

/// Known Git hosting platforms and the URL prefix used to normalise them.
static GIT_HOSTS: &[&str] = &[
    "github.com",
    "gitlab.com",
    "bitbucket.org",
    "codeberg.org",
    "sr.ht",
    "salsa.debian.org",
];

/// Parse a `/pypi/…/json` response into a `Release` using a synthetic sdist
/// entry for the given `tarball_filename`.
///
/// Used for wheel-only packages where we source the tarball from GitHub
/// (or another forge) rather than PyPI. The `sdist` field is populated with
/// placeholder values — callers must have already downloaded the tarball.
pub fn parse_release_with_tarball(json: &str, tarball_filename: &str) -> Result<Release, Error> {
    let raw: Envelope = serde_json::from_str(json)?;
    let info = raw.info;

    Ok(Release {
        homepage: homepage(&info),
        license: license(&info),
        yanked: info.yanked,
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
    })
}

/// Extract a source repository URL from a `/pypi/…/json` response body.
///
/// Searches `project_urls` and `home_page` for any URL on a known Git hosting
/// platform, then strips trailing slashes, `.git` suffixes, and sub-paths
/// so the result is a clean `owner/repo`-level URL.
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
    let source_keys = ["Source", "Source Code", "Repository", "Code", "GitHub", "GitLab"];
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
        let trimmed = url.trim();
        for host in GIT_HOSTS {
            if trimmed.contains(host) {
                let base = trimmed.trim_end_matches('/').trim_end_matches(".git");
                // Keep only up to the second path segment after the host
                // (owner/repo), dropping /tree/main, /issues, etc.
                let after_host = base
                    .find(host)
                    .map(|i| &base[i + host.len()..])
                    .and_then(|s| s.strip_prefix('/'))?;
                let parts: Vec<&str> = after_host.splitn(3, '/').collect();
                if parts.len() >= 2 && !parts[0].is_empty() && !parts[1].is_empty() {
                    return Some(format!("https://{host}/{}/{}", parts[0], parts[1]));
                }
            }
        }
    }
    None
}

/// Convenience alias kept for call sites that specifically want a GitHub URL.
#[inline]
pub fn github_url_from_json(json: &str) -> Option<String> {
    source_url_from_json(json)
}

/// Extract package names from a `requires_dist` list, filtering out deps
/// that are not relevant for an openSUSE Linux build.
pub fn parse_dep_names(requires_dist: &[String]) -> Vec<String> {
    requires_dist
        .iter()
        .filter_map(|req| {
            let (spec, marker) = req
                .split_once(';')
                .map(|(s, m)| (s.trim(), Some(m.trim())))
                .unwrap_or((req.as_str(), None));

            if let Some(m) = marker {
                // Skip extras-gated deps (optional).
                if m.contains("extra ==") || m.contains("extra==") {
                    return None;
                }
                // Skip Windows-only deps.
                if (m.contains("sys_platform") && m.contains("win32"))
                    || (m.contains("platform_system") && m.contains("Windows"))
                {
                    return None;
                }
            }

            // Strip version constraints, extras brackets, and parentheses to
            // get the bare package name.
            let name = spec
                .split(|c: char| matches!(c, '>' | '<' | '=' | '!' | '~' | '[' | '(' | ','))
                .next()
                .unwrap_or(spec)
                .trim()
                .to_lowercase();

            if name.is_empty() { None } else { Some(name) }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Trimmed from the real `https://pypi.org/pypi/markupsafe/json`: 89 files
    /// reduced to two, the long description dropped. Every field kept is
    /// verbatim, including the nulls — `license`, `home_page` and
    /// `requires_dist` really are null on a current, well-maintained package.
    const MARKUPSAFE: &str = r#"{
      "info": {
        "author": null,
        "classifiers": ["Development Status :: 5 - Production/Stable",
                        "Programming Language :: Python"],
        "home_page": null,
        "license": null,
        "license_expression": "BSD-3-Clause",
        "license_files": ["LICENSE.txt"],
        "name": "MarkupSafe",
        "project_url": "https://pypi.org/project/MarkupSafe/",
        "project_urls": {
          "Changes": "https://markupsafe.palletsprojects.com/page/changes/",
          "Documentation": "https://markupsafe.palletsprojects.com/",
          "Source": "https://github.com/pallets/markupsafe/"
        },
        "requires_dist": null,
        "requires_python": ">=3.9",
        "summary": "Safely add untrusted strings to HTML/XML markup.",
        "version": "3.0.3",
        "yanked": false
      },
      "urls": [
        {
          "digests": {"md5": "aaa", "sha256": "0123456789abcdef"},
          "filename": "markupsafe-3.0.3-cp314-cp314-manylinux_2_5_x86_64.whl",
          "packagetype": "bdist_wheel",
          "size": 21978,
          "url": "https://files.pythonhosted.org/packages/aa/markupsafe-3.0.3-cp314.whl",
          "yanked": false
        },
        {
          "digests": {"md5": "13a73126d25afa72a1ff0daed072f5fe",
                      "sha256": "722695808f4b6457b320fdc131280796bdceb04ab50fe1795cd540799ebe1698"},
          "filename": "markupsafe-3.0.3.tar.gz",
          "packagetype": "sdist",
          "size": 80313,
          "url": "https://files.pythonhosted.org/packages/7e/99/markupsafe-3.0.3.tar.gz",
          "yanked": false
        }
      ]
    }"#;

    fn markupsafe() -> Release {
        parse_release(MARKUPSAFE).expect("fixture parses")
    }

    #[test]
    fn finds_the_sdist_among_the_wheels() {
        let r = markupsafe();
        assert_eq!(r.sdist.filename, "markupsafe-3.0.3.tar.gz");
        assert_eq!(
            r.sdist.sha256,
            "722695808f4b6457b320fdc131280796bdceb04ab50fe1795cd540799ebe1698"
        );
        assert_eq!(r.sdist.size, 80313);
    }

    #[test]
    fn keeps_pypis_spelling_of_the_name() {
        // `Source:` URLs are case-sensitive, so the display name has to survive
        // intact. Lowercasing belongs to RPM naming, not here.
        assert_eq!(markupsafe().name, "MarkupSafe");
    }

    #[test]
    fn prefers_the_pep_639_expression() {
        assert_eq!(
            markupsafe().license,
            License::Spdx("BSD-3-Clause".to_string())
        );
    }

    #[test]
    fn falls_back_through_the_url_keys() {
        // The real case: `home_page` is null and there is no `Homepage` key,
        // so `Source` wins. A naive `home_page` read yields nothing here.
        assert_eq!(
            markupsafe().homepage.as_deref(),
            Some("https://github.com/pallets/markupsafe/")
        );
    }

    #[test]
    fn a_wheel_only_release_is_an_error_not_an_empty_release() {
        let json = MARKUPSAFE.replace(
            r#""packagetype": "sdist""#,
            r#""packagetype": "bdist_wheel""#,
        );
        let err = parse_release(&json).expect_err("wheel-only must not parse");
        assert!(matches!(err, Error::NoSdist { .. }), "got {err:?}");
    }

    #[test]
    fn picks_the_wheel_matching_the_requested_arch() {
        let wheel = pick_wheel_from_json(MARKUPSAFE, "x86_64").expect("a wheel exists");
        assert_eq!(wheel.filename, "markupsafe-3.0.3-cp314-cp314-manylinux_2_5_x86_64.whl");
        assert_eq!(wheel.sha256, "0123456789abcdef");
    }

    #[test]
    fn falls_back_to_any_wheel_when_none_match_the_arch() {
        // No aarch64 build in the fixture — still returns the one wheel there is
        // rather than failing outright.
        let wheel = pick_wheel_from_json(MARKUPSAFE, "aarch64").expect("falls back to the only wheel");
        assert_eq!(wheel.filename, "markupsafe-3.0.3-cp314-cp314-manylinux_2_5_x86_64.whl");
    }

    #[test]
    fn no_wheel_at_all_is_an_error() {
        let json = MARKUPSAFE.replace(r#""packagetype": "bdist_wheel""#, r#""packagetype": "sdist""#);
        let err = pick_wheel_from_json(&json, "x86_64").expect_err("no wheel in this fixture");
        assert!(matches!(err, Error::NoWheel { .. }), "got {err:?}");
    }

    #[test]
    fn legacy_free_text_license_is_kept_but_marked_unnormalised() {
        let json = MARKUPSAFE
            .replace(
                r#""license_expression": "BSD-3-Clause""#,
                r#""license_expression": null"#,
            )
            .replace(r#""license": null"#, r#""license": "BSD License""#);
        assert_eq!(
            parse_release(&json).unwrap().license,
            License::FreeText("BSD License".to_string())
        );
    }

    #[test]
    fn classifiers_are_the_last_licence_resort() {
        let json = MARKUPSAFE
            .replace(
                r#""license_expression": "BSD-3-Clause""#,
                r#""license_expression": null"#,
            )
            .replace(
                r#""Programming Language :: Python""#,
                r#""License :: OSI Approved :: BSD License""#,
            );
        assert_eq!(
            parse_release(&json).unwrap().license,
            License::Classifiers(vec!["License :: OSI Approved :: BSD License".to_string()])
        );
    }

    #[test]
    fn no_licence_information_at_all_is_reported_not_guessed() {
        let json = MARKUPSAFE.replace(
            r#""license_expression": "BSD-3-Clause""#,
            r#""license_expression": null"#,
        );
        assert_eq!(parse_release(&json).unwrap().license, License::Unknown);
    }

    #[test]
    fn empty_strings_count_as_absent() {
        // Old records use "" and "UNKNOWN" where new ones use null.
        let json = MARKUPSAFE
            .replace(r#""home_page": null"#, r#""home_page": "UNKNOWN""#)
            .replace(
                r#""license_expression": "BSD-3-Clause""#,
                r#""license_expression": "   ""#,
            );
        let r = parse_release(&json).unwrap();
        assert_eq!(r.license, License::Unknown);
        // …and the URL chain still finds Source rather than stopping at UNKNOWN.
        assert_eq!(
            r.homepage.as_deref(),
            Some("https://github.com/pallets/markupsafe/")
        );
    }

    #[test]
    fn a_yanked_file_taints_the_release() {
        let json = MARKUPSAFE.replacen(
            r#""url": "https://files.pythonhosted.org/packages/7e/99/markupsafe-3.0.3.tar.gz",
          "yanked": false"#,
            r#""url": "https://files.pythonhosted.org/packages/7e/99/markupsafe-3.0.3.tar.gz",
          "yanked": true"#,
            1,
        );
        assert!(parse_release(&json).unwrap().yanked);
        assert!(!markupsafe().yanked);
    }

    #[test]
    fn unknown_fields_do_not_break_parsing() {
        let json = MARKUPSAFE.replace(
            r#""yanked": false
      },"#,
            r#""yanked": false, "some_future_field": {"a": 1}
      },"#,
        );
        assert!(parse_release(&json).is_ok());
    }

    #[test]
    fn a_missing_digest_is_refused() {
        let json = MARKUPSAFE.replace(
            r#""sha256": "722695808f4b6457b320fdc131280796bdceb04ab50fe1795cd540799ebe1698""#,
            r#""blake2b": "unused""#,
        );
        assert!(matches!(
            parse_release(&json),
            Err(Error::NoDigest(f)) if f == "markupsafe-3.0.3.tar.gz"
        ));
    }
}
