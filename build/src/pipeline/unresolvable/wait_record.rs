//! What the last unresolvable-dependencies round concluded, kept with the
//! spec and the RPMs it was decided on. A build try spends OBS time and an
//! attempt; when nothing has changed since the round that decided its answer,
//! the same try gets the same answer, so it is not started again.
//!
//! Two answers are recorded:
//! - `Missing`: the names OBS reported are not built yet and the package now
//!   waits on them (`MissingDeps`). While the spec is unchanged and none of
//!   those names is an RPM here, the package waits without a build try.
//! - `LocalRetry`: every name OBS reported is already an RPM here, so the
//!   build is retried once. If OBS says unresolvable again with the same spec
//!   and the same RPMs, the retry is not repeated.

use super::built_in_run::built_in_this_run;
use super::rpm_names::{same_package, wait_dep_satisfied};
use super::wait_bounds::spec_bounds;
use super::wait_patience::{built_not_in_branch, carried, exhausted, needs_human_message};
use super::*;
use crate::pipeline::state::hash_spec;
use serde::{Deserialize, Serialize};

/// The file in the package's workspace directory that holds the record.
const RECORD_FILE: &str = "unresolvable-wait.json";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Wait {
    /// The package waits on these names, which are not built yet.
    Missing {
        deps: Vec<String>,
        native: Vec<String>,
        outdated: Vec<(String, Option<String>)>,
        /// The bounds the spec puts on those names (see [`spec_bounds`]); a
        /// record written before they were kept has none.
        #[serde(default)]
        bounds: Vec<(String, String)>,
    },
    /// OBS reported names that are all RPMs here, and was retried once.
    LocalRetry,
}

/// A decision and the state it was made in: the spec and the RPM directory.
/// `consulted` counts the tries that found the wait still standing, and
/// `since` is when the wait was made (unix seconds), both since the last
/// progress (see `wait_patience`). A record written before they existed reads
/// as fresh.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WaitRecord {
    pub spec: u64,
    pub rpms: u64,
    pub wait: Wait,
    #[serde(default)]
    pub consulted: u32,
    #[serde(default)]
    pub since: u64,
}

/// Whether `record` still stands for `spec`: a `Missing` answer none of whose
/// names is satisfied (see [`dep_satisfied`]). Returns the names the package
/// waits on, or `None` when a build try is needed.
pub fn waiting_on(
    record: Option<&WaitRecord>,
    spec: u64,
    rpms_dirs: &[&Path],
    branch_meets: &dyn Fn(&str, &str) -> bool,
) -> Option<Vec<String>> {
    let record = record?;
    if record.spec != spec {
        return None;
    }
    let Wait::Missing {
        deps,
        outdated,
        bounds,
        ..
    } = &record.wait
    else {
        return None;
    };
    let all: Vec<(String, Option<String>)> = outdated
        .iter()
        .cloned()
        .chain(bounds.iter().map(|(n, v)| (n.clone(), Some(v.clone()))))
        .collect();
    if deps.is_empty()
        || deps
            .iter()
            .any(|d| dep_satisfied(rpms_dirs, d, &all, branch_meets))
    {
        return None;
    }
    Some(deps.clone())
}

/// Whether the waited name `dep` is satisfied: an RPM here meets every bound
/// the record holds for it, and the repository's own copy (`branch_meets`) does
/// too. The build runs on OBS, which resolves from that copy, not from an RPM
/// built here.
fn dep_satisfied(
    rpms_dirs: &[&Path],
    dep: &str,
    all: &[(String, Option<String>)],
    branch_meets: &dyn Fn(&str, &str) -> bool,
) -> bool {
    wait_dep_satisfied(rpms_dirs, dep, all)
        && all
            .iter()
            .filter(|(n, _)| same_package(n, dep))
            .all(|(n, bound)| bound.as_deref().is_none_or(|v| branch_meets(n, v)))
}

/// Whether a retry with names that are all RPMs would repeat the last one:
/// the spec and the RPM directory are the ones that retry saw.
pub fn local_retry_repeats(record: Option<&WaitRecord>, spec: u64, rpms: u64) -> bool {
    matches!(
        record,
        Some(r) if r.spec == spec && r.rpms == rpms && r.wait == Wait::LocalRetry
    )
}

