//! Dependency bookkeeping of a build: the per-run queue records and patch/update branches.

use crate::*;

/// Branch the distro package `request` names from the linked project that ships
/// it into `project`, and the config to build the branch as: same version, so
/// the existing spec is carried forward, with `patch_request` set so the
/// pre-build turn patches it (convention 62). The error is why that is not
/// possible.
pub(crate) fn branch_for_patch(
    dep: &str,
    request: &ap_build::agent::PatchRequest,
    project: &str,
    config: &PipelineConfig,
    own_listing: &std::collections::HashSet<String>,
    linked_listings: &[(String, std::sync::Arc<std::collections::HashSet<String>>)],
    reporter: &dyn ap_core::sink::Reporter,
    emit: ap_build::proc::Emit<'_>,
) -> Result<PipelineConfig, String> {
    let rpm = ap_build::osc::rpm_name(dep);
    // A package this project already builds, with a defect a dependent found
    // (an RPM that lacks files it needs, a spec left unfinished): it is fixed
    // where it is, from its own recipe, not branched from anywhere.
    let own_key = ap_build::osc::fuzzy_key(&rpm);
    if let Some(real) = own_listing
        .iter()
        .find(|p| {
            let k = ap_build::osc::fuzzy_key(p);
            k == own_key || k == ap_build::osc::fuzzy_key(dep)
        })
        .cloned()
    {
        let version = ap_build::update::spec_for_package(project, &real, emit)
            .ok()
            .and_then(|text| ap_build::spec::parse_header(&text))
            .map(|h| h.version)
            .ok_or_else(|| format!("cannot read the Version of {project}/{real}"))?;
        ap_core::diag!("  fixing {real} {version} in {project} for another package");
        return Ok(PipelineConfig {
            patch_provenance: false,
            pkg_name: dep.to_string(),
            rpm_name: real.to_lowercase(),
            version,
            source_repo: None,
            upstream_tag: None,
            subprojects: Vec::new(),
            force: true,
            regen: false,
            patch_request: Some(request.problem.clone()),
            patch_condition: request.condition.clone(),
            bug_fix: None,
            update: false,
            unknown_build_system: false,
            ..config.clone()
        });
    }
    let (origin, shipped) = linked_listings
        .iter()
        .find_map(|(path_project, listing)| {
            listing
                .iter()
                .find(|p| {
                    let k = ap_build::osc::fuzzy_key(p);
                    k == ap_build::osc::fuzzy_key(&rpm) || k == ap_build::osc::fuzzy_key(dep)
                })
                .map(|p| (path_project.clone(), p.clone()))
        })
        .ok_or_else(|| {
            format!(
                "no linked project of {project} ships it, so there is nothing to branch and                  patch (the package that asked for the patch: {})",
                request.problem
            )
        })?;
    // The branch lives under the spelling the project already uses, if any.
    let real = ap_build::osc::obs_name(own_listing, &shipped);
    match ap_build::osc::branch_package(&origin, &shipped, project, false, emit) {
        Ok(ap_build::osc::BranchResult::Branched(b)) if b.target_package != shipped => {
            return Err(format!(
                "OBS branched {origin}/{shipped} as {}, not {shipped}; delete it and patch                  {shipped} by hand",
                b.target_package
            ));
        }
        // Branched or already there: the patch is made on the branch, which
        // builds with the flags its origin has.
        Ok(_) => copy_build_flags(reporter, project, &origin, &shipped, emit),
        Err(e) => {
            return Err(format!(
                "cannot branch {origin}/{shipped} into {project}: {e}"
            ));
        }
    }
    ap_build::osc::invalidate_project_packages_cache(project);
    let version = ap_build::update::spec_for_package(project, &real, emit)
        .ok()
        .and_then(|text| ap_build::spec::parse_header(&text))
        .map(|h| h.version)
        .ok_or_else(|| format!("cannot read the Version of {project}/{real} after branching it"))?;
    ap_core::diag!("  patching {real} {version} (branched from {origin}) for another package");
    Ok(PipelineConfig {
        patch_provenance: false,
        pkg_name: dep.to_string(),
        rpm_name: real.to_lowercase(),
        version,
        source_repo: None,
        upstream_tag: None,
        subprojects: Vec::new(),
        // The branch builds green unchanged; the patch must still be made.
        force: true,
        regen: false,
        patch_request: Some(request.problem.clone()),
        patch_condition: request.condition.clone(),
        bug_fix: None,
        update: false,
        unknown_build_system: false,
        ..config.clone()
    })
}

