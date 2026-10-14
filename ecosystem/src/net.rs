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
    fn a_loopback_target_is_refused_and_http_is_too() {
        let a = agent(ureq::Agent::config_builder().proxy(None));
        let err = a.get("https://127.0.0.1:1/").call().unwrap_err();
        assert!(err.to_string().contains("private or local"), "{err}");
        assert!(a.get("http://example.com/").call().is_err());
    }
}
