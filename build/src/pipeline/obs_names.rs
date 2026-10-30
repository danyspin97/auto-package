//! Resolving the OBS name of a package: the branch errors, the own and linked
//! spellings, and the branch made from a linked project.

use super::*;

/// Why [`branch_from_linked`] did not branch the package.
pub(super) enum BranchError {
    /// The distro ships the package under a genuinely different name (not
    /// merely another case): nothing is branched, and the package is packaged
    /// under its own name as before.
    Renamed(String),
    /// A listing or the branch call failed. A package the distro ships must not
    /// be packaged from scratch behind that failure, so the stage fails.
    Failed { reason: String, transient: bool },
}

impl BranchError {
    fn osc(e: &osc::Error) -> Self {
        Self::Failed {
            reason: e.to_string(),
            transient: e.is_transient(),
        }
    }
}

/// Whether `name` is a plain package name, safe to write into a BuildRequires
/// line: it starts with a letter or digit and holds only letters, digits and
/// `._+-` after that.
pub(super) fn is_provider_stem(name: &str) -> bool {
    let mut chars = name.chars();
    chars.next().is_some_and(|c| c.is_ascii_alphanumeric())
        && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '+' | '-'))
}

/// What a failed project lookup while branching means. Only a clear "no such
/// project" (a 404, or a 403 on a path project we do not own) means that
/// project does not carry the package: a warning, and the lookup counts as
/// empty. Everything else fails the stage, transient or not: a 401, a failed
/// launch of `osc`, a timeout, an unrecognised error. Packaging from scratch
/// behind an unanswered lookup would replace a package the distro ships.
pub(super) fn lookup_failure<T: Default>(
    reason: String,
    transient: bool,
    path_project: bool,
    emit: Emit<'_>,
) -> Result<T, BranchError> {
    if transient || !osc::lookup_says_absent(&reason, path_project) {
        return Err(BranchError::Failed { reason, transient });
    }
    emit.reporter.report(&Event::Warning {
        step: emit.step,
        message: format!("{reason}; treating that project as not carrying the package"),
    });
    Ok(T::default())
}

/// How a project's package list stands against the name the pipeline uses.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum OwnName<'a> {
    /// No package of the project matches the name, up to case and separators.
    Absent,
    /// The project has the package spelled exactly as the pipeline names it.
    Exact,
    /// The project has it under another spelling, which OBS keeps distinct.
    Variant(&'a str),
}

/// Where `rpm_name` stands in `own`. An exact spelling wins over a variant, and
/// the variant reported is the smallest one, so the answer does not depend on
/// the order of the listing.
pub(super) fn own_name<'a>(
    rpm_name: &str,
    own: &'a std::collections::HashSet<String>,
) -> OwnName<'a> {
    let key = osc::fuzzy_key(rpm_name);
    let mut variant: Option<&'a str> = None;
    for p in own.iter().filter(|p| osc::fuzzy_key(p) == key) {
        if p == rpm_name {
            return OwnName::Exact;
        }
        if variant.is_none_or(|v| p.as_str() < v) {
            variant = Some(p.as_str());
        }
    }
    variant.map_or(OwnName::Absent, OwnName::Variant)
}

/// The listed own spelling of `rpm_name` among `spellings`, which all share its
/// fuzzy key: the one equal to the lowercase `rpm_name` when listed, else the
/// lexicographically smallest. The same rule as [`own_name`], and independent
/// of the order the listing came in.
pub(super) fn preferred_own_spelling<'a>(
    rpm_name: &str,
    spellings: &[&'a String],
) -> Option<&'a String> {
    let lower = rpm_name.to_lowercase();
    spellings
        .iter()
        .copied()
        .find(|s| s.as_str() == lower)
        .or_else(|| spellings.iter().copied().min())
}

/// The spelling a linked project lists `rpm_name`'s package under, up to case
/// and separators: the exact spelling when listed, else the smallest variant,
/// so the choice does not depend on the order of the listing.
pub(super) fn linked_spelling<'a>(
    rpm_name: &str,
    listing: &'a std::collections::HashSet<String>,
) -> Option<&'a str> {
    match own_name(rpm_name, listing) {
        OwnName::Exact => listing.get(rpm_name).map(String::as_str),
        OwnName::Variant(v) => Some(v),
        OwnName::Absent => None,
    }
}

