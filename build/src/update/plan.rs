use super::check_pool;
use super::host_gate;
use super::siblings::ProjectSiblings;
use super::*;
use std::cell::Cell;
use std::collections::BTreeSet;

/// Fetch and classify one package. Pure given the injected lookups —
/// factored out of [`plan`] so it is directly testable without talking to
/// OBS, PyPI, or a git forge.
pub fn classify<SpecLookup>(
    rpm_name: &str,
    fetch_spec: &SpecLookup,
    lookups: &Lookups<'_>,
) -> Candidate
where
    SpecLookup: Fn(&str) -> Result<String, ReadError>,
{
    classify_in_project(rpm_name, fetch_spec, lookups, &ProjectSiblings::none())
}

/// [`classify`] for a package of a project: `siblings` holds every package of
/// it, so a package can be held to its major by a sibling that is on a newer
/// series (`influxdb` beside `influxdb2`).
pub fn classify_in_project<SpecLookup>(
    rpm_name: &str,
    fetch_spec: &SpecLookup,
    lookups: &Lookups<'_>,
    siblings: &ProjectSiblings<'_>,
) -> Candidate
where
    SpecLookup: Fn(&str) -> Result<String, ReadError>,
{
    let spec_text = match fetch_spec(rpm_name) {
        Ok(text) => text,
        Err(e) => return skipped(rpm_name, e.into()),
    };
    let Some(header) = spec::parse_header(&spec_text) else {
        return skipped(rpm_name, SkipReason::SpecHeaderMissing);
    };
    // The first place that knows a version decides. When none does, the first
    // failure is the one reported: it belongs to the most specific guess.
    let mut first_failure: Option<SkipReason> = None;
    let mut pypi_miss: Option<SkipReason> = None;
    // The forge's own failure, whatever guess came before it in the order.
    let mut git_failure: Option<SkipReason> = None;
    // The PyPI name came from the tarball's file name, not from a PyPI URL.
    let pypi_guessed = {
        let source = header.source.as_deref().unwrap_or_default();
        pypi::pypi_name_from_source_url(source).is_none()
            && pypi::pypi_name_and_version_from_wheel_filename(source).is_none()
    };
    let service_git = obs_scm_repo(&header, rpm_name, lookups.fetch_service);
    let mut found = None;
    // Repology marks no release as newest for some projects: the package's own
    // repository (its `tar_scm` service) is then asked, as a last resort.
    let mut repology_no_newest = false;
    for ecosystem in ecosystems(&header, service_git) {
        match lookup(&ecosystem, lookups) {
            Ok(v) => {
                found = Some((ecosystem, v));
                break;
            }
            Err(failure) => {
                repology_no_newest |= matches!(&ecosystem, Ecosystem::Repology { .. })
                    && matches!(&failure, SkipReason::LookupFailed { error, .. } if error.contains("has no newest version"));
                if matches!(&ecosystem, Ecosystem::GitRepo { .. }) {
                    git_failure.get_or_insert(failure.clone());
                }
                // A PyPI name guessed from a tarball that PyPI has never
                // heard of says nothing about the package; keep it only if no
                // other place has a more telling failure.
                let guessed_miss = matches!(&ecosystem, Ecosystem::Pypi { .. })
                    && pypi_guessed
                    && matches!(&failure, SkipReason::LookupFailed { error, .. } if error.contains("404"));
                if guessed_miss {
                    pypi_miss.get_or_insert(failure);
                } else {
                    first_failure.get_or_insert(failure);
                }
            }
        }
    }
    if found.is_none() && repology_no_newest {
        found = tar_scm_fallback(rpm_name, lookups);
    }
    let Some((ecosystem, latest)) = found else {
        if let Some(why) = suse::no_upstream_release(&header, git_failure.as_ref(), || {
            (lookups.fetch_has_service)(rpm_name)
        }) {
            return skipped(rpm_name, SkipReason::SuseNoUpstream { why });
        }
        return skipped(
            rpm_name,
            first_failure
                .or(pypi_miss)
                .unwrap_or(SkipReason::NoSourceLine),
        );
    };

    // The tag as the forge lists it (`v2.9.0-1`): version_part takes its
    // prefix off, and the spec needs the tag as it is.
    let found_tag = latest.clone();
    let latest = version_part(&latest);
    // A release suffix that only says "this is the release" is not part of it.
    let latest = ["-stable", "-release", "-final", "-src", "-source"]
        .iter()
        .find_map(|s| latest.strip_suffix(s))
        .map_or(latest.clone(), str::to_string);
    // A release that writes every separator as a dash (`1-4-2`) is the version
    // it reads as: an equal one is up to date, not refused.
    let latest = dashes_as_dots(&latest, &header.version).unwrap_or(latest);
    // `0.2.2~0` is the tag `0.2.2`: its zero offset is not a pre-release marker.
    let tag_version = version::strip_tag_offset(&header.version);
    if version::is_prerelease(&latest) && !version::is_prerelease(tag_version) {
        return skipped(
            rpm_name,
            SkipReason::Prerelease {
                packaged: header.version,
                latest,
            },
        );
    }
    // A dashed tag (`v2.9.0-1`) of a forge is compared as the RPM version it
    // maps to; the tag itself travels with the decision for the spec editor.
    // Repology and the indexes answer with versions, not tags: a dash there is
    // a distro revision (`1.5-1`) or a scheme nobody has mapped, so it is refused.
    let (latest, upstream_tag) = if latest.contains('-') && !header.version.contains('-') {
        match crate::upstream::rpm_version_for_tag(&latest) {
            Ok(rpm) if matches!(ecosystem, Ecosystem::GitRepo { .. }) => (rpm, Some(found_tag)),
            _ => return skipped(rpm_name, SkipReason::NotAnRpmVersion { latest }),
        }
    } else {
        (latest, None)
    };
    if version::is_snapshot(&header.version) {
        if let Some(decision) = snapshot::against_release(&ecosystem, &header.version, lookups) {
            return Candidate {
                rpm_name: rpm_name.to_string(),
                decision,
                notes: Vec::new(),
            };
        }
        return skipped(
            rpm_name,
            SkipReason::Snapshot {
                packaged: header.version,
                latest,
            },
        );
    }
    if let Some(known) = computed_prefix(&header.version) {
        // Only the upstream base the spec shows is compared (`5.3` of
        // `5.3.%(…)`). A newer base is for a maintainer: the script that
        // computes the patch level is not run, so nothing is updated here.
        let decision = if newer_than_known(&latest, &known) {
            Decision::Skipped(SkipReason::PatchLevelScript {
                packaged: header.version,
                known,
                latest,
            })
        } else if older_than_known(&latest, &known) || same_as_known(&latest, &known) {
            // Upstream is at or behind the base the spec names, whatever the
            // macro adds.
            Decision::UpToDate {
                ecosystem,
                version: header.version,
            }
        } else {
            Decision::Skipped(SkipReason::ComputedVersion {
                packaged: header.version,
                known,
                latest,
            })
        };
        return Candidate {
            rpm_name: rpm_name.to_string(),
            decision,
            notes: Vec::new(),
        };
    }
    let scheme = match ecosystem {
        Ecosystem::Pypi { .. } => Scheme::Pep440,
        _ => Scheme::Generic,
    };
    // Repology is compared only with a plain update: a package held to a series
    // takes the series' own newest, which Repology's newest may not be.
    let mut notes = Vec::new();
    let decision = match compare_versions(&latest, tag_version, scheme) {
        pypi::VersionCompare::Newer => {
            match held_back(rpm_name, &header, &latest, siblings) {
                None => {
                    let update = Decision::Update {
                        ecosystem,
                        old_version: header.version,
                        new_version: latest,
                        upstream_tag,
                    };
                    let (decision, found) = repology::settle(update, &header.name, lookups);
                    notes = found;
                    decision
                }
                // Held to a series or a major, whose newest is already the latest:
                // that is the release taken, and Repology is not compared with it.
                Some(Held::Pinned) => Decision::Update {
                    ecosystem,
                    old_version: header.version,
                    new_version: latest,
                    upstream_tag,
                },
                Some(Held::Stop(reason)) => Decision::Skipped(reason),
                Some(Held::Series(series)) => {
                    held_to_series(ecosystem, header.version, latest, &series, lookups)
                }
            }
        }
        pypi::VersionCompare::NotNewer => Decision::UpToDate {
            ecosystem,
            version: header.version,
        },
        pypi::VersionCompare::Unparseable => Decision::Skipped(SkipReason::VersionCompareFailed {
            packaged: header.version,
            latest,
        }),
    };
    Candidate {
        rpm_name: rpm_name.to_string(),
        decision,
        notes,
    }
}

