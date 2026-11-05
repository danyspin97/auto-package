//! The baselines of a package: saving, noting the attempts, and restoring the
//! baseline when a run fails.

use super::*;

/// Commit message of the commit that puts a baseline back.
pub const RESTORE_MESSAGE: &str = "auto-package: restore baseline after failed run";

/// The file a baseline keeps of OBS's listing (each file's name and md5) as
/// `.osc/_files` had it when the baseline was taken, one `name<TAB>md5` a line.
const OBS_LISTING: &str = "obs-md5";

/// The file names and md5s of an OBS listing (`.osc/_files`, or the `/source`
/// API), sorted by name.
pub fn listing_md5s(xml: &str) -> Vec<(String, String)> {
    let mut listing: Vec<(String, String)> = xml
        .split("<entry ")
        .skip(1)
        .filter_map(|chunk| {
            let tag = &chunk[..chunk.find('>').unwrap_or(chunk.len())];
            Some((
                crate::obs_xml::attr(tag, "name")?,
                crate::obs_xml::attr(tag, "md5")?,
            ))
        })
        .collect();
    listing.sort();
    listing
}

/// Keep in `dir` what OBS holds for `checkout_dir`, before the run first writes
/// to it: the names it tracks, and a copy of each one's pristine file (osc's
/// copy in `.osc/sources`, the revision the checkout was made or last
/// committed from). A tracked name without a pristine copy is listed anyway, so
/// the restore can say it cannot put that file back.
pub fn save_baseline(checkout_dir: &Path, dir: &Path) -> std::io::Result<()> {
    let saved = dir.join("files");
    fs::create_dir_all(&saved)?;
    let tracked = tracked_files(checkout_dir);
    for name in tracked
        .iter()
        .filter(|n| is_safe_file_arg(n) && !n.starts_with('.'))
    {
        match fs::copy(
            checkout_dir.join(".osc/sources").join(name),
            saved.join(name),
        ) {
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
    }
    fs::write(dir.join("tracked"), tracked.join("\n"))?;
    let listing = fs::read_to_string(checkout_dir.join(".osc/_files")).unwrap_or_default();
    let recorded: String = listing_md5s(&listing)
        .iter()
        .map(|(name, md5)| format!("{name}\t{md5}\n"))
        .collect();
    fs::write(dir.join(OBS_LISTING), recorded)?;
    fs::write(
        dir.join("checkout"),
        checkout_dir.to_string_lossy().as_bytes(),
    )
}

/// What OBS held for `name` when the run started, as the baseline kept in
/// `baseline` has it (see [`save_baseline`]). This is what the run changed: a
/// commit of the run moves `.osc/sources` on to what it sent. With no baseline
/// kept yet (a first fetch, `--local`), osc's copy in `.osc/sources`, which is
/// still that copy then. Empty when there is none.
pub fn run_start_copy(
    checkout_dir: &Path,
    baseline: Option<&Path>,
    name: &std::ffi::OsStr,
) -> String {
    match baseline.filter(|dir| dir.is_dir()) {
        Some(dir) => fs::read_to_string(dir.join("files").join(name)),
        None => fs::read_to_string(checkout_dir.join(".osc").join("sources").join(name)),
    }
    .unwrap_or_default()
}

/// Note in `dir` the files a commit is about to send to OBS, before it is made:
/// an attempt that fails half way may still have written them.
pub fn note_attempt(dir: &Path, files: &[String]) -> std::io::Result<()> {
    use std::io::Write;
    let mut list = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join("attempted"))?;
    for name in files {
        writeln!(list, "{name}")?;
    }
    Ok(())
}

/// The files the run noted as sent to OBS in `dir` (see [`note_attempt`]).
/// Empty when nothing was written.
pub fn attempted_files(dir: &Path) -> Vec<String> {
    fs::read_to_string(dir.join("attempted"))
        .unwrap_or_default()
        .lines()
        .filter(|l| !l.is_empty())
        .map(str::to_string)
        .collect()
}

/// Where the baselines of writes made outside a pipeline run are kept, under
/// the directory the checkouts are in (a leading dot: no OBS project has one).
pub const BASELINE_ROOT: &str = ".obs-baseline";

/// The file a baseline directory holds once a restore of it has failed: the
/// reason is written in it.
pub(super) const UNRESTORED: &str = "unrestored";