/// The OBS package name `rpm_name` is built and checked out under, given the
/// listing of the own project. The project's own spelling wins: a listed
/// case variant is the package. Otherwise the name is `rpm_name`, which is
/// also the name a branch from a linked project is made under (see
/// [`branch_from_linked`]), so a linked project's spelling never changes the
/// answer and is not an input here.
pub(super) fn resolve_obs_name(rpm_name: &str, own: &std::collections::HashSet<String>) -> String {
    match own_name(rpm_name, own) {
        OwnName::Variant(listed) => listed.to_string(),
        OwnName::Exact | OwnName::Absent => rpm_name.to_string(),
    }
}

/// Branch `rpm_name` from the linked project that ships it, when the own project
/// does not have it yet: the package is branched from there so the existing
/// recipe is the starting point, and the branch is made under `rpm_name`.
/// `Ok(None)` when nothing needs branching: the own project has it (under
/// either spelling, see [`resolve_obs_name`]), or no linked project ships it.
/// A linked project that ships it under another name entirely is `Renamed`,
/// and nothing is branched.
pub(super) fn branch_from_linked(
    config: &PipelineConfig,
    rpm_name: &str,
    emit: Emit<'_>,
) -> Result<Option<String>, BranchError> {
    let own = match osc::list_project_packages(&config.project, emit) {
        Ok(own) => own,
        Err(e) => {
            let reason = format!("could not list packages of {}: {e}", config.project);
            lookup_failure(reason, e.is_transient(), false, emit)?
        }
    };
    let key = osc::fuzzy_key(rpm_name);
    match own_name(rpm_name, &own) {
        OwnName::Exact | OwnName::Variant(_) => return Ok(None),
        OwnName::Absent => {}
    }
    // Every project the own project builds against, transitively, nearest
    // first. A project that does not answer (see `lookup_failure`) is treated as
    // not carrying the package, and the walk goes on.
    let start = match osc::build_path_pairs(&config.project, &config.distro, emit) {
        Ok(pairs) => pairs,
        Err(e) => lookup_failure(e.to_string(), e.is_transient(), false, emit)?,
    };
    let linked = osc::walk_path_pairs(
        &config.project,
        start,
        osc::MAX_BUILD_PATH_DEPTH,
        osc::MAX_BUILD_PATH_NODES,
        |p, r, _, expand| {
            let paths = if expand {
                match osc::build_path_pairs(p, r, emit) {
                    Ok(paths) => paths,
                    Err(e) => match lookup_failure::<Option<Vec<osc::PathPair>>>(
                        e.to_string(),
                        e.is_transient(),
                        true,
                        emit,
                    )? {
                        Some(paths) => paths,
                        None => return Ok(None),
                    },
                }
            } else {
                Vec::new()
            };
            let listing =
                match osc::list_project_packages(p, emit) {
                    Ok(listing) => listing,
                    Err(e) => {
                        let reason = format!("could not list packages of {p}: {e}");
                        match lookup_failure::<
                            Option<std::sync::Arc<std::collections::HashSet<String>>>,
                        >(reason, e.is_transient(), true, emit)?
                        {
                            Some(listing) => listing,
                            None => return Ok(None),
                        }
                    }
                };
            Ok(Some((listing, paths)))
        },
    )?;
    for (path_project, _, listing) in linked {
        let Some(shipped) = linked_spelling(rpm_name, &listing) else {
            continue;
        };
        // The lookup is fuzzy, so a name that differs only in case or separators
        // (PyYAML vs pyyaml, pydantic-core vs pydantic_core, ruamel.yaml vs
        // ruamel-yaml) is the same package: the distro's spelling is the source
        // of the branch, and the branch is named `rpm_name`. The own project then
        // lists `rpm_name`, which is the OBS name `resolve_obs_name` gives it.
        // Only a spelling that is not the same package up to that is refused.
        if osc::fuzzy_key(shipped) != key {
            return Err(BranchError::Renamed(format!(
                "{path_project} ships it as {shipped}, not {rpm_name}"
            )));
        }
        let result = osc::branch_package_as(
            &path_project,
            shipped,
            rpm_name,
            &config.project,
            false,
            emit,
        )
        .map_err(|e| BranchError::osc(&e));
        osc::invalidate_project_packages_cache(&config.project);
        return result.map(|_| Some(path_project));
    }
    Ok(None)
}