/// The forge repository an obs_scm package is built from, read from its
/// `_service`. Only a spec that names no repository and downloads from no URL
/// is read for it: that is what an obs_scm source looks like, and the other
/// packages save an OBS read. `None` otherwise.
pub(crate) fn obs_scm_repo(
    header: &spec::Header,
    rpm_name: &str,
    fetch_service: &dyn Fn(&str) -> Option<String>,
) -> Option<String> {
    let downloads_from_url = header
        .expanded_source
        .as_deref()
        .is_some_and(|s| s.starts_with("http://") || s.starts_with("https://"));
    match declared_upstream(header) {
        DeclaredUpstream::Unknown if !downloads_from_url => {
            fetch_service(rpm_name).as_deref().and_then(service_git_url)
        }
        _ => None,
    }
}

/// A package held to `series` takes the newest release of that series when its
/// forge repository can say what it is; any other source is left where it is,
/// and so is a package whose series has no newer release than the one packaged.
fn held_to_series(
    ecosystem: Ecosystem,
    packaged: String,
    latest: String,
    series: &[u64],
    lookups: &Lookups<'_>,
) -> Decision {
    let stop = || {
        Decision::Skipped(SkipReason::OtherSeries {
            series: series
                .iter()
                .map(u64::to_string)
                .collect::<Vec<_>>()
                .join("."),
            packaged: packaged.clone(),
            latest: latest.clone(),
        })
    };
    let Ecosystem::GitRepo { url } = &ecosystem else {
        return stop();
    };
    match (lookups.fetch_repo_in_series)(url, series) {
        Ok(newest)
            if compare_versions(&newest, &packaged, Scheme::Generic)
                == pypi::VersionCompare::Newer =>
        {
            Decision::Update {
                ecosystem,
                old_version: packaged,
                new_version: newest,
                upstream_tag: None,
            }
        }
        Ok(_) => Decision::UpToDate {
            ecosystem,
            version: packaged,
        },
        Err(_) => stop(),
    }
}

