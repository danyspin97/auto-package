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
use std::sync::OnceLock;
use std::time::Duration;

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
    /// The release's `Requires-Dist` lines (PEP 508), verbatim; empty when
    /// PyPI has none (`null`). Markers and extras are left for the reader.
    pub requires_dist: Vec<String>,
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

    #[error(transparent)]
    Parse(#[from] Error),
}

impl FetchError {
    /// Whether PyPI answered 404: the project is not there, as opposed to
    /// PyPI being unreachable or answering something unusable.
    pub fn is_not_found(&self) -> bool {
        matches!(self, FetchError::Http { source, .. } if matches!(**source, ureq::Error::StatusCode(404)))
    }

    /// Whether the download or the write failed because the disk is full.
    pub fn is_disk_full(&self) -> bool {
        matches!(self, FetchError::Io { source, .. } if ap_core::fsx::is_disk_full(source))
    }
}

/// Whole-request timeout for the small JSON endpoints.
const JSON_TIMEOUT: Duration = Duration::from_secs(120);
/// Whole-request timeout for file downloads.
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(600);
/// Cap on a JSON response body.
const MAX_JSON_BYTES: u64 = 64 << 20;
/// Download cap when the index didn't tell us the file size.
const MAX_DOWNLOAD_BYTES: u64 = 2 << 30;

/// One agent per timeout, shared after that: agents hold the connection pool,
/// so a fresh one per request would reopen a TLS connection every time.
fn agent(timeout: Duration) -> ureq::Agent {
    static JSON: OnceLock<ureq::Agent> = OnceLock::new();
    static DOWNLOAD: OnceLock<ureq::Agent> = OnceLock::new();
    let slot = if timeout == DOWNLOAD_TIMEOUT {
        &DOWNLOAD
    } else {
        &JSON
    };
    slot.get_or_init(|| {
        crate::net::agent(ureq::Agent::config_builder().timeout_global(Some(timeout)))
    })
    .clone()
}

fn get_json(url: &str) -> Result<String, FetchError> {
    let to_http_err = |e| FetchError::Http {
        url: url.to_string(),
        source: Box::new(e),
    };
    // `read_to_string` stops at 10 MiB, which a project with thousands of
    // releases exceeds. Raise the limit explicitly; ureq errors out when it is
    // hit instead of returning a truncated body.
    crate::net::call_with_retries(|| agent(JSON_TIMEOUT).get(url))
        .map_err(to_http_err)?
        .into_body()
        .with_config()
        .limit(MAX_JSON_BYTES)
        .read_to_string()
        .map_err(to_http_err)
}

/// Reduce a JSON-supplied filename to a bare file name: no directories, no
/// `..`, nothing absolute.
fn safe_filename(filename: &str) -> Option<&str> {
    let name = Path::new(filename).file_name()?.to_str()?;
    (name == filename && name != ".." && name != ".").then_some(name)
}

/// Fetch and parse the latest release for `name` from PyPI's JSON API.
pub fn fetch_latest_release(name: &str) -> Result<Release, FetchError> {
    let url = format!("https://pypi.org/pypi/{}/json", urlencoding::encode(name));
    let json = get_json(&url)?;
    Ok(parse_release(&json)?)
}

/// Just the latest-release version string, without the sdist/wheel/license
/// bookkeeping [`fetch_latest_release`] enforces.
///
/// For an update sweep: we only need to know whether upstream has moved past
/// what's packaged. A wheel-only release (common for NVIDIA's CUDA toolkit
/// bindings) has a legitimate version but no sdist, which makes the full
/// `fetch_latest_release` reject it — and misses the whole point of a check
/// that's trying to flag the bump. Reads `info.version` straight from the
/// envelope and nothing else.
pub fn fetch_latest_version(name: &str) -> Result<String, FetchError> {
    let url = format!("https://pypi.org/pypi/{}/json", urlencoding::encode(name));
    let json = get_json(&url)?;
    parse_version_only(&json).map_err(FetchError::from)
}

/// Extract `info.version` from a PyPI JSON envelope. Separated out so a
/// test can run it without an HTTP call.
pub fn parse_version_only(json: &str) -> Result<String, Error> {
    #[derive(Deserialize)]
    struct Envelope {
        info: Info,
    }
    #[derive(Deserialize)]
    struct Info {
        version: String,
    }
    let env: Envelope = serde_json::from_str(json)?;
    Ok(env.info.version)
}

impl Sdist {
    /// Download to `dest_dir/<filename>` and verify sha256.
    ///
    /// Idempotent: if the file already exists and its hash matches, the
    /// download is skipped. A corrupted file (wrong hash) is overwritten.
    pub fn fetch(&self, dest_dir: &Path) -> Result<PathBuf, FetchError> {
        let Some(name) = safe_filename(&self.filename) else {
            return Err(FetchError::Io {
                path: PathBuf::from(&self.filename),
                source: io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "filename is not a plain file name",
                ),
            });
        };
        let dest = dest_dir.join(name);

        if dest.is_file() && sha256_of(&dest).is_ok_and(|h| h == self.sha256) {
            return Ok(dest);
        }

        // Cap the body: the declared size (plus slack) when known, else a
        // global ceiling.
        let limit = if self.size > 0 {
            self.size.saturating_add(self.size / 10 + 4096)
        } else {
            MAX_DOWNLOAD_BYTES
        };

        let resp = crate::net::call_with_retries(|| agent(DOWNLOAD_TIMEOUT).get(&self.url))
            .map_err(|e| FetchError::Http {
                url: self.url.clone(),
                source: Box::new(e),
            })?;

