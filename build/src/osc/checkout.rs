//! Checking out and updating a package: the checkout lock, the unfinished
//! checkouts set aside, the pull with its recovery, and the conflict file.

use super::*;
use crate::osc::service::*;

/// The error `ensure_package` reports when the project listing cannot be read.
/// A transient failure keeps its classification so that the caller retries;
/// any other one stops the build, as it always has.
pub(super) fn listing_failure_for_create(project: &str, e: Error) -> Error {
    if e.is_transient() {
        e
    } else {
        Error::NotActionable(format!("could not list packages of {project}: {e}"))
    }
}

/// Ensure the package exists in `project`, creating it if not.
///
/// Creates it by pushing package meta directly (`osc meta pkg ... -F -`,
/// which the server creates implicitly if it doesn't exist yet) rather than
/// `osc mkpac`. `mkpac` infers "already registered" from local working-copy
/// state — a stray directory left over from a previous, incomplete attempt,
/// or a project checkout that still remembers the package from one — and
/// happily reports success without the package ever having been created on
/// the server. That false success surfaces much later as a 404 on `osc ci`.
/// Pushing meta directly has no such local state to get confused by: the
/// server is the only thing that decides whether the package exists.
pub fn ensure_package(
    project: &str,
    pkg_name: &str,
    work_dir: &Path,
    emit: Emit<'_>,
) -> Result<(), Error> {
    check_obs_name("project", project)?;
    check_obs_name("package", pkg_name)?;
    // `osc ls project/pkg` exits non-zero when the package does not exist.
    let check = proc::capture_with_timeout(
        "osc",
        &["ls", &format!("{project}/{pkg_name}")],
        work_dir,
        emit,
        OSC_QUERY_TIMEOUT,
    )
    .map_err(during(format!("ls {project}/{pkg_name}")))?;

    if check.success() {
        return Ok(());
    }
    // Only a 404 means "not there yet". Any other failure (network, auth,
    // a 5xx) says nothing about existence, and pushing meta then could
    // overwrite the title/description of a package that is in fact there.
    if !is_not_found(&check.stderr) {
        return Err(Error::OscFailed {
            cmd: format!("ls {project}/{pkg_name}"),
            code: check.code,
            stderr: check.stderr,
        });
    }
    // OBS names are case-sensitive, so the failed lookup above does not rule
    // out a package that differs only in case or separators
    // (`python-apscheduler` vs. `python-APScheduler`). Creating one would
    // leave the project with two packages for one upstream, so refuse and let
    // the caller use the existing name.
    let listing =
        list_project_packages(project, emit).map_err(|e| listing_failure_for_create(project, e))?;
    if let Some(existing) = listing
        .iter()
        .filter(|p| fuzzy_key(p) == fuzzy_key(pkg_name))
        .min()
    {
        return Err(Error::NotActionable(format!(
            "{project} already has {existing}; refusing to create {pkg_name} next to it"
        )));
    }

    let meta = format!(
        "<package name=\"{}\" project=\"{}\">\n  \
         <title/>\n  <description/>\n</package>\n",
        crate::obs_xml::escape(pkg_name),
        crate::obs_xml::escape(project)
    );
    // A fresh, uniquely named file in the private directory: a name built
    // from the package in the work directory could collide with a concurrent
    // run or an existing file. Removed when `meta_file` drops.
    let write_meta = || -> std::io::Result<tempfile::NamedTempFile> {
        let mut file = tempfile::Builder::new()
            .prefix("meta-")
            .suffix(".xml")
            .tempfile_in(proc::private_dir()?)?;
        std::io::Write::write_all(&mut file, meta.as_bytes())?;
        Ok(file)
    };
    let meta_file = write_meta().map_err(|e| Error::OscFailed {
        cmd: format!("meta pkg {project} {pkg_name} (writing temp meta file)"),
        code: -1,
        stderr: e.to_string(),
    })?;

    let out = proc::capture_with_timeout(
        "osc",
        &[
            "meta",
            "pkg",
            project,
            pkg_name,
            "-F",
            &meta_file.path().to_string_lossy(),
        ],
        work_dir,
        emit,
        OSC_QUERY_TIMEOUT,
    );
    drop(meta_file);
    let out = out.map_err(during(format!("meta pkg {project} {pkg_name}")))?;

    if !out.success() {
        return Err(Error::OscFailed {
            cmd: format!("meta pkg {project} {pkg_name} -F -"),
            code: out.code,
            stderr: out.stderr,
        });
    }

    Ok(())
}

