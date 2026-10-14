//! Outbound HTTP agents that only reach public hosts over https.
//!
//! The URLs fetched come from upstream metadata, so a redirect or a DNS answer
//! could point at the loopback interface, the local network or a cloud
//! metadata endpoint. The check sits in the resolver, which ureq consults for
//! every connection including each redirect hop, and it applies to the address
//! actually connected to, so a name that resolves differently later is covered.
//!
//! Behind a proxy the proxy resolves the target, so only literal IP targets
//! can be refused (see [`LiteralIpGuard`]); a name that resolves to a private
//! address is the proxy's to deal with.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

use ureq::config::Config;
use ureq::unversioned::resolver::{DefaultResolver, ResolvedSocketAddrs, Resolver};
use ureq::unversioned::transport::{
    ConnectionDetails, Connector, DefaultConnector, NextTimeout, Transport,
};

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
        // 240.0.0.0/4, reserved
        || o[0] >= 240
        // 198.18.0.0/15, benchmarking
        || (o[0] == 198 && (o[1] & 0xfe) == 18)
        // 192.0.0.0/24, IETF protocol assignments
        || (o[0] == 192 && o[1] == 0 && o[2] == 0)
}

fn forbidden_v6(ip: Ipv6Addr) -> bool {
    if let Some(v4) = ip.to_ipv4_mapped() {
        return forbidden_v4(v4);
    }
    let seg = ip.segments();
    // The v4 address carried in the low 32 bits of the translation forms.
    let low = Ipv4Addr::new(
        (seg[6] >> 8) as u8,
        seg[6] as u8,
        (seg[7] >> 8) as u8,
        seg[7] as u8,
    );
    // 64:ff9b::/96, NAT64
    if seg[..6] == [0x64, 0xff9b, 0, 0, 0, 0] {
        return forbidden_v4(low);
    }
    // 2002::/16, 6to4: the v4 address is in segments 1 and 2
    if seg[0] == 0x2002 {
        let embedded = Ipv4Addr::new(
            (seg[1] >> 8) as u8,
            seg[1] as u8,
            (seg[2] >> 8) as u8,
            seg[2] as u8,
        );
        return forbidden_v4(embedded);
    }
    // ::a.b.c.d, the deprecated IPv4-compatible form (::1 is loopback, below)
    if seg[..6] == [0; 6] && !ip.is_loopback() && !ip.is_unspecified() {
        return forbidden_v4(low);
    }
    ip.is_loopback()
        || ip.is_unspecified()
        || ip.is_multicast()
        // fe80::/10 link-local, fc00::/7 unique local
        || (seg[0] & 0xffc0) == 0xfe80
        || (seg[0] & 0xfe00) == 0xfc00
}

/// Whether `host` (as in a URI, IPv6 in brackets) is an IP literal that is
/// forbidden, or a numeric form (`2130706433`, `0x7f.1`) that std does not
/// parse but a proxy's resolver might read as an address.
fn is_forbidden_literal(host: &str) -> bool {
    let bare = host.trim_start_matches('[').trim_end_matches(']');
    if let Ok(ip) = bare.parse::<IpAddr>() {
        return is_forbidden_ip(ip);
    }
    // No real name ends in a numeric label.
    let last = bare.rsplit('.').next().unwrap_or("");
    !last.is_empty()
        && (last.bytes().all(|b| b.is_ascii_digit()) || last.to_ascii_lowercase().starts_with("0x"))
}

/// Refuses requests whose target is a forbidden IP literal when a proxy is
/// configured. The proxy resolves the target, so [`PublicOnlyResolver`] never
/// sees it; without a proxy the resolver covers literals too.
#[derive(Debug)]
struct LiteralIpGuard;

impl<In: Transport> Connector<In> for LiteralIpGuard {
    type Out = In;

    fn connect(
        &self,
        details: &ConnectionDetails,
        chained: Option<In>,
    ) -> Result<Option<In>, ureq::Error> {
        if details.config.proxy().is_some()
            && let Some(host) = details.uri.host()
            && is_forbidden_literal(host)
        {
            return Err(ureq::Error::Io(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                format!("{}: private or local address", details.uri),
            )));
        }
        Ok(chained)
    }
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
        LiteralIpGuard.chain(DefaultConnector::default()),
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
            "240.0.0.1",
            "255.255.255.254",
            "198.18.0.1",
            "198.19.255.255",
            "192.0.0.8",
            "64:ff9b::7f00:1",
            "64:ff9b::a9fe:a9fe",
            "2002:7f00:1::",
            "2002:c0a8:101::1",
            "::127.0.0.1",
            "::10.0.0.1",
        ] {
            assert!(is_forbidden_ip(ip.parse().unwrap()), "{ip}");
        }
        for ip in [
            "1.1.1.1",
            "140.82.112.5",
            "2606:4700::1111",
            "198.20.0.1",
            "192.0.2.1",
            "64:ff9b::808:808",
            "2002:808:808::1",
        ] {
            assert!(!is_forbidden_ip(ip.parse().unwrap()), "{ip}");
        }
    }

    #[test]
    fn literal_hosts_are_judged_including_odd_numeric_forms() {
        for host in [
            "127.0.0.1",
            "[::1]",
            "[fd00::1]",
            "2130706433",
            "0x7f.1",
            "10.1",
        ] {
            assert!(is_forbidden_literal(host), "{host}");
        }
        for host in [
            "example.com",
            "1.1.1.1",
            "[2606:4700::1111]",
            "a1.example.org",
        ] {
            assert!(!is_forbidden_literal(host), "{host}");
        }
    }

    #[test]
    fn a_literal_private_target_is_refused_even_behind_a_proxy() {
        let proxy = ureq::Proxy::new("http://127.0.0.1:9").unwrap();
        let a = agent(ureq::Agent::config_builder().proxy(Some(proxy)));
        let err = a.get("https://169.254.169.254/").call().unwrap_err();
        assert!(err.to_string().contains("private or local"), "{err}");
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
