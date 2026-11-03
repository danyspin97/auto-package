//! `update --revision REV --to-version VERSION`: branch one specific revision
//! of a package and update it to one specific version instead of the newest.
//!
//! The argument checks, the plan decision for a forced version and the check
//! that the version exists upstream are here; the checks that need the network
//! take their lookups as closures so the decision is tested without it.

use crate::osc;
use crate::proc::Emit;
use crate::spec;
use crate::update::{self, Candidate, Decision, Ecosystem, ReadError, SkipReason};

/// Whether `s` is an OBS revision: a number, or a srcmd5 (32 hex digits).
pub fn is_revision(s: &str) -> bool {
    let digits = !s.is_empty() && s.len() <= 9 && s.bytes().all(|b| b.is_ascii_digit());
    let md5 = s.len() == 32 && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'));
    digits || md5
}

/// Whether `s` can be an RPM `Version:`: no `-`, no whitespace or slash, and
/// it starts like a release number or a word.
pub fn is_rpm_version(s: &str) -> bool {
    s.starts_with(|c: char| c.is_ascii_alphanumeric())
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '+' | '~' | '^'))
}

/// The checks of `--revision` and `--to-version` that need no network: both
/// name one package, so exactly one `--only`; neither works on the project
/// itself (`in_place` has no branch to pin, and a version is built from the
/// recipe of what is branched), and `--to-version` is for `update` only (`lint`
/// keeps the version).
pub fn validate_args(
    only: &[String],
    revision: Option<&str>,
    to_version: Option<&str>,
    in_place: bool,
    improve: bool,
) -> Result<(), String> {
    if revision.is_none() && to_version.is_none() {
        return Ok(());
    }
    let flags = match (revision, to_version) {
        (Some(_), Some(_)) => "--revision and --to-version",
        (Some(_), None) => "--revision",
        _ => "--to-version",
    };
    if only.len() != 1 {
        return Err(format!(
            "{flags} names one package: pass exactly one with --only (got {})",
            only.len()
        ));
    }
    if improve {
        return Err(format!(
            "{flags} is only for update: lint audits the newest revision and keeps its version"
        ));
    }
    if in_place {
        return Err(format!(
            "{flags} cannot be combined with --in-place: they need a branch to put the package in"
        ));
    }
    if let Some(r) = revision
        && !is_revision(r)
    {
        return Err(format!(
            "--revision {r:?} is neither an OBS revision number nor a srcmd5 (32 hex digits)"
        ));
    }
    if let Some(v) = to_version
        && !is_rpm_version(v)
    {
        return Err(format!(
            "--to-version {v:?} cannot be an RPM Version (no '-', spaces or slashes)"
        ));
    }
    Ok(())
}

/// How a forced version compares with the packaged one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    Upgrade,
    Downgrade,
    Same,
    /// Neither is newer by the comparison rules (a snapshot, a computed version).
    Unordered,
}

pub fn direction(old: &str, new: &str) -> Direction {
    if old == new {
        Direction::Same
    } else if update::is_newer(new, old) {
        Direction::Upgrade
    } else if update::is_newer(old, new) {
        Direction::Downgrade
    } else {
        Direction::Unordered
    }
}

/// A note for the plan and the summary: empty for an ordinary upgrade.
pub fn direction_note(old: &str, new: &str) -> &'static str {
    match direction(old, new) {
        Direction::Downgrade => "downgrade",
        Direction::Unordered => "version order unknown, not checked as newer",
        _ => "",
    }
}

/// The plan line for a version forced with `--to-version`.
pub fn forced_line(pkg: &str, old: &str, new: &str, revision: Option<&str>) -> String {
    let at = revision.map_or(String::new(), |r| format!(" of revision {r}"));
    let note = match direction_note(old, new) {
        "" => String::new(),
        n => format!(" ({n})"),
    };
    format!("{pkg}: updating{at} to the requested version {new}, packaged {old}{note}")
}

/// A check that `version` of the package exists where `ecosystem` says its
/// source lives: `(ecosystem, packaged version, version)`. `Ok` carries the
/// forge's tag for the version when the forge lists one (`v2.9.0-1`).
pub type Verify<'a> = &'a dyn Fn(&Ecosystem, &str, &str) -> Result<Option<String>, String>;