/// Whether `latest` is newer than `packaged` in `scheme`'s order, or cannot be
/// told (an unexpanded macro or a word as the packaged version must stay
/// unknown, not become "current"). See [`version::cmp`].
pub(super) fn compare_versions(
    latest: &str,
    packaged: &str,
    scheme: Scheme,
) -> pypi::VersionCompare {
    use pypi::VersionCompare::{Newer, NotNewer, Unparseable};
    match version::cmp(latest, packaged, scheme) {
        Some(std::cmp::Ordering::Greater) => Newer,
        Some(_) => NotNewer,
        None => Unparseable,
    }
}

/// The release number inside what a lookup returned. A tag may carry a project
/// name that is not the repository's (`accel-config-v4.1.9` in the repository
/// `idxd-config`), which no prefix rule can know; the version is what follows
/// the last separator that is followed by a number, without its `v`.
pub(super) fn version_part(found: &str) -> String {
    let found = found.trim();
    if plain_version(found) {
        return version::strip_prefix(found).to_string();
    }
    let bytes = found.as_bytes();
    for (i, b) in bytes.iter().enumerate() {
        if !matches!(b, b'-' | b'_' | b'/') {
            continue;
        }
        let rest = &found[i + 1..];
        let number = version::strip_prefix(rest);
        if number.starts_with(|c: char| c.is_ascii_digit()) && plain_version(number) {
            return number.to_string();
        }
    }
    found.to_string()
}

/// The known start of a `Version:` that a shell macro finishes
/// (`5.3.%(bash get_version_number.sh)` is `5.3`), when that start is a release
/// number.
pub(super) fn computed_prefix(version: &str) -> Option<String> {
    let start = version.find("%(").or_else(|| version.find('%'))?;
    let known = version[..start].trim_end_matches(['.', '-', '_', '+']);
    plain_version(known).then(|| known.to_string())
}

/// Whether `latest` is ahead of `known` in the components `known` has.
pub(super) fn newer_than_known(latest: &str, known: &str) -> bool {
    version::leading_numbers(latest).len() >= version::leading_numbers(known).len()
        && version::cmp_common_prefix(latest, known) == Some(std::cmp::Ordering::Greater)
}

/// Whether `latest` is behind `known` in the components both have.
pub(super) fn older_than_known(latest: &str, known: &str) -> bool {
    version::cmp_common_prefix(latest, known) == Some(std::cmp::Ordering::Less)
}

/// Whether `latest` has every component `known` has, and they are equal.
pub(super) fn same_as_known(latest: &str, known: &str) -> bool {
    version::leading_numbers(latest).len() >= version::leading_numbers(known).len()
        && version::cmp_common_prefix(latest, known) == Some(std::cmp::Ordering::Equal)
}

/// A version that starts like a release number (`2.3.1`, `v1.0-rc2`, `20240101`)
/// and holds only the characters one is written with.
pub(super) fn plain_version(v: &str) -> bool {
    version::strip_prefix(v).starts_with(|c: char| c.is_ascii_digit())
        && v.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_' | '+' | '~'))
}

