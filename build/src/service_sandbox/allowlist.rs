use super::*;

/// Hosts the services fetch from, besides the trusted source hosts of
/// [`crate::discovery`], the fixed forges and `AUTO_PACKAGE_SOURCE_HOSTS`.
/// Subdomains of an entry are accepted too, as there.
pub(crate) const SERVICE_HOSTS: &[&str] = &[
    "github.com",
    "codeload.github.com",
    "api.github.com",
    "objects.githubusercontent.com",
    // GitHub release downloads redirect here since 2025.
    "release-assets.githubusercontent.com",
    "gitlab.com",
    "gitlab.freedesktop.org",
    "gitlab.gnome.org",
    "salsa.debian.org",
    "bitbucket.org",
    "sourceforge.net",
    "downloads.sourceforge.net",
    "pypi.org",
    "files.pythonhosted.org",
    "crates.io",
    "static.crates.io",
    "index.crates.io",
    "registry.npmjs.org",
    "proxy.golang.org",
    "sum.golang.org",
];

/// The switch that runs services unsandboxed, as before the sandbox.
pub(crate) const UNSAFE_ENV: &str = "AUTO_PACKAGE_UNSAFE_SERVICE";

/// Where osc finds the service helpers.
pub(super) const SERVICE_DIR: &str = "/usr/lib/obs/service";

/// The interpreter running the loopback bridge; osc itself needs it.
pub(super) const PYTHON: &str = "/usr/bin/python3";

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

/// `raw` as a host name to compare: lowercase, without one trailing dot.
/// IP literals, numeric forms a resolver may read as an address, non-ASCII
/// (unencoded IDN) names and malformed labels are refused.
pub(crate) fn normalize_host(raw: &str) -> Result<String, String> {
    if !raw.is_ascii() {
        return Err(format!("host `{raw}` is not ASCII"));
    }
    let lower = raw.to_ascii_lowercase();
    let host = lower.strip_suffix('.').unwrap_or(&lower);
    if host.starts_with('[') || host.parse::<IpAddr>().is_ok() {
        return Err(format!("host `{raw}` is an IP address"));
    }
    if host.is_empty() || host.len() > 253 {
        return Err(format!("host `{raw}` is not a valid name"));
    }
    for label in host.split('.') {
        if label.is_empty()
            || label.len() > 63
            || label.starts_with('-')
            || label.ends_with('-')
            || !label
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-')
        {
            return Err(format!("host `{raw}` is not a valid name"));
        }
    }
    // No real name ends in a numeric label (`2130706433`, `0x7f.1`).
    let last = host.rsplit('.').next().unwrap_or("");
    if last.bytes().all(|b| b.is_ascii_digit()) || last.starts_with("0x") {
        return Err(format!("host `{raw}` is an IP address"));
    }
    Ok(host.to_string())
}

/// Whether the normalized `host` is a service host, a trusted source host, a
/// fixed forge, or under one of `extra`.
pub(crate) fn host_allowed_in(host: &str, extra: &[String]) -> bool {
    SERVICE_HOSTS
        .iter()
        .chain(TRUSTED_SOURCE_HOSTS)
        .chain(ap_ecosystem::forge::FIXED_FORGE_HOSTS)
        .any(|d| host_is_under(host, d))
        || extra.iter().any(|d| host_is_under(host, d))
}

/// Whether a service may connect to `host:port`: port 443 on an allowed host
/// only. Returns the normalized host.
pub(crate) fn check_target(host: &str, port: u16, extra: &[String]) -> Result<String, String> {
    if port != 443 {
        return Err(format!("port {port} is not allowed, only 443"));
    }
    let host = normalize_host(host)?;
    if !host_allowed_in(&host, extra) {
        return Err(format!(
            "{host} is not an allowed source host; set AUTO_PACKAGE_SOURCE_HOSTS={host} to allow it"
        ));
    }
    Ok(host)
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
/// on port 443.
pub(crate) fn check_service_url(url: &str) -> Result<(), String> {
    check_service_url_in(url, &extra_source_hosts())
}

pub(super) fn check_service_url_in(url: &str, extra: &[String]) -> Result<(), String> {
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
    check_target(host, port, extra)
        .map(|_| ())
        .map_err(|e| format!("url `{url}`: {e}"))
}
