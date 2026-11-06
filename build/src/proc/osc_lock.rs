//! One lock per project working copy for the osc calls that write into it.
//!
//! A project's packages share one `<project>/.osc/_packages` file, which osc
//! rewrites (write `.new`, rename) on every checkout, update, commit and
//! add/remove. Packages of one project run on their own threads, and two of
//! them writing at once race on that rename: one fails with
//! `FileNotFoundError` after its commit may already be on OBS. The lock is
//! taken around each such call in [`capture_with_timeout_env`](super::capture_with_timeout_env),
//! so the writes of one project run one after another, and other projects are
//! not held up. It is a leaf lock: nothing else is acquired while it is held
//! except the child process it guards.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

/// The osc subcommands that write a project working copy: its `.osc` store or
/// a package's files under it. Reads (`ls`, `cat`, `api`, `status`, `results`,
/// `rdiff`, ...) run unlocked, and so do the long ones that write nothing
/// shared (`service`, `build`, `getbinaries`), so one package's download does
/// not hold up another's commit.
const WRITERS: &[&str] = &[
    "co", "up", "ci", "commit", "add", "rm", "revert", "vc", "repairwc",
];

/// The key of the lock an osc call must hold: the project working copy it
/// writes into, or `None` when the call writes into none (any other program,
/// a read, a call outside a project working copy).
///
/// A `co project/package` checks out into `cwd/project`, which may not exist
/// yet, so its key is taken from the argument. Any other writer runs in a
/// package checkout `<project>/<package>` (or in the project itself), and its
/// key is the nearest ancestor of `cwd` that holds `.osc/_packages`.
pub fn lock_key(program: &str, args: &[String], cwd: &Path) -> Option<PathBuf> {
    if Path::new(program).file_name().is_none_or(|n| n != "osc") {
        return None;
    }
    let sub = args.iter().find(|a| !a.starts_with('-'))?;
    if !WRITERS.contains(&sub.as_str()) {
        return None;
    }
    let cwd = std::fs::canonicalize(cwd).unwrap_or_else(|_| cwd.to_path_buf());
    if sub == "co" {
        if let Some(project) = checkout_project(args) {
            return Some(cwd.join(project));
        }
    }
    project_working_copy(&cwd)
}

/// The project named by a `co` argument of the form `project/package`.
pub(super) fn checkout_project(args: &[String]) -> Option<&str> {
    args.iter().find_map(|a| {
        let (project, package) = a.split_once('/')?;
        let valid = !project.is_empty() && !project.starts_with('-') && !package.is_empty();
        (valid && !package.contains('/')).then_some(project)
    })
}

/// The nearest directory at or above `cwd` that osc keeps a project store in.
fn project_working_copy(cwd: &Path) -> Option<PathBuf> {
    cwd.ancestors()
        .find(|d| d.join(".osc").join("_packages").is_file())
        .map(Path::to_path_buf)
}

/// The mutex for one project working copy, created on first use and kept for
/// the life of the process (one per project, so the map stays small).
fn lock_for(key: &Path) -> Arc<Mutex<()>> {
    static LOCKS: OnceLock<Mutex<HashMap<PathBuf, Arc<Mutex<()>>>>> = OnceLock::new();
    LOCKS
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .entry(key.to_path_buf())
        .or_insert_with(|| Arc::new(Mutex::new(())))
        .clone()
}

/// Run `f` holding the lock for `key`, or straight away when `key` is `None`.
/// A panic in `f` does not leave the lock poisoned for the other packages.
pub fn with_lock<T>(key: Option<&Path>, f: impl FnOnce() -> T) -> T {
    let Some(key) = key else {
        return f();
    };
    let lock = lock_for(key);
    let _guard = lock.lock().unwrap_or_else(|e| e.into_inner());
    f()
}