/// Decide the plan for `rpm_name` when the target is forced to `to_version`:
/// the first place the spec's source points at that has the version decides.
/// None having it is a skip that says why; the same version as packaged is
/// up to date. An older version is an update like any other (a downgrade).
pub fn classify_forced(
    rpm_name: &str,
    fetch_spec: &dyn Fn(&str) -> Result<String, ReadError>,
    verify: Verify<'_>,
    service_git: Option<String>,
    to_version: &str,
) -> Candidate {
    let skipped = |reason| Candidate {
        rpm_name: rpm_name.to_string(),
        decision: Decision::Skipped(reason),
        notes: Vec::new(),
    };
    let text = match fetch_spec(rpm_name) {
        Ok(text) => text,
        Err(e) => return skipped(e.into()),
    };
    let Some(header) = spec::parse_header(&text) else {
        return skipped(SkipReason::SpecHeaderMissing);
    };
    let mut failures: Vec<String> = Vec::new();
    for ecosystem in update::ecosystems(&header, service_git) {
        if !ecosystem.can_build() {
            continue;
        }
        match verify(&ecosystem, &header.version, to_version) {
            Ok(tag) => {
                // A dashed tag (`v2.9.0-1`) travels with the decision, as plan
                // carries it, so the spec editor can write the tag into the spec.
                let upstream_tag = tag.filter(|t| t.contains('-'));
                let decision = if header.version == to_version {
                    Decision::UpToDate {
                        ecosystem,
                        version: header.version,
                    }
                } else {
                    Decision::Update {
                        ecosystem,
                        old_version: header.version,
                        new_version: to_version.to_string(),
                        upstream_tag,
                    }
                };
                return Candidate {
                    rpm_name: rpm_name.to_string(),
                    decision,
                    notes: Vec::new(),
                };
            }
            Err(e) => failures.push(e),
        }
    }
    skipped(SkipReason::VersionNotFound {
        version: to_version.to_string(),
        why: if failures.is_empty() {
            "the spec's Source: names no PyPI project, forge or release URL the build can fetch"
                .to_string()
        } else {
            failures.join("; ")
        },
    })
}

/// The real check for [`classify_forced`], through the paths the build
/// fetches by: PyPI's JSON for the release, the forge's tag list, and a HEAD
/// request for the spec's source URL with the version swapped in.
pub fn verify_upstream(
    ecosystem: &Ecosystem,
    old: &str,
    version: &str,
) -> Result<Option<String>, String> {
    match ecosystem {
        Ecosystem::Pypi { pypi_name } => match crate::pipeline::fetch_pypi_json(pypi_name, version)
        {
            Ok(Some(_)) => Ok(None),
            Ok(None) => Err(format!("PyPI has no release {version} of {pypi_name}")),
            Err(e) => Err(e),
        },
        Ecosystem::GitRepo { url } => crate::sources::tag_for_version(url, version)
            .map(Some)
            .ok_or_else(|| {
                format!("{url} lists no tag for version {version} (or its API did not answer)")
            }),
        Ecosystem::Index { url } => {
            let expanded = url.replace(old, version);
            if expanded == *url {
                return Err(format!(
                    "the source URL {url} does not contain the packaged version {old}, so it \
                     cannot be pointed at {version}"
                ));
            }
            crate::http::head(&expanded)
                .call()
                .map(|_| None)
                .map_err(|e| format!("{expanded}: {e}"))
        }
        Ecosystem::Repology { .. } => Err("Repology names no source to fetch".to_string()),
        Ecosystem::Rubygems { .. } => Err("rubygems.org names no source to fetch".to_string()),
        Ecosystem::Cpan { .. } => Err("MetaCPAN names no source to fetch".to_string()),
    }
}