/// What an osc failure says, for a message: an uncaught Python exception is
/// reduced to its last lines (the traceback names osc's own source, not the
/// cause), and a full disk is called what it is, since every package that
/// downloads or checks out then fails the same way.
pub(super) fn osc_error_text(stderr: &str) -> String {
    let text = match stderr.find("Traceback (most recent call last):") {
        Some(at) => {
            let lines: Vec<&str> = stderr[at..]
                .lines()
                .filter(|l| !l.trim().is_empty())
                .collect();
            lines[lines.len().saturating_sub(2)..].join("\n")
        }
        None => stderr.trim_end().to_string(),
    };
    if child_output_says_disk_full(stderr) {
        format!("{text}\nthe disk is full: free space under the work directory and run again")
    } else {
        text
    }
}

/// Check out `project/pkg_name` into `into_dir`.
///
/// Returns the path of the checkout directory, which is
/// `into_dir/<project>/<pkg_name>/`.
pub fn checkout(
    project: &str,
    pkg_name: &str,
    into_dir: &Path,
    emit: Emit<'_>,
) -> Result<PathBuf, Error> {
    check_obs_name("project", project)?;
    check_obs_name("package", pkg_name)?;
    let checkout_dir = into_dir.join(project).join(pkg_name);

    // The decision below (discard an unfinished checkout, re-use or check out)
    // and the osc run it leads to hold this directory's lock, so a concurrent
    // call for the same package waits instead of renaming a checkout that is
    // still being written. Other packages are not held up. The process-wide
    // lock further down is taken inside this one, never the other way round.
    let dir_lock = checkout_dir_lock(&checkout_dir);
    let _dir_guard = dir_lock.lock().unwrap_or_else(|e| e.into_inner());

    // Moved-aside copies of an unfinished checkout are kept for a week, then
    // they go: each can hold a large partial download.
    prune_unfinished_asides(&checkout_dir, None, Some(UNFINISHED_ASIDE_MAX_AGE));

    // A checkout osc never finished (killed, or out of disk space mid-download)
    // tracks nothing, and `osc up` on it cannot recover: it trips over the files
    // the half-run already wrote. It is checked out again instead.
    discard_unfinished_checkout(&checkout_dir, emit);

    // Re-use an existing checkout rather than re-cloning every run.
    if checkout_dir.exists() {
        let out = osc_up_recovering(&checkout_dir, emit)?;

        if !out.success() {
            // 404 means `ensure_package`'s meta push hasn't propagated to
            // this checkout's view of the server yet — use the directory
            // as-is rather than failing on a lag that resolves itself.
            if is_not_found(&out.stderr) {
                return Ok(checkout_dir);
            }
            return Err(Error::OscFailed {
                cmd: format!("up {project}/{pkg_name} in {}", checkout_dir.display()),
                code: out.code,
                stderr: out.stderr,
            });
        }

        return Ok(checkout_dir);
    }

    // Every `osc co` into the same directory rewrites the shared
    // `<project>/.osc/_packages` file (write `.new`, rename). With several
    // packages checking out in parallel (`--jobs`, or roots added from the
    // TUI) the renames race and one fails with FileNotFoundError, so
    // serialise checkouts process-wide.
    static CHECKOUT_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _guard = CHECKOUT_LOCK.lock().unwrap_or_else(|e| e.into_inner());

    let out = proc::capture_with_timeout(
        "osc",
        &["co", &format!("{project}/{pkg_name}")],
        into_dir,
        emit,
        OSC_TRANSFER_TIMEOUT,
    )
    .map_err(during(format!("co {project}/{pkg_name}")))?;

    if !out.success() {
        // Whatever osc wrote before it failed would make the next run's `osc up`
        // fail on it.
        discard_unfinished_checkout(&checkout_dir, emit);
        return Err(Error::OscFailed {
            cmd: format!("co {project}/{pkg_name}"),
            code: out.code,
            stderr: out.stderr,
        });
    }

    Ok(checkout_dir)
}