        // Download to a private temp name and rename into place, so parallel
        // fetches of one file never read a half-written file or delete each
        // other's result.
        let part = dest_dir.join(format!(
            "{name}.part-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let result = self.download_to(resp, &part, limit);
        if let Err(e) = result {
            let _ = fs::remove_file(&part);
            return Err(e);
        }
        fs::rename(&part, &dest).map_err(|e| {
            let _ = fs::remove_file(&part);
            FetchError::Io {
                path: dest.clone(),
                source: e,
            }
        })?;
        Ok(dest)
    }

    /// Stream `resp` into `part`, enforcing `limit` and the sha256.
    fn download_to(
        &self,
        resp: ureq::http::Response<ureq::Body>,
        part: &Path,
        limit: u64,
    ) -> Result<(), FetchError> {
        let io_err = |e| FetchError::Io {
            path: part.to_path_buf(),
            source: e,
        };
        let mut hasher = Sha256::new();
        let mut buf = [0u8; 65536];
        let mut file = fs::File::create(part).map_err(io_err)?;
        let mut body = resp.into_body();
        let mut reader = body.with_config().limit(limit).reader();

        loop {
            let n = reader.read(&mut buf).map_err(io_err)?;
            if n == 0 {
                break;
            }
            hasher.update(&buf[..n]);
            file.write_all(&buf[..n]).map_err(io_err)?;
        }
        file.flush().map_err(io_err)?;

        let actual = hex::encode(hasher.finalize());
        if actual != self.sha256 {
            return Err(FetchError::BadDigest {
                filename: self.filename.clone(),
                expected: self.sha256.clone(),
                actual,
            });
        }
        Ok(())
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
        requires_dist: info.requires_dist,
    })
}

/// The source tarball among the release's files.
///
/// A release usually has one sdist and many wheels, but the format is not
/// guaranteed: older uploads are `.zip`, and a few have both. Prefer
/// `.tar.gz`, which is what every openSUSE Python spec expects.
///
/// A yanked file is disqualifying for the release (see `parse_release`), so a
/// live sdist of any format beats a yanked `.tar.gz`. Among candidates with the
/// same yanked state the format preference still applies; if every sdist is
/// yanked, the result is the same as before this rule existed.
fn pick_sdist(urls: &[RawFile]) -> Option<&RawFile> {
    let sdists = || urls.iter().filter(|f| f.packagetype == "sdist");
    let live = || sdists().filter(|f| !f.yanked);
    live()
        .find(|f| f.filename.ends_with(".tar.gz"))
        .or_else(|| live().next())
        .or_else(|| sdists().find(|f| f.filename.ends_with(".tar.gz")))
        .or_else(|| sdists().next())
}

/// Pick a wheel to repackage as a prebuilt binary, for a release that has
/// neither an sdist nor a discoverable source repository. Prefers a
/// universal `py3-none-any` wheel (installable regardless of platform), then
/// a wheel whose platform tag names `arch`, then whatever wheel exists.
///
/// `single_locked_flavor_abi` is the CPython ABI tag (`cp313`, not
/// `python313`) of the one Python flavor this project is configured to build
/// for, when it is configured for exactly one — see
/// `ap_build::flavors::PythonFlavors::single_flavor`, which is the only
/// producer of this value and documents why anything other than exactly one
/// configured flavor must pass `None` here. `None` otherwise (including
/// "don't know"), which keeps the historical, maximally conservative
/// behavior of refusing every CPython-ABI-locked wheel.
pub fn pick_wheel_from_json(
    json: &str,
    arch: &str,
    single_locked_flavor_abi: Option<&str>,
) -> Result<WheelFile, Error> {
    let raw: Envelope = serde_json::from_str(json)?;
    let info = &raw.info;
    let wheel =
        pick_wheel(&raw.urls, arch, single_locked_flavor_abi).ok_or_else(|| Error::NoWheel {
            name: info.name.clone(),
            version: info.version.clone(),
        })?;
    let sha256 = wheel
        .digests
        .sha256
        .clone()
        .ok_or_else(|| Error::NoDigest(wheel.filename.clone()))?;
    Ok(WheelFile {
        filename: wheel.filename.clone(),
        url: wheel.url.clone(),
        sha256,
        size: wheel.size,
    })
}

/// One wheel per CPython flavor, for a release whose wheels are each locked to
/// a single CPython (`cp313-cp313`, `cp314-cp314` — Triton's shape), where no
/// single wheel can serve every flavor. `abis` are ABI tags (`cp313`).
///
/// Fails with [`Error::NoWheel`] if ANY flavor has no installable wheel for
/// `arch`: a package missing one flavor must not be silently half-built.
pub fn pick_wheels_per_flavor(
    json: &str,
    arch: &str,
    abis: &[String],
) -> Result<Vec<(String, WheelFile)>, Error> {
    let mut out: Vec<(String, WheelFile)> = Vec::new();
    for abi in abis {
        let wheel = pick_wheel_from_json(json, arch, Some(abi))?;
        out.push((abi.clone(), wheel));
    }
    Ok(out)
}

fn pick_wheel<'a>(
    urls: &'a [RawFile],
    arch: &str,
    single_locked_flavor_abi: Option<&str>,
) -> Option<&'a RawFile> {
    let installable = || {
        urls.iter().filter(|f| {
            f.packagetype == "bdist_wheel"
                && !f.yanked
                && wheel_is_installable(&f.filename, arch, single_locked_flavor_abi)
        })
    };
    // Prefer the most-portable `-none-any.whl` among the installable set,
    // then anything else that passes. Previously the fallback chain ended in
    // `wheels().next()` (any wheel of the right ABI, regardless of its
    // platform or Python tag), which the Oct 6 review reproduced as:
    //   - an aarch64 request accepting `py3-none-win_amd64`,
    //   - a Python 3.13-locked project accepting `py2-none-any`.
    // The platform/Python checks are inside `wheel_is_installable` now, so
    // nothing incompatible can land in `installable()` to fall back onto.
    installable()
        .find(|f| f.filename.ends_with("-none-any.whl"))
        .or_else(|| installable().next())
}

