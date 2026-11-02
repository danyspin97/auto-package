//! A build that fetches its dependencies over the network. OBS builds run with no
//! network, so a build that reaches for a git or crate host fails the same way
//! every time, and no spec edit made by an agent changes that.

/// The reason a package ends when its build fetches over the network.
pub(super) const NETWORK_FETCH_REASON: &str =
    "the build fetches dependencies from the network; needs a maintainer or a vendoring step";

/// Text that says the build is fetching something (a zig or cargo fetch, a git
/// dependency, a download).
const FETCH_MARKERS: &[&str] = &[
    "unable to discover remote git server",
    "failed to download",
    "failed to fetch",
    "zig fetch",
    "git clone",
    "cargo fetch",
    "go: downloading",
    "npm ERR!",
];

/// Text that says a network name or address could not be reached.
const NETWORK_MARKERS: &[&str] = &[
    "NameServerFailure",
    "Could not resolve host",
    "Couldn't resolve host",
    "Could not resolve hostname",
    "Temporary failure in name resolution",
    "Name or service not known",
    "failed to lookup address information",
    "no such host",
    "Network is unreachable",
    "EAI_AGAIN",
    "ENOTFOUND",
];

/// The first line of a build log that reports a network failure, when the same
/// log also shows the build fetching something. A failure line alone is not
/// enough: a test may print the same words.
pub(super) fn network_fetch_failure(log: &str) -> Option<String> {
    if !FETCH_MARKERS.iter().any(|m| log.contains(m)) {
        return None;
    }
    log.lines()
        .find(|line| NETWORK_MARKERS.iter().any(|m| line.contains(m)))
        .map(|line| line.trim().chars().take(300).collect())
}
