//! Outbound HTTP agents that only reach public hosts over https.
//!
//! The URLs fetched come from upstream metadata, so a redirect or a DNS answer
//! could point at the loopback interface, the local network or a cloud
//! metadata endpoint. The check sits in the resolver, which ureq consults for
//! every connection including each redirect hop, and it applies to the address
//! actually connected to, so a name that resolves differently later is covered.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

use ureq::config::Config;
use ureq::unversioned::resolver::{DefaultResolver, ResolvedSocketAddrs, Resolver};
use ureq::unversioned::transport::{DefaultConnector, NextTimeout};

/// Addresses a fetch must never connect to: loopback, private, link-local,
/// carrier-grade NAT, multicast and unspecified ones.
pub fn is_forbidden_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => forbidden_v4(v4),
        IpAddr::V6(v6) => forbidden_v6(v6),
    }
}

fn forbidden_v4(ip: Ipv4Addr) -> bool {
    let o = ip.octets();
    ip.is_loopback()
        || ip.is_private()
        || ip.is_link_local()
        || ip.is_unspecified()
        || ip.is_broadcast()
        || ip.is_multicast()
        // 100.64.0.0/10, carrier-grade NAT
        || (o[0] == 100 && (o[1] & 0xc0) == 64)
        // 0.0.0.0/8
        || o[0] == 0
}

fn forbidden_v6(ip: Ipv6Addr) -> bool {
    if let Some(v4) = ip.to_ipv4_mapped() {
        return forbidden_v4(v4);
    }
    let seg = ip.segments();
    ip.is_loopback()
        || ip.is_unspecified()
        || ip.is_multicast()
        // fe80::/10 link-local, fc00::/7 unique local
        || (seg[0] & 0xffc0) == 0xfe80
        || (seg[0] & 0xfe00) == 0xfc00
}

#[derive(Debug, Default)]
struct PublicOnlyResolver {
    inner: DefaultResolver,
}

impl Resolver for PublicOnlyResolver {
    fn resolve(
        &self,
        uri: &ureq::http::Uri,
        config: &Config,
        timeout: NextTimeout,
    ) -> Result<ResolvedSocketAddrs, ureq::Error> {
        let resolved = self.inner.resolve(uri, config, timeout)?;
        // Through a proxy the address resolved here is the proxy's, which an
        // operator chose and which may well be on the local network.
        if config.proxy().is_some() {
            return Ok(resolved);
        }
        let mut allowed = self.empty();
        for addr in resolved.iter().filter(|a| !is_forbidden_ip(a.ip())) {
            allowed.push(*addr);
        }
        if allowed.is_empty() {
            return Err(ureq::Error::Io(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                format!("{uri}: resolves only to private or local addresses"),
            )));
        }
        Ok(allowed)
    }
}

/// An agent for `config` that speaks https only (redirects included) and
/// refuses private and local addresses. `config` should come from
/// `ureq::Agent::config_builder()` so proxy settings are read from the
/// environment.
pub fn agent(config: ureq::config::ConfigBuilder<ureq::typestate::AgentScope>) -> ureq::Agent {
    ureq::Agent::with_parts(
        config.https_only(true).build(),
        DefaultConnector::default(),
        PublicOnlyResolver::default(),
    )
}

/// Requests made for one call: the first try plus two retries.
pub const ATTEMPTS: u32 = 3;
/// Longest pause taken between attempts, whatever the server asks for.
const MAX_RETRY_WAIT: std::time::Duration = std::time::Duration::from_secs(30);

/// Whether a failed request is worth repeating: a dropped connection, a
/// timeout or a failed lookup. A refused private target, a bad URL, an
/// oversized body or a TLS fault would fail the same way again.
fn is_transient(err: &ureq::Error) -> bool {
    match err {
        ureq::Error::Io(e) => e.kind() != std::io::ErrorKind::PermissionDenied,
        ureq::Error::Timeout(_) | ureq::Error::ConnectionFailed | ureq::Error::HostNotFound => true,
        _ => false,
    }
}