/// Full PEP 425 / PEP 600 tag-triple compatibility check: a wheel's Python
/// tag, ABI tag, AND platform tag must all be installable on the target.
///
/// Composed of three independent predicates — `wheel_is_flavor_agnostic`
/// (ABI), [`wheel_python_is_installable`] (Python tag), and
/// [`wheel_platform_matches_linux_arch`] (platform tag). The Oct 6 review
/// found this check previously existed only for the ABI tag, so a wheel like
/// `py3-none-win_amd64` passed every filter on an aarch64 Linux target and
/// got silently repackaged into the spec; now the whole triple is validated
/// up front and the pick fails loudly rather than producing a broken binary.
fn wheel_is_installable(
    filename: &str,
    arch: &str,
    single_locked_flavor_abi: Option<&str>,
) -> bool {
    let Some((python, abi, platform)) = split_wheel_tags(filename) else {
        return false;
    };
    wheel_is_flavor_agnostic(filename, single_locked_flavor_abi)
        && wheel_python_is_installable(python, abi, single_locked_flavor_abi)
        && wheel_platform_matches_linux_arch(platform, arch)
}

/// Split a wheel filename into its (Python tag, ABI tag, platform tag)
/// triple, as the last three `-`-separated segments before `.whl`. Returns
/// `None` for a non-wheel filename or one that doesn't have those segments.
fn split_wheel_tags(filename: &str) -> Option<(&str, &str, &str)> {
    let stem = filename.strip_suffix(".whl")?;
    let (rest, platform) = stem.rsplit_once('-')?;
    let (rest, abi) = rest.rsplit_once('-')?;
    let (_, python) = rest.rsplit_once('-')?;
    Some((python, abi, platform))
}

/// Does this wheel's platform tag describe a filesystem layout installable
/// on `arch`-architecture Linux?
///
/// `any` is universal. The three Linux platform families — the historical
/// `linux_<arch>`, the baseline-pegged `manylinux*_<arch>`, and the
/// musl-targeted `musllinux_*_<arch>` — all end in `_<arch>`. Everything
/// else (`win_amd64`, `macosx_*`, `android_*`, ...) is not an openSUSE
/// target. A compressed platform tag like
/// `manylinux1_x86_64.manylinux_2_17_x86_64` is PEP 425's way of listing
/// multiple compatible platforms in one wheel; the `.`-split handles that.
pub(crate) fn wheel_platform_matches_linux_arch(tag: &str, arch: &str) -> bool {
    if tag == "any" {
        return true;
    }
    let suffix = format!("_{arch}");
    tag.split('.').any(|p| {
        let is_linux =
            p.starts_with("linux_") || p.starts_with("manylinux") || p.starts_with("musllinux_");
        is_linux && p.ends_with(&suffix)
    })
}

/// Does this wheel's Python tag describe an interpreter the openSUSE spec
/// will build for?
///
/// Three admitted shapes:
///   - `py<N>` / `py<N>.py<M>` — pure Python language level. `py3`/`py3x`
///     admitted; `py2` refused (we never build Python 2 flavors).
///   - `cp<XYZ>` — specific CPython version. Only safe when either (a) the
///     project is locked to exactly this flavor
///     (`single_locked_flavor_abi`), which is the same gate
///     `wheel_is_flavor_agnostic` enforces for the ABI tag, or (b) the ABI
///     tag is `abi3` (CPython's forward-compatible stable ABI), in which
///     case the wheel installs under any CPython ≥ the tagged version.
///   - Compressed `.`-separated combinations of either admitted shape.
///
/// Everything else — `py2*`, PyPy's `pp*`, Jython's `jp*`, IronPython's
/// `ip*`, anything we don't recognise — is refused. The Oct 6 review probed
/// this with a `py2-none-any` wheel being accepted for a Python 3.13 build.
pub(crate) fn wheel_python_is_installable(
    tag: &str,
    abi: &str,
    single_locked_flavor_abi: Option<&str>,
) -> bool {
    tag.split('.').any(|p| {
        if let Some(ver) = p.strip_prefix("py") {
            // `py<N>` or `py<N><M>` — language-level tag. First digit must
            // be '3' or newer (never Python 2).
            ver.chars()
                .next()
                .is_some_and(|c| c.is_ascii_digit() && c >= '3')
        } else if let Some(ver) = p.strip_prefix("cp") {
            // CPython-locked. Accept iff the project is pinned to exactly
            // this flavor OR the wheel uses the stable ABI (`abi3`) and we
            // have *some* CPython flavor configured to install into. A
            // bare `cp313` wheel against a dual-flavor project with no
            // single lock stays refused — same reasoning as
            // `wheel_is_flavor_agnostic`.
            if Some(p) == single_locked_flavor_abi {
                return true;
            }
            if abi == "abi3" {
                // Must still have a plausible digit-shape Python version
                // (not `cpABC`) and be an openSUSE-era Python (>= 3).
                return ver.chars().next().is_some_and(|c| c == '3');
            }
            false
        } else {
            false
        }
    })
}