/// The lock for one checkout directory, created on first use and kept for the
/// life of the process (one per package checked out).
pub(super) fn checkout_dir_lock(checkout_dir: &Path) -> Arc<Mutex<()>> {
    static LOCKS: OnceLock<Mutex<HashMap<PathBuf, Arc<Mutex<()>>>>> = OnceLock::new();
    LOCKS
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .entry(checkout_dir.to_path_buf())
        .or_insert_with(|| Arc::new(Mutex::new(())))
        .clone()
}

/// Remove `checkout_dir` when it is a checkout that was never completed: osc
/// leaves `.osc/_in_update` while it downloads and removes it at the end, and
/// `.osc/_files` lists nothing until the download has been recorded. A working
/// copy that tracks files was completed once and keeps its local changes.
///
/// Files in an unfinished checkout that nothing tracks may be local work (a
/// new package's spec or `.changes` never committed), so such a checkout is
/// moved aside to `<name>.unfinished-<millis>` next to it instead of deleted.
///
/// Only the newest moved-aside copy of a package is kept, and its `.osc`
/// metadata is dropped: the copy is no longer a checkout, so nothing scanning
/// for package directories (by `.osc`) picks it up.
pub(super) fn discard_unfinished_checkout(checkout_dir: &Path, emit: Emit<'_>) {
    if !checkout_dir.join(".osc").join("_in_update").exists()
        || !tracked_files(checkout_dir).is_empty()
    {
        return;
    }
    if has_entries_besides_osc(checkout_dir) {
        let aside = unfinished_aside_path(checkout_dir);
        match fs::rename(checkout_dir, &aside) {
            Ok(()) => {
                let osc_dir = aside.join(".osc");
                if let Err(e) = fs::remove_dir_all(&osc_dir) {
                    ap_core::diag!("  could not remove {}: {e}", osc_dir.display());
                }
                prune_unfinished_asides(checkout_dir, Some(&aside), None);
                emit.reporter.report(&Event::Warning {
                    step: emit.step,
                    message: format!(
                        "{} is an unfinished checkout with untracked files; moved to {} and checking it out again",
                        checkout_dir.display(),
                        aside.display()
                    ),
                });
            }
            Err(e) => ap_core::diag!("  could not move {} aside: {e}", checkout_dir.display()),
        }
        return;
    }
    match fs::remove_dir_all(checkout_dir) {
        Ok(()) => emit.reporter.report(&Event::Warning {
            step: emit.step,
            message: format!(
                "{} is the remains of a checkout osc never finished; checking it out again",
                checkout_dir.display()
            ),
        }),
        Err(e) => ap_core::diag!("  could not remove {}: {e}", checkout_dir.display()),
    }
}

/// Whether `checkout_dir` holds anything but its `.osc` metadata directory.
fn has_entries_besides_osc(checkout_dir: &Path) -> bool {
    fs::read_dir(checkout_dir).is_ok_and(|entries| {
        entries
            .flatten()
            .any(|e| e.file_name() != std::ffi::OsStr::new(".osc"))
    })
}

/// How long a moved-aside unfinished checkout is kept.
pub(super) const UNFINISHED_ASIDE_MAX_AGE: Duration = Duration::from_secs(7 * 24 * 60 * 60);

