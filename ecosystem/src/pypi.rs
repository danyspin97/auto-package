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

#[derive(Deserialize)]
struct Envelope {
    info: RawInfo,
    #[serde(default)]
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
    #[serde(default)]
    classifiers: Vec<String>,
    #[serde(default)]
    requires_python: Option<String>,
    #[serde(default)]
    home_page: Option<String>,
    #[serde(default)]
    project_url: Option<String>,
    #[serde(default)]
    project_urls: std::collections::BTreeMap<String, String>,
    #[serde(default)]
    yanked: bool,
}

#[derive(Deserialize)]
struct RawFile {
    filename: String,
    url: String,
    packagetype: String,
    #[serde(default)]
    size: u64,
    #[serde(default)]
    digests: RawDigests,
    #[serde(default)]
    yanked: bool,
}

#[derive(Deserialize, Default)]
struct RawDigests {
    #[serde(default)]
    sha256: Option<String>,
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