/// Whether a wheel's ABI tag makes it safe to repackage as a single file,
/// either because it is agnostic to which Python flavor(s) an openSUSE spec
/// builds for (this project builds `python313`+`python314` side by side,
/// sometimes more), or because the project builds for only one flavor and
/// this wheel is locked to exactly that one.
///
/// ABI tag `none` means no CPython C-API dependency at all — pure Python, or
/// (the shape every NVIDIA CUDA toolkit wheel this fallback was built for
/// uses) a native library loaded via `dlopen`/`ctypes` rather than compiled
/// as a Python C extension — so the same file installs correctly under any
/// flavor. `abi3` is similar: a stable CPython ABI that works across every
/// CPython 3.x. A `cpXYZ` ABI tag (`cp313`, `cp310t`, ...) means the
/// opposite: compiled against one specific CPython's C API. Picking one and
/// repackaging it as if it worked everywhere produces a package that is
/// broken for every flavor except the one it happened to be built for —
/// better to find no usable wheel at all than a silently wrong one (Triton
/// is the real example: it publishes wheels, but one per CPython version
/// with no universal one, so this fallback must refuse rather than pick an
/// arbitrary `cp3xx` build) — *unless* `single_locked_flavor_abi` says this
/// project only ever builds for the one flavor this exact wheel is locked
/// to, in which case there is no second flavor left for it to be broken
/// under.
fn wheel_is_flavor_agnostic(filename: &str, single_locked_flavor_abi: Option<&str>) -> bool {
    let Some((_python, abi, _platform)) = split_wheel_tags(filename) else {
        return false;
    };
    abi == "none" || abi == "abi3" || Some(abi) == single_locked_flavor_abi
}

/// The best upstream URL, preferring the project's own page over its host.
///
/// `home_page` is null on anything packaged in the last few years, and
/// `project_urls` has no required keys — MarkupSafe publishes `Source` and
/// `Documentation` but no `Homepage`. The last resort is the PyPI page, which
/// is a poor `URL:` tag but better than an empty one.
fn homepage(info: &RawInfo) -> Option<String> {
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
    #[serde(default, deserialize_with = "deserialize_null_default")]
    requires_dist: Vec<String>,
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

/// Three-way result of a PEP 440 comparison between two version strings.
///
/// Collapsing `Unparseable` into `false`/`NotNewer` (as `is_newer_pep440`
/// used to) made the update sweep classify a package it *couldn't actually
/// compare* as `UpToDate` — the Oct 6 review's probe with a non-PEP 440
/// packaged version and a real-newer upstream reproduced the false-current
/// label. Callers that want "yes/no newer" still have `is_newer_pep440`;
/// callers that need to distinguish "I checked, it's current" from "I
/// couldn't check" (every update-plan classifier) use this enum.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VersionCompare {
    Newer,
    NotNewer,
    Unparseable,
}

/// Compare two PEP 440 version strings three-way, strictly: a version that is
/// not PEP 440 is `Unparseable` (no generic fallback, no epoch or `v`/`r`
/// prefix handling; [`crate::version::cmp`] has those).
pub fn compare_pep440(latest: &str, packaged: &str) -> VersionCompare {
    let (Some(l), Some(p)) = (
        crate::version::parse_pep440(latest),
        crate::version::parse_pep440(packaged),
    ) else {
        return VersionCompare::Unparseable;
    };
    if l > p {
        VersionCompare::Newer
    } else {
        VersionCompare::NotNewer
    }
}