/// Whether `dir` holds a baseline that is not yet put back: an attempt noted
/// in it (see [`note_attempt`]) has not been undone. A run or restore that
/// succeeds removes the directory, so this is only true while an attempt may
/// still be on OBS. It also holds after a restore that was cut short (the
/// process exited), when no `UNRESTORED` reason was written.
pub fn is_pending(dir: &Path) -> bool {
    !attempted_files(dir).is_empty() || dir.join(UNRESTORED).exists()
}

/// Keep the baseline of `checkout_dir` for a write made outside a pipeline run,
/// in `root/<branch>/<pkg>`. Returns that dir.
///
/// An earlier baseline that is pending (see [`is_pending`]) is restored first,
/// since a new baseline taken now would hold the failed revision. When that
/// restore fails the error is returned and the earlier baseline stays where it
/// is. Any other earlier baseline was undone or is no longer needed, and is
/// replaced.
pub fn begin_baseline(
    root: &Path,
    checkout_dir: &Path,
    branch: &str,
    pkg: &str,
    discard: &str,
    emit: Emit<'_>,
) -> Result<PathBuf, String> {
    let dir = root.join(ap_core::run::sanitize_name(branch)).join(pkg);
    if is_pending(&dir) {
        restore_baseline(&dir, checkout_dir, discard, emit).map_err(|why| {
            format!(
                "the OBS baseline kept at {} was NOT restored ({why}); \
                 it is kept there, and nothing was written",
                dir.display()
            )
        })?;
    }
    let _ = fs::remove_dir_all(&dir);
    save_baseline(checkout_dir, &dir).map_err(|e| e.to_string())?;
    Ok(dir)
}

/// The command that drops the pending baseline of OBS package `name` of
/// `project` (the branch project), without putting it back. `arch` is left out
/// when it is not known (the baseline roots are per project, whatever its
/// arch), and `workspace` is the work directory the update or build used, which
/// is quoted for the shell.
pub fn discard_command(name: &str, project: &str, arch: Option<&str>, workspace: &Path) -> String {
    let arch = arch
        .map(|a| format!(" --arch {}", shell_word(a)))
        .unwrap_or_default();
    format!(
        "auto-package baseline discard {} --project {}{arch} --work-dir {}",
        shell_word(name),
        shell_word(project),
        shell_word(&workspace.to_string_lossy())
    )
}

/// `word` as one shell word: as it is when it has only plain characters,
/// otherwise in single quotes.
fn shell_word(word: &str) -> String {
    let plain = !word.is_empty()
        && word
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_./:@+=".contains(&b));
    if plain {
        word.to_string()
    } else {
        format!("'{}'", word.replace('\'', "'\\''"))
    }
}

/// A refusal of a restore with the command that discards the baseline, which
/// puts OBS as it is in place of the baseline.
pub fn with_discard_hint(why: &str, discard: &str) -> String {
    format!("{why}; if OBS is right as it is, discard the baseline with `{discard}`")
}

/// The baseline directories of `pkg` under `root`, one per branch
/// (`root/<branch>/<pkg>`, see [`begin_baseline`]).
pub fn baselines_of(root: &Path, pkg: &str) -> Vec<PathBuf> {
    fs::read_dir(root)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path().join(pkg))
        .filter(|p| p.is_dir())
        .collect()
}

/// Drop the pending baseline in `dir` without putting it back: OBS is then
/// taken as it is. Returns `false` when `dir` holds no pending baseline.
pub fn discard_pending(dir: &Path) -> std::io::Result<bool> {
    if !is_pending(dir) {
        return Ok(false);
    }
    fs::remove_dir_all(dir)?;
    Ok(true)
}

