//! Startup self-check: what the work directory says that its parts disagree
//! about, found before the run builds on it. Every finding is a warning, never
//! an error: the run goes on, and the finding says what it will do about it.

use crate::osc::BuildStatus;
use crate::pipeline::PkgState;
use crate::workspace::Workspace;
use std::collections::HashMap;
use std::path::Path;

/// Check every package with a `state.json` under `state_root` against its
/// checkout under `checkout_root` and against `statuses`, which is called at
/// most once and says what OBS shows for each package (`None`: unknown, the
/// OBS comparison is skipped). Returns one finding per problem.
pub fn run(
    state_root: &Path,
    checkout_root: &Path,
    statuses: impl FnOnce() -> Option<HashMap<String, BuildStatus>>,
) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(state_root)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.path().join("state.json").is_file())
        .filter_map(|e| e.file_name().into_string().ok())
        .collect();
    names.sort();
    let statuses = statuses().map(|m| {
        m.into_iter()
            .map(|(k, v)| (k.to_lowercase(), v))
            .collect::<HashMap<_, _>>()
    });
    let mut findings = Vec::new();
    for name in names {
        let Some(ws) = Workspace::find_existing(state_root, &[&name]) else {
            continue;
        };
        let mut own: Vec<String> = Vec::new();
        if let Ok(text) = std::fs::read_to_string(ws.state_json())
            && let Err(e) = serde_json::from_str::<PkgState>(&text)
        {
            own.push(format!(
                "{name}: state.json cannot be read ({e}); it is treated as empty"
            ));
        }
        let state = PkgState::load(&ws);
        // The checkout is named by the OBS name the state records, as the
        // pipeline and `reconcile_with_obs` name it; `rpm_name` otherwise.
        let checkout = checkout_root.join(state.obs_name.as_deref().unwrap_or(&name));
        own.extend(check_checkout(&name, &state, &ws, &checkout));
        if let Some(statuses) = &statuses
            && let Some(f) = check_obs(&name, &state, statuses.get(&name.to_lowercase()))
        {
            own.push(f);
        }
        // One line per package: the stale state of a package is one note, not one
        // line per file or per field.
        if !own.is_empty() {
            findings.push(package_summary(&name, &own));
        }
    }
    findings
}

/// The findings of one package on one line: `name: first; second`, each
/// finding's own `name: ` prefix dropped.
fn package_summary(name: &str, findings: &[String]) -> String {
    let prefix = format!("{name}: ");
    let parts: Vec<&str> = findings
        .iter()
        .map(|f| f.strip_prefix(&prefix).unwrap_or(f))
        .collect();
    format!("{name}: {}", parts.join("; "))
}

/// Files the tool itself leaves in a checkout. They are not package content, so
/// an uncommitted one is not a finding: `series` is the quilt scratch of the
/// patch stage (published only when OBS already has one), `state.json` is the
/// bookkeeping of the state directory, and `wheel-origin.txt` and
/// `<pkg>.spec.pre-wheel-switch` are what a wheel switch writes.
pub(crate) fn is_tool_bookkeeping(name: &str) -> bool {
    matches!(name, "series" | "state.json" | "wheel-origin.txt")
        || name.ends_with(".spec.pre-wheel-switch")
}

/// What the checkout and the state directory of one package say about each other.
fn check_checkout(name: &str, state: &PkgState, ws: &Workspace, checkout: &Path) -> Vec<String> {
    let mut out = Vec::new();
    let obs = state.obs_name.as_deref().unwrap_or(name);
    let spec = std::fs::read_to_string(checkout.join(format!("{obs}.spec"))).ok();
    // A spec that builds from a wheel got there by a switch that wrote its
    // provenance into the state directory (see the check below).
    let wheel_spec = spec.as_deref().is_some_and(|spec| {
        spec.lines().any(|l| {
            crate::spec::parse_source_line(l.trim_start(), &["source"], str::to_string).is_some()
                && l.contains(".whl")
        })
    });
    if checkout.is_dir() {
        let archives = source_archives(checkout);
        let tracked = crate::osc::tracked_files(checkout);
        let spec_version = spec
            .as_deref()
            .and_then(|s| crate::spec::parse_header(s).map(|h| h.version));
        let mut untracked = Vec::new();
        let mut on_obs = Vec::new();
        for file in crate::osc::publish_plan(checkout).skipped {
            // The switch's own files have a finding of their own below, and
            // the source archive it replaced is an expected leftover.
            if is_tool_bookkeeping(&file) || (wheel_spec && archives.contains(&file)) {
                continue;
            }
            if tracked.contains(&file) {
                // An archive of another release that OBS holds is removed by
                // the next commit, as a version bump does: nothing to do.
                let other_release = spec
                    .as_deref()
                    .zip(spec_version.as_deref())
                    .is_some_and(|(spec, version)| is_other_release(&file, spec, version));
                if !other_release {
                    on_obs.push(file);
                }
            } else {
                untracked.push(file);
            }
        }
        if !untracked.is_empty() {
            out.push(format!(
                "{name}: {} not named by the spec and not published: delete them, or name them \
                 in the spec",
                untracked.join(", ")
            ));
        }
        if !on_obs.is_empty() {
            out.push(format!(
                "{name}: OBS holds {}, which the spec does not name: the next commit removes it \
                 from OBS unless the spec names it",
                on_obs.join(", ")
            ));
        }
    }
    let Some(spec) = spec else {
        return out;
    };
    let spec_version = crate::spec::parse_header(&spec).map(|h| h.version);
    if state.last_outcome.as_deref() == Some("success")
        && let (Some(built), Some(spec_version)) = (&state.last_built_version, &spec_version)
        && built != spec_version
    {
        out.push(format!(
            "{name}: state.json records a successful build of {built} but the spec says \
             {spec_version}; the package is built again"
        ));
    }
    // A source archive for another release than the spec names: the spec was
    // not bumped with it, or the archive is left from an older release.
    if let Some(spec_version) = &spec_version {
        let archives = source_archives(checkout);
        if !archives.is_empty()
            && !archives
                .iter()
                .any(|a| names_version(a, spec_version) || spec_names_archive(&spec, a))
        {
            out.push(format!(
                "{name}: the spec says {spec_version} but the checkout's source archive(s) are {}",
                archives.join(", ")
            ));
        }
    }
    // None there, or one left in the checkout, means the switch was made by an
    // older version of the tool; the pipeline copies it into the state
    // directory the next time it runs the package.
    let state_has = |f: &str| ws.dir().join(f).is_file();
    let switched = state_has(&format!("{name}.spec.pre-wheel-switch"))
        || checkout
            .join(format!("{name}.spec.pre-wheel-switch"))
            .is_file()
        || checkout.join("wheel-origin.txt").is_file();
    if wheel_spec && switched && !state_has("wheel-origin.txt") {
        out.push(format!(
            "{name}: the spec was switched to an upstream wheel but its wheel-origin.txt is \
             missing from the state directory"
        ));
    }
    out
}

