//! Waiting for a build: the poll delays, the recheck schedule and the jitter, and
//! the details of the build being waited for.

use super::*;

/// Fail before waiting when the project does not build for `repo`/`arch`:
/// OBS would answer with no result at all, forever. A meta that cannot be
/// read skips the check; the wait's own "no results" limit still applies.
pub(super) fn ensure_builds_for(project: &str, repo: &str, arch: &str) -> Result<(), Error> {
    let Ok(Some(meta)) = get_project_meta(project, Emit::quiet()) else {
        return Ok(());
    };
    check_repo_arch(&parse_project_info(&meta), project, repo, arch).map_err(Error::NotActionable)
}

/// How long to wait before the next status poll. A status that keeps
/// holding is polled less often (15s, then 30s after 4 unchanged polls, then
/// 60s after 8) and a failing poll backs off exponentially up to 2 minutes, so
/// a long build or an ailing server is not hammered. `jitter` in `[0, 1)`
/// spreads concurrent waiters by up to +-20%.
pub(super) fn poll_delay(unchanged_polls: u32, errors: u32, jitter: f64) -> Duration {
    let base = match unchanged_polls {
        0..=3 => 15,
        4..=7 => 30,
        _ => 60,
    };
    let secs = if errors == 0 {
        base
    } else {
        (base << errors.min(4)).min(120)
    };
    Duration::from_secs_f64(secs as f64 * (0.8 + 0.4 * jitter))
}

/// The unchanged-poll count that sets the delay while waiting for a build. A
/// status that only awaits the new revision (see
/// [`obs_status::awaiting_new_revision`]) is not "holding still": the build is
/// about to start, so it never backs off past the 30s tier.
pub(super) fn wait_polls(unchanged_polls: u32, awaiting_new_revision: bool) -> u32 {
    if awaiting_new_revision {
        unchanged_polls.min(7)
    } else {
        unchanged_polls
    }
}

/// How long an unchanged status holds before the independent `osc results`
/// cross-check, asked once per stuck period (see [`recheck_due`]).
const NO_PROGRESS_RECHECK: Duration = Duration::from_secs(300);

// A `stale`/`unconfirmed` status is the previous revision's verdict served
// while OBS has not taken up the commit (or has, and the document has not
// caught up): the independent read says which, so it is asked early and again
// at this pace, instead of once after minutes.
const STALE_RECHECK_FIRST: Duration = Duration::from_secs(30);

const STALE_RECHECK_EVERY: Duration = Duration::from_secs(60);

/// Whether the independent `osc results` cross-check of a non-terminal status
/// is due: `since_change` is how long the status has not changed,
/// `since_recheck` how long ago the last check of this stuck period was. A
/// status that awaits the new revision is checked after 30s and then every
/// 60s; any other unchanged status once, after 300s.
pub(super) fn recheck_due(
    awaiting_new_revision: bool,
    since_change: Duration,
    since_recheck: Option<Duration>,
) -> bool {
    match (awaiting_new_revision, since_recheck) {
        (true, None) => since_change >= STALE_RECHECK_FIRST,
        (true, Some(last)) => last >= STALE_RECHECK_EVERY,
        (false, None) => since_change >= NO_PROGRESS_RECHECK,
        (false, Some(_)) => false,
    }
}

/// A pseudo-random value in `[0, 1)` from the clock, enough to de-synchronise
/// pollers without a randomness dependency.
pub(super) fn jitter() -> f64 {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.subsec_nanos());
    f64::from(nanos % 1000) / 1000.0
}

/// The `<details>` OBS gives a package's result in `repo`/`arch` (what a
/// `blocked` or `scheduled` package is waiting for), if any.
pub(super) fn build_details(
    project: &str,
    pkg_name: &str,
    repo: &str,
    arch: &str,
) -> Option<String> {
    if ![project, pkg_name, repo, arch]
        .iter()
        .all(|n| is_valid_obs_name(n))
    {
        return None;
    }
    let entries = obs_package_entries(project, repo, arch, pkg_name, None)
        .map_err(|e| match e {
            ApiFailure::Http(_, m) | ApiFailure::Other(m) => ap_core::diag!("  {m}"),
        })
        .ok()??;
    obs_status::entry_in(&entries, repo, arch, pkg_name)?
        .details
        .clone()
}

/// Sleep for `total`, waking each second to notice a shutdown request, so a
/// long poll delay does not hold up an interrupt.
pub(crate) fn sleep_interruptible(total: Duration) {
    let end = Instant::now() + total;
    while !crate::proc::shutting_down() {
        let left = end.saturating_duration_since(Instant::now());
        if left.is_zero() {
            break;
        }
        std::thread::sleep(left.min(Duration::from_secs(1)));
    }
}
