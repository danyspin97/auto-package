//! The commits an attempt makes to OBS, as the baseline keeps them: a marker
//! before each commit, and the revision each landed as after it. A later run
//! then tells a revision its own attempt created from one someone else made.

use super::*;

/// The marker a commit leaves in the baseline while it is in flight: the
/// checkout's revision before the commit. It is written before `osc ci` runs,
/// so a commit cut short still leaves it.
const PENDING_COMMIT: &str = "pending-commit";

/// The revisions the commits of the attempts landed as, one a line, oldest
/// first. Written after each commit lands.
const COMMITTED: &str = "revisions";

/// The revisions the restores of a baseline landed as (see
/// `restore_baseline_outcome`), kept apart from the attempts' so a restore that
/// landed and was cut short before the baseline was removed is recognised.
const RESTORED: &str = "restored";

/// The revision the checkout holds, as osc records it in `.osc/_files`.
pub fn checkout_revision(checkout: &Path) -> Option<String> {
    fs::read_to_string(checkout.join(".osc/_files"))
        .ok()
        .and_then(|xml| parse_directory_srcmd5(&xml))
}

/// The revision the checkout held before the commit in flight, when the marker
/// is set. `None` when no commit is in flight.
pub fn pending_commit_base(dir: &Path) -> Option<String> {
    fs::read_to_string(dir.join(PENDING_COMMIT))
        .ok()
        .map(|s| s.trim().to_string())
}

/// The revisions the attempts' commits landed as in `dir`, oldest first.
pub fn committed_revisions(dir: &Path) -> Vec<String> {
    revisions_in(dir, COMMITTED)
}

/// The revisions the restores of `dir` landed as, oldest first.
pub fn restored_revisions(dir: &Path) -> Vec<String> {
    revisions_in(dir, RESTORED)
}

fn revisions_in(dir: &Path, log: &str) -> Vec<String> {
    fs::read_to_string(dir.join(log))
        .unwrap_or_default()
        .lines()
        .filter(|l| !l.is_empty())
        .map(str::to_string)
        .collect()
}

/// Commit `files` of `checkout` to OBS for an attempt whose baseline is `dir`
/// (see [`record_commit`]).
pub fn commit_attempt(
    dir: &Path,
    checkout: &Path,
    message: &str,
    files: &[String],
    log: Option<&Path>,
    emit: Emit<'_>,
) -> Result<(), Error> {
    record_commit(dir, checkout, || {
        commit(checkout, message, files, log, emit)
    })
}

/// Runs `commit` for an attempt whose baseline is `dir`, with the marker set
/// before it and the landed revision recorded after it (see [`append_landed`]).
/// A commit that osc reports as failed is not recorded here, even if it landed:
/// the next restore then refuses that revision, and the discard command the
/// refusal names is the way out.
pub fn record_commit(
    dir: &Path,
    checkout: &Path,
    commit: impl FnOnce() -> Result<(), Error>,
) -> Result<(), Error> {
    let before = checkout_revision(checkout).unwrap_or_default();
    fs::create_dir_all(dir).map_err(|e| Error::NotActionable(e.to_string()))?;
    fs::write(dir.join(PENDING_COMMIT), &before)
        .map_err(|e| Error::NotActionable(format!("cannot note the commit: {e}")))?;
    commit()?;
    if append_landed(dir, checkout, COMMITTED, &before) {
        let _ = fs::remove_file(dir.join(PENDING_COMMIT));
    }
    Ok(())
}

/// Records `head` as a revision an attempt committed, once the checkout is
/// known to hold it and it was judged the tool's own (see
/// [`own_attempt_commit`]). The revision is appended to the attempts' log and
/// the in-flight marker of its commit, which was never cleared, is removed.
pub fn adopt_own_head(dir: &Path, head: &str) -> std::io::Result<()> {
    use std::io::Write;
    fs::create_dir_all(dir)?;
    let mut log = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join(COMMITTED))?;
    writeln!(log, "{head}")?;
    let _ = fs::remove_file(dir.join(PENDING_COMMIT));
    Ok(())
}

/// Whether the package of `checkout` has a `_service`: the file in the checkout,
/// or an entry of `.osc/_files`, which osc can leave out of the checkout (its
/// `exclude_files` and size settings).
fn has_service(checkout: &Path) -> bool {
    checkout.join("_service").exists() || tracked_files(checkout).iter().any(|n| n == "_service")
}