pub(super) fn skipped(rpm_name: &str, reason: SkipReason) -> Candidate {
    Candidate {
        rpm_name: rpm_name.to_string(),
        decision: Decision::Skipped(reason),
        notes: Vec::new(),
    }
}

/// Classify every package in `project` (optionally filtered to just `only`).
///
/// Hits OBS twice per package (`osc ls` to find the real spec filename, then
/// `osc cat <pkg> <picked>.spec`), plus one PyPI call per PyPI-sourced
/// package and one git-forge API call per repo-sourced one. Returned in
/// sorted RPM-name order so `--check` output is stable across runs. Fails
/// when the project's package listing cannot be read.
pub fn plan(project: &str, only: Option<&str>, emit: Emit<'_>) -> Result<Vec<Candidate>, String> {
    plan_where(
        project,
        &|n| only.is_none_or(|o| osc::fuzzy_key(o) == osc::fuzzy_key(n)),
        emit,
    )
}

/// [`plan`] over the packages of `project` that `wanted` accepts. Packages it
/// rejects are not fetched at all, which is what makes a few names out of a
/// large project cheap.
pub fn plan_where(
    project: &str,
    wanted: &dyn Fn(&str) -> bool,
    emit: Emit<'_>,
) -> Result<Vec<Candidate>, String> {
    let mut candidates = Vec::new();
    plan_each(project, wanted, emit, &mut |c| {
        candidates.push(c);
        true
    })?;
    Ok(candidates)
}