/// Put the baseline saved in `dir` back into its checkout, and commit what the
/// attempts sent to OBS with `commit`, the OBS write (the real one is
/// [`commit`]; tests record the call). A tracked file gets its saved copy back.
/// `expected` is the checkout the caller writes for this package: a baseline
/// that names another one is refused, since the restore writes into the path it
/// names.
/// A file the attempts added is removed from the checkout, and committed as a
/// deletion, only when OBS lists it now (`.osc/_files`, which osc updates on a
/// commit that lands): a file that never reached OBS has nothing to undo.
///
/// `obs_check` runs before anything is written, with the checkout: it refuses
/// (an error) when OBS is no longer where the checkout is (see
/// [`obs_still_at_checkout`]).
pub fn restore_baseline_with(
    dir: &Path,
    expected: &Path,
    obs_check: impl FnOnce(&Path) -> Result<(), String>,
    commit: impl FnOnce(&Path, &str, &[String]) -> Result<(), Error>,
) -> Result<(), String> {
    let attempted = attempted_files(dir);
    if attempted.is_empty() {
        return Ok(());
    }
    let checkout = fs::read_to_string(dir.join("checkout"))
        .map(PathBuf::from)
        .map_err(|e| format!("the checkout the baseline was saved from is unknown: {e}"))?;
    if checkout != expected {
        return Err(format!(
            "the baseline was saved from {}, not from {}: nothing was written",
            checkout.display(),
            expected.display()
        ));
    }
    let tracked = fs::read_to_string(dir.join("tracked")).unwrap_or_default();
    let on_obs = tracked_files(&checkout);
    if let Some(bad) = attempted.iter().find(|n| !is_safe_file_arg(n)) {
        return Err(format!(
            "refusing to restore {bad:?}: not a plain file name"
        ));
    }
    obs_check(&checkout)?;
    let mut sent: Vec<String> = Vec::new();
    for name in &attempted {
        if sent.contains(name) {
            continue;
        }
        let target = checkout.join(name);
        if tracked.lines().any(|t| t == name.as_str()) {
            let saved = fs::read(dir.join("files").join(name))
                .map_err(|e| format!("{name}: no saved copy of the baseline ({e})"))?;
            ap_core::fsx::write_atomic(&target, saved).map_err(|e| format!("{name}: {e}"))?;
        } else if on_obs.contains(name) {
            match fs::remove_file(&target) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(format!("{name}: {e}")),
            }
        } else {
            continue;
        }
        sent.push(name.clone());
    }
    if sent.is_empty() {
        return Ok(());
    }
    commit(&checkout, RESTORE_MESSAGE, &sent).map_err(|e| e.to_string())
}

/// The `project`/`package` a checkout belongs to (its `.osc/_project` and
/// `.osc/_package`).
fn checkout_project_package(checkout: &Path) -> Result<(String, String), String> {
    let read = |name: &str| {
        fs::read_to_string(checkout.join(".osc").join(name))
            .map(|s| s.trim().to_string())
            .map_err(|e| format!("cannot read .osc/{name} of {}: {e}", checkout.display()))
    };
    Ok((read("_project")?, read("_package")?))
}

/// The revision of a package listing (`/source/<project>/<package>`) in the
/// form osc records in `.osc/_files`: a link's expanded `xsrcmd5`, or the
/// directory srcmd5 otherwise. The unexpanded directory of a link carries the
/// link's own revision (`lsrcmd5`), which osc never records, so comparing that
/// with `.osc/_files` reports a move that never happened.
///
/// The srcmd5 is derived from the content of the revision: two listings with
/// the same srcmd5 hold the same files.
pub fn expanded_revision(listing_xml: &str) -> Option<String> {
    let linkinfo = listing_xml.find("<linkinfo").map(|i| {
        let rest = &listing_xml[i..];
        &rest[..rest.find('>').unwrap_or(rest.len())]
    });
    linkinfo
        .and_then(|tag| crate::obs_xml::attr(tag, "xsrcmd5"))
        .filter(|r| !r.is_empty())
        .or_else(|| parse_directory_srcmd5(listing_xml))
}

/// The revision OBS holds for the checkout's package must be the one the
/// checkout holds (`.osc/_files`, which osc updates when a commit lands). When
/// it is not, someone committed after the attempt, or the attempt landed but
/// osc did not record it: either way the restore would overwrite a revision
/// nobody here has looked at, so it is refused. `dir` is the baseline, which
/// says whether the revision OBS holds is one an attempt committed.
pub fn obs_still_at_checkout(checkout: &Path, dir: &Path) -> Result<(), String> {
    let (project, package) = checkout_project_package(checkout)?;
    let xml = source_listing_expanded_xml(&project, &package, Emit::quiet())
        .map_err(|e| format!("cannot read the OBS revision of {project}/{package}: {e}"))?;
    head_matches_checkout(checkout, dir, &xml, source_history)
}

