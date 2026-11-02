//! The other-architecture check of an updated package, as `update` runs it:
//! the check itself lives in `ap_build::arch_check`; this reports it.

use super::*;
use ap_build::arch_check::{self, ArchRow, CheckRequest, LiveArchObs, Port};
use ap_core::event::ArchCheckResult;

/// The `--arches` list, validated, or `None` when it was not given.
pub(super) fn wanted_arches(a: &UpdateProjectArgs) -> Result<Option<Vec<String>>, String> {
    if a.arches.is_empty() {
        return Ok(None);
    }
    arch_check::parse_arch_list(&a.arches.join(",")).map(Some)
}

/// The ports the check uses: every port under `--ports`, and the port of each
/// arch that `--arches` names (which implies it), narrowed to `wanted`.
pub(super) fn ports_for(a: &UpdateProjectArgs, wanted: Option<&[String]>) -> Vec<Port> {
    let named = wanted.is_some_and(|w| w.iter().any(|x| arch_check::port_for_arch(x).is_some()));
    if a.ports || named {
        arch_check::ports_wanted(wanted)
    } else {
        Vec::new()
    }
}

/// Check the package `item` just built in `branch` on the other architectures.
/// Its rows go into the report and an event; a check that cannot run is a
/// warning, never a failed update.
pub(super) fn check_item(
    a: &UpdateProjectArgs,
    branch: &str,
    item: &Item,
    ctx: &Ctx<'_>,
    report: &mut Report,
) {
    if a.no_arch_check {
        return;
    }
    if a.local {
        ctx.say(format!(
            "arch check of {} skipped: --local builds on this machine",
            item.pkg
        ));
        if !a.arches.is_empty() {
            ctx.say(format!(
                "--arches {} is not used with --local: nothing is checked on them",
                a.arches.join(",")
            ));
        }
        return;
    }
    let wanted = match wanted_arches(a) {
        Ok(w) => w,
        Err(e) => {
            ctx.warn(format!("arch check of {} skipped: {e}", item.pkg));
            return;
        }
    };
    let ports = ports_for(a, wanted.as_deref());
    if !ports.is_empty() && item.origin != "openSUSE:Factory" {
        ctx.say(format!(
            "the ports build openSUSE:Factory's sources, not {}'s: {} is judged against Factory",
            item.origin, item.pkg
        ));
    }
    let distro = crate::choose_distro(a.distro.as_deref(), branch, &a.arch);
    let log_dir = ctx.run_dir.path.join("arch-logs");
    if let Err(e) = std::fs::create_dir_all(&log_dir) {
        ctx.warn(format!("arch check of {} skipped: {e}", item.pkg));
        return;
    }
    let say = |message: String| ctx.say(message);
    let req = CheckRequest {
        branch,
        origin: &item.origin,
        pkg: &item.pkg,
        primary: (&distro, &a.arch),
        wanted: wanted.as_deref(),
        // Adding an arch makes OBS schedule every package of the branch on it,
        // so it is done only for the arches asked for by name.
        add_arches: wanted.is_some(),
        ports: ports.clone(),
        // An in-place update's source is only read: it is never written.
        read_only: a.in_place,
        bound: std::time::Duration::from_secs(a.arch_wait_minutes * 60),
        log_dir: &log_dir,
        say: &say,
    };
    ctx.say(format!(
        "checking {} on the other architectures in {branch}",
        item.pkg
    ));
    let out = match arch_check::check_other_arches(
        &LiveArchObs {
            emit: Emit::run(ctx.reporter.as_ref()),
        },
        &req,
    ) {
        Ok(out) => out,
        Err(e) => {
            ctx.warn(format!("arch check of {} did not run: {e}", item.pkg));
            return;
        }
    };
    // Only a check of every branch arch lets the submit skip the wait: a narrowed
    // one leaves the others unawaited (see `prepare_request`).
    if out.covered {
        ctx.arch_checked
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .insert(item.pkg.clone());
    }
    for note in &out.notes {
        ctx.say(format!("{}: {note}", item.pkg));
    }
    let failing: Vec<&ArchRow> = out.rows.iter().filter(|r| r.blocks_submit()).collect();
    if !failing.is_empty() {
        let names: Vec<String> = failing
            .iter()
            .map(|r| format!("{}/{}", r.repo, r.arch))
            .collect();
        ctx.warn(format!(
            "{} fails on {} (see Other architectures in the summary)",
            item.pkg,
            names.join(", ")
        ));
    }
    ctx.reporter.report(&Event::ArchCheck {
        package: item.pkg.clone(),
        results: out.rows.iter().map(event_result).collect(),
    });
    report.arch.extend(out.rows);
}

/// One row as the run log records it.
pub(super) fn event_result(row: &ArchRow) -> ArchCheckResult {
    ArchCheckResult {
        repo: row.repo.clone(),
        arch: row.arch.clone(),
        state: row.state.label(),
        detail: row.state.detail().map(str::to_string),
        origin_fails: row.baseline_fails,
        log: row.log.clone(),
    }
}
