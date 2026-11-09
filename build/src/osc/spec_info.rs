//! Facts read from a spec or from the listing: the spec sections, the source
//! version, and the empty or missing package warnings.

use super::*;

/// Section headers of an RPM spec, for [`spec_section`].
const SPEC_SECTIONS: &[&str] = &[
    "%package",
    "%description",
    "%prep",
    "%conf",
    "%generate_buildrequires",
    "%build",
    "%install",
    "%check",
    "%pre",
    "%post",
    "%preun",
    "%postun",
    "%pretrans",
    "%posttrans",
    "%files",
    "%changelog",
];

/// The lines of every `name` section (`%files`, `%install`, ...) of `spec`,
/// header lines excluded.
pub(super) fn spec_section<'a>(spec: &'a str, name: &str) -> Vec<&'a str> {
    let mut inside = false;
    let mut lines = Vec::new();
    for line in spec.lines() {
        let word = line.split_whitespace().next().unwrap_or("");
        if SPEC_SECTIONS.contains(&word) {
            inside = word == name;
            continue;
        }
        if inside {
            lines.push(line);
        }
    }
    lines
}

/// Whether `spec` is an unfinished skeleton whose package builds but ships
/// next to nothing, and why: a `%files` that lists only `%license`/`%doc`
/// (nothing the build installed is packaged), or an `%install` that is empty
/// apart from comments next to a `TODO`. Both are what the skeleton
/// generators leave for a monorepo or a wheel they could not lay out, and OBS
/// builds either one "successfully".
pub fn unfinished_spec_reason(spec: &str) -> Option<String> {
    let body = spec.split("\n%changelog").next().unwrap_or(spec);
    let is_code = |l: &&str| {
        let t = l.trim();
        !t.is_empty() && !t.starts_with('#')
    };
    let files: Vec<&str> = spec_section(body, "%files")
        .into_iter()
        .filter(is_code)
        .collect();
    let files_listed = files.iter().any(|l| {
        let t = l.trim_start();
        !["%license", "%doc", "%defattr", "%dir"]
            .iter()
            .any(|p| t.starts_with(p))
    });
    let mut reasons = Vec::new();
    if body.contains("\n%files") && !files_listed {
        reasons.push("its %files lists nothing but %license/%doc".to_string());
    }
    if !spec_section(body, "%install").iter().any(is_code)
        && body
            .lines()
            .any(|l| l.trim_start().starts_with('#') && l.contains("TODO"))
    {
        reasons.push("its %install has no commands, only TODO comments".to_string());
    }
    (!reasons.is_empty()).then(|| reasons.join(" and "))
}

/// The `<version>` of a `/source/<project>/<package>?view=info&parse=1`
/// answer, or `None` when it names none (a package with no parsable spec).
pub(super) fn parse_sourceinfo_version(xml: &str) -> Option<String> {
    let rest = &xml[xml.find("<version>")? + "<version>".len()..];
    let version = rest[..rest.find("</version>")?].trim();
    (!version.is_empty()).then(|| version.to_string())
}

/// The version `pkg_name` has in the sources of `project`, read from the spec
/// OBS parsed, or `None` when it cannot be told. The version a linked project
/// ships is what a dependent's bound has to be met by, so this is how the
/// closure check learns it without checking the package out.
pub fn source_version(project: &str, pkg_name: &str) -> Option<String> {
    let xml = osc_api(&format!(
        "/source/{}/{}?view=info&parse=1",
        enc(project),
        enc(pkg_name)
    ))?;
    parse_sourceinfo_version(&xml)
}

/// A warning for a package OBS reports `succeeded` whose result a dependent
/// cannot use, or `None` when nothing is wrong or it cannot be told. The test
/// is deterministic: the spec OBS holds is an unfinished skeleton (see
/// [`unfinished_spec_reason`]), and the RPMs it published are listed with
/// their sizes as evidence. A dependent installs such a package and still
/// finds its modules missing, which reads as "the dependency is missing".
pub fn empty_package_warning(
    project: &str,
    pkg_name: &str,
    repo: &str,
    arch: &str,
) -> Option<String> {
    let spec_name = package_spec_name(project, pkg_name, Emit::quiet());
    let spec = osc_api(&format!(
        "/source/{}/{}/{}",
        enc(project),
        enc(pkg_name),
        enc(&spec_name)
    ))?;
    let reason = unfinished_spec_reason(&spec)?;
    let sizes = osc_api(&format!(
        "/build/{}/{}/{}/{}",
        enc(project),
        enc(repo),
        enc(arch),
        enc(pkg_name)
    ))
    .map(|xml| obs_status::parse_binary_rpm_sizes(&xml))
    .unwrap_or_default();
    Some(empty_package_message(pkg_name, project, &reason, &sizes))
}

pub(super) fn empty_package_message(
    pkg: &str,
    project: &str,
    reason: &str,
    sizes: &[(String, u64)],
) -> String {
    let published = if sizes.is_empty() {
        String::new()
    } else {
        let list: Vec<String> = sizes
            .iter()
            .map(|(f, b)| format!("{f}: {b} bytes"))
            .collect();
        format!(" (published: {})", list.join(", "))
    };
    format!(
        "{pkg} is built in {project}, but its spec there is an unfinished skeleton: {reason}, \
         so the RPMs it published are effectively empty{published}. A package that needs it \
         installs it and still cannot import it. Finish {pkg}'s spec, then rebuild it with \
         `auto-package build --project {project} --force <name>`"
    )
}

/// Package names to queue for building, from the RPM names a build or an
/// agent reports missing: see [`crate::depclass::triage_all`], here without
/// asking the distro (the names come from OBS saying the target has nothing
/// providing them). Callers that want to tell the user why a name was left out
/// use `triage_all` directly.
pub fn missing_package_names(rpm_names: &[String]) -> Vec<String> {
    crate::depclass::triage_all(rpm_names, &|_| false).0
}

/// Whether `name` is an RPM capability (a shared library such as
/// `libcuda.so.1()(64bit)`, a file path, an `rpmlib(...)` feature) rather than
/// a package name. OBS reports these as `nothing provides` too, but there is
/// no project to build for them: `libcuda.so.1` is the NVIDIA driver's own
/// library (convention 3 (section 60)), supplied at runtime, so queueing it as a package
/// can never succeed. The fix is on the package that requires it (an
/// `%global __requires_exclude`), which convention 3 (section 60) tells the agent.
pub fn is_rpm_capability(name: &str) -> bool {
    let name = name.trim();
    if name.starts_with('/') || name.starts_with("rpmlib(") || name.contains("()") {
        return true;
    }
    // `lib<x>.so`, `.so.<n>`: not a name that merely contains ".so".
    name.match_indices(".so")
        .any(|(i, _)| matches!(name[i + 3..].chars().next(), None | Some('.') | Some('(')))
}
