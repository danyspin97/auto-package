use super::*;

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