/// The names of the RPM files in `rpms_dir`, as one string, in sorted order.
fn rpm_listing(rpms_dir: &Path) -> String {
    let mut names: Vec<String> = std::fs::read_dir(rpms_dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.ends_with(".rpm"))
        .collect();
    names.sort();
    names.join("\n")
}

/// The spec and RPM-directory hashes as they are now.
fn current(ctx: &RunCtx) -> Option<(u64, u64)> {
    let spec = std::fs::read_to_string(&ctx.spec_path).ok()?;
    Some((
        hash_spec(&spec),
        hash_spec(&rpm_listing(&ctx.config.rpms_dir)),
    ))
}

fn load(ws_dir: &Path) -> Option<WaitRecord> {
    let text = std::fs::read_to_string(ws_dir.join(RECORD_FILE)).ok()?;
    serde_json::from_str(&text).ok()
}

fn store(ws_dir: &Path, record: &WaitRecord) {
    // Not saving only costs a later try that could have been skipped.
    if let Ok(text) = serde_json::to_string(record) {
        let _ = std::fs::write(ws_dir.join(RECORD_FILE), text);
    }
}

fn save(ctx: &RunCtx, wait: Wait) {
    let Some((spec, rpms)) = current(ctx) else {
        return;
    };
    store(
        ctx.ws.dir(),
        &WaitRecord {
            spec,
            rpms,
            wait,
            consulted: 0,
            since: 0,
        },
    );
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// The version bound the record keeps for `dep`: from the spec's own lines, or
/// the one OBS's outdated list gives.
fn bound_of(
    dep: &str,
    bounds: &[(String, String)],
    outdated: &[(String, Option<String>)],
) -> Option<String> {
    bounds
        .iter()
        .find(|(n, _)| same_package(n, dep))
        .map(|(_, v)| v.clone())
        .or_else(|| {
            outdated
                .iter()
                .find(|(n, v)| v.is_some() && same_package(n, dep))
                .and_then(|(_, v)| v.clone())
        })
}

/// Before the review turn of a try: `MissingDeps` when the round that decided
/// the last answer is still valid (see [`waiting_on`]), so that no turn, no
/// validation, no commit and no build try is spent on it. A record whose names
/// are RPMs now is discarded, and the try goes on to build. A wait that is
/// exhausted (see `wait_patience`) ends the package as `NeedsMaintainer`.
pub fn precheck(ctx: &RunCtx) -> Option<PipelineResult> {
    precheck_with(ctx, &|dep, bound| branch_meets(ctx, dep, bound), &|dep| {
        branch_version(ctx, dep)
    })
}

/// [`precheck`] with the repository's copy answered by `branch_meets` and
/// `branch_version` (`None` when it cannot be read).
pub fn precheck_with(
    ctx: &RunCtx,
    branch_meets: &dyn Fn(&str, &str) -> bool,
    branch_version: &dyn Fn(&str) -> Option<String>,
) -> Option<PipelineResult> {
    let (spec, _) = current(ctx)?;
    let record = load(ctx.ws.dir())?;
    let local = ctx.config.rpms_dir.join(osc::LOCAL_RPMS_DIR);
    let dirs = [ctx.config.rpms_dir.as_path(), local.as_path()];
    let Some(deps) = waiting_on(Some(&record), spec, &dirs, branch_meets) else {
        if record.spec == spec && matches!(record.wait, Wait::Missing { .. }) {
            // Not waiting any more: the names are RPMs here, so the try builds.
            let _ = std::fs::remove_file(ctx.ws.dir().join(RECORD_FILE));
        }
        return None;
    };
    let Wait::Missing {
        native,
        outdated,
        bounds,
        ..
    } = record.wait.clone()
    else {
        return None;
    };
    let now = now_secs();
    // A record without a creation time reads as made now.
    let created = if record.since == 0 { now } else { record.since };
    let consulted = record.consulted.saturating_add(1);
    if let Some(why) = exhausted(consulted, created, now) {
        let _ = std::fs::remove_file(ctx.ws.dir().join(RECORD_FILE));
        let dep = deps
            .iter()
            .find(|d| bound_of(d, &bounds, &outdated).is_some())
            .or(deps.first())?;
        let bound = bound_of(dep, &bounds, &outdated);
        let branch = bound.as_ref().and_then(|_| branch_version(dep));
        return Some(ctx.failed_as(
            FailureKind::NeedsMaintainer,
            needs_human_message(
                dep,
                why,
                bound.as_deref(),
                branch.as_deref(),
                built_in_this_run(dep),
            ),
        ));
    }
    store(
        ctx.ws.dir(),
        &WaitRecord {
            consulted,
            since: created,
            ..record.clone()
        },
    );
    // A dependency built in this run whose commit is not in the repository's
    // copy yet: the wait says so, since it is the usual cause.
    let notes: Vec<String> = deps
        .iter()
        .filter(|d| {
            built_in_this_run(d)
                && bound_of(d, &bounds, &outdated).is_some_and(|b| !branch_meets(d, &b))
        })
        .map(|d| built_not_in_branch(d))
        .collect();
    let mut message = format!(
        "{}: still waiting for {}; nothing changed since the last attempt, no build tried",
        ctx.rpm_name,
        deps.join(", ")
    );
    for note in notes {
        message.push_str("; ");
        message.push_str(&note);
    }
    ctx.reporter.report(&Event::Progress {
        step: None,
        message,
    });
    Some(PipelineResult::MissingDeps {
        rpm_name: ctx.rpm_name.clone(),
        deps,
        patches: Vec::new(),
        native,
        outdated,
    })
}

/// Record that this round ends `MissingDeps` on `deps`. A record that waits on
/// the same as the one it replaces keeps its counters; anything else is progress
/// and starts them again (see `wait_patience::carried`).
pub fn save_missing(
    ctx: &RunCtx,
    deps: &[String],
    native: &[String],
    outdated: &[(String, Option<String>)],
) {
    let bounds = std::fs::read_to_string(&ctx.spec_path)
        .map(|text| spec_bounds(&text, deps))
        .unwrap_or_default();
    let wait = Wait::Missing {
        deps: deps.to_vec(),
        native: native.to_vec(),
        outdated: outdated.to_vec(),
        bounds,
    };
    let Some((spec, rpms)) = current(ctx) else {
        return;
    };
    let previous = load(ctx.ws.dir());
    let same = previous
        .as_ref()
        .is_some_and(|p| p.spec == spec && p.rpms == rpms && p.wait == wait);
    let (consulted, since) = carried(previous.map(|p| (p.consulted, p.since)), same, now_secs());
    store(
        ctx.ws.dir(),
        &WaitRecord {
            spec,
            rpms,
            wait,
            consulted,
            since,
        },
    );
}

/// The version of `dep` the repository's own copy has, read from its spec.
/// `None` when the copy cannot be read.
fn branch_version(ctx: &RunCtx, dep: &str) -> Option<String> {
    let emit = Emit::run(ctx.reporter);
    let rpm = osc::rpm_name(dep);
    let text = [rpm.as_str(), dep]
        .iter()
        .find_map(|name| crate::update::spec_for_package(&ctx.config.project, name, emit).ok())?;
    crate::spec::parse_header(&text).map(|h| h.version)
}

/// Whether the repository's own copy of `dep` meets `required`, read from its
/// spec. A copy that cannot be read does not say otherwise, so it meets.
fn branch_meets(ctx: &RunCtx, dep: &str, required: &str) -> bool {
    branch_version(ctx, dep).is_none_or(|v| !crate::update::is_newer(required, &v))
}

/// Whether the retry about to be made for names that are all RPMs here would
/// repeat the last one (see [`local_retry_repeats`]). When it would not, the
/// retry is recorded so the next one can be compared with it.
pub fn local_retry_is_new(ctx: &RunCtx) -> bool {
    let Some((spec, rpms)) = current(ctx) else {
        return true;
    };
    if local_retry_repeats(load(ctx.ws.dir()).as_ref(), spec, rpms) {
        return false;
    }
    save(ctx, Wait::LocalRetry);
    true
}
