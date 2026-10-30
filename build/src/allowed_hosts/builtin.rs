//! The hosts built into the tool. They cannot be removed; a user allows more
//! through the other origins (see [`super::Origin`]).

use super::Use;
use crate::discovery::host_is_under;

/// Hosts the services fetch from (sandboxed `_service` runs only).
pub(super) const SERVICE_HOSTS: &[&str] = &[
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

/// Package indexes and release hosts the agent may name, besides the forges.
pub(super) const TRUSTED_SOURCE_HOSTS: &[&str] = &[
    "pypi.org",
    "files.pythonhosted.org",
    "crates.io",
    "static.crates.io",
    "rubygems.org",
    "registry.npmjs.org",
    "metacpan.org",
    "hackage.haskell.org",
    "ftp.gnu.org",
    "download.gnu.org",
    "download.savannah.gnu.org",
    "sourceforge.net",
    "downloads.sourceforge.net",
    "kernel.org",
    "download.kde.org",
    "download.gnome.org",
];

/// Whether a built-in entry allows `host` (already normalized) for `use_`.
/// Subdomains of an entry are accepted too; the fixed forges are on both lists.
pub(super) fn allows(host: &str, use_: Use) -> bool {
    let lists: &[&[&str]] = match use_ {
        Use::Service => &[SERVICE_HOSTS, TRUSTED_SOURCE_HOSTS],
        Use::Source => &[TRUSTED_SOURCE_HOSTS],
    };
    lists
        .iter()
        .flat_map(|l| l.iter())
        .any(|d| host_is_under(host, d))
        || ap_ecosystem::forge::is_fixed_forge_host(host)
}

/// Every built-in host name, service and source lists and forges, once each.
pub(super) fn all() -> impl Iterator<Item = &'static str> {
    let mut names: Vec<&'static str> = SERVICE_HOSTS
        .iter()
        .chain(TRUSTED_SOURCE_HOSTS)
        .chain(ap_ecosystem::forge::FIXED_FORGE_HOSTS)
        .copied()
        .collect();
    names.sort_unstable();
    names.dedup();
    names.into_iter()
}