/// Dependencies known to be native (C/C++) libraries, by `fuzzy_key`, as the
/// pipeline reported them: a translated name no longer shows it, and the queue
/// would look it up on PyPI first.
#[derive(Default)]
pub(crate) struct NativeNames {
    keys: std::cell::RefCell<std::collections::HashSet<String>>,
}

impl NativeNames {
    pub(crate) fn record(&self, names: &[String]) {
        let mut keys = self.keys.borrow_mut();
        for n in names {
            keys.insert(ap_build::osc::fuzzy_key(ap_build::osc::strip_rpm_prefix(n)));
        }
    }

    pub(crate) fn contains(&self, dep: &str) -> bool {
        self.keys
            .borrow()
            .contains(&ap_build::osc::fuzzy_key(ap_build::osc::strip_rpm_prefix(
                dep,
            )))
    }
}

/// Dependencies the repository has at a version older than a package's
/// constraint wants, by `fuzzy_key`: they are updated, not built from scratch
/// or dropped as available.
#[derive(Default)]
pub(crate) struct OutdatedNames {
    /// By `fuzzy_key`, with the release the constraint names, when it names one.
    keys: std::cell::RefCell<std::collections::HashMap<String, Option<String>>>,
}

impl OutdatedNames {
    pub(crate) fn key(dep: &str) -> String {
        ap_build::osc::fuzzy_key(ap_build::osc::strip_rpm_prefix(dep))
    }

    pub(crate) fn record(&self, names: &[(String, Option<String>)]) {
        let mut keys = self.keys.borrow_mut();
        for (n, required) in names {
            keys.insert(Self::key(n), required.clone());
        }
    }

    pub(crate) fn contains(&self, dep: &str) -> bool {
        self.keys.borrow().contains_key(&Self::key(dep))
    }

    pub(crate) fn required(&self, dep: &str) -> Option<String> {
        self.keys.borrow().get(&Self::key(dep)).cloned().flatten()
    }
}

/// The config to bring `dep`, which the repository has at an older version than
/// a dependent needs, up to the newest PyPI release: its existing recipe is
/// carried forward (a package of this project as it is, a distro package after
/// the pipeline branches it). The error says why that is not possible.
pub(crate) fn update_config_for_outdated(
    dep: &str,
    required: Option<&str>,
    project: &str,
    config: &PipelineConfig,
    own_listing: &std::collections::HashSet<String>,
    reporter: &dyn Reporter,
    emit: ap_build::proc::Emit<'_>,
) -> Result<PipelineConfig, String> {
    let rpm = ap_build::osc::rpm_name(dep);
    let real = own_listing
        .iter()
        .find(|p| {
            let k = ap_build::osc::fuzzy_key(p);
            k == ap_build::osc::fuzzy_key(&rpm) || k == ap_build::osc::fuzzy_key(dep)
        })
        .cloned()
        .unwrap_or(rpm);
    let header = ap_build::update::spec_for_package(project, &real, emit)
        .ok()
        .and_then(|text| ap_build::spec::parse_header(&text));
    let current = header.as_ref().map(|h| h.version.clone());
    // Where the recipe says its upstream is decides where "newest" comes from:
    // a package that does not come from PyPI is not looked up there, whatever
    // an unrelated PyPI project of the same name has to offer.
    let upstream = header
        .as_ref()
        .map(ap_build::update::declared_upstream)
        .unwrap_or(ap_build::update::DeclaredUpstream::Unknown);
    let (newest, source_repo) = match upstream {
        ap_build::update::DeclaredUpstream::Git(url) => {
            let v = ap_build::sources::latest_release(&url)
                .map_err(|e| format!("the latest release of {url} is unknown: {e}"))?;
            (v, Some(url))
        }
        // The dependency's own name decides, not the dependent's: a Python
        // package can need a C library, which PyPI knows nothing about.
        _ if real.to_ascii_lowercase().starts_with("python-")
            || upstream == ap_build::update::DeclaredUpstream::Pypi =>
        {
            // The release the constraint names, when PyPI has it: the packages
            // of one release family are pinned to each other, and the newest of
            // one may not match the rest. Otherwise the newest release.
            let named = required.filter(|v| {
                ap_build::pipeline::fetch_pypi_json(dep, v)
                    .ok()
                    .flatten()
                    .is_some()
            });
            let v = match named {
                Some(v) => v.to_string(),
                None => pypi_latest_version(dep, reporter)
                    .ok_or_else(|| "it has no PyPI release to update to".to_string())?,
            };
            (v, None)
        }
        _ => {
            return Err(format!(
                "{real} does not name a PyPI or git upstream, so there is no way to tell \
                 its newest release"
            ));
        }
    };
    if let Some(req) = required
        && ap_build::update::is_newer(req, &newest)
    {
        return Err(format!(
            "upstream's newest release of {real} is {newest}, below the {req} the dependent \
             asks for: no release can meet that bound, so drop it from the spec (or patch the \
             dependent) instead of requesting {real} again"
        ));
    }
    if current.as_deref() == Some(newest.as_str()) {
        return Err(format!(
            "{real} is already at the newest release ({newest}); a newer one than upstream \
             has cannot be built"
        ));
    }
    // Never "update" to something older than the recipe already has.
    if let Some(cur) = current.as_deref()
        && !ap_build::update::is_newer(&newest, cur)
    {
        return Err(format!(
            "upstream's newest release of {real} ({newest}) is not newer than {cur}"
        ));
    }
    ap_core::diag!(
        "  updating {real} from {} to {newest} for a dependent that needs a newer one",
        current.as_deref().unwrap_or("the distro's version")
    );
    Ok(PipelineConfig {
        patch_provenance: false,
        pkg_name: dep.to_string(),
        rpm_name: real.to_lowercase(),
        version: newest,
        source_repo,
        upstream_tag: None,
        subprojects: Vec::new(),
        force: true,
        regen: false,
        patch_request: None,
        patch_condition: None,
        bug_fix: None,
        update: false,
        unknown_build_system: false,
        ..config.clone()
    })
}

