//! One place for outbound HTTP: every request gets a timeout and every body a
//! size cap. `ureq` has neither by default, so a stalled server or a
//! multi-gigabyte "archive" would hang the run or exhaust memory.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

use sha2::{Digest, Sha256};

/// Agents built so far, by (timeout, redirect limit). See [`agent`].
static AGENTS: Mutex<Vec<(Duration, u32, ureq::Agent)>> = Mutex::new(Vec::new());

/// Total time allowed for a metadata request (JSON, HTML listing).
pub const API_TIMEOUT: Duration = Duration::from_secs(60);
/// Total time allowed for a HEAD probe.
pub const HEAD_TIMEOUT: Duration = Duration::from_secs(10);
/// Total time allowed for downloading a source archive.
pub const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(600);
/// Largest metadata body we will read.
pub const MAX_API_BODY: u64 = 32 * 1024 * 1024;
/// Largest archive we will download into memory.
pub const MAX_DOWNLOAD: u64 = 1024 * 1024 * 1024;

/// An agent with a global timeout and the given redirect limit. Statuses
/// 4xx/5xx are still errors; with `redirects == 0` a 3xx comes back as a
/// plain response for the caller to inspect.
///
/// One agent is built per (timeout, redirects) pair and shared after that:
/// agents hold the connection pool, so a fresh one per request would reopen
/// a TLS connection for every call.
pub fn agent(timeout: Duration, redirects: u32) -> ureq::Agent {
    let mut agents = AGENTS.lock().unwrap_or_else(|e| e.into_inner());
    if let Some((_, _, agent)) = agents
        .iter()
        .find(|(t, r, _)| *t == timeout && *r == redirects)
    {
        return agent.clone();
    }
    let agent = ureq::Agent::new_with_config(
        ureq::Agent::config_builder()
            .timeout_global(Some(timeout))
            .max_redirects(redirects)
            .max_redirects_will_error(false)
            .build(),
    );
    agents.push((timeout, redirects, agent.clone()));
    agent
}

pub fn get(url: &str) -> ureq::RequestBuilder<ureq::typestate::WithoutBody> {
    agent(API_TIMEOUT, 5).get(url)
}

/// A HEAD request is an existence check, not a fetch: it gets a short limit
/// so a dead host cannot stall a candidate list for a minute each.
pub fn head(url: &str) -> ureq::RequestBuilder<ureq::typestate::WithoutBody> {
    agent(HEAD_TIMEOUT, 5).head(url)
}

pub fn get_download(url: &str) -> ureq::RequestBuilder<ureq::typestate::WithoutBody> {
    agent(DOWNLOAD_TIMEOUT, 5).get(url)
}

/// The body as text, refusing more than [`MAX_API_BODY`].
pub fn read_text(resp: ureq::http::Response<ureq::Body>) -> Result<String, ureq::Error> {
    resp.into_body()
        .with_config()
        .limit(MAX_API_BODY)
        .read_to_string()
}

/// Why a download produced no file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FetchFailure {
    /// HTTP 404: this URL is not the archive. Not an error for the caller to
    /// report; it just moves on to the next candidate.
    NotFound,
    /// HTTP 429: the host is refusing requests for now.
    RateLimited,
    /// The body is larger than [`MAX_DOWNLOAD`].
    TooLarge,
    /// Connection, timeout, other HTTP status, or local I/O.
    Transport(String),
}

impl std::fmt::Display for FetchFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FetchFailure::NotFound => write!(f, "not found (404)"),
            FetchFailure::RateLimited => write!(f, "rate limited (429)"),
            FetchFailure::TooLarge => {
                write!(f, "larger than the {} MiB limit", MAX_DOWNLOAD >> 20)
            }
            FetchFailure::Transport(e) => write!(f, "{e}"),
        }
    }
}

/// What a finished download left on disk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Download {
    pub bytes: u64,
    /// SHA-256 of the file, lowercase hex.
    pub sha256: String,
}

/// Stream `url` into `dest`, hashing as it goes, so the body is never held in
/// memory. The bytes go to a `.part` sibling that is renamed into place only
/// when complete. A transport failure is retried once: a GET is safe to
/// repeat and a dropped connection is common. Other failures are answers
/// and are returned as they are.
pub fn download_file(url: &str, dest: &Path) -> Result<Download, FetchFailure> {
    match download_once(url, dest) {
        Err(FetchFailure::Transport(_)) => download_once(url, dest),
        other => other,
    }
}

