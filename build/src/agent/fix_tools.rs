//! The web grants of a FixBug turn: the upstream forge and the hosts of the
//! distributions' patch collections, read only. A host the user has not
//! allowed is not granted, and nothing the turn says can allow one.

use crate::allowed_hosts::{AllowedHosts, Use};

/// The GitHub hosts: one forge, so they are granted together, and only while
/// the tool allows `github.com`.
const GITHUB_HOSTS: &[&str] = &["github.com", "raw.githubusercontent.com", "api.github.com"];

/// Where distributions keep the patches they apply.
pub(crate) const DISTRO_PATCH_HOSTS: &[&str] = &[
    "salsa.debian.org",
    "src.fedoraproject.org",
    "gitweb.gentoo.org",
    "lore.kernel.org",
];

/// `,WebFetch(domain:<host>)` for each host a FixBug turn may read, and no
/// other web access (no `WebSearch`; `pypi.org`, which every checkout turn has,
/// is the one grant not made here): every grant goes through
/// `AllowedHosts::allows(host, Use::Source)`, so the turn reaches what the user
/// allowed and what the tool allows without an entry. With an empty
/// allowed-hosts file that is `github.com` (and `raw.githubusercontent.com` and
/// `api.github.com`, granted with it), `salsa.debian.org` (a fixed forge) and
/// `lore.kernel.org` (under the built-in `kernel.org`), and a fixed forge among
/// `fetch_hosts` (the upstream repository's and the `--commit-url`s': GitLab,
/// Codeberg, Bitbucket, sourcehut). `src.fedoraproject.org`, `gitweb.gentoo.org`,
/// a self-hosted forge and any other `--commit-url` host need an entry
/// (`auto-package hosts allow`, `--allow-host`); the turn cannot add one.
pub(super) fn web_grants(hosts: &AllowedHosts, fetch_hosts: &[String]) -> String {
    let github = hosts.allows("github.com", Use::Source);
    let mut granted: Vec<String> = Vec::new();
    for h in GITHUB_HOSTS
        .iter()
        .filter(|_| github)
        .chain(DISTRO_PATCH_HOSTS)
        .map(|h| h.to_string())
        .chain(fetch_hosts.iter().map(|h| h.to_ascii_lowercase()))
    {
        let sibling = GITHUB_HOSTS.contains(&h.as_str());
        if !granted.contains(&h) && (hosts.allows(&h, Use::Source) || (sibling && github)) {
            granted.push(h);
        }
    }
    granted
        .iter()
        .map(|h| format!(",WebFetch(domain:{h})"))
        .collect()
}