/// [`obs_still_at_checkout`] for a listing already read (`listing_xml`). The
/// restore goes ahead only when OBS holds a revision an attempt of this baseline
/// committed, and the checkout holds that same revision. Equality alone is not
/// ownership: a checkout refreshed from OBS (`osc up`) holds whatever OBS holds,
/// including a commit made by someone else. The history is read (`history`)
/// only when the restore is refused, to name the revision.
pub fn head_matches_checkout(
    checkout: &Path,
    dir: &Path,
    listing_xml: &str,
    history: impl FnOnce(&str, &str) -> Result<LatestRevision, String>,
) -> Result<(), String> {
    let (project, package) = checkout_project_package(checkout)?;
    let local = checkout_revision(checkout)
        .ok_or_else(|| format!("the checkout of {project}/{package} records no revision"))?;
    let head = expanded_revision(listing_xml)
        .ok_or_else(|| format!("OBS returned no revision for {project}/{package}"))?;
    if head == local && committed_revisions(dir).contains(&head) {
        return Ok(());
    }
    let latest = history(&project, &package);
    let own = parse_directory_srcmd5(listing_xml);
    Err(moved_on_reason(
        &project,
        &package,
        &head,
        &local,
        own.as_deref(),
        dir,
        latest,
    ))
}

/// What [`restore_baseline_outcome`] found on OBS.
#[derive(Debug, PartialEq, Eq)]
pub enum Restored {
    /// The attempts were put back on OBS.
    Undone,
    /// OBS holds what the baseline holds: no attempt landed, so there was
    /// nothing to put back and nothing was written.
    NothingWritten,
}

/// Whether OBS (`obs_xml`, its `/source` listing) holds the listing the baseline
/// in `dir` recorded, for the checkout `expected` it was saved from.
fn obs_listing_matches(dir: &Path, expected: &Path, obs_xml: &str) -> bool {
    let Ok(saved_from) = fs::read_to_string(dir.join("checkout")) else {
        return false;
    };
    if Path::new(&saved_from) != expected {
        return false;
    }
    let Ok(text) = fs::read_to_string(dir.join(OBS_LISTING)) else {
        return false;
    };
    let mut recorded: Vec<(String, String)> = text
        .lines()
        .filter_map(|line| line.split_once('\t'))
        .map(|(name, md5)| (name.to_string(), md5.to_string()))
        .collect();
    recorded.sort();
    // An empty record says nothing about OBS: it is not a match.
    !recorded.is_empty() && recorded == listing_md5s(obs_xml)
}

/// [`restore_baseline_with`], preceded by the check that OBS still holds the
/// recorded listing: when it does, no attempt landed and nothing is restored
/// or committed. `obs_xml` reads OBS's listing of the checkout's package;
/// when that fails, the restore goes on to `obs_check`, which refuses as before.
pub fn restore_baseline_listing(
    dir: &Path,
    expected: &Path,
    obs_xml: impl FnOnce(&Path) -> Result<String, String>,
    obs_check: impl FnOnce(&Path) -> Result<(), String>,
    commit: impl FnOnce(&Path, &str, &[String]) -> Result<(), Error>,
) -> Result<Restored, String> {
    if let Ok(xml) = obs_xml(expected)
        && (obs_listing_matches(dir, expected, &xml)
            || commit_never_landed(dir, &xml)
            || restore_already_landed(dir, expected, &xml))
    {
        // Nothing landed on OBS, but the failed attempt's edits are still in the
        // checkout: put the saved copies back there (nothing is committed), so a
        // later publish does not carry them. The caller drops `dir` only after this.
        restore_baseline_with(dir, expected, |_| Ok(()), |_, _, _| Ok(()))?;
        return Ok(Restored::NothingWritten);
    }
    restore_baseline_with(dir, expected, obs_check, commit).map(|()| Restored::Undone)
}

/// Whether the commit in flight (see `record_commit`) never landed: OBS is
/// still at the revision the checkout had before it.
fn commit_never_landed(dir: &Path, obs_xml: &str) -> bool {
    // A base an attempt of this baseline committed is on OBS as an attempt:
    // the baseline must be put back over it, not dropped.
    pending_commit_base(dir).is_some_and(|before| {
        !committed_revisions(dir).contains(&before)
            && expanded_revision(obs_xml).as_deref() == Some(before.as_str())
    })
}

