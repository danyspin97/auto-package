//! Patch conditions: the patch requests of a package, the gate that checks their
//! condition, and the decline and failure of a request.

use super::*;
use crate::pipeline::archive::*;
use crate::pipeline::python_spec::*;
use crate::pipeline::render::*;

/// How many different patch requests one package takes in total. A dependent
/// words the same defect differently every round (python-torch was asked eleven
/// times to keep `torch/utils/benchmark/utils` in the main package), so the
/// text alone cannot tell a repeat: past this many turns that already patched
/// the package, a further request is treated as met.
const MAX_PATCH_REQUESTS_PER_PACKAGE: usize = 2;

/// Patch turns that may fail a condition before the package goes to its
/// upstream wheel.
const CONDITION_PATCH_TURNS: u32 = 2;

/// Failures of one condition after which the request is over anyway (the
/// patches and the wheel switch were all spent): a dependent is not made to
/// wait on a request nothing can meet.
pub(super) const CONDITION_GIVE_UP: u32 = 3;

pub(super) fn condition_key(cond: &crate::ship_condition::FilesCondition) -> u64 {
    hash_spec(&format!("{cond:?}"))
}

/// Whether a patch request is over for a package in this `state`. With a
/// condition that is the condition: true now (`holds`), found true before, or
/// given up on after [`CONDITION_GIVE_UP`] failed attempts. Without one, the
/// text and the per-package cap of [`patch_request_settled`].
pub(super) fn patch_request_over(
    state: &PkgState,
    request: &crate::agent::PatchRequest,
    holds: &dyn Fn(&crate::ship_condition::FilesCondition) -> bool,
) -> bool {
    match request.condition.as_ref().filter(|c| c.is_valid()) {
        Some(cond) => {
            let key = condition_key(cond);
            state.satisfied_conditions.contains(&key)
                || state.condition_failures_of(key) >= CONDITION_GIVE_UP
                || holds(cond)
        }
        None => patch_request_settled(&state.patched_problems, &request.problem),
    }
}

/// How an `rpm` query is run: the arguments (`-qp --qf ...`, `-qlp`) and the
/// RPM file; the command's stdout, or `None` when it could not be run or failed.
type RpmQuery<'a> = dyn Fn(&[&str], &Path) -> Option<String> + 'a;

/// Runs the `rpm` binary.
fn run_rpm_query(args: &[&str], rpm: &Path) -> Option<String> {
    let out = std::process::Command::new("rpm")
        .args(args)
        .arg(rpm)
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
        .ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

/// The RPMs the project built from `rpm_name`'s spec, with their file lists:
/// the main package and every subpackage, found by their source RPM.
pub(super) fn package_files_from_rpms(
    rpms_dir: &Path,
    rpm_name: &str,
) -> Vec<crate::ship_condition::PackageFiles> {
    package_files_from_rpms_with(rpms_dir, rpm_name, &run_rpm_query)
}

/// [`package_files_from_rpms`] with the `rpm` command run by `query`.
pub(super) fn package_files_from_rpms_with(
    rpms_dir: &Path,
    rpm_name: &str,
    query: &RpmQuery<'_>,
) -> Vec<crate::ship_condition::PackageFiles> {
    fn walk(dir: &Path, depth: usize, out: &mut Vec<PathBuf>) {
        for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
            let path = entry.path();
            if path.is_dir() {
                if depth < 3 {
                    walk(&path, depth + 1, out);
                }
            } else if path.extension().is_some_and(|e| e == "rpm")
                && !path.to_string_lossy().ends_with(".src.rpm")
            {
                out.push(path);
            }
        }
    }
    let key = osc::fuzzy_key(osc::strip_rpm_prefix(rpm_name));
    let mut files = Vec::new();
    walk(rpms_dir, 0, &mut files);
    files.sort();
    let mut out = Vec::new();
    for rpm in files
        .iter()
        .filter(|f| {
            osc::fuzzy_key(&f.file_name().unwrap_or_default().to_string_lossy()).contains(&key)
        })
        .take(40)
    {
        let Some(head) = query(&["-qp", "--qf", "%{SOURCERPM}\n%{NAME}\n"], rpm) else {
            continue;
        };
        let mut lines = head.lines();
        let (source, name) = (lines.next().unwrap_or(""), lines.next().unwrap_or(""));
        if !source
            .strip_prefix(&format!("{rpm_name}-"))
            .is_some_and(|r| r.starts_with(|c: char| c.is_ascii_digit()))
        {
            continue;
        }
        let listing = query(&["-qlp"], rpm).unwrap_or_default();
        out.push(crate::ship_condition::PackageFiles {
            name: name.to_string(),
            files: listing.lines().map(str::to_string).collect(),
            excludes: Vec::new(),
            from_spec: false,
        });
    }
    out
}

