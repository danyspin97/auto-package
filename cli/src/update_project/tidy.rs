use super::*;

/// Commit the style fixes of `quality::tidy` for `item` into `branch`, without
/// a build of our own: OBS rebuilds the commit. `ldconfig` says whether the
/// target defines `%ldconfig_scriptlets`. The rpmlintrc files the spec names
/// lose their `from Config import *` line in the same commit.
pub(super) fn tidy_item(
    branch: &str,
    item: &Item,
    ctx: &Ctx<'_>,
    ldconfig: bool,
) -> Result<(), String> {
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
    let after = quality::tidy(&before, ldconfig);
    let rpm_fixes = rpmlintrc_fixes(&dir, &before)?;
    if after == before && rpm_fixes.is_empty() {
        ctx.say(format!("{}: already tidy in {branch}", item.pkg));
        return Ok(());
    }
    let mut bullets = Vec::new();
    if after != before {
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
    let baseline = osc::begin_baseline(
        &ctx.work_dir.join(osc::BASELINE_ROOT),
        &dir,
        branch,
        &item.pkg,
        emit,
    )
    .map_err(|e| format!("cannot keep the OBS baseline of {branch}: {e}"))?;
    ap_core::fsx::write_atomic(&spec_path, after.as_bytes()).map_err(|e| e.to_string())?;
    let committed = (|| -> Result<(), String> {
        for fix in &rpm_fixes {
            ap_core::fsx::write_atomic(&dir.join(&fix.name), fix.after.as_bytes())
                .map_err(|e| e.to_string())?;
        }
        osc::add_changes_entry(&dir, &bullet, emit).map_err(|e| e.to_string())?;
        let mut commit_files = vec![spec_name.clone()];
        commit_files.extend(rpm_fixes.iter().map(|f| f.name.clone()));
        if changes_path.exists() {
            commit_files.push(changes.clone());
        }
        osc::note_attempt(&baseline, &commit_files).map_err(|e| e.to_string())?;
        osc::commit(
            &dir,
            "Tidy the spec (auto-package lint)",
            &commit_files,
            None,
            emit,
        )
        .map_err(|e| e.to_string())
    })();
    let committed = match committed {
        Ok(()) => {
            let _ = std::fs::remove_dir_all(&baseline);
            Ok(())
        }
        Err(why) => match osc::restore_baseline(&baseline, &dir, emit) {
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
        if let Err(e) = restored_spec.and(restored_changes).and(restored_rpm) {
            return Err(format!(
                "{why}; could not restore the checkout of {branch}: {e}"
            ));
        }
        return Err(why);
    }
    ctx.say(format!(
        "{}: committed the style fixes to {branch}",
        item.pkg
    ));
    Ok(())
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
pub(super) fn ask_line(prompt: &str, warn: &dyn Fn(&str)) -> Option<String> {
    use std::io::{BufRead, IsTerminal, Write};
    if !std::io::stdin().is_terminal() {
        warn("cannot ask for confirmation without a terminal; pass --yes to submit unasked");
        return None;
    }
    eprint!("{prompt}");
    let _ = std::io::stderr().flush();
    let mut answer = String::new();
    std::io::stdin().lock().read_line(&mut answer).ok()?;
    Some(answer.trim().to_string())
}
