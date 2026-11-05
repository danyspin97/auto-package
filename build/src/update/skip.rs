//! The reasons `update` gives for a package it leaves out, with the text each
//! one prints.

use super::candidates::is_dated;
use super::*;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SkipReason {
    /// The package has sources and no spec file (not retried, nothing to read).
    NoSpec,
    /// OBS lists no files for the package.
    NoFiles,
    /// The package is an `_aggregate`: the text says which project holds it.
    Aggregate(String),
    /// OBS could not be asked or did not answer, after the retries.
    ReadFailed(String),
    /// OBS answered 404: the package is not there. Not a failed read.
    NotOnObs(String),
    /// The package is a `_link` whose target OBS no longer has: only the
    /// maintainer can decide what happens to it. The text says why.
    NeedsMaintainer(String),
    SpecHeaderMissing,
    /// An open submit request to the origin already brings the version.
    AlreadyRequested(AlreadyRequested),
    NoSourceLine,
    /// No ecosystem matched the `Source:` line.
    NotRecognized {
        source: String,
    },
    LookupFailed {
        ecosystem_tag: String,
        name: String,
        error: String,
    },
    /// Both versions were fetched but at least one is not valid PEP 440, so
    /// the comparator cannot decide which is newer. Previously this fell
    /// into `UpToDate`, which labelled a package `[current]` when the tool
    /// had actually failed to make the comparison at all — the Oct 6
    /// review's probe with a packaged `Version: %{package_version}` that
    /// never got expanded reproduced exactly this false-current label.
    VersionCompareFailed {
        packaged: String,
        latest: String,
    },
    /// The package follows a repository's head (`2+git20260727.241e0ff`), so
    /// the newest tag or release says nothing about whether it is current.
    Snapshot {
        packaged: String,
        latest: String,
    },
    /// The packaged `Version:` is computed by a shell macro (`%(…)`), so only
    /// the part before it is known, and upstream has the same there.
    ComputedVersion {
        packaged: String,
        known: String,
        latest: String,
    },
    /// The packaged `Version:` is computed by a script and upstream has a newer
    /// base than the one the spec shows: a maintainer decides the patch level.
    PatchLevelScript {
        packaged: String,
        known: String,
        latest: String,
    },
    /// The package is one series of a project that ships several side by side
    /// (`fmt-9` stays on 9.x), and the newest release is of another series.
    OtherSeries {
        series: String,
        packaged: String,
        latest: String,
    },
    /// Upstream's newest release has a `-` in it (`25.12.19-2026-02-06-03fffb2`,
    /// `1.3.12-pre3`), which an RPM `Version:` cannot hold.
    NotAnRpmVersion {
        latest: String,
    },
    /// Upstream's newest release is a pre-release (`1.3.12-pre3`, `2.0rc1`) and
    /// the package is on a stable one; a pre-release is never offered over it.
    Prerelease {
        packaged: String,
        latest: String,
    },
    /// `--to-version` asked for a version that cannot be fetched from where
    /// the package's source lives.
    VersionNotFound {
        version: String,
        why: String,
    },
    /// The name or URL a lookup would use still holds a macro after the
    /// spec's own defines are expanded, so the lookup is not made.
    UnexpandedMacro {
        lookup: String,
        text: String,
    },
    /// The package is SUSE-maintained and has no upstream release to follow:
    /// its SUSE forge repository has no release or tag, or its `Source:` has no
    /// URL and nothing fetches it. Report only.
    SuseNoUpstream {
        why: String,
    },
    /// A snapshot package whose forge repository has a release published after
    /// the snapshot was taken. Only a maintainer can move the package to it.
    SnapshotBehind {
        release: String,
        released: String,
        snapshot: String,
    },
    /// The branch project already holds a version newer than upstream's, so
    /// the update would replace it with an older one. Only a maintainer can say
    /// which one is right.
    BranchAhead {
        branch: String,
        upstream: String,
    },
    /// The package's own source and Repology disagree in the major or minor
    /// number, and Repology is the newer: the version is a maintainer's choice,
    /// so nothing is retargeted to either one.
    RepologyDisagrees {
        source: String,
        target: String,
        repology: String,
    },
    /// openSUSE:Factory has the package from another devel project than the
    /// source (`devel`), so the source's copy is not the one Factory takes:
    /// `update` leaves it out unless it is asked for with `--deprecated`.
    Deprecated {
        devel: String,
        source: String,
    },
}

