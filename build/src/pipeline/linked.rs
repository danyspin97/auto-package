//! Linked projects: the bound conflicts with them, the published defects and the
//! local wheel source.

use super::*;
use crate::pipeline::archive::*;

/// A package a linked project ships whose version misses a bound this package
/// declares for it.
pub(super) struct LinkedConflict {
    pub(super) dep: String,
    pub(super) kind: crate::distro_bounds::Kind,
    pub(super) specifier: String,
    pub(super) shipped: String,
    pub(super) linked_project: String,
    pub(super) linked_pkg: String,
}

/// For each of `names` a linked project ships, whether the shipped version meets
/// the bound this package's `Requires-Dist` puts on it.
pub(super) fn linked_bound_conflicts(
    ctx: &RunCtx,
    step: ap_core::event::StepId,
    names: &[String],
) -> Vec<LinkedConflict> {
    if names.is_empty() {
        return Vec::new();
    }
    let config = ctx.config;
    let emit = Emit::new(ctx.reporter, step);
    // A failure here skips the bound check for this package: say so, so the
    // skipped check is visible rather than a silent "no conflicts".
    let skipped = |why: String| {
        ctx.reporter.report(&Event::Warning {
            step: Some(step),
            message: format!(
                "{}: the bound check against linked projects is skipped: {why}",
                config.pkg_name
            ),
        });
    };
    let requires_json = match fetch_pypi_json(&config.pkg_name, &config.version) {
        Ok(json) => json,
        Err(e) => {
            skipped(format!("could not read the PyPI metadata ({e})"));
            None
        }
    };
    let requires: Vec<String> = requires_json
        .and_then(|json| serde_json::from_str::<serde_json::Value>(&json).ok())
        .and_then(|v| {
            v["info"]["requires_dist"].as_array().map(|a| {
                a.iter()
                    .filter_map(|s| s.as_str().map(str::to_string))
                    .collect()
            })
        })
        .unwrap_or_default();
    if requires.is_empty() {
        return Vec::new();
    }
    let linked = match osc::linked_project_listings(&config.project, &config.distro, emit) {
        Ok(linked) => linked,
        Err(e) => {
            skipped(format!("could not list the linked projects ({e})"));
            return Vec::new();
        }
    };
    let none = Default::default();
    let no_statuses = std::collections::HashMap::new();
    names
        .iter()
        .filter_map(|dep| {
            let crate::depclass::Availability::InLinked { project, pkg } =
                crate::depclass::availability(dep, &none, &no_statuses, &linked)
            else {
                return None;
            };
            let shipped = osc::source_version(&project, &pkg)?;
            let (kind, specifier) =
                crate::distro_bounds::bound_violation(&requires, dep, &shipped)?;
            Some(LinkedConflict {
                dep: dep.clone(),
                kind,
                specifier,
                shipped,
                linked_project: project,
                linked_pkg: pkg,
            })
        })
        .collect()
}

/// `spec` with the defects repaired that make a package build and still be
/// unusable by what needs it: a conditional dependency without parentheses
/// (OBS: `nothing provides if`) and the auditwheel-hashed libraries of a wheel
/// left in its Requires. A package built before these were repaired stays
/// green in OBS while its dependents are unresolvable, so `preflight` also
/// asks this whether an already built package still needs a new revision.
pub(super) fn spec_with_published_defects_fixed(
    spec: &str,
    checkout_dir: &Path,
    rpms_dir: &Path,
) -> String {
    let mut fixed = crate::spec::parenthesize_conditional_dependencies(spec);
    if crate::spec::may_link_driver_library(&fixed) {
        fixed = crate::spec::with_driver_requires_exclude(&fixed);
    }
    // A wheel generated before the sibling wheels that fill its tree in sitelib
    // were known sits in sitearch: the split namespace cannot be imported
    // (convention 83).
    if fixed.contains("python_sitearch")
        && let Some(name) = fixed
            .lines()
            .find_map(|l| l.strip_prefix("Name:"))
            .map(|n| n.trim().trim_start_matches("python-").to_string())
        && let Some(wheel) = local_wheel_source(&fixed, checkout_dir)
        && let Some(layout) = ap_ecosystem::sdist::wheel_layout(&wheel)
    {
        let siblings = crate::spec::sibling_wheels(rpms_dir, checkout_dir.parent(), &name);
        fixed = crate::spec::with_sibling_tree_in_sitelib(&fixed, &layout, &siblings);
    }
    if !fixed.contains("[0-9a-f]{8}")
        && let Some(wheel) = local_wheel_source(&fixed, checkout_dir)
        && ap_ecosystem::sdist::wheel_has_hashed_libs(&wheel)
    {
        fixed = crate::spec::with_hashed_libs_requires_exclude(&fixed);
    }
    fixed
}

