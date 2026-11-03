//! The commit step: the commit and its message and summary, the publish checks,
//! the recorded outcome, and the retries of service runs.

use super::source_signature::{SignatureCheck, check_signed_sources};
use super::*;
use crate::pipeline::patch_check::*;
use crate::pipeline::prompt_notes::*;

/// The commit step, except under `--local`: nothing goes to OBS from a build
/// that runs on this machine, against the checkout as it is.
pub(super) fn commit_unless_local(ctx: &RunCtx, st: &mut RunState) -> Result<(), PipelineResult> {
    commit_unless(osc::local_build().is_some(), || commit_step(ctx, st))
}

/// Run `commit` unless `local` is set. The gate is a parameter so a test can
/// hold it without enabling `--local` for the whole process.
pub(super) fn commit_unless(
    local: bool,
    commit: impl FnOnce() -> Result<(), PipelineResult>,
) -> Result<(), PipelineResult> {
    if local { Ok(()) } else { commit() }
}

/// The signature check of the new sources, run as soon as they are fetched and
/// before any agent turn: a key the keyring lacks costs nothing then. Only a
/// refusal stops here; a skip is reported by the commit step, which runs the
/// check again as the agent may change the sources. Under `--local` nothing
/// goes to OBS, so nothing is refused.
pub(super) fn check_signatures_early(ctx: &RunCtx) -> Result<(), PipelineResult> {
    early_signature_gate(osc::local_build().is_some(), || {
        check_signed_sources(&ctx.checkout_dir)
    })
    .map_err(|reason| ctx.failed_as(FailureKind::NeedsMaintainer, reason))
}

/// The refusal reason of the early check, `None` when it does not stop the
/// package. `check` is run only when the build is not local.
pub(super) fn early_signature_gate(
    local: bool,
    check: impl FnOnce() -> SignatureCheck,
) -> Result<(), String> {
    if local {
        return Ok(());
    }
    match check() {
        SignatureCheck::Refused(reason) => Err(reason),
        SignatureCheck::Ok | SignatureCheck::Skipped(_) => Ok(()),
    }
}

/// Runs the signature check of the checkout: a refusal fails the package as
/// needing a maintainer, a skip is reported (under `step` when there is one).
fn signature_gate(
    ctx: &RunCtx,
    step: Option<ap_core::event::StepId>,
) -> Result<(), PipelineResult> {
    match check_signed_sources(&ctx.checkout_dir) {
        SignatureCheck::Ok => Ok(()),
        SignatureCheck::Skipped(note) => {
            ctx.reporter.report(&Event::Warning {
                step,
                message: note,
            });
            Ok(())
        }
        SignatureCheck::Refused(reason) => Err(ctx.failed_as(FailureKind::NeedsMaintainer, reason)),
    }
}

