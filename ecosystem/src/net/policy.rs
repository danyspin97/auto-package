use super::*;

/// Addresses a fetch must never connect to: loopback, private, link-local,
/// carrier-grade NAT, multicast and unspecified ones.
pub fn is_forbidden_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => forbidden_v4(v4),
        IpAddr::V6(v6) => forbidden_v6(v6),
    }
}

pub(super) fn forbidden_v4(ip: Ipv4Addr) -> bool {
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

pub(super) fn forbidden_v6(ip: Ipv6Addr) -> bool {
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
pub(super) fn is_forbidden_literal(host: &str) -> bool {
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
pub(super) struct LiteralIpGuard;

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
pub(super) struct PublicOnlyResolver {
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
