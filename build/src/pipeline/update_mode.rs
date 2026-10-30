//! Update mode: a package that is already in the project and is brought to a new
//! version. The update is measured against the devel project the package was
//! branched from (its `_link` origin), not against the checkout: a checkout a
//! previous run already bumped says nothing about what the update changes.

use super::*;
use crate::pipeline::commit::missing_license_problem;
use std::collections::BTreeSet;
use std::path::Path;

/// What the package's origin says about an update of it.
pub(super) struct OriginBase {
    /// The `Version:` of the origin's spec.
    pub(super) version: String,
    /// The origin's spec text, when it could be read.
    pub(super) spec: Option<String>,
}

/// The origin of `project/obs_name` and its spec, `None` when the package is
/// not a link or the origin cannot be read (reported as a warning).
pub(super) fn origin_base(
    project: &str,
    obs_name: &str,
    reporter: &dyn Reporter,
) -> Option<OriginBase> {
    let emit = Emit::run(reporter);
    let origin = match osc::link_origin(project, obs_name, emit) {
        Ok(Some(origin)) => origin,
        Ok(None) => return None,
        Err(e) => {
            reporter.report(&Event::Warning {
                step: None,
                message: format!(
                    "{obs_name}: reading the origin of the package failed, so the update is \
                     judged from the checkout alone: {e}"
                ),
            });
            return None;
        }
    };
    let spec = osc::cat_file(
        &origin.project,
        &origin.package,
        &format!("{}.spec", origin.package),
        emit,
    );
    let version = spec
        .as_deref()
        .and_then(crate::spec::parse_header)
        .map(|h| h.version)?;
    Some(OriginBase { version, spec })
}

/// The update mode of a run: `Some` when the run is an update (`update`) that
/// keeps the package's recipe (`kept`: carried forward or already checked out).
/// A spec generated from scratch is not an update, whatever the origin ships.
/// The origin's spec is kept for the checks against it, even when the origin is
/// already at the target version.
pub(super) fn update_mode(
    update: bool,
    origin: Option<&OriginBase>,
    kept: bool,
) -> Option<UpdateMode> {
    (update && kept).then(|| UpdateMode {
        origin_spec: origin.and_then(|o| o.spec.clone()),
        reported: BTreeSet::new(),
    })
}

/// The style problems a reviewer sends a spec back for at once: an emoji, a
/// description that only repeats the Summary, and a `%files` with no `%license`
/// although the unpacked `source` ships a licence file (conventions 66, 69).
pub(super) fn style_problems(spec: &str, source: &Path) -> Vec<String> {
    let mut problems = crate::spec::description_problems(spec);
    problems.extend(missing_license_problem(spec, source));
    problems
}

/// The problems of a spec that an update leaves to the origin: those the origin's
/// spec has too (`origin`). The rest are introduced by this version and are for
/// the gate to fix, as they would be for a new package. Returns
/// `(introduced, inherited)`.
pub(super) fn split_inherited(
    problems: Vec<String>,
    origin: &[String],
) -> (Vec<String>, Vec<String>) {
    problems.into_iter().partition(|p| !origin.contains(p))
}

/// The inherited problems of `inherited` that this run has not reported yet;
/// they are recorded in `reported`, so a problem the gate meets again is not
/// reported twice.
pub(super) fn newly_reported(
    reported: &mut BTreeSet<String>,
    inherited: Vec<String>,
) -> Vec<String> {
    inherited
        .into_iter()
        .filter(|p| reported.insert(p.clone()))
        .collect()
}

/// An update of a package that already exists.
pub(super) struct UpdateMode {
    /// The origin's spec text: what a problem must already be in to be
    /// inherited rather than introduced by this update.
    pub(super) origin_spec: Option<String>,
    /// The inherited problems already reported in this run, so that a problem
    /// the gate meets again after each fix is reported once.
    pub(super) reported: BTreeSet<String>,
}