/// True iff `latest` is a strictly newer version than `packaged` in PyPI's
/// order ([`crate::version::cmp`] with [`crate::version::Scheme::Pep440`]).
///
/// Returns `false` for an unparseable input too; callers that must
/// distinguish "cannot compare" from "not newer" need
/// [`crate::version::cmp`]'s `Option` instead — see [`VersionCompare`] for why.
pub fn is_newer_pep440(latest: &str, packaged: &str) -> bool {
    crate::version::cmp(latest, packaged, crate::version::Scheme::Pep440)
        == Some(std::cmp::Ordering::Greater)
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
          "filename": "markupsafe-3.0.3-py3-none-manylinux_2_5_x86_64.whl",
          "packagetype": "bdist_wheel",
          "size": 21978,
          "url": "https://files.pythonhosted.org/packages/aa/markupsafe-3.0.3-py3-none.whl",
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
    fn an_empty_project_url_does_not_hide_the_fallbacks() {
        // `"Homepage": ""` is unset, so the `Source` URL is the homepage, and
        // whitespace-only values are unset too.
        let info = |urls: &str| -> RawInfo {
            serde_json::from_str(&format!(
                r#"{{"name": "x", "version": "1", "home_page": null,
                    "project_urls": {urls}}}"#
            ))
            .unwrap()
        };
        let empty_homepage = info(r#"{"Homepage": "", "Source": "https://github.com/example/x"}"#);
        assert_eq!(
            homepage(&empty_homepage).as_deref(),
            Some("https://github.com/example/x")
        );
        let blank_homepage =
            info(r#"{"Homepage": "  \t ", "Source": "https://github.com/example/x"}"#);
        assert_eq!(
            homepage(&blank_homepage).as_deref(),
            Some("https://github.com/example/x")
        );
        // A real value is trimmed, and nothing usable leaves the homepage unset.
        let padded = info(r#"{"Homepage": " https://example.org/x "}"#);
        assert_eq!(homepage(&padded).as_deref(), Some("https://example.org/x"));
        let nothing = info(r#"{"Homepage": "", "Source": ""}"#);
        assert_eq!(homepage(&nothing), None);
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
        let wheel = pick_wheel_from_json(MARKUPSAFE, "x86_64", None).expect("a wheel exists");
        assert_eq!(
            wheel.filename,
            "markupsafe-3.0.3-py3-none-manylinux_2_5_x86_64.whl"
        );
        assert_eq!(wheel.sha256, "0123456789abcdef");
    }

    #[test]
    fn refuses_the_wrong_arch_rather_than_falling_back_to_whatever_wheel_exists() {
        // The fixture only ships an x86_64 wheel. An aarch64 request used to
        // silently return that x86_64 wheel ("falls back to the only wheel")
        // and let the spec-generation code paper over the mismatch, which
        // the Oct 6 review probed as `py3-none-win_amd64` being accepted for
        // an aarch64 target. Now the pick fails outright so the agent can
        // report "no usable upstream wheel" instead of generating a broken
        // package.
        let err = pick_wheel_from_json(MARKUPSAFE, "aarch64", None)
            .expect_err("the only wheel is x86_64; aarch64 must refuse it");
        assert!(matches!(err, Error::NoWheel { .. }), "got {err:?}");
    }

    #[test]
    fn refuses_a_wheel_whose_platform_tag_is_not_linux() {
        // The win_amd64 case from the Oct 6 review: a `py3-none-win_amd64`
        // wheel passed every filter when the project arch was aarch64 (or
        // anything non-win), because the previous fallback ended at
        // `wheels().next()` with no platform validation at all.
        let json = MARKUPSAFE.replace(
            "markupsafe-3.0.3-py3-none-manylinux_2_5_x86_64.whl",
            "markupsafe-3.0.3-py3-none-win_amd64.whl",
        );
        assert!(matches!(
            pick_wheel_from_json(&json, "x86_64", None),
            Err(Error::NoWheel { .. }),
        ));
        assert!(matches!(
            pick_wheel_from_json(&json, "aarch64", None),
            Err(Error::NoWheel { .. }),
        ));
    }

    #[test]
    fn refuses_a_wheel_whose_python_tag_is_python_2() {
        // The `py2-none-any` case from the Oct 6 review: the ABI check
        // alone said "none, OK" and the universal `any` platform passed,
        // but the wheel's Python tag was py2 and no openSUSE spec this
        // project writes builds for Python 2.
        let json = MARKUPSAFE.replace(
            "markupsafe-3.0.3-py3-none-manylinux_2_5_x86_64.whl",
            "markupsafe-3.0.3-py2-none-any.whl",
        );
        assert!(matches!(
            pick_wheel_from_json(&json, "x86_64", None),
            Err(Error::NoWheel { .. }),
        ));
        // The locked-flavor variant must also reject it — the lock covers
        // the ABI, not the language level.
        assert!(matches!(
            pick_wheel_from_json(&json, "x86_64", Some("cp313")),
            Err(Error::NoWheel { .. }),
        ));
    }

    #[test]
    fn accepts_a_compressed_platform_tag_covering_our_arch() {
        // PEP 425 allows a wheel to list several compatible platforms in
        // one `.`-separated tag. A `manylinux1_x86_64.manylinux_2_17_x86_64`
        // wheel is installable on x86_64 even though neither segment is the
        // exact tag osc/zypper would print.
        let json = MARKUPSAFE.replace(
            "markupsafe-3.0.3-py3-none-manylinux_2_5_x86_64.whl",
            "markupsafe-3.0.3-py3-none-manylinux1_x86_64.manylinux_2_17_x86_64.whl",
        );
        let wheel = pick_wheel_from_json(&json, "x86_64", None).expect("compressed tag OK");
        assert!(wheel.filename.contains("manylinux"));
    }

    #[test]
    fn accepts_an_abi3_wheel_regardless_of_its_cp_tag() {
        // A stable-ABI wheel tagged `cp310-abi3-...` installs under every
        // CPython 3.x, so no single-flavor lock is needed.
        let json = MARKUPSAFE.replace(
            "markupsafe-3.0.3-py3-none-manylinux_2_5_x86_64.whl",
            "markupsafe-3.0.3-cp310-abi3-manylinux_2_17_x86_64.whl",
        );
        let wheel = pick_wheel_from_json(&json, "x86_64", None).expect("abi3 is universal");
        assert!(wheel.filename.contains("abi3"));
    }

    #[test]
    fn flavor_agnostic_check_reads_the_abi_tag_not_the_platform_tag() {
        assert!(wheel_is_flavor_agnostic(
            "nvidia_nccl_cu13-2.29.7-py3-none-manylinux2014_x86_64.whl",
            None
        ));
        assert!(wheel_is_flavor_agnostic(
            "somepkg-1.0-py3-none-any.whl",
            None
        ));
        assert!(!wheel_is_flavor_agnostic(
            "markupsafe-3.0.3-cp314-cp314-manylinux_2_5_x86_64.whl",
            None
        ));
        assert!(!wheel_is_flavor_agnostic(
            "triton-3.8.0-cp310-cp310-manylinux_2_27_x86_64.whl",
            None
        ));
    }

    #[test]
    fn refuses_a_wheel_locked_to_one_cpython_abi() {
        // Triton's real shape: genuine compiled extensions, published per
        // CPython version (cp310, cp311, ...) with no universal build. A
        // single such wheel cannot satisfy every Python flavor an openSUSE
        // spec builds for (python313 *and* python314 side by side) — picking
        // one and repackaging it as if it were universal would silently
        // break every flavor except the one it was built for, so this must
        // report no usable wheel rather than pick an arbitrary one.
        let json = MARKUPSAFE.replace(
            "markupsafe-3.0.3-py3-none-manylinux_2_5_x86_64.whl",
            "markupsafe-3.0.3-cp314-cp314-manylinux_2_5_x86_64.whl",
        );
        let err = pick_wheel_from_json(&json, "x86_64", None)
            .expect_err("cp-locked wheel must be refused");
        assert!(matches!(err, Error::NoWheel { .. }), "got {err:?}");
    }

    #[test]
    fn a_locked_wheel_matching_the_single_configured_flavor_is_accepted() {
        // A project configured (via flavors.toml) to build for exactly one
        // Python flavor has no second flavor left for a cp-locked wheel to be
        // broken under, so the one wheel matching that exact ABI is safe to
        // repackage automatically.
        let json = MARKUPSAFE.replace(
            "markupsafe-3.0.3-py3-none-manylinux_2_5_x86_64.whl",
            "markupsafe-3.0.3-cp314-cp314-manylinux_2_5_x86_64.whl",
        );
        let wheel = pick_wheel_from_json(&json, "x86_64", Some("cp314"))
            .expect("the single configured flavor's own wheel must be accepted");
        assert_eq!(
            wheel.filename,
            "markupsafe-3.0.3-cp314-cp314-manylinux_2_5_x86_64.whl"
        );
    }

    /// Triton's shape: one wheel per CPython, none universal. Each flavor gets
    /// its own; a flavor with no wheel fails the whole pick.
    #[test]
    fn one_locked_wheel_per_flavor_is_picked_and_a_gap_fails_the_set() {
        let mut v: serde_json::Value = serde_json::from_str(MARKUPSAFE).unwrap();
        let first = v["urls"][0].clone();
        let mk = |name: &str| {
            let mut u = first.clone();
            u["filename"] = name.into();
            u["url"] = format!("https://files.example/{name}").into();
            u
        };
        v["urls"] = serde_json::Value::Array(vec![
            mk("markupsafe-3.0.3-cp313-cp313-manylinux_2_5_x86_64.whl"),
            mk("markupsafe-3.0.3-cp314-cp314-manylinux_2_5_x86_64.whl"),
        ]);
        let json = v.to_string();
        let abis = vec!["cp313".to_string(), "cp314".to_string()];
        let picked = pick_wheels_per_flavor(&json, "x86_64", &abis).unwrap();
        assert_eq!(picked[0].0, "cp313");
        assert!(picked[0].1.filename.contains("cp313-cp313"));
        assert!(picked[1].1.filename.contains("cp314-cp314"));

        let with_gap = vec!["cp313".to_string(), "cp312".to_string()];
        assert!(pick_wheels_per_flavor(&json, "x86_64", &with_gap).is_err());
    }

    #[test]
    fn a_locked_wheel_not_matching_the_single_configured_flavor_is_still_refused() {
        // Configured for cp314 but the only wheel on offer is cp313 — still
        // broken for the one flavor this project actually builds.
        let json = MARKUPSAFE.replace(
            "markupsafe-3.0.3-py3-none-manylinux_2_5_x86_64.whl",
            "markupsafe-3.0.3-cp313-cp313-manylinux_2_5_x86_64.whl",
        );
        let err = pick_wheel_from_json(&json, "x86_64", Some("cp314"))
            .expect_err("a wheel for the wrong flavor must still be refused");
        assert!(matches!(err, Error::NoWheel { .. }), "got {err:?}");
    }

    #[test]
    fn no_wheel_at_all_is_an_error() {
        let json = MARKUPSAFE.replace(
            r#""packagetype": "bdist_wheel""#,
            r#""packagetype": "sdist""#,
        );
        let err =
            pick_wheel_from_json(&json, "x86_64", None).expect_err("no wheel in this fixture");
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

    /// A release with one `.tar.gz` and one `.zip` sdist, each with its own
    /// file-level `yanked` flag.
    fn two_sdists(tar_yanked: bool, zip_yanked: bool) -> String {
        format!(
            r#"{{"info": {{"name": "pkg", "version": "1.0", "yanked": false}},
              "urls": [
                {{"digests": {{"sha256": "aaaa"}}, "filename": "pkg-1.0.tar.gz",
                  "packagetype": "sdist", "size": 1,
                  "url": "https://example.test/pkg-1.0.tar.gz", "yanked": {tar_yanked}}},
                {{"digests": {{"sha256": "bbbb"}}, "filename": "pkg-1.0.zip",
                  "packagetype": "sdist", "size": 2,
                  "url": "https://example.test/pkg-1.0.zip", "yanked": {zip_yanked}}}
              ]}}"#
        )
    }

    #[test]
    fn a_yanked_tarball_yields_to_a_live_zip_sdist() {
        let r = parse_release(&two_sdists(true, false)).expect("parses");
        assert_eq!(r.sdist.filename, "pkg-1.0.zip");
        assert!(!r.yanked, "the chosen sdist is live, so the release is too");
    }

    #[test]
    fn a_live_tarball_keeps_its_format_preference() {
        for zip_yanked in [false, true] {
            let r = parse_release(&two_sdists(false, zip_yanked)).expect("parses");
            assert_eq!(r.sdist.filename, "pkg-1.0.tar.gz");
            assert!(!r.yanked);
        }
    }

    #[test]
    fn when_every_sdist_is_yanked_the_tarball_is_still_picked_and_taints_the_release() {
        let r = parse_release(&two_sdists(true, true)).expect("parses");
        assert_eq!(r.sdist.filename, "pkg-1.0.tar.gz");
        assert!(r.yanked);
    }

    #[test]
    fn tarball_path_honours_the_file_level_yanked_flag() {
        let yanked = MARKUPSAFE.replacen(
            r#""url": "https://files.pythonhosted.org/packages/7e/99/markupsafe-3.0.3.tar.gz",
          "yanked": false"#,
            r#""url": "https://files.pythonhosted.org/packages/7e/99/markupsafe-3.0.3.tar.gz",
          "yanked": true"#,
            1,
        );
        let r = parse_release_with_tarball(&yanked, "markupsafe-3.0.3.tar.gz").expect("parses");
        assert!(r.yanked, "a yanked tarball file must taint the release");
        assert_eq!(r.sdist.filename, "markupsafe-3.0.3.tar.gz");

        let live =
            parse_release_with_tarball(MARKUPSAFE, "markupsafe-3.0.3.tar.gz").expect("parses");
        assert!(!live.yanked);
    }

    #[test]
    fn tarball_path_honours_the_release_level_yanked_flag() {
        let json = MARKUPSAFE.replacen(
            r#""yanked": false
      },"#,
            r#""yanked": true
      },"#,
            1,
        );
        let r = parse_release_with_tarball(&json, "markupsafe-3.0.3.tar.gz").expect("parses");
        assert!(r.yanked);
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

    #[test]
    fn pypi_name_from_canonical_source_url() {
        assert_eq!(
            pypi_name_from_source_url(
                "https://files.pythonhosted.org/packages/source/a/attrs/attrs-%{version}.tar.gz"
            )
            .as_deref(),
            Some("attrs"),
        );
    }

    #[test]
    fn pypi_name_rejects_github_source() {
        assert_eq!(
            pypi_name_from_source_url("https://github.com/pallets/markupsafe/archive/3.0.3.tar.gz"),
            None,
        );
    }

    #[test]
    fn pypi_name_preserves_case() {
        assert_eq!(
            pypi_name_from_source_url(
                "https://files.pythonhosted.org/packages/source/M/MarkupSafe/MarkupSafe-%{version}.tar.gz"
            )
            .as_deref(),
            Some("MarkupSafe"),
        );
    }

    #[test]
    fn is_newer_decimal_segments_compared_numerically() {
        assert!(is_newer_pep440("1.10.0", "1.9.0"));
        assert!(!is_newer_pep440("1.9.0", "1.10.0"));
        assert!(!is_newer_pep440("1.9.0", "1.9.0"));
    }

    #[test]
    fn is_newer_prerelease_is_older_than_release() {
        assert!(!is_newer_pep440("2.0.0rc1", "2.0.0"));
        assert!(is_newer_pep440("2.0.0", "2.0.0rc1"));
    }

    #[test]
    fn is_newer_refuses_on_parse_failure() {
        assert!(!is_newer_pep440("not-a-version", "1.0"));
        assert!(!is_newer_pep440("1.0", "not-a-version"));
    }

    #[test]
    fn wheel_filename_yields_distribution_and_version() {
        assert_eq!(
            pypi_name_and_version_from_wheel_filename(
                "nvidia_cublas-13.1.1.3-py3-none-manylinux_2_27_x86_64.whl"
            ),
            Some(("nvidia_cublas".to_string(), "13.1.1.3".to_string())),
        );
    }

    #[test]
    fn wheel_filename_keeps_local_version_qualifier() {
        // PyPI wheels can embed `+<local>` — the version segment is still just
        // the single dash-delimited token at position 1.
        assert_eq!(
            pypi_name_and_version_from_wheel_filename(
                "torch-2.4.0+cpu-cp311-cp311-linux_x86_64.whl"
            ),
            Some(("torch".to_string(), "2.4.0+cpu".to_string())),
        );
    }

    #[test]
    fn wheel_filename_rejects_non_wheel() {
        assert_eq!(
            pypi_name_and_version_from_wheel_filename("attrs-26.1.0.tar.gz"),
            None,
        );
    }

    #[test]
    fn wheel_filename_rejects_truncated_filename() {
        // Fewer than 5 hyphen-separated segments means not a real wheel name.
        assert_eq!(
            pypi_name_and_version_from_wheel_filename("foo-1.0.whl"),
            None
        );
    }

    #[test]
    fn tarball_filename_extracts_name_from_template() {
        assert_eq!(
            pypi_name_from_versioned_tarball("torchaudio-%{version}.tar.gz", "2.4.0").as_deref(),
            Some("torchaudio"),
        );
        assert_eq!(
            pypi_name_from_versioned_tarball("nvidia-cuda-cccl-%{version}.tar.gz", "13.4.92")
                .as_deref(),
            Some("nvidia-cuda-cccl"),
        );
    }

    #[test]
    fn tarball_filename_extracts_name_against_literal_version() {
        assert_eq!(
            pypi_name_from_versioned_tarball("cuda-core-1.2.0.tar.gz", "1.2.0").as_deref(),
            Some("cuda-core"),
        );
        assert_eq!(
            pypi_name_from_versioned_tarball("cuda-python-13.4.1.tar.gz", "13.4.1").as_deref(),
            Some("cuda-python"),
        );
    }

    #[test]
    fn tarball_filename_literal_form_needs_the_right_version() {
        assert!(
            pypi_name_from_versioned_tarball("cuda-core-1.2.0.tar.gz", "1.3.0").is_none(),
            "spec version has to actually match the embedded one"
        );
    }

    #[test]
    fn tarball_filename_rejects_pure_name() {
        assert!(pypi_name_from_versioned_tarball("something.tar.gz", "9.9.9").is_none());
    }

    #[test]
    fn tarball_filename_rejects_urls_and_other_macros() {
        for src in [
            "https://github.com/o/r/releases/download/v%{version}/r-%{version}.tar.gz",
            "https://github.com/o/r/archive/foo-1.2.3.tar.gz",
            "%{name}-%{version}.tar.gz",
        ] {
            assert!(
                pypi_name_from_versioned_tarball(src, "1.2.3").is_none(),
                "{src}"
            );
        }
    }

    /// A wheel-only release — NVIDIA's CUDA toolkit packages are the real
    /// case — has `info.version` but no sdist. `parse_release` refuses it,
    /// which is correct for the full sdist consumers; `parse_version_only`
    /// must still succeed, because the version is the only thing an update
    /// sweep is asking for.
    const WHEEL_ONLY: &str = r#"{
      "info": {
        "name": "nvidia-nccl-cu13",
        "version": "2.29.7",
        "classifiers": [],
        "project_urls": {}
      },
      "urls": [
        {
          "digests": {"sha256": "deadbeef"},
          "filename": "nvidia_nccl_cu13-2.29.7-py3-none-manylinux2014_x86_64.whl",
          "packagetype": "bdist_wheel",
          "size": 123,
          "url": "https://files.pythonhosted.org/packages/x/nvidia_nccl_cu13-2.29.7-py3-none-manylinux2014_x86_64.whl",
          "yanked": false
        }
      ]
    }"#;

    #[test]
    fn safe_filename_rejects_anything_but_a_bare_name() {
        assert_eq!(safe_filename("a-1.0.tar.gz"), Some("a-1.0.tar.gz"));
        for bad in ["../x", "/etc/passwd", "a/b", "..", ".", ""] {
            assert_eq!(safe_filename(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn fetch_refuses_a_traversing_filename() {
        let dir = tempfile::tempdir().unwrap();
        let s = Sdist {
            filename: "../evil".into(),
            url: "http://127.0.0.1:1/x".into(),
            sha256: String::new(),
            size: 0,
        };
        assert!(matches!(s.fetch(dir.path()), Err(FetchError::Io { .. })));
    }

    #[test]
    fn parse_version_only_works_on_wheel_only_release() {
        // The whole point of this function: wheel-only releases have no sdist,
        // but still have a valid info.version. The full parser would error
        // here (NoSdist) and the update sweep would lose the ability to flag
        // a bump for every NVIDIA CUDA package.
        assert_eq!(parse_version_only(WHEEL_ONLY).unwrap(), "2.29.7");
        assert!(matches!(
            parse_release(WHEEL_ONLY),
            Err(Error::NoSdist { .. })
        ));
    }

    #[test]
    fn source_url_from_json_follows_the_source_project_url_key() {
        let r = source_url_from_json(MARKUPSAFE);
        assert_eq!(r.as_deref(), Some("https://github.com/pallets/markupsafe"));
    }

    #[test]
    fn source_url_from_json_preserves_nested_gitlab_group() {
        // The real bug: a project_urls entry like
        // `https://gitlab.com/group/subgroup/project` used to be truncated
        // to `group/subgroup`, which is a group URL, not a project URL.
        let json = r#"{
          "info": {
            "name": "x", "version": "1",
            "project_urls": {
              "Source": "https://gitlab.com/group/subgroup/project"
            }
          },
          "urls": []
        }"#;
        assert_eq!(
            source_url_from_json(json).as_deref(),
            Some("https://gitlab.com/group/subgroup/project"),
        );
    }

    #[test]
    fn source_url_from_json_strips_github_subpaths() {
        let json = r#"{
          "info": {
            "name": "x", "version": "1",
            "project_urls": {
              "Source": "https://github.com/pallets/markupsafe/tree/main"
            }
          },
          "urls": []
        }"#;
        assert_eq!(
            source_url_from_json(json).as_deref(),
            Some("https://github.com/pallets/markupsafe"),
        );
    }

    #[test]
    fn parse_version_only_ignores_the_urls_array_entirely() {
        // Even if the urls array is completely absent (malformed response),
        // info.version is still extractable. Belt-and-braces: the whole point
        // is that this path never touches urls.
        let json = r#"{"info":{"name":"foo","version":"1.2.3"}}"#;
        assert_eq!(parse_version_only(json).unwrap(), "1.2.3");
    }

    /// Live PyPI check of the wheel lookup `--nvidia-redist` and the wheel switch
    /// use: public GETs only. Run with `cargo test -- --ignored`.
    #[test]
    #[ignore = "network: GETs pypi.org"]
    fn live_pypi_wheel_lookup() {
        // A wheel-only NVIDIA release: no sdist, one manylinux wheel per arch.
        let json = get_json("https://pypi.org/pypi/nvidia-nvjitlink/json").unwrap();
        let wheel = pick_wheel_from_json(&json, "x86_64", None).unwrap();
        assert!(
            wheel.filename.ends_with(".whl") && wheel.filename.contains("x86_64"),
            "{}",
            wheel.filename
        );
        assert_eq!(wheel.sha256.len(), 64);
        assert!(
            wheel.url.starts_with("https://files.pythonhosted.org/"),
            "{}",
            wheel.url
        );
        assert!(pypi_name_and_version_from_wheel_filename(&wheel.filename).is_some());
        let aarch = pick_wheel_from_json(&json, "aarch64", None).unwrap();
        assert!(aarch.filename.contains("aarch64"), "{}", aarch.filename);
        // A CPython-locked family: one wheel per flavor.
        let tri = get_json("https://pypi.org/pypi/nvidia-cutlass-dsl-libs-cu13/json").unwrap();
        let abis = vec!["cp313".to_string(), "cp314".to_string()];
        let per = pick_wheels_per_flavor(&tri, "x86_64", &abis).unwrap();
        assert!(per[0].1.filename.contains("cp313") && per[1].1.filename.contains("cp314"));
        // The release endpoints.
        let release = fetch_latest_release("flask").unwrap();
        assert!(!release.version.is_empty() && !release.sdist.url.is_empty());
        assert!(fetch_latest_version("nvidia-nvjitlink").is_ok());
        assert!(
            fetch_latest_release("this-package-does-not-exist-ap-r3a")
                .unwrap_err()
                .is_not_found()
        );
    }
}