/// The spec of `project/pkg` at `revision` (else the newest), or why it
/// cannot be read (the inner error). A revision the package does not have is
/// an error of its own (the outer one).
pub fn spec_at(
    project: &str,
    pkg: &str,
    revision: Option<&str>,
    emit: Emit<'_>,
) -> Result<Result<String, ReadError>, String> {
    let Some(rev) = revision else {
        return Ok(update::spec_for_package(project, pkg, emit));
    };
    let files =
        osc::package_files_at_revision(project, pkg, rev, emit).map_err(|e| e.to_string())?;
    let Some(file) = update::pick_spec_filename(pkg, &files) else {
        return Err(format!("{project}/{pkg} has no spec at revision {rev}"));
    };
    Ok(osc::cat_file_at_revision(project, pkg, &file, rev, emit)
        .ok_or_else(|| ReadError::Failed(format!("reading {file} at revision {rev} failed"))))
}

/// The one package of `project` that `wanted` accepts, for a plan that names
/// its package: several matches (OBS names differing in case) are an error.
pub fn the_package(
    project: &str,
    wanted: &dyn Fn(&str) -> bool,
    emit: Emit<'_>,
) -> Result<String, String> {
    let packages = osc::try_list_project_packages(project, emit)?;
    let mut hits: Vec<&String> = packages.iter().filter(|n| wanted(n)).collect();
    hits.sort();
    match hits.as_slice() {
        [one] => Ok((*one).clone()),
        [] => Err(format!("{project} has no such package")),
        many => Err(format!(
            "--only matches several packages of {project}: {}",
            many.iter()
                .map(|s| s.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        )),
    }
}

/// Plan the one package of `project` for `--revision` / `--to-version`:
/// reads its spec at `revision` (the newest when `None`) and decides with the
/// target forced to `to_version` when given, else with the ordinary lookup of
/// the newest release.
pub fn plan_one(
    project: &str,
    wanted: &dyn Fn(&str) -> bool,
    revision: Option<&str>,
    to_version: Option<&str>,
    emit: Emit<'_>,
) -> Result<Candidate, String> {
    let pkg = the_package(project, wanted, emit)?;
    // Fails early when the revision does not exist.
    let spec = spec_at(project, &pkg, revision, emit)?;
    let fetch = |_: &str| spec.clone();
    // The `_service` of the package as it is at `revision` (else the newest): an
    // obs_scm source is built from the repository it names.
    let fetch_service = |name: &str| match revision {
        Some(rev) => osc::cat_file_at_revision(project, name, "_service", rev, emit),
        None => osc::cat_file(project, name, "_service", emit),
    };
    let candidate = match to_version {
        Some(v) => {
            let service_git = spec
                .as_deref()
                .ok()
                .and_then(spec::parse_header)
                .and_then(|h| update::obs_scm_repo(&h, &pkg, &fetch_service));
            classify_forced(&pkg, &fetch, &verify_upstream, service_git, v)
        }
        None => {
            // The ordinary lookup of the newest release, as the sweep does it.
            use crate::upstream_cache::lookup;
            let pypi = |n: &str| {
                lookup(&format!("pypi:{n}"), || {
                    ap_ecosystem::pypi::fetch_latest_version(n).map_err(|e| e.to_string())
                })
            };
            let repo = |u: &str| lookup(&format!("repo:{u}"), || crate::sources::latest_release(u));
            let index = |u: &str| {
                lookup(&format!("index:{u}"), || {
                    crate::upstream::latest_in_index(u)
                })
            };
            let repology = |n: &str| {
                lookup(&format!("repology:{n}"), || {
                    crate::upstream::latest_on_repology(n)
                })
            };
            let in_series = |u: &str, s: &[u64]| {
                lookup(&format!("repo-series:{u}:{s:?}"), || {
                    crate::sources::latest_release_in_series(u, s)
                })
            };
            let gem = |g: &str| {
                lookup(&format!("rubygems:{g}"), || {
                    crate::upstream::latest_on_rubygems(g)
                })
            };
            let cpan = |d: &str| {
                lookup(&format!("cpan:{d}"), || {
                    crate::upstream_cpan::latest_on_metacpan(d)
                })
            };
            // An obs_scm source is looked up in the repository its `_service`
            // names, then the release listing, then Repology.
            let lookups = update::Lookups {
                fetch_pypi_latest: &pypi,
                fetch_repo_latest: &repo,
                fetch_index_latest: &index,
                fetch_repology_latest: &repology,
                fetch_gem_latest: &gem,
                fetch_cpan_latest: &cpan,
                fetch_service: &fetch_service,
                fetch_has_service: &|pkg: &str| update::project_has_service(project, pkg, emit),
                fetch_release_date: &|url: &str| update::cached_release_date(url),
                fetch_repo_in_series: &in_series,
            };
            update::classify(&pkg, &fetch, &lookups)
        }
    };
    crate::upstream_cache::flush();
    Ok(candidate)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn only(names: &[&str]) -> Vec<String> {
        names.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn revisions_are_numbers_or_srcmd5() {
        assert!(is_revision("12"));
        assert!(is_revision("0123456789abcdef0123456789abcdef"));
        for bad in [
            "",
            "r12",
            "12a",
            "0123456789ABCDEF0123456789ABCDEF",
            "abc",
            "-1",
        ] {
            assert!(!is_revision(bad), "{bad}");
        }
    }

    #[test]
    fn a_target_version_must_be_an_rpm_version() {
        assert!(is_rpm_version("1.2.3~rc1"));
        for bad in ["", "1.2-3", "1 2", "../1", "-1"] {
            assert!(!is_rpm_version(bad), "{bad}");
        }
    }

    #[test]
    fn the_flags_need_exactly_one_package() {
        let ok = |o: &[&str], r, v| validate_args(&only(o), r, v, false, false);
        assert!(ok(&[], None, None).is_ok());
        assert!(ok(&["a", "b"], None, None).is_ok());
        assert!(ok(&["a"], Some("3"), None).is_ok());
        assert!(ok(&["a"], None, Some("1.0")).is_ok());
        assert!(ok(&["a"], Some("3"), Some("1.0")).is_ok());
        for (o, r, v) in [
            (&[][..], Some("3"), None),
            (&["a", "b"][..], Some("3"), None),
            (&[][..], None, Some("1.0")),
            (&["a", "b"][..], Some("3"), Some("1.0")),
        ] {
            let e = ok(o, r, v).unwrap_err();
            assert!(e.contains("exactly one"), "{e}");
        }
    }

    #[test]
    fn the_flags_refuse_lint_in_place_and_bad_values() {
        let one = only(&["a"]);
        let lint = validate_args(&one, None, Some("1.0"), false, true).unwrap_err();
        assert!(lint.contains("only for update"), "{lint}");
        let place = validate_args(&one, Some("3"), None, true, false).unwrap_err();
        assert!(place.contains("--in-place"), "{place}");
        assert!(validate_args(&one, Some("zz"), None, false, false).is_err());
        assert!(validate_args(&one, None, Some("1-2"), false, false).is_err());
    }

    const SPEC: &str = "\
Name:           python-attrs
Version:        26.1.0
Source:         https://files.pythonhosted.org/packages/source/a/attrs/attrs-%{version}.tar.gz
";

    fn plan(to: &str, has: &'static [&'static str]) -> Candidate {
        let spec = |_: &str| Ok(SPEC.to_string());
        let verify = move |_: &Ecosystem, _: &str, v: &str| {
            if has.contains(&v) {
                Ok(None)
            } else {
                Err(format!("PyPI has no release {v} of attrs"))
            }
        };
        classify_forced("python-attrs", &spec, &verify, None, to)
    }

    #[test]
    fn a_forced_newer_version_is_an_update() {
        let c = plan("27.0.0", &["27.0.0"]);
        assert!(matches!(
            c.decision,
            Decision::Update { ref old_version, ref new_version, .. }
                if old_version == "26.1.0" && new_version == "27.0.0"
        ));
        assert_eq!(direction_note("26.1.0", "27.0.0"), "");
    }

    #[test]
    fn a_forced_older_version_is_an_update_called_a_downgrade() {
        let c = plan("25.4.0", &["25.4.0"]);
        assert!(matches!(
            c.decision,
            Decision::Update { ref new_version, .. } if new_version == "25.4.0"
        ));
        assert_eq!(direction("26.1.0", "25.4.0"), Direction::Downgrade);
        assert_eq!(direction_note("26.1.0", "25.4.0"), "downgrade");
        let line = forced_line("python-attrs", "26.1.0", "25.4.0", Some("4"));
        assert_eq!(
            line,
            "python-attrs: updating of revision 4 to the requested version 25.4.0, \
             packaged 26.1.0 (downgrade)"
        );
    }

    #[test]
    fn the_packaged_version_is_up_to_date() {
        assert!(matches!(
            plan("26.1.0", &["26.1.0"]).decision,
            Decision::UpToDate { .. }
        ));
    }

    #[test]
    fn a_version_upstream_lacks_is_skipped_with_the_reason() {
        let c = plan("99.0", &[]);
        let Decision::Skipped(reason) = c.decision else {
            panic!("expected a skip");
        };
        let text = reason.to_string();
        assert!(
            text.contains("99.0") && text.contains("PyPI has no release 99.0"),
            "{text}"
        );
    }

    #[test]
    fn a_source_nothing_can_fetch_is_skipped() {
        let spec = |_: &str| Ok("Name: x\nVersion: 1\n".to_string());
        let verify = |_: &Ecosystem, _: &str, _: &str| Ok(None);
        let c = classify_forced("x", &spec, &verify, None, "2");
        assert!(matches!(
            c.decision,
            Decision::Skipped(SkipReason::VersionNotFound { .. })
        ));
    }

    /// An obs_scm package names no repository in its spec: a forced version is
    /// looked for in the repository its `_service` names, as the sweep does.
    #[test]
    fn a_forced_version_of_an_obs_scm_package_is_looked_for_in_its_service_repo() {
        const REPO: &str = "https://github.com/MastroHD/ashell";
        let spec =
            |_: &str| Ok("Name: ashell\nVersion: 0.5.0\nSource: ashell.tar.gz\n".to_string());
        // A fake tag list of the repository.
        let tags = ["0.5.0", "0.6.0"];
        let verify = |eco: &Ecosystem, _: &str, v: &str| match eco {
            Ecosystem::GitRepo { url } if url == REPO && tags.contains(&v) => Ok(None),
            _ => Err(format!("no tag {v}")),
        };
        let c = classify_forced("ashell", &spec, &verify, Some(REPO.into()), "0.6.0");
        assert_eq!(
            c.decision,
            Decision::Update {
                ecosystem: Ecosystem::GitRepo { url: REPO.into() },
                old_version: "0.5.0".into(),
                new_version: "0.6.0".into(),
                upstream_tag: None,
            }
        );
        // Without the `_service` there is no repository to look in.
        let c = classify_forced("ashell", &spec, &verify, None, "0.6.0");
        assert!(matches!(
            c.decision,
            Decision::Skipped(SkipReason::VersionNotFound { .. })
        ));
        // A tag the repository does not list is not found.
        let c = classify_forced("ashell", &spec, &verify, Some(REPO.into()), "9.9");
        assert!(matches!(
            c.decision,
            Decision::Skipped(SkipReason::VersionNotFound { .. })
        ));
    }

    /// A forced version that a dashed tag of the forge carries (`2.9.0.1` is the
    /// tag `v2.9.0-1`) is an update that carries that tag, for the spec editor.
    #[test]
    fn a_forced_version_carries_the_dashed_tag_it_is_found_under() {
        const REPO: &str = "https://github.com/CuarzoSoftware/Louvre";
        let spec = |_: &str| {
            Ok("Name: liblouvre\nVersion: 2.8.0.1\n\
                Source: https://github.com/CuarzoSoftware/Louvre/archive/refs/tags/v%{version}.tar.gz\n"
                .to_string())
        };
        // The forge's tags; a version is found under the tag that maps to it.
        let tags = ["v2.8.0-1", "v2.9.0-1"];
        let verify = |eco: &Ecosystem, _: &str, v: &str| match eco {
            Ecosystem::GitRepo { url } if url == REPO => tags
                .iter()
                .find(|t| crate::upstream::rpm_version_for_tag(t).as_deref() == Ok(v))
                .map(|t| Some(t.to_string()))
                .ok_or_else(|| format!("no tag {v}")),
            _ => Err(format!("no tag {v}")),
        };
        let c = classify_forced("liblouvre", &spec, &verify, None, "2.9.0.1");
        assert_eq!(
            c.decision,
            Decision::Update {
                ecosystem: Ecosystem::GitRepo { url: REPO.into() },
                old_version: "2.8.0.1".into(),
                new_version: "2.9.0.1".into(),
                upstream_tag: Some("v2.9.0-1".into()),
            }
        );
    }
}
