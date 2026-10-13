//! Process identity helpers shared by the pid-based lock files.

use std::path::Path;

/// Start time of `pid` in clock ticks since boot (field 22 of
/// `/proc/<pid>/stat`), or `None` when it cannot be read. Recorded in a lock
/// so one whose pid was reused by an unrelated process is not mistaken for a
/// live one.
pub fn process_start(pid: u32) -> Option<u64> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    // The command name is parenthesised and may contain spaces or parens, so
    // count fields from the last `)`: the next one is field 3 (state).
    let rest = &stat[stat.rfind(')')? + 1..];
    rest.split_whitespace().nth(19)?.parse().ok()
}

/// Whether the process that wrote a lock is still running. `start` is the
/// start time it recorded; a different one means the pid was reused. Without
/// `/proc` this cannot be told, so it answers yes: refusing to start is safer
/// than clobbering a live run.
pub fn owner_is_running(pid: u32, start: Option<u64>) -> bool {
    if pid == std::process::id() || !Path::new("/proc/self").exists() {
        return true;
    }
    match process_start(pid) {
        None => false,
        Some(now) => start.is_none_or(|recorded| recorded == now),
    }
}
