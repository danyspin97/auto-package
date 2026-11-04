//! After a fix built: the branch must hold the fix patch and apply it. A later
//! turn (the fix-after-build rounds) can drop or disable a patch despite the
//! gates, and then nothing may be submitted.

use super::*;
use ap_build::backport::{self, Applied};
use ap_build::bugfix::{self, FixTask};

/// Whether the fix is in the branch: each staged patch as `classify` reads it
/// from what the branch holds, and the patch the fix turn made (`written`)
/// present and applied by the spec. `committed` reads a file of the branch.
pub(super) fn landed(
    task: &FixTask,
    end: Option<&bugfix::FixEnd>,
    spec: &str,
    committed: &dyn Fn(&str) -> Option<String>,
) -> Result<(), String> {
    let FixTask::Bug(job) = task;
    // Nothing is submitted for a package the fix stage did not settle in this run.
    let Some(end) = end.filter(|e| e.verdict == "fixed") else {
        return Err(
            "the fix did not go in: the fix stage recorded no fixed verdict for this run".into(),
        );
    };
    let written = end.patch.as_deref();
    for p in &job.patches {
        if let Applied::Missing(why) =
            backport::classify(p, committed(&p.file_name).as_deref(), spec)
        {
            return Err(format!("the fix did not go in: {why}"));
        }
    }
    if let Some(name) = written.filter(|n| !job.patches.iter().any(|p| p.file_name == *n)) {
        if committed(name).is_none() {
            return Err(format!("the fix did not go in: the branch has no {name}"));
        }
        if !backport::spec_applies(spec, name) {
            return Err(format!(
                "the fix did not go in: the spec does not apply {name}"
            ));
        }
    }
    Ok(())
}

/// [`landed`] on the branch package, after its build: a fix that is missing is
/// recorded as one that does not apply, for the exit code.
pub(super) fn check(
    branch: &str,
    item: &Item,
    task: &FixTask,
    ctx: &Ctx<'_>,
) -> Result<(), String> {
    let FixTask::Bug(job) = task;
    let emit = Emit::run(ctx.reporter.as_ref());
    let ws = ap_build::workspace::Workspace::find_by_obs_name(&ctx.state_root, &item.pkg);
    let end = ws
        .as_ref()
        .and_then(|w| bugfix::read_fix_end(w.dir(), &job.nonce));
    let spec = update::spec_for_package(branch, &item.pkg, emit)
        .map_err(|e| format!("could not read the spec of {} in {branch}: {e}", item.pkg))?;
    let read = |name: &str| osc::cat_file(branch, &item.pkg, name, emit);
    let result = landed(task, end.as_ref(), &spec, &read);
    if let (Err(why), Some(w), Some(mut end)) = (&result, &ws, end) {
        end.verdict = "does_not_apply".to_string();
        end.reason = why.clone();
        if let Err(e) = bugfix::write_fix_end(w.dir(), &end) {
            ctx.warn(format!("{}: {e}", item.pkg));
        }
    }
    result
}