/// Whether the spec checked out at `spec_path` carries such a defect.
pub(super) fn spec_has_published_defects(spec_path: &Path, rpms_dir: &Path) -> bool {
    std::fs::read_to_string(spec_path).is_ok_and(|text| {
        let dir = spec_path.parent().unwrap_or(Path::new("."));
        spec_with_published_defects_fixed(&text, dir, rpms_dir) != text
    })
}

/// The prebuilt wheel a spec's `Source` names, when it is in the checkout.
fn local_wheel_source(spec: &str, checkout_dir: &Path) -> Option<PathBuf> {
    let version = crate::spec::parse_header(spec).map(|h| h.version);
    spec.lines()
        .filter_map(|l| crate::spec::parse_source_line(l.trim_start(), &["source"], str::to_string))
        .map(|source| source.file().to_string())
        // `%{version}` is the spec's own, so the file is still the one on disk.
        .map(|f| match &version {
            Some(v) => f.replace("%{version}", v).replace("%version", v),
            None => f.to_string(),
        })
        .filter(|f| f.ends_with(".whl") && !f.contains('%'))
        .map(|f| checkout_dir.join(f))
        .find(|p| p.is_file())
}

/// How a linked project spells the package for `dep` (a bare name), when it is
/// spelled differently: the source package `python-PyYAML` for `pyyaml`. The
/// binary search is case-sensitive and cannot enumerate every mixed-case name, so
/// the project listing (matched by `project_key`) settles what it cannot.
pub(super) fn linked_package_spelling(
    ctx: &RunCtx,
    step: ap_core::event::StepId,
    dep: &str,
) -> Option<String> {
    let emit = Emit::new(ctx.reporter, step);
    let linked =
        osc::linked_project_listings(&ctx.config.project, &ctx.config.distro, emit).ok()?;
    let none = Default::default();
    let no_statuses = std::collections::HashMap::new();
    let crate::depclass::Availability::InLinked { pkg, .. } =
        crate::depclass::availability(dep, &none, &no_statuses, &linked)
    else {
        return None;
    };
    let spelled = osc::strip_rpm_prefix(&pkg).to_string();
    (spelled != dep && osc::project_key(&spelled) == osc::project_key(dep)).then_some(spelled)
}

/// The version a linked project ships of `dep` (a bare upstream name), or `None`
/// when none does or the projects cannot be read.
pub(super) fn linked_shipped_version(
    ctx: &RunCtx,
    step: ap_core::event::StepId,
    dep: &str,
) -> Option<String> {
    let emit = Emit::new(ctx.reporter, step);
    let linked =
        osc::linked_project_listings(&ctx.config.project, &ctx.config.distro, emit).ok()?;
    let none = Default::default();
    let no_statuses = std::collections::HashMap::new();
    let crate::depclass::Availability::InLinked { project, pkg } =
        crate::depclass::availability(dep, &none, &no_statuses, &linked)
    else {
        return None;
    };
    osc::source_version(&project, &pkg)
}

/// The lines of this package's last build log that look like an ABI mismatch
/// with a library it links (`undefined symbol`), or `None`.
pub(super) fn abi_symptom(ctx: &RunCtx) -> Option<String> {
    let log = std::fs::read_to_string(ctx.ws.build_log()).ok()?;
    let lines: Vec<String> = log
        .lines()
        .filter(|l| l.contains("undefined symbol"))
        .map(|l| l.trim().chars().take(300).collect::<String>())
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .take(3)
        .collect();
    (!lines.is_empty()).then(|| lines.join("\n"))
}