/// The dependencies a package asked to have patched instead of built
/// (convention 62), by the name's `fuzzy_key` without its `python-` prefix, so
/// the dependency queue can tell them from the ones it builds.
#[derive(Default)]
pub(crate) struct PatchRequests {
    by_key: std::cell::RefCell<std::collections::HashMap<String, ap_build::agent::PatchRequest>>,
}

impl PatchRequests {
    pub(crate) fn key(dep: &str) -> String {
        ap_build::osc::fuzzy_key(ap_build::osc::strip_rpm_prefix(dep))
    }

    pub(crate) fn record(&self, patches: &[ap_build::agent::PatchRequest]) {
        let mut by_key = self.by_key.borrow_mut();
        for p in patches {
            by_key.insert(Self::key(&p.name), p.clone());
        }
    }

    pub(crate) fn get(&self, dep: &str) -> Option<ap_build::agent::PatchRequest> {
        self.by_key.borrow().get(&Self::key(dep)).cloned()
    }
}

/// Why each dependency left the dependency queue without being built (or what
/// became of it when it was), kept so that the message that finally gives up on
/// a package can say why every one of its dependencies is out of reach instead
/// of pointing at earlier output that never said.
///
/// Keyed by the name with its separators and `python-` prefix ignored, since
/// the same project comes back spelled differently from one agent turn to the
/// next. The first reason recorded for a dependency is kept: it is the reason
/// it was dropped; later drops of the same name only repeat that it was seen
/// before. [`DepDrops::note_outcome`] overwrites, as the outcome of a build.
#[derive(Default)]
pub(crate) struct DepDrops {
    reasons: std::cell::RefCell<std::collections::HashMap<String, String>>,
}

impl DepDrops {
    pub(crate) fn key(dep: &str) -> String {
        ap_build::osc::fuzzy_key(ap_build::osc::strip_rpm_prefix(dep))
    }

    /// Record why `dep` is dropped from the queue and report it as a warning.
    pub(crate) fn dropped(&self, reporter: &dyn Reporter, dep: &str, reason: impl Into<String>) {
        let reason = reason.into();
        reporter.report(&Event::Warning {
            step: None,
            message: format!("dependency {dep} dropped from the build queue: {reason}"),
        });
        self.reasons
            .borrow_mut()
            .entry(Self::key(dep))
            .or_insert(reason);
    }

    /// Record what became of a dependency that was queued and built.
    pub(crate) fn note_outcome(&self, dep: &str, outcome: impl Into<String>) {
        self.reasons
            .borrow_mut()
            .insert(Self::key(dep), outcome.into());
    }

    /// The reason recorded for `dep`, if any.
    pub(crate) fn reason(&self, dep: &str) -> Option<String> {
        self.reasons.borrow().get(&Self::key(dep)).cloned()
    }

    /// `deps` with the reason each is out of reach, one per line, for a
    /// message that gives up on the package needing them.
    pub(crate) fn explain(&self, deps: &[String]) -> String {
        deps.iter()
            .map(|d| {
                let why = self.reason(d).unwrap_or_else(|| {
                    "queued earlier this run, but nothing was recorded about what \
                     became of it"
                        .to_string()
                });
                format!("  - {d}: {why}")
            })
            .collect::<Vec<_>>()
            .join("\n")
    }
}
