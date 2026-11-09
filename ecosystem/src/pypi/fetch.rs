use super::*;

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

pub(super) fn get_json(url: &str) -> Result<String, FetchError> {
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
pub(super) fn safe_filename(filename: &str) -> Option<&str> {
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
        self.fetch_as(dest_dir, &self.filename)
    }

    /// [`Sdist::fetch`], saved as `saved` in `dest_dir`: the same bytes, checked
    /// the same way, under a name the caller chose (a spec's `Source` may spell
    /// the file's name in another case than PyPI does).
    pub fn fetch_as(&self, dest_dir: &Path, saved: &str) -> Result<PathBuf, FetchError> {
        let Some(name) = safe_filename(saved) else {
            return Err(FetchError::Io {
                path: PathBuf::from(saved),
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
