use super::*;

/// Commit the style fixes of `quality::tidy` for `item` into `branch`, without
/// a build of our own: OBS rebuilds the commit. `ldconfig` says whether the
/// target defines `%ldconfig_scriptlets`. The rpmlintrc files the spec names
/// lose their `from Config import *` line in the same commit. `no_agent` is the
/// run's `--no-agent`, which the upstream recap of the entry is filled in under.
pub(super) fn tidy_item(
    branch: &str,
    item: &Item,
    ctx: &Ctx<'_>,
    ldconfig: bool,
    no_agent: bool,
) -> Result<TidyDone, String> {
    let _obs_write = ap_build::proc::obs_write();
    let emit = Emit::run(ctx.reporter.as_ref());
    let dir = osc::checkout(branch, &item.pkg, &ctx.work_dir.join("improve"), emit)
        .map_err(|e| e.to_string())?;
    let files: Vec<String> = std::fs::read_dir(&dir)
        .map_err(|e| e.to_string())?
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    let spec_name = update::pick_spec_filename(&item.pkg, &files)
        .ok_or_else(|| "the package has no spec".to_string())?;
    let spec_path = dir.join(&spec_name);
    let before = std::fs::read_to_string(&spec_path).map_err(|e| e.to_string())?;
    let tidied = quality::tidy(&before, ldconfig);
    let rpm_fixes = rpmlintrc_fixes(&dir, &before)?;
    // The provenance of the upstream patches: headers and spec comments. They
    // are not style changes, so they get no `.changes` bullet.
    let hosts = ap_build::allowed_hosts::load_default();
    let patches = super::provenance::checkout_patches(&dir, &tidied);
    let changes_text = super::provenance::checkout_changes(&dir, &item.pkg, &files);
    let edits =
        super::provenance::header_edits(&item.pkg, &tidied, &patches, &changes_text, &hosts);
    let after = edits.spec.clone();
    let tidy_changed = tidied != before;
    let provenance_changed = after != tidied || !edits.patches.is_empty();
    // Without a style change the spec and the rpmlintrc files are not written,
    // and only the leftover cleanup can change the checkout: it commits only
    // when it did (see the closure below).
    let style_changed = tidy_changed || !rpm_fixes.is_empty() || provenance_changed;
    let mut bullets = Vec::new();
    if tidy_changed {
        bullets.push(quality::tidy_bullet(&before, ldconfig));
    }
    if !rpm_fixes.is_empty() {
        bullets.push(ap_build::rpmlintrc_lint::BULLET);
    }
    let bullet = bullets.join("; ");
    // Kept so that a failure after the write leaves the reused checkout as it was.
    let changes = format!("{}.changes", item.pkg);
    let changes_path = dir.join(&changes);
    // A file that is absent is removed again on restore; any other read error
    // stops before the spec is edited, rather than being taken for "absent".
    let changes_before = match std::fs::read(&changes_path) {
        Ok(bytes) => Some(bytes),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Err(format!("could not read {}: {e}", changes_path.display())),
    };
    // What OBS holds is kept before the first write, so a failed commit can be
    // undone on OBS as well as in the checkout.
    // The update's project dir is `<workspace>/<branch>-<arch>`: the command
    // names the workspace, and the arch is found from the project dirs.
    let workspace = ctx.work_dir.parent().unwrap_or(&ctx.work_dir);
    let discard = osc::discard_command(&item.pkg, branch, None, workspace);
    let baseline = osc::begin_baseline(
        &ctx.work_dir.join(osc::BASELINE_ROOT),
        &dir,
        branch,
        &item.pkg,
        &discard,
        emit,
    )
    .map_err(|e| format!("cannot keep the OBS baseline of {branch}: {e}"))?;
    ap_core::fsx::write_atomic(&spec_path, after.as_bytes()).map_err(|e| e.to_string())?;
    let mut cleaned: Vec<String> = Vec::new();
    let mut leftovers = false;
    let mut recap = false;
    let committed = (|| -> Result<bool, String> {
        for fix in &rpm_fixes {
            ap_core::fsx::write_atomic(&dir.join(&fix.name), fix.after.as_bytes())
                .map_err(|e| e.to_string())?;
        }
        for (name, _, text) in &edits.patches {
            ap_core::fsx::write_atomic(&dir.join(name), text.as_bytes())
                .map_err(|e| e.to_string())?;
        }
        let mut commit_files = vec![spec_name.clone()];
        commit_files.extend(rpm_fixes.iter().map(|f| f.name.clone()));
        commit_files.extend(edits.patches.iter().map(|(name, _, _)| name.clone()));
        if !bullet.is_empty() {
            osc::add_changes_entry(&dir, &bullet, emit).map_err(|e| e.to_string())?;
            if changes_path.exists() {
                commit_files.push(changes.clone());
            }
        }
        // The leftovers the build commit cleans too, so both leave the same ones.
        let named = osc::spec_source_names(&after);
        cleaned = osc::clean_branch_leftovers(&dir, branch, &item.pkg, &named, no_agent, emit);
        // Before the bullet is added the `.changes` is the cleanup's alone.
        recap = cleaned
            .iter()
            .any(|m| m.starts_with(ap_build::release_notes::FILLED));
        let cleanup = cleaned
            .iter()
            .any(|m| !m.starts_with(ap_build::release_notes::FILLED))
            || (!style_changed && changes_differ(&changes_path, changes_before.as_deref()));
        leftovers = cleanup || recap;
        if !style_changed && !leftovers {
            return Ok(false);
        }
        if changes_differ(&changes_path, changes_before.as_deref())
            && !commit_files.contains(&changes)
        {
            commit_files.push(changes.clone());
        }
        osc::note_attempt(&baseline, &commit_files).map_err(|e| e.to_string())?;
        osc::commit_attempt(
            &baseline,
            &dir,
            &commit_message(
                tidy_changed || !rpm_fixes.is_empty(),
                provenance_changed,
                cleanup,
                recap,
            ),
            &commit_files,
            None,
            emit,
        )
        .map_err(|e| e.to_string())?;
        Ok(true)
    })();
    for message in &cleaned {
        ctx.say(format!("{}: {message}", item.pkg));
    }
    let committed = match committed {
        Ok(committed) => {
            let _ = std::fs::remove_dir_all(&baseline);
            Ok(committed)
        }
        Err(why) => match osc::restore_baseline(&baseline, &dir, &discard, emit) {
            Ok(()) => Err(why),
            Err(e) => Err(format!("{why}; the OBS baseline was NOT restored: {e}")),
        },
    };
    if let Err(why) = committed {
        let restored_spec = ap_core::fsx::write_atomic(&spec_path, before.as_bytes());
        let restored_changes = match changes_before {
            Some(bytes) => ap_core::fsx::write_atomic(&changes_path, bytes),
            None => match std::fs::remove_file(&changes_path) {
                Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
                _ => Ok(()),
            },
        };
        let restored_rpm = rpm_fixes
            .iter()
            .try_for_each(|f| ap_core::fsx::write_atomic(&dir.join(&f.name), f.before.as_bytes()));
        let restored_patches = edits.patches.iter().try_for_each(|(name, before, _)| {
            ap_core::fsx::write_atomic(&dir.join(name), before.as_bytes())
        });
        if let Err(e) = restored_spec
            .and(restored_changes)
            .and(restored_rpm)
            .and(restored_patches)
        {
            return Err(format!(
                "{why}; could not restore the checkout of {branch}: {e}"
            ));
        }
        return Err(why);
    }
    if committed == Ok(false) {
        ctx.say(format!("{}: already tidy in {branch}", item.pkg));
        return Ok(TidyDone::AlreadyTidy);
    }
    ctx.say(format!(
        "{}: committed the {} to {branch}",
        item.pkg,
        if style_changed {
            "style fixes"
        } else {
            "leftover cleanup"
        }
    ));
    Ok(tidy_done(
        tidy_changed,
        !rpm_fixes.is_empty(),
        provenance_changed,
        leftovers,
    ))
}