/// Appends the revision osc now records for `checkout` to the `log` file of
/// `dir`, when it is a revision the commit made. Nothing is recorded when the
/// revision is the one before the commit (nothing landed), or when the package
/// has a `_service` file: osc then waits for a server-side service run and
/// rewrites `.osc/_files` to the head of the package, which may be a commit by
/// someone else. The package has a `_service` whenever the server runs one for
/// it, so that is the case to refuse. Returns whether it was appended.
fn append_landed(dir: &Path, checkout: &Path, log: &str, before: &str) -> bool {
    if has_service(checkout) {
        ap_core::diag!(
            "  {} has a _service file: its commit's revision is not recorded (a server run may rewrite it)",
            checkout.display()
        );
        return false;
    }
    let Some(after) = checkout_revision(checkout) else {
        ap_core::diag!(
            "  the commit to {} landed, but its revision is not in .osc/_files",
            checkout.display()
        );
        return false;
    };
    if after == before {
        return false;
    }
    use std::io::Write;
    let recorded = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join(log))
        .and_then(|mut f| writeln!(f, "{after}"));
    match recorded {
        Ok(()) => true,
        Err(e) => {
            ap_core::diag!("  cannot record the revision of the commit: {e}");
            false
        }
    }
}

/// Runs the restore commit `commit` of `dir` and records the revision it landed
/// as in the restored log. No marker is set: a restore cut short before its
/// commit leaves OBS at the attempt's recorded revision, so the next run
/// restores again. A restore cut short after osc recorded its commit, but
/// before this append, leaves a revision this baseline does not know: the next
/// run refuses it (equality is not ownership) and the baseline is discarded by
/// hand, with the command the refusal names. A second restore commit is not made
/// over a revision of unknown origin.
///
/// What a failed restore reads from OBS: the listing of the package, and whether
/// its head is the restore commit of the configured user (see
/// [`restore_head_by`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObsAfterRestore {
    pub listing: String,
    pub head_is_restore: bool,
}

/// A commit osc reports as failed may still have landed: osc can fail after
/// the server took the commit (see [`restore_landed_despite_error`]). Then
/// `obs` reads OBS as it is now, and the restore is recorded as landed. Any
/// other failure is returned as it was.
pub fn record_restore(
    dir: &Path,
    checkout: &Path,
    commit: impl FnOnce() -> Result<(), Error>,
    obs: impl FnOnce() -> Result<ObsAfterRestore, String>,
) -> Result<(), Error> {
    let before = checkout_revision(checkout).unwrap_or_default();
    match commit() {
        Ok(()) => {
            append_landed(dir, checkout, RESTORED, &before);
            Ok(())
        }
        Err(e) => {
            let Ok(obs) = obs() else {
                return Err(e);
            };
            if !restore_landed_despite_error(dir, checkout, &before, &obs) {
                return Err(e);
            }
            ap_core::diag!(
                "  the restore's commit to {} reported an error, but OBS holds the restored state: recorded as landed",
                checkout.display()
            );
            append_landed(dir, checkout, RESTORED, &before);
            Ok(())
        }
    }
}

/// Whether a restore whose commit osc reported as failed landed anyway, read
/// from OBS as it is now, `obs` (see [`landed_despite_error`]).
fn restore_landed_despite_error(
    dir: &Path,
    checkout: &Path,
    before: &str,
    obs: &ObsAfterRestore,
) -> bool {
    if has_service(checkout) {
        return false;
    }
    let Some(head) = expanded_revision(&obs.listing) else {
        return false;
    };
    landed_despite_error(
        before,
        checkout_revision(checkout).as_deref(),
        &head,
        &recorded_listing(dir),
        &listing_md5s(&obs.listing),
        obs.head_is_restore,
    )
}

/// Whether a restore whose commit failed landed: OBS's head `head` is a revision
/// newer than the one the checkout held before the commit (`before`), and the
/// checkout records that same revision (`after`, osc's record of the commit).
/// The head is then the restore when OBS's listing `obs` is the listing the
/// baseline recorded (`recorded`, the state the restore puts back; both empty
/// for a package with no files before its first attempt), or when the head is
/// the restore commit of the configured user (`head_is_restore`). Both listings
/// are sorted by name.
pub fn landed_despite_error(
    before: &str,
    after: Option<&str>,
    head: &str,
    recorded: &[(String, String)],
    obs: &[(String, String)],
    head_is_restore: bool,
) -> bool {
    head != before && after == Some(head) && (recorded == obs || head_is_restore)
}