/// [`plan_where`] handing each package to `on` as soon as it is classified,
/// so a caller can start on the first update while the rest are still being
/// checked. `on` returns whether to go on; `false` stops the sweep.
pub fn plan_each(
    project: &str,
    wanted: &dyn Fn(&str) -> bool,
    emit: Emit<'_>,
    on: &mut dyn FnMut(Candidate) -> bool,
) -> Result<(), String> {
    // A failed listing is an error: treating it as "no packages" would make
    // `update` report success for a project it never looked at.
    // The candidates are the source packages OBS lists, never a multibuild flavor
    // (`boost:base`, `rpm:python-rpm`): a flavor is built as part of its source,
    // and a second spec of a package is a file of it.
    let sources = osc::list_source_package_names(project, emit)
        .map_err(|e| format!("could not list packages of {project}: {e}"))?;
    let mut names: Vec<String> = sources.iter().filter(|n| wanted(n)).cloned().collect();
    names.sort();
    let (names, dropped) = spellings::one_spelling_each(names);
    for (dropped, kept) in dropped {
        emit.reporter.report(&Event::Warning {
            step: emit.step,
            message: format!("{project} lists {dropped} and {kept}: only {kept} is checked"),
        });
    }

    // Enumerate the package's files instead of guessing `<pkg>.spec`.
    // AppCo's versioned-library convention gives one OBS package per
    // upstream major version (`pytorch+2.4`, `pytorch+2.5`), each holding a
    // plain `<base-name>.spec` (`pytorch.spec`) that has no relation to the
    // OBS package name. The old guess silently missed every one of these
    // (Oct 6 review finding #3): `demo+1.0` looked for `demo+1.0.spec`,
    // not the actual `demo.spec`.
    // A sibling check reads the specs of the other packages too, so a spec is
    // read once for the whole run.
    let read_specs: SpecCells = Mutex::new(HashMap::new());
    // The packages whose spec this plan read and stashed (see `stash_spec`).
    let stashed: Mutex<BTreeSet<String>> = Mutex::new(BTreeSet::new());
    let fetch_spec = |rpm_name: &str| {
        read_once(&read_specs, rpm_name, || {
            let spec = spec_for_package(project, rpm_name, emit);
            stash_spec(project, rpm_name, spec.as_ref().ok());
            if spec.is_ok() {
                stashed
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .insert(rpm_name.to_string());
            }
            spec
        })
    };
    let fetch_pypi_latest = |pypi_name: &str| {
        upstream_cache::lookup(&format!("pypi:{pypi_name}"), || {
            let _slot = host_gate::enter("pypi.org");
            pypi::fetch_latest_version(pypi_name).map_err(|e| e.to_string())
        })
    };
    let fetch_repo_latest = |url: &str| {
        upstream_cache::lookup(&format!("repo:{url}"), || {
            let _slot = host_gate::enter(host_gate::host_of(url));
            crate::sources::latest_release(url)
        })
    };
    let fetch_index_latest = |url: &str| {
        upstream_cache::lookup(&format!("index:{url}"), || {
            let _slot = host_gate::enter(host_gate::host_of(url));
            crate::upstream::latest_in_index(url)
        })
    };
    let fetch_repology_latest = |name: &str| {
        upstream_cache::lookup(&format!("repology:{name}"), || {
            let _slot = host_gate::enter("repology.org");
            crate::upstream::latest_on_repology(name)
        })
    };
    let fetch_gem_latest = |gem: &str| {
        upstream_cache::lookup(&format!("rubygems:{gem}"), || {
            let _slot = host_gate::enter("rubygems.org");
            crate::upstream::latest_on_rubygems(gem)
        })
    };
    let fetch_cpan_latest = |dist: &str| {
        upstream_cache::lookup(&format!("cpan:{dist}"), || {
            let _slot = host_gate::enter("metacpan.org");
            crate::upstream_cpan::latest_on_metacpan(dist)
        })
    };
    let fetch_service = |rpm_name: &str| osc::cat_file(project, rpm_name, "_service", emit);
    let fetch_repo_in_series = |url: &str, series: &[u64]| {
        upstream_cache::lookup(&format!("repo-series:{url}:{series:?}"), || {
            let _slot = host_gate::enter(host_gate::host_of(url));
            crate::sources::latest_release_in_series(url, series)
        })
    };
    let (hits_before, fetched_before) = upstream_cache::stats();
    let fetch_has_service = |pkg: &str| suse::project_has_service(project, pkg, emit);
    let fetch_release_date = |url: &str| snapshot::cached_release_date(url);
    let sibling_progress = |done: usize, total: usize| {
        let mut message =
            format!("reading the specs of {project} for the sibling check ({done}/{total})");
        if done == 0 {
            message.push_str(": a one-time read for this plan");
        }
        emit.reporter.report(&Event::Progress {
            step: emit.step,
            message,
        });
    };
    let siblings = ProjectSiblings::new(
        sources.iter().cloned(),
        check_pool::check_jobs(),
        &fetch_spec,
        &sibling_progress,
    );
    // The packages are checked `check_jobs()` at a time. `Lookups` holds
    // closures that are not `Sync`, so each check builds its own from the
    // shared ones above.
    let check = |n: &str| {
        let lookups = Lookups {
            fetch_pypi_latest: &fetch_pypi_latest,
            fetch_repo_latest: &fetch_repo_latest,
            fetch_index_latest: &fetch_index_latest,
            fetch_repology_latest: &fetch_repology_latest,
            fetch_gem_latest: &fetch_gem_latest,
            fetch_cpan_latest: &fetch_cpan_latest,
            fetch_service: &fetch_service,
            fetch_has_service: &fetch_has_service,
            fetch_release_date: &fetch_release_date,
            fetch_repo_in_series: &fetch_repo_in_series,
        };
        classify_in_project(n, &fetch_spec, &lookups, &siblings)
    };
    let total = names.len();
    let jobs = check_pool::check_jobs();
    // With a pool a package is reported when its answer is handed on, so the
    // word is "checked", not "checking".
    let progress = |i: usize| {
        let verb = if jobs > 1 { "checked" } else { "checking" };
        emit.reporter.report(&Event::Progress {
            step: emit.step,
            message: format!("{verb} {} ({}/{total})", names[i], i + 1),
        });
    };
    // A check that panics is one package's failed check, not the end of the run.
    let panicked = |name: &str, why: &str| {
        skipped(
            name,
            SkipReason::LookupFailed {
                ecosystem_tag: "check".into(),
                name: name.into(),
                error: format!("the check panicked: {why}"),
            },
        )
    };
    let handed = Mutex::new(BTreeSet::new());
    let stopped = Cell::new(false);
    check_pool::check_in_order(&names, jobs, &check, &panicked, &progress, &mut |i, c| {
        handed
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(names[i].clone());
        let keep = on(c);
        if !keep {
            siblings.abandon();
        }
        stopped.set(!keep);
        keep
    });
    if stopped.get() {
        let handed = handed.into_inner().unwrap_or_else(|e| e.into_inner());
        let stashed = stashed.into_inner().unwrap_or_else(|e| e.into_inner());
        discard_stashes(project, &stashed, &handed);
    }
    upstream_cache::flush();
    let (hits, fetched) = upstream_cache::stats();
    let (hits, fetched) = (hits - hits_before, fetched - fetched_before);
    if hits > 0 {
        emit.reporter.report(&Event::Progress {
            step: emit.step,
            message: format!(
                "upstream versions: {hits} from the cache, {fetched} asked upstream \
                 (--refresh asks them all)"
            ),
        });
    }
    Ok(())
}

#[cfg(test)]
#[path = "plan_tests.rs"]
mod tests;