/// Whether a restore of `dir` already landed and was cut short before the
/// baseline was removed: OBS and the checkout both hold the revision that
/// restore made. Nothing is left to put back, so the next run drops the baseline.
fn restore_already_landed(dir: &Path, expected: &Path, obs_xml: &str) -> bool {
    let Some(head) = expanded_revision(obs_xml) else {
        return false;
    };
    restored_revisions(dir).contains(&head)
        && checkout_revision(expected).as_deref() == Some(head.as_str())
}

/// [`restore_baseline_listing`] against OBS, committing through [`commit`], and
/// removing `dir` once it is settled. A failure leaves `dir` in place, marked
/// as unrestored (see [`begin_baseline`]). The commit may run after a
/// shutdown: see [`proc::restoring_after_shutdown`].
pub fn restore_baseline_outcome(
    dir: &Path,
    expected: &Path,
    emit: Emit<'_>,
) -> Result<Restored, String> {
    let _restoring = proc::restoring_after_shutdown();
    let outcome = restore_baseline_listing(
        dir,
        expected,
        |checkout| {
            let (project, package) = checkout_project_package(checkout)?;
            source_listing_expanded_xml(&project, &package, Emit::quiet())
                .map_err(|e| e.to_string())
        },
        |checkout| obs_still_at_checkout(checkout, dir),
        |checkout, message, files| {
            record_restore(dir, checkout, || {
                commit(checkout, message, files, None, emit)
            })
        },
    );
    let outcome = outcome.and_then(|restored| match restored {
        Restored::Undone if !attempted_files(dir).is_empty() => {
            restore_landed_as_made(dir, expected).map(|()| restored)
        }
        Restored::Undone | Restored::NothingWritten => Ok(restored),
    });
    match outcome {
        Ok(restored) => {
            let _ = fs::remove_dir_all(dir);
            Ok(restored)
        }
        Err(why) => {
            let _ = fs::write(dir.join(UNRESTORED), &why);
            Err(why)
        }
    }
}

/// After the restore's commit: OBS must still hold the revision the restore
/// made, which the checkout records. Between the commit and this read another
/// commit could land (the window is small but not closed: a commit cannot be
/// fenced). When one did, the restore is reported as not done and the baseline
/// is kept for inspection, with the reason.
fn restore_landed_as_made(dir: &Path, expected: &Path) -> Result<(), String> {
    let (project, package) = checkout_project_package(expected)?;
    let made = checkout_revision(expected)
        .ok_or_else(|| format!("the checkout of {project}/{package} records no revision"))?;
    let xml = source_listing_expanded_xml(&project, &package, Emit::quiet()).map_err(|e| {
        format!(
            "the restore's commit cannot be checked on OBS ({e}); the baseline is kept in {}",
            dir.display()
        )
    })?;
    let head = expanded_revision(&xml)
        .ok_or_else(|| format!("OBS returned no revision for {project}/{package}"))?;
    landed_as_made(dir, &format!("{project}/{package}"), &made, &head)
}

/// Whether OBS's head `head` is the revision the restore `made`. When it is not,
/// the reason says so, and the baseline stays in `dir`.
pub fn landed_as_made(dir: &Path, package: &str, made: &str, head: &str) -> Result<(), String> {
    if head == made {
        return Ok(());
    }
    Err(format!(
        "a commit landed on OBS during the restore: {package} is at revision {head}, \
         the restore made {made}; the baseline is kept in {} for inspection",
        dir.display()
    ))
}

/// [`restore_baseline_outcome`] for callers that only need to know it worked.
/// Says on stderr when nothing was written to OBS (the run-start path reports
/// the same through its reporter).
pub fn restore_baseline(
    dir: &Path,
    expected: &Path,
    discard: &str,
    emit: Emit<'_>,
) -> Result<(), String> {
    match restore_baseline_outcome(dir, expected, emit) {
        Ok(Restored::Undone) => {}
        Ok(Restored::NothingWritten) => ap_core::diag!(
            "nothing was written to OBS since the failed attempt: the saved baseline is dropped and the work continues"
        ),
        Err(why) => return Err(with_discard_hint(&why, discard)),
    }
    Ok(())
}
