//! A package packaged from a repository snapshot (`1.4.8+git20261025.af521ae`)
//! is compared with the newest release of its forge repository by date: a
//! release published after the snapshot was taken needs a maintainer, a
//! release older than it means the snapshot is current. The tool never moves
//! a snapshot package to a release itself.

use super::*;

/// The day a snapshot was taken, from the `git<YYYYMMDD>` or `git.<epoch>` in
/// its version. A version that says neither (`1.3.git10`) has no date.
pub(super) fn snapshot_date(version: &str) -> Option<String> {
    for (at, _) in version.match_indices("git") {
        let rest = &version[at + 3..];
        let rest = rest.strip_prefix('.').unwrap_or(rest);
        let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
        match digits.len() {
            8 => return calendar_day(&digits),
            10 => return digits.parse().ok().map(day_of_epoch),
            _ => {}
        }
    }
    None
}

/// `YYYYMMDD` as `YYYY-MM-DD`, when the month and day are in range.
fn calendar_day(digits: &str) -> Option<String> {
    let (y, m, d) = (&digits[..4], &digits[4..6], &digits[6..]);
    let month: u32 = m.parse().ok()?;
    let day: u32 = d.parse().ok()?;
    ((1..=12).contains(&month) && (1..=31).contains(&day)).then(|| format!("{y}-{m}-{d}"))
}

/// The UTC day of a Unix time, as `YYYY-MM-DD`.
pub(super) fn day_of_epoch(secs: i64) -> String {
    // Howard Hinnant's civil-from-days, on days since 1970-01-01.
    let z = secs.div_euclid(86_400) + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!("{year:04}-{month:02}-{day:02}")
}

/// The newest release of the forge repository `url` and the day it came out,
/// read through the lookup cache. The cache keeps one string per key, so the
/// pair is stored as `<version> <date>`.
pub fn cached_release_date(url: &str) -> Result<(String, String), String> {
    let cached = upstream_cache::lookup(&format!("release-date:{url}"), || {
        let _slot = super::host_gate::enter(super::host_gate::host_of(url));
        crate::sources::latest_release_date(url).map(|(v, d)| format!("{v} {d}"))
    })?;
    cached
        .rsplit_once(' ')
        .map(|(v, d)| (v.to_string(), d.to_string()))
        .ok_or_else(|| format!("unreadable release date answer {cached:?}"))
}

/// The verdict on a snapshot package of a forge repository whose release and
/// snapshot dates are both known. `None` when either is not known, which keeps
/// the package a plain snapshot skip.
pub(super) fn against_release(
    ecosystem: &Ecosystem,
    packaged: &str,
    lookups: &Lookups<'_>,
) -> Option<Decision> {
    let Ecosystem::GitRepo { url } = ecosystem else {
        return None;
    };
    let snapshot = snapshot_date(packaged)?;
    let (release, released) = (lookups.fetch_release_date)(url).ok()?;
    Some(if released > snapshot {
        Decision::Skipped(SkipReason::SnapshotBehind {
            release,
            released,
            snapshot,
        })
    } else {
        Decision::UpToDate {
            ecosystem: ecosystem.clone(),
            version: packaged.to_string(),
        }
    })
}
