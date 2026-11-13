//! Update mode: a package that is already in the project and is brought to a new
//! version. The update is measured against the devel project the package was
//! branched from (its `_link` origin), not against the checkout: a checkout a
//! previous run already bumped says nothing about what the update changes.

use super::*;
use crate::pipeline::commit::missing_license_problem;
use std::collections::BTreeSet;
use std::path::Path;

pub(super) mod spec_changes;
use spec_changes::SpecPasses;

#[cfg(test)]
mod minimum_tests;

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
/// although the unpacked `source` ships a licence file (conventions 63 (section 66), 5 (section 69)).
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

/// The deterministic normalisations of a spec in the validation round, in the
/// order they are applied. On an update (`update`) a pass is kept when leaving
/// it out would leave the build, the install or the package's metadata wrong, or
/// would leave an edit this version makes wrong. The others are style, and an
/// inherited recipe keeps them as the origin has them:
/// - kept: `add_spec_header` (a spec with no licence header gets the standard one;
///   an existing header is never changed), `normalize_python_module_versions` (a name that does not exist),
///   `fix_stray_dollar_macros` (a bare `$python_sitelib` outside its expansion),
///   `fix_missing_alternatives_requires` (an install-time failure),
///   `drop_unused_pytest_build_requires` (a pytest BuildRequires that neither
///   `%check` nor the unpacked tests need, and the origin does not have),
///   `parenthesize_conditional_dependencies` (OBS:
///   `nothing provides if`), `normalize_runtime_python_module` (a runtime name of
///   another flavor), `hoist_dependency_tags` (a Requires written after
///   `%python_subpackages` lands on the wrong subpackage), `dedupe_changelog_header`
///   (a second `%changelog` is not a changelog), `unwrap_python_expand_block` (a
///   `#` rpm reads as a comment), `use_version_macro` (a literal version an
///   update leaves stale), `escape_comment_macros` (a macro in a comment, which
///   rpm expands and rpmlint reports), `own_new_dirs` (a `%dir` for a directory
///   `%files` lists files in and nothing owns, which fails the OBS build),
///   `add_upstream_minimums` (a dependency the update adds or changes, with no
///   bound, gets the minimum the source declares; convention 75).
/// - skipped on an update: `collapse_dist_info_license_lines` (the form of the
///   `%files` licence lines), `normalize_nvidia_license` (licence spelling, which
///   rpmlint reports), `move_check_before_files` (section order),
///   `add_noarch_to_pure_python` (the architecture the origin chose),
///   `drop_unneeded_wheel_build_requires` (a build dependency that is not needed,
///   only slower to build).
///
/// `origin` is the origin's spec on an update and `source` the unpacked sources,
/// `None` when they are not there yet.
pub(super) fn normalize_spec_text(
    current: &str,
    update: bool,
    origin: Option<&str>,
    source: Option<&Path>,
    package: &str,
) -> SpecPasses {
    let pytest_needed = origin
        .is_some_and(|o| o.lines().any(crate::spec::is_pytest_build_requires))
        || source.is_none_or(crate::spec::tests_may_need_pytest);
    let passes = SpecPasses::new(current)
        .apply("add_spec_header", |s| {
            crate::spec_tidy::header_add::add_spec_header(
                s,
                package,
                crate::spec_tidy::header::current_year(),
            )
        })
        .summarise_added("add_spec_header", crate::spec_tidy::header::is_header_line)
        .apply(
            "normalize_python_module_versions",
            crate::spec::normalize_python_module_versions,
        )
        .apply(
            "fix_stray_dollar_macros",
            crate::spec::fix_stray_dollar_macros,
        )
        .apply(
            "fix_missing_alternatives_requires",
            crate::spec::fix_missing_alternatives_requires,
        )
        .apply("drop_unused_pytest_build_requires", |s| {
            crate::spec::drop_unused_pytest_build_requires(s, pytest_needed)
        });
    let passes = if update {
        passes
    } else {
        passes.apply(
            "collapse_dist_info_license_lines",
            crate::spec::collapse_dist_info_license_lines,
        )
    };
    let passes = passes
        .apply(
            "parenthesize_conditional_dependencies",
            crate::spec::parenthesize_conditional_dependencies,
        )
        .apply(
            "normalize_runtime_python_module",
            crate::spec::normalize_runtime_python_module,
        )
        .apply("hoist_dependency_tags", crate::spec::hoist_dependency_tags);
    // A minimum upstream declares on a dependency the update adds or changes
    // (convention 75). The origin's unchanged lines are its own, so they stay.
    let passes = match (update, origin, source) {
        (true, Some(origin), Some(source)) => {
            let upstream = crate::spec::UpstreamMinimums::read(source);
            passes.apply("add_upstream_minimums", |s| {
                crate::spec::add_upstream_minimums(s, origin, &upstream)
            })
        }
        _ => passes,
    };
    let passes = if update {
        passes
    } else {
        passes.apply(
            "normalize_nvidia_license",
            crate::spec::normalize_nvidia_license,
        )
    };
    let passes = passes
        .apply(
            "dedupe_changelog_header",
            crate::spec::dedupe_changelog_header,
        )
        .apply(
            "unwrap_python_expand_block",
            crate::spec_tidy::unwrap_python_expand_block,
        );
    let passes = if update {
        passes
    } else {
        passes.apply(
            "move_check_before_files",
            crate::spec_tidy::move_check_before_files,
        )
    };
    let passes = if update {
        passes
    } else {
        passes.apply(
            "add_noarch_to_pure_python",
            crate::spec_tidy::add_noarch_to_pure_python,
        )
    };
    let passes = passes.apply("use_version_macro", crate::spec_tidy::use_version_macro);
    let passes = if update {
        passes
    } else {
        passes.apply(
            "drop_unneeded_wheel_build_requires",
            crate::spec_tidy::drop_unneeded_wheel_build_requires,
        )
    };
    // A macro in a comment is expanded by rpm and reported by rpmlint, whatever
    // the origin has; the escape is the same on an update.
    let passes = passes.apply("escape_comment_macros", |s| {
        crate::spec_tidy::comment_macros::escape_comment_macros(s).0
    });
    // OBS fails a build for a directory no package owns; the `%files` entries
    // say which directories the package needs, whatever the origin has.
    passes.apply("own_new_dirs", crate::spec_tidy::dir_owners::own_new_dirs)
}
