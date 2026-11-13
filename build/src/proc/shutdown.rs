use super::*;

/// Process groups of children currently running, each with whether it belongs
/// to a baseline restore (`true`: spared by [`kill_all_groups`]). Children get
/// their own group (so a timeout can kill their descendants), which also means
/// a terminal Ctrl-C no longer reaches them — when this process is told to
/// stop it must kill them itself or `osc`/`claude` keep running, orphaned.
pub(super) static ACTIVE_GROUPS: std::sync::Mutex<std::collections::BTreeMap<u32, bool>> =
    std::sync::Mutex::new(std::collections::BTreeMap::new());

/// Registration of a running child's group; removed on drop.
pub(crate) struct GroupGuard(u32);

/// Set once [`kill_all_groups`] runs. A child spawned just before Ctrl-C but
/// not yet registered is missed by that sweep, and being in its own group it
/// would be orphaned; `track_group` checks this flag so it is killed on
/// registration instead.
pub(super) static SHUTTING_DOWN: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

thread_local! {
    /// Set by [`restoring_after_shutdown`] on the thread that restores an OBS
    /// baseline.
    static RESTORING: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// The longest a child of a restore may run: the restore is one short commit,
/// and a stalled one must not hold up the exit of a stopped run. A restore runs
/// two commands (`osc addremove`, `osc ci`), so it takes at most twice this.
pub const RESTORE_TIMEOUT: Duration = Duration::from_secs(120);

/// Lets the calling thread start children after the run was stopped, for the
/// commit that puts an OBS baseline back: a run that is stopped must still
/// leave the project as it was. Only `osc::restore_baseline` takes it. Each
/// child of the thread then gets at most [`RESTORE_TIMEOUT`]. The permission
/// ends when the guard drops.
pub fn restoring_after_shutdown() -> RestoreGuard {
    RESTORING.with(|r| r.set(true));
    RestoreGuard(obs_write())
}

/// OBS writes in progress, counted across threads: a package run (which may
/// write its project and then restore it), a restore, and an update or
/// maintenance commit. Taken from the moment a write may start, not from the
/// first command, so the interrupt handler waits for a run that has not yet
/// reached its restore.
pub(super) static OBS_WRITES: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);

/// Holds one OBS write in progress (see [`obs_write`]); released on drop.
pub struct ObsWriteGuard(());

/// Count an OBS write as in progress until the guard drops.
pub fn obs_write() -> ObsWriteGuard {
    OBS_WRITES.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    ObsWriteGuard(())
}

impl Drop for ObsWriteGuard {
    fn drop(&mut self) {
        OBS_WRITES.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
    }
}

/// See [`restoring_after_shutdown`].
pub struct RestoreGuard(ObsWriteGuard);

impl Drop for RestoreGuard {
    fn drop(&mut self) {
        RESTORING.with(|r| r.set(false));
    }
}

/// How many OBS writes are in progress right now (see [`obs_write`]).
pub fn obs_writes_in_flight() -> usize {
    OBS_WRITES.load(std::sync::atomic::Ordering::SeqCst)
}

/// Wait up to `timeout` for every OBS write to return. `true` when none is
/// left; `false` when the wait ran out first, and the caller exits with a
/// write still pending. Its baseline then stays on disk, marked pending, and
/// the next run of that package restores it before writing.
pub fn wait_for_obs_writes(timeout: Duration) -> bool {
    wait_until_idle(&OBS_WRITES, timeout, Duration::from_millis(100))
}

/// Poll `counter` until it reads zero or `timeout` has passed.
pub(super) fn wait_until_idle(
    counter: &std::sync::atomic::AtomicUsize,
    timeout: Duration,
    poll: Duration,
) -> bool {
    let deadline = Instant::now() + timeout;
    loop {
        if counter.load(std::sync::atomic::Ordering::SeqCst) == 0 {
            return true;
        }
        let now = Instant::now();
        if now >= deadline {
            return false;
        }
        std::thread::sleep(poll.min(deadline - now));
    }
}

pub(super) fn restoring() -> bool {
    RESTORING.with(|r| r.get())
}

