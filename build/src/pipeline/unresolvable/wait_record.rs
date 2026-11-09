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

use super::rpm_names::{same_package, wait_dep_satisfied};
use super::wait_bounds::spec_bounds;
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
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WaitRecord {
    pub spec: u64,
    pub rpms: u64,
    pub wait: Wait,
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

fn save(ctx: &RunCtx, wait: Wait) {
    let Some((spec, rpms)) = current(ctx) else {
        return;
    };
    let record = WaitRecord { spec, rpms, wait };
    // Not saving only costs a later try that could have been skipped.
    if let Ok(text) = serde_json::to_string(&record) {
        let _ = std::fs::write(ctx.ws.dir().join(RECORD_FILE), text);
    }
}

/// Before the review turn of a try: `MissingDeps` when the round that decided
/// the last answer is still valid (see [`waiting_on`]), so that no turn, no
/// validation, no commit and no build try is spent on it. A record whose names
/// are RPMs now is discarded, and the try goes on to build.
pub fn precheck(ctx: &RunCtx) -> Option<PipelineResult> {
    precheck_with(ctx, &|dep, bound| branch_meets(ctx, dep, bound))
}

/// [`precheck`] with the repository's copy answered by `branch_meets`.
pub fn precheck_with(
    ctx: &RunCtx,
    branch_meets: &dyn Fn(&str, &str) -> bool,
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
        native, outdated, ..
    } = record.wait
    else {
        return None;
    };
    ctx.reporter.report(&Event::Progress {
        step: None,
        message: format!(
            "{}: still waiting for {}; nothing changed since the last attempt, no build tried",
            ctx.rpm_name,
            deps.join(", ")
        ),
    });
    Some(PipelineResult::MissingDeps {
        rpm_name: ctx.rpm_name.clone(),
        deps,
        patches: Vec::new(),
        native,
        outdated,
    })
}

/// Record that this round ends `MissingDeps` on `deps`.
pub fn save_missing(
    ctx: &RunCtx,
    deps: &[String],
    native: &[String],
    outdated: &[(String, Option<String>)],
) {
    let bounds = std::fs::read_to_string(&ctx.spec_path)
        .map(|text| spec_bounds(&text, deps))
        .unwrap_or_default();
    save(
        ctx,
        Wait::Missing {
            deps: deps.to_vec(),
            native: native.to_vec(),
            outdated: outdated.to_vec(),
            bounds,
        },
    );
}

/// Whether the repository's own copy of `dep` meets `required`, read from its
/// spec. A copy that cannot be read does not say otherwise, so it meets.
fn branch_meets(ctx: &RunCtx, dep: &str, required: &str) -> bool {
    let emit = Emit::run(ctx.reporter);
    let rpm = osc::rpm_name(dep);
    let text = [rpm.as_str(), dep]
        .iter()
        .find_map(|name| crate::update::spec_for_package(&ctx.config.project, name, emit).ok());
    let Some(header) = text.and_then(|t| crate::spec::parse_header(&t)) else {
        return true;
    };
    !crate::update::is_newer(required, &header.version)
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
