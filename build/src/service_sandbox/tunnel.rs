use super::*;
use crate::osc::SOURCE_PARAMS;

/// The most hosts one sandbox maps: 127.0.0.2 up to 127.0.0.251.
const MAX_MAPPED_HOSTS: usize = 250;

/// The normalized host of each URL-valued parameter of a service: `url`, or a
/// [`SOURCE_PARAMS`] value with a `:`. Each one is checked as
/// [`check_service_url`] checks it, against `allowed` as it is now: a host
/// allowed since the last run is approved here. A refused host is recorded
/// for `asker`. The error names the URL.
pub(crate) fn source_hosts(
    params: &[(String, String)],
    allowed: &AllowedHosts,
    asker: &Asker,
) -> Result<Vec<String>, String> {
    params
        .iter()
        .filter(|(name, value)| {
            name == "url" || (SOURCE_PARAMS.contains(&name.as_str()) && value.trim().contains(':'))
        })
        .map(|(_, value)| service_url_host_in(value.trim(), allowed, asker))
        .collect()
}

/// The loopback address each of `names` gets inside the sandbox, from
/// 127.0.0.2 up, in name order: `(address, name)`. Each name must be allowed
/// on port 443 by `allowed`, as [`check_target`] says.
pub(crate) fn bridge_addresses(
    names: &[String],
    allowed: &AllowedHosts,
    asker: &Asker,
) -> Result<Vec<(String, String)>, String> {
    let names: std::collections::BTreeSet<String> = names
        .iter()
        .map(|h| check_target(h, 443, allowed, asker))
        .collect::<Result<_, _>>()?;
    if names.len() > MAX_MAPPED_HOSTS {
        return Err(format!(
            "{} source hosts, the sandbox has addresses for {MAX_MAPPED_HOSTS}",
            names.len()
        ));
    }
    Ok(names
        .into_iter()
        .enumerate()
        .map(|(i, name)| (format!("127.0.0.{}", i + 2), name))
        .collect())
}

/// The first lines of the sandbox's `/etc/hosts`, so `localhost` resolves as
/// it does on the host. A connection to 127.0.0.1:443 reaches the bridge,
/// which sends it as `CONNECT 127.0.0.1:443`, and the proxy refuses that.
const LOCALHOST_LINES: &str = "127.0.0.1 localhost\n::1 localhost\n";

/// The sandbox's `/etc/hosts` for an address map from [`bridge_addresses`]:
/// the `localhost` lines, then one line per mapped name.
pub(crate) fn hosts_file_text(map: &[(String, String)]) -> String {
    let mapped: String = map
        .iter()
        .map(|(ip, name)| format!("{ip} {name}\n"))
        .collect();
    format!("{LOCALHOST_LINES}{mapped}")
}