/// Step: Commit.
pub(super) fn commit_step(ctx: &RunCtx, st: &mut RunState) -> Result<(), PipelineResult> {
    let commit_step = begin_or_fail!(
        ctx.pkg_run,
        ctx.reporter,
        StepKind::Commit,
        None,
        &mut st.step_attempts,
        ctx.rpm_name.clone()
    );
    // What OBS holds is kept before this step changes the checkout, unless a
    // baseline is already kept: an earlier commit of this run, or an earlier
    // attempt's that the run start restored (or refused to start over). `osc
    // rm` in this step may take a stray's pristine copy with it.
    let baseline = ctx.ws.dir().join(OBS_BASELINE_DIR);
    if !baseline.exists()
        && let Err(e) = osc::save_baseline(&ctx.checkout_dir, &baseline)
    {
        commit_step.finish(StepOutcome::Failed);
        return Err(ctx.failed(format!(
            "not committed: the OBS baseline cannot be kept: {e}"
        )));
    }
    // The only write to OBS: nothing without a spec, without its sources, or
    // with a placeholder in it goes there, whatever an earlier stage let by.
    let expanded = match ensure_publishable(ctx, commit_step.id) {
        Ok(expanded) => expanded,
        Err(reason) => {
            commit_step.finish(StepOutcome::Failed);
            return Err(ctx.failed(reason));
        }
    };
    // A source the spec names must not be missing from what is published. The
    // restore runs first: a source OBS still tracks but the checkout lost is
    // brought back here, and only what is still missing afterwards is refused.
    let restored =
        osc::restore_named_sources(&ctx.checkout_dir, Emit::new(ctx.reporter, commit_step.id));
    for name in &restored {
        ctx.reporter.report(&Event::Warning {
            step: Some(commit_step.id),
            message: format!("restored '{name}' from OBS: the spec names it as a source"),
        });
    }
    if let Err(reason) = ensure_sources_present(ctx, &expanded) {
        commit_step.finish(StepOutcome::Failed);
        return Err(ctx.failed(reason));
    }
    // A signed new archive must verify against the package's keyring before
    // OBS's own validator, which runs inside `osc ci`, sees it.
    if let Err(refused) = signature_gate(ctx, Some(commit_step.id)) {
        commit_step.finish(StepOutcome::Failed);
        return Err(refused);
    }
    for name in osc::restore_dropped_recipe_files(
        &ctx.checkout_dir,
        Emit::new(ctx.reporter, commit_step.id),
    ) {
        ctx.reporter.report(&Event::Warning {
            step: Some(commit_step.id),
            message: format!(
                "{}: restored '{name}' from the package this one was branched from: the spec uses it",
                ctx.rpm_name
            ),
        });
    }
    for name in osc::drop_unused_recipe_files(&ctx.checkout_dir) {
        ctx.reporter.report(&Event::Warning {
            step: Some(commit_step.id),
            message: format!(
                "{}: dropped '{name}': the spec does not use it",
                ctx.rpm_name
            ),
        });
    }
    settle_fix_flags(ctx, &baseline, commit_step.id);
    settle_risk_flags(ctx, &baseline);
    // The entry is rewritten once every turn has added its bullets, so it
    // reads as the net change against what OBS holds (no model call).
    osc::rewrite_unpublished_entry(&ctx.checkout_dir, &baseline);
    osc::wrap_unpublished_changes(&ctx.checkout_dir);
    let plan = osc::publish_plan(&ctx.checkout_dir);
    for name in &plan.skipped {
        ctx.reporter.report(&Event::Warning {
            step: Some(commit_step.id),
            message: format!(
                "not committing '{name}': it is not part of the package \
                 (spec, archives, service files, changelog, rpmlint config or patches)"
            ),
        });
    }
    // What OBS holds that the package no longer names goes with this commit.
    let removed = osc::remove_tracked_strays(
        &ctx.checkout_dir,
        &plan.skipped,
        Emit::new(ctx.reporter, commit_step.id),
    );
    for name in &removed {
        ctx.reporter.report(&Event::Warning {
            step: Some(commit_step.id),
            message: format!("removing '{name}' from OBS: the package does not use it"),
        });
    }
    for name in osc::discard_untracked_archives(&ctx.checkout_dir, &plan.skipped) {
        ctx.reporter.report(&Event::Warning {
            step: Some(commit_step.id),
            message: format!("deleted '{name}' from the checkout: the package does not use it"),
        });
    }
    let mut files = plan.files.clone();
    files.extend(removed);
    // osc reads the checkout's `_service` before it commits; a malformed one
    // stops it with the reason on stdout, so it is refused here, with the line.
    if let Some(why) = osc::service_problem(&ctx.checkout_dir) {
        commit_step.finish(StepOutcome::Failed);
        return Err(ctx.failed(format!("not committed: {why}; nothing was sent to OBS")));
    }
    // Noted before the write: a commit that fails half way may still have sent them.
    if let Err(e) = osc::note_attempt(&baseline, &files) {
        commit_step.finish(StepOutcome::Failed);
        return Err(ctx.failed(format!("not committed: the attempt cannot be noted: {e}")));
    }
    if let Err(e) = osc::commit(
        &ctx.checkout_dir,
        &commit_message(&ctx.checkout_dir, &baseline),
        &files,
        Some(&commit_step.file("osc-ci.log")),
        Emit::new(ctx.reporter, commit_step.id),
    ) {
        return Err(ctx.failed_osc(&e));
    }
    commit_step.finish(StepOutcome::Ok);
    Ok(())
}

/// The `osc ci` message: "auto-package", and when the newest `.changes`
/// entry is not in the OBS baseline of the run (`baseline`) yet, a one-line
/// summary of what it says.
pub(super) fn commit_message(checkout_dir: &Path, baseline: &Path) -> String {
    const BASE: &str = "auto-package";
    let Some(file) = osc::newest_changes_file(checkout_dir) else {
        return BASE.to_string();
    };
    let working = std::fs::read_to_string(&file).unwrap_or_default();
    let pristine = file
        .file_name()
        .map(|n| osc::run_start_copy(checkout_dir, Some(baseline), n))
        .unwrap_or_default();
    match changes_summary(&working, &pristine) {
        Some(summary) => one_line(&format!("{BASE}: {summary}")),
        None => BASE.to_string(),
    }
}

