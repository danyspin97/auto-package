//! One place for outbound HTTP: every request gets a timeout and every body a
//! size cap. `ureq` has neither by default, so a stalled server or a
//! multi-gigabyte "archive" would hang the run or exhaust memory.

use std::sync::Mutex;
use std::time::Duration;

/// Agents built so far, by (timeout, redirect limit). See [`agent`].
static AGENTS: Mutex<Vec<(Duration, u32, ureq::Agent)>> = Mutex::new(Vec::new());

/// Total time allowed for a metadata request (JSON, HTML listing, HEAD).
pub const API_TIMEOUT: Duration = Duration::from_secs(60);
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

pub fn head(url: &str) -> ureq::RequestBuilder<ureq::typestate::WithoutBody> {
    agent(API_TIMEOUT, 5).head(url)
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

/// The body as bytes, refusing more than [`MAX_DOWNLOAD`].
pub fn read_download(resp: ureq::http::Response<ureq::Body>) -> Result<Vec<u8>, ureq::Error> {
    resp.into_body()
        .with_config()
        .limit(MAX_DOWNLOAD)
        .read_to_vec()
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