/// Whether `file` is an archive of another release than `version`: a source
/// archive the spec does not name, whose name does not carry `version`.
fn is_other_release(file: &str, spec: &str, version: &str) -> bool {
    crate::osc::ARCHIVE_SUFFIXES
        .iter()
        .any(|s| file.ends_with(s))
        && !names_version(file, version)
        && !spec_names_archive(spec, file)
}

/// Whether the archive file name `archive` names `version` as a whole version:
/// it starts the name or follows a `-`/`_` (a `v` may precede it, as in
/// `v1.0.zip`), and it is followed by the end or an archive extension, not by
/// more of a version. A wheel's name also takes a `-<tag>` after the version
/// (`foo-1.0-py3-none-any.whl`); any other name's `-suffix` names another
/// archive (`foo-1.0-cpp.tar.gz`, `foo-1.0-python.tar.gz`). `1.0` is in
/// `a-1.0.tar.gz`, not in `a-1.0.1.tar.gz` or `a-11.0.tar.gz`, and
/// `foo-1.0-docs.tar.gz` names another archive. The NVIDIA main archives
/// (`…-13.8.0.4-archive.tar.xz`, `…-9.27.0.42_cuda13-archive.tar.xz`) are also
/// accepted.
pub(crate) fn names_version(archive: &str, version: &str) -> bool {
    let bare = version.replace('~', "");
    let is_wheel = archive.ends_with(".whl");
    [version, bare.as_str()].iter().any(|v| {
        !v.is_empty()
            && archive.match_indices(*v).any(|(at, _)| {
                let before = &archive[..at];
                let before = before.strip_suffix(['v', 'V']).unwrap_or(before);
                let starts_here = before.is_empty() || before.ends_with(['-', '_']);
                let rest = &archive[at + v.len()..];
                starts_here
                    && (rest.is_empty()
                        || [".tar", ".zip", ".tgz", "_", "-archive"]
                            .iter()
                            .any(|ext| rest.starts_with(ext))
                        || (is_wheel && (rest.starts_with('-') || rest == ".whl")))
            })
    })
}

/// Whether the spec's own `Source` lines name `archive`, with `%{name}`,
/// `%{version}` and its `%global`/`%define` values expanded. A spec may name an
/// archive with a version that is not its `Version:` (`dmraid-%{src_version}`
/// for `1.0.0.rc16-3` under `Version: 1.0.0.rc16.3`, `popt-%{version}-release`):
/// what the spec names is the archive it builds from.
pub(crate) fn spec_names_archive(spec: &str, archive: &str) -> bool {
    crate::osc::spec_names_source(spec, archive)
        || crate::spec::expanded_sources(spec)
            .iter()
            .any(|source| source.file == archive)
}

/// File names of the source archives in a checkout.
fn source_archives(checkout: &Path) -> Vec<String> {
    let mut found: Vec<String> = std::fs::read_dir(checkout)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| e.file_name().into_string().ok())
        .filter(|n| {
            [".tar.gz", ".tar.xz", ".tar.bz2", ".tar.zst", ".tgz", ".zip"]
                .iter()
                .any(|ext| n.ends_with(ext))
        })
        .collect();
    found.sort();
    found
}

/// Whether OBS and `state.json` disagree about one package.
fn check_obs(name: &str, state: &PkgState, obs: Option<&BuildStatus>) -> Option<String> {
    let obs = obs?;
    let outcome = state.last_outcome.as_deref()?;
    let bad = matches!(
        obs,
        BuildStatus::Failed | BuildStatus::Unresolvable | BuildStatus::Broken
    );
    if outcome == "success" && bad {
        return Some(format!(
            "{name}: state.json says it built, but OBS shows {obs:?}; it is checked again"
        ));
    }
    if matches!(
        outcome,
        "failed" | "interrupted" | "missing_deps" | "skipped"
    ) && *obs == BuildStatus::Succeeded
    {
        return Some(format!(
            "{name}: state.json says {outcome}, but OBS shows it built (the start-up \
             reconcile fetches its RPMs when the checkout is clean and at the recorded version)"
        ));
    }
    None
}

#[cfg(test)]
mod tests;