/// `text` as one line for `osc ci -m`: every run of white space or control
/// character is a single space.
pub(super) fn one_line(text: &str) -> String {
    text.split(|c: char| c.is_whitespace() || c.is_control())
        .filter(|w| !w.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

/// The bullets of the newest entry of `working` joined on one line and cut at
/// 100 characters, or `None` when that entry is empty or already in `pristine`.
pub(super) fn changes_summary(working: &str, pristine: &str) -> Option<String> {
    let is_separator = |l: &str| l.len() >= 20 && l.bytes().all(|b| b == b'-');
    let lines: Vec<&str> = working.lines().collect();
    let first = lines.iter().position(|l| is_separator(l.trim()))?;
    let end = lines[first + 1..]
        .iter()
        .position(|l| is_separator(l.trim()))
        .map_or(lines.len(), |i| first + 1 + i);
    if pristine.contains(lines[first..end].join("\n").trim_end()) {
        return None;
    }
    let mut bullets: Vec<String> = Vec::new();
    for l in &lines[(first + 2).min(end)..end] {
        if let Some(b) = l.strip_prefix("- ") {
            bullets.push(b.trim().to_string());
        } else if l.starts_with("  ")
            && !l.trim().is_empty()
            && let Some(last) = bullets.last_mut()
        {
            last.push(' ');
            last.push_str(l.trim());
        }
    }
    let joined = one_line(&bullets.join("; "));
    if joined.is_empty() {
        return None;
    }
    let mut out: String = joined.chars().take(100).collect();
    if joined.chars().count() > 100 {
        out = out
            .rsplit_once(' ')
            .map_or(out.clone(), |(h, _)| h.to_string());
        out = out.trim_end_matches([',', ';', ':', ' ']).to_string();
    }
    Some(out)
}

/// Whether the unpacked source has a Python package (an `__init__.py`) within a
/// few levels, outside any `.dist-info`/`tests` directory.
pub(super) fn ships_python_modules(root: &Path) -> bool {
    fn walk(dir: &Path, depth: usize) -> bool {
        if depth > 4 {
            return false;
        }
        let Ok(entries) = std::fs::read_dir(dir) else {
            return false;
        };
        for e in entries.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            let path = e.path();
            if path.is_dir() {
                if name.ends_with(".dist-info") || name == "tests" || name == "test" {
                    continue;
                }
                if walk(&path, depth + 1) {
                    return true;
                }
            } else if name == "__init__.py" {
                return true;
            }
        }
        false
    }
    walk(root, 0)
}

pub(super) fn spec_text_now(spec_path: &Path) -> String {
    std::fs::read_to_string(spec_path).unwrap_or_default()
}

/// A `%files` without `%license` while the unpacked source ships a licence file
/// (`LICENSE*`, `COPYING*`, or one under a `.dist-info/licenses`), for a spec
/// text, against the unpacked `source`.
pub(super) fn missing_license_problem(spec: &str, source: &Path) -> Option<String> {
    if crate::spec::has_license_file_entry(spec) {
        return None;
    }
    let names: Vec<String> = std::fs::read_dir(&source)
        .ok()?
        .flatten()
        .filter_map(|e| e.file_name().into_string().ok())
        .collect();
    let is_license = |n: &str| {
        let u = n.to_ascii_uppercase();
        ["LICENSE", "LICENCE", "COPYING", "LICENSES"]
            .iter()
            .any(|p| u.starts_with(p))
    };
    let found = names.iter().find(|n| is_license(n)).cloned().or_else(|| {
        names
            .iter()
            .filter(|n| n.ends_with(".dist-info"))
            .find_map(|d| {
                std::fs::read_dir(source.join(d).join("licenses"))
                    .ok()?
                    .flatten()
                    .next()
                    .map(|e| format!("{d}/licenses/{}", e.file_name().to_string_lossy()))
            })
    })?;
    Some(format!(
        "the source ships a licence file ({found}) but `%files` has no `%license` entry for it"
    ))
}

/// Why the checkout must not be committed to OBS yet, or the expanded spec when
/// it may be: the spec is missing, it still has a TODO/FIXME placeholder or is
/// an unfinished skeleton, it does not parse, or a patch escapes the tree. The
/// sources the spec names are checked afterwards by [`ensure_sources_present`],
/// once the commit has restored the ones OBS still tracks.
fn ensure_publishable(ctx: &RunCtx, step: ap_core::event::StepId) -> Result<String, String> {
    let spec = std::fs::read_to_string(&ctx.spec_path)
        .map_err(|_| format!("there is no spec ({}) to commit", ctx.spec_path.display()))?;
    let todos = crate::spec::todo_placeholders(&spec);
    if !todos.is_empty() {
        return Err(format!(
            "the spec still has placeholders and is not committed: {}",
            todos
                .iter()
                .map(|(n, t)| format!("{n}: {t}"))
                .collect::<Vec<_>>()
                .join("; ")
        ));
    }
    if let Some(why) = osc::unfinished_spec_reason(&spec) {
        return Err(format!("the spec is an unfinished skeleton ({why})"));
    }
    // Last line of the patch-escape gate: whatever path kept the agent's
    // edits, a patch that escapes the tree is never committed. Patches OBS
    // already publishes are not judged here: they are not this run's edits.
    // Only the patches the commit would publish are judged (`publish_plan`):
    // an unreferenced patch left in the checkout never ships.
    let published = PatchBaseline::NotPublished(obs_tracked_names(&ctx.checkout_dir));
    let publishable = osc::publish_plan(&ctx.checkout_dir).files;
    if let Some((_, problem)) =
        patch_escape_offender(&ctx.spec_path, &spec, &published, Some(&publishable))
    {
        return Err(format!(
            "a patch escapes the source tree, so it is not committed: {problem}"
        ));
    }
    osc::rpmspec_check(&ctx.spec_path, Emit::new(ctx.reporter, step))
        .map_err(|e| format!("the spec does not parse, so it is not committed: {e}"))
}

/// The sources and patches the expanded spec names that are not in the
/// checkout, as the reason the commit is refused, or `Ok`.
fn ensure_sources_present(ctx: &RunCtx, expanded: &str) -> Result<(), String> {
    let missing = missing_at_checkout(expanded, &ctx.checkout_dir);
    if missing.is_empty() {
        return Ok(());
    }
    Err(format!(
        "the sources the spec names are not in the checkout, so it is not committed: {}",
        missing.join(", ")
    ))
}

/// The `last_outcome` and reason recorded for an unsuccessful result; `None`
/// when the package's recorded outcome is left alone.
pub(super) fn recorded_outcome(result: &PipelineResult) -> Option<(&'static str, String)> {
    match result {
        // Nothing was attempted, so the package's last outcome is left alone.
        PipelineResult::Failed {
            kind: FailureKind::Busy,
            ..
        } => None,
        PipelineResult::Failed {
            reason,
            kind: FailureKind::AgentUnavailable | FailureKind::Transient | FailureKind::DiskFull,
            ..
        } => Some(("interrupted", reason.clone())),
        PipelineResult::Failed { reason, .. } => Some(("failed", reason.clone())),
        PipelineResult::MissingDeps { deps, .. } => Some((
            "missing_deps",
            format!("waiting on dependencies: {}", deps.join(", ")),
        )),
        PipelineResult::Built { .. } | PipelineResult::AlreadyBuilt { .. } => None,
    }
}

/// An older version of the tool wrote a wheel switch's provenance
/// (`wheel-origin.txt`, `<pkg>.spec.pre-wheel-switch`) into the checkout; it
/// now lives in the state directory. Copy what the checkout still holds there
/// when the state directory lacks it. The checkout's copy is left alone: it may
/// be tracked by OBS, and the commit step removes what the package no longer names.
pub(super) fn migrate_wheel_provenance(
    checkout: &Path,
    ws: &Workspace,
    rpm_name: &str,
    reporter: &dyn Reporter,
) {
    let pre_switch = format!(
        "{}.spec.pre-wheel-switch",
        ap_core::run::sanitize_name(rpm_name)
    );
    for file in ["wheel-origin.txt", pre_switch.as_str()] {
        let (from, to) = (checkout.join(file), ws.dir().join(file));
        if from.is_file() && !to.exists() {
            warn_if_err(
                reporter,
                "moving the wheel switch's provenance to the state directory",
                std::fs::read(&from).and_then(|b| ap_core::fsx::write_atomic(&to, b)),
            );
        }
    }
}

/// `osc service runs` (a git tag fetch, a crates.io vendoring), tried again
/// when the network failed it.
pub(super) fn run_service_with_retries(
    dir: &Path,
    package: &str,
    baseline: Option<&Path>,
    emit: Emit<'_>,
) -> Result<(), osc::Error> {
    retry_transient(
        SERVICE_ATTEMPTS,
        SERVICE_PAUSE,
        osc::Error::is_transient,
        || osc::run_service(dir, package, baseline, emit),
    )
}

const SERVICE_ATTEMPTS: u32 = 3;

const SERVICE_PAUSE: std::time::Duration = std::time::Duration::from_secs(20);

/// Call `f` up to `attempts` times, pausing `pause` after each failure that
/// `transient` calls transient; any other failure, and the last one, is returned.
pub(super) fn retry_transient<T, E>(
    attempts: u32,
    pause: std::time::Duration,
    transient: impl Fn(&E) -> bool,
    mut f: impl FnMut() -> Result<T, E>,
) -> Result<T, E> {
    let mut n = 1;
    loop {
        match f() {
            Err(e) if n < attempts && transient(&e) && !crate::proc::shutting_down() => {
                n += 1;
                // In slices, so a shutdown does not wait out the pause.
                let mut left = pause;
                let slice = std::time::Duration::from_millis(500);
                while !left.is_zero() && !crate::proc::shutting_down() {
                    let step = left.min(slice);
                    std::thread::sleep(step);
                    left -= step;
                }
            }
            other => return other,
        }
    }
}