/// Delete the moved-aside copies of `checkout_dir` (`<name>.unfinished-*`
/// siblings), except `keep`. With `max_age`, only copies last modified longer
/// ago than that are deleted.
pub(super) fn prune_unfinished_asides(
    checkout_dir: &Path,
    keep: Option<&Path>,
    max_age: Option<Duration>,
) {
    let (Some(parent), Some(name)) = (checkout_dir.parent(), checkout_dir.file_name()) else {
        return;
    };
    let prefix = format!("{}.unfinished-", name.to_string_lossy());
    let Ok(rd) = fs::read_dir(parent) else {
        return;
    };
    for entry in rd.flatten() {
        let path = entry.path();
        if !entry.file_name().to_string_lossy().starts_with(&prefix)
            || keep.is_some_and(|k| k == path)
            || !entry.file_type().is_ok_and(|t| t.is_dir())
        {
            continue;
        }
        if let Some(max_age) = max_age {
            let old_enough = entry
                .metadata()
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| std::time::SystemTime::now().duration_since(t).ok())
                .is_some_and(|age| age > max_age);
            if !old_enough {
                continue;
            }
        }
        if let Err(e) = fs::remove_dir_all(&path) {
            ap_core::diag!("  could not remove {}: {e}", path.display());
        }
    }
}

/// A free sibling path `<checkout_dir>.unfinished-<millis>` (suffixed with a
/// counter if that name is taken).
fn unfinished_aside_path(checkout_dir: &Path) -> PathBuf {
    let name = checkout_dir
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned();
    let millis = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis());
    let parent = checkout_dir.parent().unwrap_or(Path::new("."));
    let mut candidate = parent.join(format!("{name}.unfinished-{millis}"));
    let mut n = 1;
    while candidate.exists() {
        candidate = parent.join(format!("{name}.unfinished-{millis}-{n}"));
        n += 1;
    }
    candidate
}

/// Run `osc up` from `checkout_dir` to update the working copy.
///
/// Non-fatal: an already-up-to-date or brand-new checkout exits non-zero; we
/// ignore those cleanly.
pub fn pull(checkout_dir: &Path, emit: Emit<'_>) -> Result<(), Error> {
    let out = osc_up_recovering(checkout_dir, emit)?;
    if !out.success() {
        // 404 means `ensure_package`'s meta push hasn't propagated to this
        // checkout's view of the server yet — the directory is valid,
        // nothing to pull.
        if !is_benign_up_output(&out.stdout)
            && !is_benign_up_output(&out.stderr)
            && !is_not_found(&out.stdout)
            && !is_not_found(&out.stderr)
        {
            return Err(Error::OscFailed {
                cmd: format!("up in {}", checkout_dir.display()),
                code: out.code,
                stderr: out.stderr,
            });
        }
    }
    Ok(())
}

/// Whether `text` is osc's own "nothing to do" report from `osc up`: a whole
/// line that is `nothing to update` or `At revision <n>.`. Matching a line
/// rather than a substring keeps a real failure that merely quotes those
/// words (a path, a package name, a traceback) an error.
pub(super) fn is_benign_up_output(text: &str) -> bool {
    let mut lines = text
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .peekable();
    lines.peek().is_some()
        && lines.all(|l| {
            l == "nothing to update"
                || l.strip_prefix("At revision ")
                    .and_then(|r| r.strip_suffix('.'))
                    .is_some_and(|r| !r.is_empty() && r.bytes().all(|b| b.is_ascii_digit()))
        })
}