/// The pause before attempt `attempt + 1` (`attempt` counts from 1): the
/// server's `Retry-After` in seconds when it sent one, else 1s, 2s, ...
/// Capped at [`MAX_RETRY_WAIT`].
fn retry_wait(retry_after: Option<&str>, attempt: u32) -> std::time::Duration {
    let asked = retry_after
        .and_then(|v| v.trim().parse::<u64>().ok())
        .map(std::time::Duration::from_secs);
    asked
        .unwrap_or_else(|| std::time::Duration::from_secs(1 << (attempt - 1).min(5)))
        .min(MAX_RETRY_WAIT)
}

/// Send the request `build` makes, up to [`ATTEMPTS`] times. Transport
/// errors and 429/5xx answers are repeated after a pause; any other answer
/// is final. A final 4xx/5xx comes back as `ureq::Error::StatusCode`, as it
/// does without this wrapper. Only for requests that are safe to repeat.
pub fn call_with_retries(
    mut build: impl FnMut() -> ureq::RequestBuilder<ureq::typestate::WithoutBody>,
) -> Result<ureq::http::Response<ureq::Body>, ureq::Error> {
    let mut attempt = 1;
    loop {
        // Status errors are switched off so the headers of a 429 stay readable.
        let result = build().config().http_status_as_error(false).build().call();
        let wait = match &result {
            Ok(resp) => {
                let status = resp.status().as_u16();
                if status < 400 {
                    return result;
                }
                if (status != 429 && status < 500) || attempt >= ATTEMPTS {
                    return Err(ureq::Error::StatusCode(status));
                }
                let after = resp
                    .headers()
                    .get("retry-after")
                    .and_then(|v| v.to_str().ok());
                retry_wait(after, attempt)
            }
            Err(e) if is_transient(e) && attempt < ATTEMPTS => retry_wait(None, attempt),
            Err(_) => return result,
        };
        std::thread::sleep(wait);
        attempt += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn private_and_local_addresses_are_forbidden() {
        for ip in [
            "127.0.0.1",
            "10.1.2.3",
            "172.16.0.1",
            "192.168.1.1",
            "169.254.169.254",
            "100.64.0.1",
            "0.0.0.0",
            "::1",
            "fe80::1",
            "fd00::1",
            "::ffff:127.0.0.1",
            "::ffff:169.254.169.254",
        ] {
            assert!(is_forbidden_ip(ip.parse().unwrap()), "{ip}");
        }
        for ip in ["1.1.1.1", "140.82.112.5", "2606:4700::1111"] {
            assert!(!is_forbidden_ip(ip.parse().unwrap()), "{ip}");
        }
    }

    #[test]
    fn retry_wait_honours_retry_after_up_to_the_cap() {
        use std::time::Duration;
        assert_eq!(retry_wait(None, 1), Duration::from_secs(1));
        assert_eq!(retry_wait(None, 2), Duration::from_secs(2));
        assert_eq!(retry_wait(Some("5"), 1), Duration::from_secs(5));
        assert_eq!(retry_wait(Some("86400"), 1), MAX_RETRY_WAIT);
        // An HTTP-date is not understood; fall back to the backoff.
        assert_eq!(
            retry_wait(Some("Wed, 21 Oct 2026 07:28:00 GMT"), 2),
            Duration::from_secs(2)
        );
    }

    #[test]
    fn only_transport_faults_are_transient() {
        assert!(is_transient(&ureq::Error::HostNotFound));
        assert!(!is_transient(&ureq::Error::StatusCode(500)));
        assert!(!is_transient(&ureq::Error::Io(std::io::Error::from(
            std::io::ErrorKind::PermissionDenied
        ))));
    }

    #[test]
    fn a_loopback_target_is_refused_and_http_is_too() {
        let a = agent(ureq::Agent::config_builder().proxy(None));
        let err = a.get("https://127.0.0.1:1/").call().unwrap_err();
        assert!(err.to_string().contains("private or local"), "{err}");
        assert!(a.get("http://example.com/").call().is_err());
    }
}