impl SkipReason {
    /// The kind of the reason, as a short label: the variant's name. The plan
    /// line counts skips under it.
    pub fn kind(&self) -> &'static str {
        match self {
            SkipReason::NoSpec => "NoSpec",
            SkipReason::NoFiles => "NoFiles",
            SkipReason::Aggregate(_) => "Aggregate",
            SkipReason::ReadFailed(_) => "ReadFailed",
            SkipReason::NotOnObs(_) => "NotOnObs",
            SkipReason::NeedsMaintainer(_) => "NeedsMaintainer",
            SkipReason::SpecHeaderMissing => "SpecHeaderMissing",
            SkipReason::AlreadyRequested(_) => "AlreadyRequested",
            SkipReason::NoSourceLine => "NoSourceLine",
            SkipReason::NotRecognized { .. } => "NotRecognized",
            SkipReason::LookupFailed { .. } => "LookupFailed",
            SkipReason::VersionCompareFailed { .. } => "VersionCompareFailed",
            SkipReason::Snapshot { .. } => "Snapshot",
            SkipReason::ComputedVersion { .. } => "ComputedVersion",
            SkipReason::PatchLevelScript { .. } => "PatchLevelScript",
            SkipReason::OtherSeries { .. } => "OtherSeries",
            SkipReason::NotAnRpmVersion { .. } => "NotAnRpmVersion",
            SkipReason::Prerelease { .. } => "Prerelease",
            SkipReason::VersionNotFound { .. } => "VersionNotFound",
            SkipReason::UnexpandedMacro { .. } => "UnexpandedMacro",
            SkipReason::SuseNoUpstream { .. } => "SuseNoUpstream",
            SkipReason::SnapshotBehind { .. } => "SnapshotBehind",
            SkipReason::BranchAhead { .. } => "BranchAhead",
            SkipReason::RepologyDisagrees { .. } => "RepologyDisagrees",
            SkipReason::Deprecated { .. } => "Deprecated",
        }
    }
}

impl SkipReason {
    /// Whether a lookup failed in a way that may pass: the host answered with
    /// a rate limit or a 502, 503 or 504 (in the forms the lookups' errors take:
    /// `http status: 503`, `HTTP 503`, `503 Service Unavailable`), or timed out,
    /// or dropped the connection. Such a package is not checked, so the run is
    /// incomplete, rather than the package being skipped for good. A status
    /// number inside a URL is not a status.
    pub fn is_transient_lookup(&self) -> bool {
        let SkipReason::LookupFailed { error, .. } = self else {
            return false;
        };
        let error = error.to_ascii_lowercase();
        let status = [429u16, 502, 503, 504].iter().any(|code| {
            ["http status: ", "http error ", "http ", "status "]
                .iter()
                .any(|prefix| error.contains(&format!("{prefix}{code}")))
                || error.contains(&format!("{code} "))
                    && [
                        "too many requests",
                        "bad gateway",
                        "service unavailable",
                        "gateway timeout",
                    ]
                    .iter()
                    .any(|phrase| error.contains(phrase))
        });
        status
            || [
                "timed out",
                "timeout:",
                "connection reset",
                "temporarily unavailable",
            ]
            .iter()
            .any(|p| error.contains(p))
    }
}