fn download_once(url: &str, dest: &Path) -> Result<Download, FetchFailure> {
    let resp = match get_download(url).call() {
        Ok(resp) => resp,
        Err(ureq::Error::StatusCode(404)) => return Err(FetchFailure::NotFound),
        Err(ureq::Error::StatusCode(429)) => return Err(FetchFailure::RateLimited),
        Err(e) => return Err(FetchFailure::Transport(e.to_string())),
    };
    let part = PathBuf::from(format!("{}.part", dest.display()));
    let local = |e: std::io::Error| FetchFailure::Transport(format!("{}: {e}", part.display()));
    let mut file = std::fs::File::create(&part).map_err(local)?;
    let mut body = resp.into_body();
    let reader = body.with_config().limit(MAX_DOWNLOAD).reader();
    let streamed = stream_to(reader, &mut file);
    let streamed = streamed.and_then(|d| {
        file.flush().map_err(local)?;
        if d.bytes == 0 {
            return Err(FetchFailure::Transport("empty response body".into()));
        }
        Ok(d)
    });
    let download = match streamed {
        Ok(d) => d,
        Err(e) => {
            let _ = std::fs::remove_file(&part);
            return Err(e);
        }
    };
    std::fs::rename(&part, dest).map_err(|e| {
        let _ = std::fs::remove_file(&part);
        FetchFailure::Transport(format!("{}: {e}", dest.display()))
    })?;
    Ok(download)
}

/// Copy `reader` into `file`, hashing it. Maps ureq's size-limit error to
/// [`FetchFailure::TooLarge`].
fn stream_to(mut reader: impl Read, file: &mut std::fs::File) -> Result<Download, FetchFailure> {
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 64 * 1024];
    let mut bytes = 0u64;
    loop {
        let n = match reader.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) => {
                return Err(
                    match e
                        .get_ref()
                        .and_then(|inner| inner.downcast_ref::<ureq::Error>())
                    {
                        Some(ureq::Error::BodyExceedsLimit(_)) => FetchFailure::TooLarge,
                        _ => FetchFailure::Transport(format!("reading download: {e}")),
                    },
                );
            }
        };
        hasher.update(&buf[..n]);
        file.write_all(&buf[..n])
            .map_err(|e| FetchFailure::Transport(format!("writing download: {e}")))?;
        bytes += n as u64;
    }
    Ok(Download {
        bytes,
        sha256: hex::encode(hasher.finalize()),
    })
}

/// `host` of an `http(s)://` URL, lowercased, without userinfo or port.
pub fn host_of(url: &str) -> Option<String> {
    let rest = url.split_once("://")?.1;
    let authority = rest.split(['/', '?', '#']).next()?;
    let host = authority.rsplit('@').next()?;
    let host = host.split(':').next()?;
    (!host.is_empty()).then(|| host.to_ascii_lowercase())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stream_to_hashes_what_it_copies() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("body");
        let mut file = std::fs::File::create(&path).unwrap();
        let d = stream_to(&b"abc"[..], &mut file).unwrap();
        assert_eq!(d.bytes, 3);
        assert_eq!(
            d.sha256,
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(std::fs::read(&path).unwrap(), b"abc");
    }

    #[test]
    fn stream_to_reports_the_size_cap_as_too_large() {
        struct Capped;
        impl Read for Capped {
            fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
                Err(std::io::Error::other(ureq::Error::BodyExceedsLimit(
                    MAX_DOWNLOAD,
                )))
            }
        }
        let dir = tempfile::tempdir().unwrap();
        let mut file = std::fs::File::create(dir.path().join("body")).unwrap();
        assert_eq!(
            stream_to(Capped, &mut file).unwrap_err(),
            FetchFailure::TooLarge
        );
    }

    #[test]
    fn host_of_ignores_userinfo_port_and_case() {
        assert_eq!(
            host_of("https://API.GitHub.com/x").as_deref(),
            Some("api.github.com")
        );
        assert_eq!(
            host_of("https://u:p@gitlab.com:8443/a/b").as_deref(),
            Some("gitlab.com")
        );
        assert_eq!(
            host_of("https://evil.com/api.github.com/x").as_deref(),
            Some("evil.com")
        );
        assert_eq!(host_of("not a url"), None);
    }

    #[test]
    fn agents_are_built_once_per_timeout_and_redirect_limit() {
        // An agent owns its connection pool; asking again for the same
        // settings must not build (and pool) another one.
        let timeout = Duration::from_secs(7_331);
        agent(timeout, 5);
        let built = AGENTS.lock().unwrap().len();
        agent(timeout, 5);
        assert_eq!(AGENTS.lock().unwrap().len(), built);
        agent(timeout, 0);
        assert_eq!(AGENTS.lock().unwrap().len(), built + 1);
    }
}