/// Whether a child may start: never once shutdown has begun, except while
/// restoring a baseline.
pub(super) fn may_spawn(shutting_down: bool, restoring: bool) -> bool {
    !shutting_down || restoring
}

/// Record `pid` (spawned with `process_group(0)`) as running until the guard drops.
///
/// When shutdown has begun the group is killed at once and an error returned,
/// so the caller must reap the child and give up.
pub(crate) fn track_group(pid: u32) -> io::Result<GroupGuard> {
    // The flag is checked under the lock that `kill_all_groups` takes after
    // setting it: either the sweep sees this pid, or this sees the flag.
    let mut groups = ACTIVE_GROUPS.lock().unwrap_or_else(|e| e.into_inner());
    if !may_spawn(
        SHUTTING_DOWN.load(std::sync::atomic::Ordering::SeqCst),
        restoring(),
    ) {
        drop(groups);
        kill_process_group(pid);
        return Err(io::Error::new(
            io::ErrorKind::Interrupted,
            "shutting down; child killed",
        ));
    }
    groups.insert(pid, restoring());
    Ok(GroupGuard(pid))
}

impl Drop for GroupGuard {
    fn drop(&mut self) {
        ACTIVE_GROUPS
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&self.0);
    }
}

/// Whether the process has been told to stop. Long polling loops and the
/// scheduler check this so in-flight work gives up instead of waiting out a
/// full build timeout.
pub fn shutting_down() -> bool {
    SHUTTING_DOWN.load(std::sync::atomic::Ordering::SeqCst)
}

/// Set by [`request_interrupt`]: the user stopped the run (Ctrl-C, SIGTERM).
static INTERRUPTED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Record that the user stopped the run. Called before [`kill_all_groups`], so
/// a child that dies of that kill sees the flag when its failure is read.
/// Unlike [`abort_run`] this is not a run that cannot continue.
pub fn request_interrupt() {
    INTERRUPTED.store(true, std::sync::atomic::Ordering::SeqCst);
}

/// Whether the user stopped the run (see [`request_interrupt`]). A failure
/// that follows from the stop is an interruption, not the package's failure.
pub fn interrupted() -> bool {
    INTERRUPTED.load(std::sync::atomic::Ordering::SeqCst)
}

/// SIGKILL every child still running, except a baseline restore's: that one
/// is let finish (see [`wait_for_obs_writes`]). For process shutdown (Ctrl-C,
/// quit).
pub fn kill_all_groups() {
    SHUTTING_DOWN.store(true, std::sync::atomic::Ordering::SeqCst);
    ap_ecosystem::net::request_stop();
    let pids = sweep_targets(&ACTIVE_GROUPS.lock().unwrap_or_else(|e| e.into_inner()));
    for pid in pids {
        kill_process_group(pid);
    }
}

/// The groups a shutdown sweep kills: every registered one but the restores'.
pub(super) fn sweep_targets(groups: &std::collections::BTreeMap<u32, bool>) -> Vec<u32> {
    groups
        .iter()
        .filter(|(_, spared)| !**spared)
        .map(|(pid, _)| *pid)
        .collect()
}

/// Why the run was stopped by [`abort_run`]; the first reason wins.
pub(super) static ABORT_REASON: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);

/// Stop the whole run because it cannot continue (e.g. the agent CLI is not
/// logged in): record `reason` (only the first call's is kept), raise the
/// shutdown flag so the scheduler and the wait loops wind down, and kill the
/// running child groups. Unlike [`kill_all_groups`] this is not an interrupt:
/// the caller decides the exit code, see [`abort_reason`].
pub fn abort_run(reason: String) {
    {
        let mut slot = ABORT_REASON.lock().unwrap_or_else(|e| e.into_inner());
        if slot.is_none() {
            *slot = Some(reason);
        }
    }
    kill_all_groups();
}

/// The reason given to the first [`abort_run`], if the run was aborted.
pub fn abort_reason() -> Option<String> {
    ABORT_REASON
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone()
}

/// SIGKILL the group `pid` leads. Only while the leader is still unreaped (see
/// [`ap_core::pgroup`]): after that the number may belong to another group.
pub(crate) fn kill_process_group(pid: u32) {
    ap_core::pgroup::kill_group(pid);
}