/// Whether a condition on `rpm_name` is true now: of the recipe in its project
/// checkout (the patch may be made and not built yet), or of its built RPMs.
/// `checkout_name` is the OBS name the checkout is made under (see
/// [`checkout_paths`]); the RPMs and the condition's own name are `rpm_name`.
pub(super) fn condition_holds_now(
    config: &PipelineConfig,
    rpm_name: &str,
    checkout_name: &str,
    cond: &crate::ship_condition::FilesCondition,
) -> bool {
    use crate::ship_condition::{Verdict, check, spec_packages};
    let (_, spec_path) = checkout_paths(&config.work_dir, &config.project, checkout_name);
    if let Ok(spec) = std::fs::read_to_string(&spec_path)
        && check(cond, &spec_packages(&spec), rpm_name) == Verdict::Satisfied
    {
        return true;
    }
    check(
        cond,
        &package_files_from_rpms(&config.rpms_dir, rpm_name),
        rpm_name,
    ) == Verdict::Satisfied
}

/// The failure count when the state could not be saved: the stored count (0
/// when it cannot be read either) plus the failures this process holds unsaved.
pub(super) fn count_when_unsaved(stored: std::io::Result<u32>, unsaved: u32) -> u32 {
    stored.unwrap_or(0) + unsaved
}

/// After a patch turn on a package with a condition: true of the spec now, the
/// request is met. Not true, the failure is counted: the first one fails the
/// patch (a dependent asks again), the second switches the package to its
/// upstream wheel, which ships every file the sdist's split dropped.
pub(super) fn patch_condition_gate(
    ctx: &RunCtx,
    st: &mut RunState,
    apply: ApplyTurnCtx<'_>,
    cond: &crate::ship_condition::FilesCondition,
    lookup: PypiLookup<'_>,
) -> Result<(), PipelineResult> {
    use crate::ship_condition::{Verdict, check, spec_packages};
    let key = condition_key(cond);
    let spec = std::fs::read_to_string(ctx.spec_path.as_path()).unwrap_or_default();
    let why = match check(cond, &spec_packages(&spec), &ctx.rpm_name) {
        Verdict::Satisfied => {
            update_state(&ctx.ws, ctx.reporter, |s| s.record_condition_satisfied(key));
            return Ok(());
        }
        // A recipe whose `%files` cannot be read says nothing: the patch stands.
        Verdict::Unknown => return Ok(()),
        Verdict::Violated(why) => why,
    };
    // The count comes from the stored state. If the state cannot be saved (the
    // lock timed out, the file is unreadable), it is kept in memory for this
    // process instead, so the cap is still reached.
    // Keyed by package as well as condition: two packages can name the same
    // condition, and one's failures must not count against the other.
    static UNSAVED_FAILURES: std::sync::LazyLock<
        std::sync::Mutex<std::collections::HashMap<(PathBuf, u64), u32>>,
    > = std::sync::LazyLock::new(Default::default);
    let slot = (ctx.ws.dir().to_path_buf(), key);
    // Failures counted in memory by earlier turns that could not be saved are
    // added to the stored count, so the total is never short.
    let unsaved = UNSAVED_FAILURES
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .get(&slot)
        .copied()
        .unwrap_or(0);
    let failures = match PkgState::update(&ctx.ws, |s| {
        s.record_condition_failure_after(key, unsaved);
    }) {
        Ok(state) => {
            UNSAVED_FAILURES
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .remove(&slot);
            state.condition_failures_of(key)
        }
        Err(e) => {
            ctx.reporter.report(&Event::Warning {
                step: None,
                message: format!(
                    "{}: cannot save the condition failure count: {e}",
                    ctx.rpm_name
                ),
            });
            let unsaved_now = {
                let mut counts = UNSAVED_FAILURES.lock().unwrap_or_else(|p| p.into_inner());
                let n = counts.entry(slot).or_insert(0);
                *n += 1;
                *n
            };
            // The stored count is still the total: this failure is only the
            // one not saved yet.
            let stored = PkgState::try_load(&ctx.ws)
                .map(|s| s.condition_failures_of(key))
                .inspect_err(|e| {
                    ctx.reporter.report(&Event::Warning {
                        step: None,
                        message: format!(
                            "{}: cannot read the condition failure count: {e}; counting from 0",
                            ctx.rpm_name
                        ),
                    });
                });
            count_when_unsaved(stored, unsaved_now)
        }
    };
    append_notes(
        ctx.ws.notes(),
        &format!(
            "\n## patch condition not met ({failures} of {CONDITION_PATCH_TURNS})\n{}: {why}\n",
            cond.describe()
        ),
    );
    if failures < CONDITION_PATCH_TURNS {
        return Err(ctx.failed_as(FailureKind::ConditionUnmet, format!(
            "the patch of {} did not make this true: {} ({why}); the request is queued again in \
             this run, and a second failed patch switches the package to the upstream wheel",
            ctx.rpm_name,
            cond.describe()
        )));
    }
    if !ctx.rpm_name.starts_with("python-") {
        return Err(ctx.failed_as(
            FailureKind::Exhausted,
            format!(
                "{} patches did not make this true: {} ({why}); the package has no upstream \
                 wheel to switch to",
                failures,
                cond.describe()
            ),
        ));
    }
    ctx.reporter.report(&Event::Progress {
        step: Some(apply.step.id),
        message: format!(
            "{}: {failures} patches did not make `{}` true; switching to the upstream wheel",
            ctx.rpm_name, cond.path
        ),
    });
    let wheel = pipeline_wheel_outcome(&format!(
        "{failures} patch turns did not make this true ({why}): {}",
        cond.describe()
    ));
    // The switch writes files the caller's snapshot does not hold (the wheel,
    // its spec, the rpmlintrc): a rejected switch removes them too, which needs
    // a snapshot taken right before it.
    let switch_snapshot = CheckoutSnapshot::take(apply.checkout_dir);
    let effect = apply_turn_and_reset_with(
        ApplyTurnCtx {
            snapshot: Some(&switch_snapshot),
            ..apply
        },
        &wheel,
        ctx.reporter,
        &mut st.sdist_top_level,
        &mut st.from_repo_archive,
        lookup,
    );
    match effect {
        Ok(TurnEffect::SwitchedToWheel) => {
            update_state(&ctx.ws, ctx.reporter, |s| s.record_condition_satisfied(key));
            Ok(())
        }
        Ok(_) => Err(ctx.failed_as(
            FailureKind::Exhausted,
            format!(
                "{}: the wheel switch changed nothing after {failures} failed patches",
                ctx.rpm_name
            ),
        )),
        // A refused switch (it would drop a security patch) needs a maintainer.
        Err(e) if e.ends_package() => Err(e.into_failed(ctx)),
        Err(e) => Err(ctx.failed_as(
            FailureKind::Exhausted,
            format!(
                "{}: {failures} patches did not make `{}` true and the upstream wheel is not \
                 usable either: {e}",
                ctx.rpm_name, cond.path
            ),
        )),
    }
}