/// Run `osc up` in `checkout_dir`, recovering in-place from the two known
/// self-inflicted working-copy failure modes before handing the (possibly
/// still-failing) result back to the caller: a prior run that died
/// mid-`up`/`ci` leaving `.osc/` inconsistent ([`is_inconsistent_wc_error`],
/// fixed by [`repair_wc`]), and an untracked local file colliding with a
/// same-named file the server gained in the meantime
/// ([`changes_conflict_file`], fixed by moving the local one aside).
///
/// Shared by [`checkout`]'s reuse path and [`pull`] so both get the same
/// recovery — `checkout`'s "osc up" on an existing directory is exactly the
/// same command `pull` runs, and the same interrupted-run history that leaves
/// `.osc/` inconsistent or a stray untracked `.changes` file behind can be hit
/// from either entry point, not just the one a fix happened to land in first.
/// Returns the final `osc up`'s output either way — success or not — so each
/// caller can keep interpreting failure (which stderr patterns are benign,
/// whether 404 is a non-error) in its own, different way.
fn osc_up_recovering(checkout_dir: &Path, emit: Emit<'_>) -> Result<Captured, Error> {
    // Each recovery runs at most once per call, so a checkout that keeps
    // failing the same way is reported, not retried forever.
    let mut repaired = false;
    let mut moved_aside = false;
    let up = || {
        proc::capture_with_timeout("osc", &["up"], checkout_dir, emit, OSC_TRANSFER_TIMEOUT)
            .map_err(during(format!("up in {}", checkout_dir.display())))
    };
    let mut out = up()?;
    loop {
        if out.success() {
            return Ok(out);
        }
        if !repaired && is_inconsistent_wc_error(&out.stderr) {
            repair_wc(checkout_dir, emit)?;
            repaired = true;
        } else if !moved_aside && let Some(name) = changes_conflict_file(&out.stderr) {
            // osc only raises this when `name` is untracked in this working
            // copy's `.osc/_files` (see `Package.__update`'s check against
            // `filenamelist_unvers`), so it is always safe to move aside — it
            // was never osc's file to begin with. The usual cause here is our
            // own `create_initial_changes` writing `<pkg>.changes` directly
            // before that revision's commit reached the server, and the
            // server later gaining its own file of the same name in the
            // meantime (e.g. a prior interrupted run that got further).
            // If the move fails the checkout is not usable: that is the
            // error to report, not a retry that will hit the same conflict.
            let backup = move_aside(checkout_dir, name)?;
            emit.reporter.report(&Event::Warning {
                step: emit.step,
                message: format!(
                    "osc up: untracked local '{name}' conflicted with a newer revision's \
                     file of the same name; moved it aside as '{}' and retrying",
                    backup.display()
                ),
            });
            moved_aside = true;
        } else {
            return Ok(out);
        }
        out = up()?;
    }
}

/// Move the untracked `name` out of `checkout_dir` so `osc up` can add the
/// server's file in its place, and return where it went.
///
/// The backup goes beside the checkout (`<project>/.preexisting/<pkg>/`), not
/// inside it: `osc addremove` runs on the package directory and would commit
/// a backup left there. A backup already present is never overwritten (a
/// checkout can go through this more than once), so each one gets a numbered
/// name instead.
pub(super) fn move_aside(checkout_dir: &Path, name: &str) -> Result<PathBuf, Error> {
    let pkg = checkout_dir.file_name().unwrap_or(checkout_dir.as_os_str());
    let dir = checkout_dir
        .parent()
        .unwrap_or(checkout_dir)
        .join(".preexisting")
        .join(pkg);
    fs::create_dir_all(&dir)?;
    let mut backup = dir.join(name);
    let mut n = 1;
    while backup.exists() {
        n += 1;
        backup = dir.join(format!("{name}.{n}"));
    }
    fs::rename(checkout_dir.join(name), &backup)?;
    Ok(backup)
}

/// Extract the filename osc names in its uncaught `PackageFileConflict`
/// message from `Package.__update()` (`osc/obs_scm/package.py`): `failed to
/// add file '<name>' file/dir with the same name already exists`. Raised by
/// `osc up` when a file of that name sits on disk but untracked in this
/// working copy while the server's newer revision also adds a file with the
/// same name — an unhandled exception in osc itself, not one of its usual
/// "inconsistent working copy" messages, so `repairwc` does not apply here.
///
/// Only a `.changes` file (a bare name, not a path) is returned: that is the
/// one file this tool writes into a checkout itself, so the only conflict it
/// is safe to resolve by moving the local copy aside. Any other conflict is
/// left for the caller to report.
pub(super) fn changes_conflict_file(stderr: &str) -> Option<&str> {
    let name = stderr
        .split("failed to add file '")
        .nth(1)?
        .split('\'')
        .next()?;
    (name.ends_with(".changes")
        && !name.contains('/')
        && !name.contains("..")
        && !name.starts_with('-'))
    .then_some(name)
}