/// Whether the `.changes` at `path` no longer holds `before`: its bytes before
/// the tidy wrote it, `None` when it was absent.
fn changes_differ(path: &Path, before: Option<&[u8]>) -> bool {
    std::fs::read(path).ok().as_deref() != before
}

/// What a tidy commit changed, for the outcome of the item.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum TidyDone {
    /// The spec or an rpmlintrc was tidied: a change users see.
    Changed,
    /// Only upstream headers or spec comments were written: nothing to submit.
    ProvenanceOnly,
    /// Nothing to change.
    AlreadyTidy,
}

/// Why a tidy's item is not submitted, if it is not: only a header-only change
/// (its srcmd5 differs from the origin's, so the request guard cannot see it).
/// A tidy that changed nothing is not Skipped: the request guard refuses an
/// unchanged branch, and an earlier real change left in the branch still goes.
pub(super) fn tidy_skip(done: TidyDone) -> Option<&'static str> {
    match done {
        TidyDone::ProvenanceOnly => Some("only upstream headers changed: nothing to submit"),
        TidyDone::Changed | TidyDone::AlreadyTidy => None,
    }
}

/// The kind of a tidy commit (see [`TidyDone`]). A commit that only cleaned
/// the branch's leftovers is a change to submit.
fn tidy_done(
    tidy_changed: bool,
    rpm_changed: bool,
    provenance_changed: bool,
    leftovers: bool,
) -> TidyDone {
    if tidy_changed || rpm_changed || leftovers {
        TidyDone::Changed
    } else if provenance_changed {
        TidyDone::ProvenanceOnly
    } else {
        TidyDone::AlreadyTidy
    }
}