/// Whether a patch request is over: this very text was patched, or the
/// package has already been patched [`MAX_PATCH_REQUESTS_PER_PACKAGE`] times.
pub(super) fn patch_request_settled(patched_problems: &[u64], problem: &str) -> bool {
    patched_problems.contains(&hash_spec(problem))
        || patched_problems.len() >= MAX_PATCH_REQUESTS_PER_PACKAGE
}

/// Remember that the patch request `problem` was declined, so the dependent
/// does not ask for the same change again (see `patch_request_settled`).
pub(super) fn decline_patch_request(ws: &Workspace, reporter: &dyn Reporter, problem: &str) {
    let h = hash_spec(problem);
    update_state(ws, reporter, |state| {
        if !state.declined_patches.contains(&h) {
            state.declined_patches.push(h);
        }
    });
}

/// Whether a patch turn (convention 62) failed to deliver what a dependent asked
/// for. Not when it edited, and not when it answered `no_change`: that is the
/// turn finding the change already in the recipe (the project's `ffmpeg-8` was
/// licence-clean already), and the dependent is waiting on a package that is
/// built, not on an edit. Failing it re-queued the same request ten times. A
/// `blocked` or other answer is a failure, whether or not the spec was reviewed
/// earlier: a reviewed spec says nothing about whether this request was met.
pub(super) fn patch_turn_failed(
    patching: bool,
    effect: TurnEffect,
    action: &crate::agent::Action,
) -> bool {
    // A switch to the upstream wheel replaces the recipe: the wheel carries every
    // file the dependent looked for, so the request is met, not refused.
    patching
        && !matches!(effect, TurnEffect::Edited | TurnEffect::SwitchedToWheel)
        && !matches!(action, crate::agent::Action::NoChange)
}