impl std::fmt::Display for SkipReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SkipReason::AlreadyRequested(r) => r.fmt(f),
            SkipReason::NoSpec => write!(f, "the package has no spec file"),
            SkipReason::NoFiles => write!(f, "the package has no files on OBS"),
            SkipReason::Aggregate(report) => write!(f, "{report}"),
            SkipReason::ReadFailed(why) => write!(f, "the spec could not be read: {why}"),
            SkipReason::NotOnObs(why) => write!(f, "the package is not on OBS: {why}"),
            SkipReason::NeedsMaintainer(why) => write!(f, "needs the maintainer: {why}"),
            SkipReason::NotAnRpmVersion { latest } if is_dated(latest) => write!(
                f,
                "needs a maintainer: upstream's newest release ({latest}) is a date, which is \
                 not a valid RPM Version: a date cannot be compared with the package's version. \
                 A maintainer must choose the version to package and run update with \
                 --to-version (no '-', spaces or slashes)"
            ),
            SkipReason::NotAnRpmVersion { latest } => write!(
                f,
                "upstream's newest release ({latest}) is not a valid RPM Version: an RPM \
                 Version cannot hold a '-', so the package needs a maintainer. A maintainer \
                 must choose the version to package and run update with --to-version (no \
                 '-', spaces or slashes)"
            ),
            SkipReason::Prerelease { packaged, latest } => write!(
                f,
                "upstream's newest release ({latest}) is a pre-release and the package is on \
                 the stable {packaged}; no stable release is newer"
            ),
            SkipReason::VersionNotFound { version, why } => {
                write!(f, "version {version} was not found upstream: {why}")
            }
            SkipReason::UnexpandedMacro { lookup, text } => write!(
                f,
                "{lookup} lookup refused: {text:?} still holds a macro after the spec's \
                 defines are expanded"
            ),
            SkipReason::SpecHeaderMissing => write!(f, "spec has no Name:/Version:"),
            SkipReason::NoSourceLine => write!(f, "spec has no Source: line"),
            SkipReason::NotRecognized { source } => {
                write!(f, "no recognised ecosystem for source ({source})")
            }
            SkipReason::LookupFailed {
                ecosystem_tag,
                name,
                error,
            } => {
                write!(
                    f,
                    "{ecosystem_tag} lookup for {name} failed, so whether a newer version exists is unknown: \
                     {error}{}",
                    lookup_hint(error)
                )
            }
            SkipReason::Snapshot { packaged, latest } => write!(
                f,
                "packaged from a repository snapshot ({packaged}), which a release \
                 ({latest}) cannot say is current or not"
            ),
            SkipReason::PatchLevelScript {
                packaged,
                known,
                latest,
            } => write!(
                f,
                "needs a maintainer: patch-level script (the version {packaged} is computed \
                 by a script; upstream's newest {latest} is past the {known} it shows)"
            ),
            SkipReason::ComputedVersion {
                packaged,
                known,
                latest,
            } => write!(
                f,
                "the version is computed by a script ({packaged}), so only {known} of it is \
                 known, and upstream's {latest} starts the same"
            ),
            SkipReason::OtherSeries {
                series,
                packaged,
                latest,
            } => write!(
                f,
                "the package follows the {series} series ({packaged}), and upstream's \
                 newest ({latest}) is of another one"
            ),
            SkipReason::SuseNoUpstream { why } => {
                write!(f, "SUSE-maintained, no upstream release: {why}")
            }
            SkipReason::SnapshotBehind {
                release,
                released,
                snapshot,
            } => write!(
                f,
                "needs a maintainer: release {release} ({released}) is newer than the snapshot ({snapshot})"
            ),
            SkipReason::BranchAhead { branch, upstream } => write!(
                f,
                "needs a maintainer: the branch project holds {branch}, newer than upstream's \
                 {upstream}; the update would replace it with an older version"
            ),
            SkipReason::RepologyDisagrees {
                source,
                target,
                repology,
            } => write!(
                f,
                "Repology lists {repology}; {source} says {target}: a maintainer must pick \
                 the version, pass --to-version"
            ),
            SkipReason::Deprecated { devel, source } => write!(
                f,
                "openSUSE:Factory lists {devel} as the devel project of this package, not \
                 {source}: pass --deprecated to include it"
            ),
            SkipReason::VersionCompareFailed { packaged, latest } => {
                write!(
                    f,
                    "cannot compare versions (packaged {packaged:?} vs latest {latest:?} — \
                     one or both are not PEP 440); report as unknown rather than UpToDate"
                )
            }
        }
    }
}

#[cfg(test)]
mod transient_tests {
    use super::SkipReason;

    fn lookup(error: &str) -> SkipReason {
        SkipReason::LookupFailed {
            ecosystem_tag: "Repology".into(),
            name: "x".into(),
            error: error.into(),
        }
    }

    #[test]
    fn a_rate_limit_a_gateway_error_or_a_timeout_may_pass() {
        assert!(lookup("http status: 503").is_transient_lookup());
        assert!(lookup("http status: 429 (retry after 60s)").is_transient_lookup());
        assert!(lookup("https://example.org/x: HTTP 502").is_transient_lookup());
        assert!(lookup("HTTP Error 504: Gateway Timeout").is_transient_lookup());
        assert!(lookup("503 Service Unavailable").is_transient_lookup());
        assert!(lookup("operation timed out").is_transient_lookup());
        assert!(lookup("Connection reset by peer").is_transient_lookup());
    }

    #[test]
    fn a_deterministic_failure_stays_a_skip_even_with_a_503_in_its_url() {
        assert!(!lookup("https://example.org/503.tar.gz: http status: 404").is_transient_lookup());
        assert!(
            !lookup("https://example.org/v503/releases: http status: 404").is_transient_lookup()
        );
        assert!(!lookup("http status: 403").is_transient_lookup());
        assert!(!lookup("no newest version").is_transient_lookup());
        assert!(!SkipReason::NoSpec.is_transient_lookup());
    }
}