/// The commit message of a tidy: each change it makes, in order.
fn commit_message(tidy: bool, provenance: bool, cleanup: bool, recap: bool) -> String {
    let mut parts = Vec::new();
    if tidy {
        parts.push("Tidy the spec (auto-package lint)");
    }
    if provenance {
        parts.push("Add upstream provenance to patch headers (auto-package lint)");
    }
    if cleanup {
        parts.push("Clean the branch leftovers (auto-package lint)");
    }
    if recap {
        parts.push("Fill in the upstream recap (auto-package lint)");
    }
    if parts.is_empty() {
        parts.push("Tidy the spec (auto-package lint)");
    }
    parts.join("; ")
}

/// An rpmlintrc file of the spec whose tidy changes it, with both texts.
struct RpmFix {
    name: String,
    before: String,
    after: String,
}

/// The rpmlintrc files the spec names that [`ap_build::rpmlintrc_lint::tidy`]
/// changes. An unnamed one is left alone: the publish plan leaves it out, so
/// a commit of it would change nothing that is published, and `lint` only
/// reports it. The tidy only removes lines, so the rpmlintrc guard must accept
/// it; anything else stops the tidy.
fn rpmlintrc_fixes(dir: &Path, spec: &str) -> Result<Vec<RpmFix>, String> {
    let mut fixes = Vec::new();
    for name in ap_build::rpmlintrc_lint::spec_rpmlintrc_files(spec) {
        let path = dir.join(&name);
        let before = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err(format!("could not read {}: {e}", path.display())),
        };
        let after = ap_build::rpmlintrc_lint::tidy(&before);
        if after == before {
            continue;
        }
        let problems =
            ap_build::pipeline::rpmlintrc_guard::rpmlintrc_problems(Some(&before), &after);
        if !problems.is_empty() {
            return Err(format!(
                "the tidy of {name} would leave it malformed: {}",
                problems.join("; ")
            ));
        }
        fixes.push(RpmFix {
            name,
            before,
            after,
        });
    }
    Ok(fixes)
}

#[cfg(test)]
#[path = "tidy_tests.rs"]
mod tests;

/// The newest `.changes` entry of a package in the branch project.
pub(super) fn newest_changes(branch: &str, pkg: &str, emit: Emit<'_>) -> Option<String> {
    let files = osc::list_package_files(branch, pkg, emit)?;
    let own = format!("{pkg}.changes");
    let name = files
        .iter()
        .find(|f| **f == own)
        .or_else(|| files.iter().find(|f| f.ends_with(".changes")))?;
    up::latest_changes_entry(&osc::cat_file(branch, pkg, name, emit)?)
}

/// Ask a yes/no question on the terminal. Without a terminal there is nobody
/// to ask, which counts as no.
pub(super) fn confirm(question: &str, ctx: &Ctx<'_>) -> bool {
    confirm_with(question, &|m| ctx.warn(m))
}

/// [`confirm`] with the warning going to `warn`.
pub(crate) fn confirm_with(question: &str, warn: &dyn Fn(&str)) -> bool {
    matches!(
        ask_line(&format!("{question} [y/N] "), warn)
            .as_deref()
            .map(str::to_lowercase)
            .as_deref(),
        Some("y" | "yes")
    )
}

/// A line typed on the terminal after `prompt`, trimmed; `None` without a
/// terminal (which is said through `warn`) or when it cannot be read.
pub(crate) fn ask_line(prompt: &str, warn: &dyn Fn(&str)) -> Option<String> {
    use std::io::{BufRead, IsTerminal, Write};
    if !std::io::stdin().is_terminal() {
        warn("cannot ask for confirmation without a terminal; pass --yes to submit unasked");
        return None;
    }
    // A build wait in this process may be drawing its live line: hold it off
    // the row until the answer is read.
    let _prompt = ap_core::sink::PromptGuard::open();
    eprint!("{prompt}");
    let _ = std::io::stderr().flush();
    let mut answer = String::new();
    std::io::stdin().lock().read_line(&mut answer).ok()?;
    Some(answer.trim().to_string())
}
