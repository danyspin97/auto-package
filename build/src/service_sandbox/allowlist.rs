use crate::allowed_hosts::{self, AllowedHosts, Asker, Use};

/// The switch that runs services unsandboxed, as before the sandbox.
pub(crate) const UNSAFE_ENV: &str = "AUTO_PACKAGE_UNSAFE_SERVICE";

/// Where osc finds the service helpers.
pub(crate) const SERVICE_DIR: &str = "/usr/lib/obs/service";

/// The interpreter running the loopback bridge; osc itself needs it.
pub(crate) const PYTHON: &str = "/usr/bin/python3";

/// util-linux `setpriv`, which starts each service without capabilities.
pub(crate) const SETPRIV: &str = "/usr/bin/setpriv";

/// Where the proxy's socket directory appears inside the sandbox.
pub(super) const SANDBOX_PROXY_DIR: &str = "/run/ap-proxy";

pub(super) const SOCKET_NAME: &str = "proxy.sock";

/// The bridge's port on the sandbox's own loopback interface.
pub(super) const PROXY_PORT: u16 = 3128;

/// The empty home directory inside the sandbox (on its private `/tmp`).
pub(super) const SANDBOX_HOME: &str = "/tmp/home";

/// Any non-empty value: tar_scm clones into the working directory under osc.
pub(super) const OSC_VERSION: &str = "1.0.0";

// ── the allowlist ───────────────────────────────────────────────────────────

/// Whether a service may connect to `host:port`: port 443 on a host the
/// shared allowlist allows for services. Returns the normalized host.
pub(crate) fn check_target(
    host: &str,
    port: u16,
    hosts: &AllowedHosts,
    asker: &Asker,
) -> Result<String, String> {
    if port != 443 {
        return Err(format!("port {port} is not allowed, only 443"));
    }
    hosts.check_host(host, Use::Service, asker)
}

/// `host[:port]` split; `default` is the port when none is given (`None`:
/// a port is required). Bracketed IPv6 literals are refused.
pub(super) fn split_host_port(
    authority: &str,
    default: Option<u16>,
) -> Result<(&str, u16), String> {
    if authority.starts_with('[') {
        return Err(format!("`{authority}` is an IP address"));
    }
    match (authority.rsplit_once(':'), default) {
        (Some((host, port)), _) => {
            let port = port
                .parse::<u16>()
                .map_err(|_| format!("`{authority}` has a bad port"))?;
            Ok((host, port))
        }
        (None, Some(port)) => Ok((authority, port)),
        (None, None) => Err(format!("`{authority}` has no port")),
    }
}

/// Check a `url` service parameter: https only, no user info, an allowed host
/// on port 443. A refused host is recorded for `asker`.
pub(crate) fn check_service_url(url: &str, asker: &Asker) -> Result<(), String> {
    check_service_url_in(url, &allowed_hosts::shared(), asker)
}

pub(super) fn check_service_url_in(
    url: &str,
    hosts: &AllowedHosts,
    asker: &Asker,
) -> Result<(), String> {
    service_url_host_in(url, hosts, asker).map(|_| ())
}

/// The normalized host of a service `url` that [`check_service_url`] allows.
pub(super) fn service_url_host_in(
    url: &str,
    hosts: &AllowedHosts,
    asker: &Asker,
) -> Result<String, String> {
    if url.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return Err(format!("url `{url}` contains whitespace"));
    }
    let lower = url.to_ascii_lowercase();
    if let Some((scheme, _)) = lower.split_once("://")
        && scheme != "https"
    {
        return Err(format!("url `{url}` uses {scheme}://, it must be https://"));
    }
    if !lower.starts_with("https://") {
        return Err(format!("url `{url}` must be https://"));
    }
    let rest = &url["https://".len()..];
    let end = rest.find(['/', '?', '#', '\\']).unwrap_or(rest.len());
    let authority = &rest[..end];
    if authority.contains('@') {
        return Err(format!("url `{url}` carries user info"));
    }
    let (host, port) =
        split_host_port(authority, Some(443)).map_err(|e| format!("url `{url}`: {e}"))?;
    check_target(host, port, hosts, asker).map_err(|e| format!("url `{url}`: {e}"))
}
