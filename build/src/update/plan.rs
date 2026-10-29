use super::*;

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
    // The PyPI name came from the tarball's file name, not from a PyPI URL.
    let pypi_guessed = {
        let source = header.source.as_deref().unwrap_or_default();
        pypi::pypi_name_from_source_url(source).is_none()
            && pypi::pypi_name_and_version_from_wheel_filename(source).is_none()
    };
    let mut found = None;
    for ecosystem in ecosystems(&header) {
        let (tag, name, latest) = match &ecosystem {
            Ecosystem::Pypi { pypi_name } => {
                ("PyPI", pypi_name, (lookups.fetch_pypi_latest)(pypi_name))
            }
            Ecosystem::GitRepo { url } => ("git", url, (lookups.fetch_repo_latest)(url)),
            Ecosystem::Index { url } => ("release listing", url, (lookups.fetch_index_latest)(url)),
            Ecosystem::Repology { project } => (
                "Repology",
                project,
                (lookups.fetch_repology_latest)(project),
            ),
        };
        match latest {
            Ok(v) => {
                found = Some((ecosystem, v));
                break;
            }
            Err(error) => {
                let failure = SkipReason::LookupFailed {
                    ecosystem_tag: tag.into(),
                    name: name.clone(),
                    error,
                };
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
    let Some((ecosystem, latest)) = found else {
        return skipped(
            rpm_name,
            first_failure
                .or(pypi_miss)
                .unwrap_or(SkipReason::NoSourceLine),
        );
    };

    let latest = version_part(&latest);
    // A release suffix that only says "this is the release" is not part of it.
    let latest = ["-stable", "-release", "-final", "-src", "-source"]
        .iter()
        .find_map(|s| latest.strip_suffix(s))
        .map_or(latest.clone(), str::to_string);
    if version::is_prerelease(&latest) && !version::is_prerelease(&header.version) {
        return skipped(
            rpm_name,
            SkipReason::Prerelease {
                packaged: header.version,
                latest,
            },
        );
    }
    if latest.contains('-') && !header.version.contains('-') {
        return skipped(rpm_name, SkipReason::NotAnRpmVersion { latest });
    }
    if version::is_snapshot(&header.version) {
        return skipped(
            rpm_name,
            SkipReason::Snapshot {
                packaged: header.version,
                latest,
            },
        );
    }
    if let Some(known) = computed_prefix(&header.version) {
        // Repology's newest is numbered by whichever distribution it came from
        // (ca-certificates "15-GA" against the spec's snapshot "2+git…"), so it
        // is not taken as a release of a version the spec only half names.
        let decision = if newer_than_known(&latest, &known)
            && !matches!(ecosystem, Ecosystem::Repology { .. })
        {
            Decision::Update {
                ecosystem,
                old_version: header.version,
                new_version: latest,
            }
        } else if older_than_known(&latest, &known) {
            // Upstream is behind what the spec already names, whatever the
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
        };
    }
    let scheme = match ecosystem {
        Ecosystem::Pypi { .. } => Scheme::Pep440,
        _ => Scheme::Generic,
    };
    let decision = match compare_versions(&latest, &header.version, scheme) {
        pypi::VersionCompare::Newer => match series_of(rpm_name, &header.version) {
            Some(series) if !in_series(&latest, &series) => {
                Decision::Skipped(SkipReason::OtherSeries {
                    series: series
                        .iter()
                        .map(u64::to_string)
                        .collect::<Vec<_>>()
                        .join("."),
                    packaged: header.version,
                    latest,
                })
            }
            _ => Decision::Update {
                ecosystem,
                old_version: header.version,
                new_version: latest,
            },
        },
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

/// The series a package is pinned to by its name: `fmt-9` and `boost-1.85`
/// name `[9]` and `[1, 85]`. The number must also start the packaged version,
/// or it is part of the name (`foo-2` packaging 1.4) and pins nothing.
pub(super) fn series_of(rpm_name: &str, version: &str) -> Option<Vec<u64>> {
    let (_, suffix) = rpm_name.rsplit_once('-')?;
    if suffix.is_empty()
        || !suffix
            .split('.')
            .all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()))
    {
        return None;
    }
    let series: Vec<u64> = suffix.split('.').filter_map(|p| p.parse().ok()).collect();
    let packaged = version::leading_numbers(version);
    (series.len() == suffix.split('.').count()
        && packaged.len() >= series.len()
        && packaged[..series.len()] == series[..])
        .then_some(series)
}

/// Whether `version` belongs to `series`: it starts with the same numbers.
pub(super) fn in_series(version: &str, series: &[u64]) -> bool {
    version::leading_numbers(version).starts_with(series)
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
    let packages = osc::try_list_project_packages(project, emit)?;
    // A multibuild flavor (`boost:base`) is part of its source package, which is
    // listed too and has the spec; the flavor has none of its own.
    let mut names: Vec<String> = packages
        .iter()
        .filter(|n| wanted(n))
        .filter(|n| {
            n.split_once(':')
                .is_none_or(|(base, _)| !packages.iter().any(|p| p == base))
        })
        .cloned()
        .collect();
    names.sort();

    // Enumerate the package's files instead of guessing `<pkg>.spec`.
    // AppCo's versioned-library convention gives one OBS package per
    // upstream major version (`pytorch+2.4`, `pytorch+2.5`), each holding a
    // plain `<base-name>.spec` (`pytorch.spec`) that has no relation to the
    // OBS package name. The old guess silently missed every one of these
    // (Oct 6 review finding #3): `demo+1.0` looked for `demo+1.0.spec`,
    // not the actual `demo.spec`.
    let fetch_spec = |rpm_name: &str| {
        let spec = spec_for_package(project, rpm_name, emit);
        stash_spec(project, rpm_name, spec.as_ref().ok());
        spec
    };
    let fetch_pypi_latest = |pypi_name: &str| {
        upstream_cache::lookup(&format!("pypi:{pypi_name}"), || {
            pypi::fetch_latest_version(pypi_name).map_err(|e| e.to_string())
        })
    };
    let fetch_repo_latest = |url: &str| {
        upstream_cache::lookup(&format!("repo:{url}"), || {
            crate::sources::latest_release(url)
        })
    };
    let fetch_index_latest = |url: &str| {
        upstream_cache::lookup(&format!("index:{url}"), || {
            crate::upstream::latest_in_index(url)
        })
    };
    let fetch_repology_latest = |name: &str| {
        upstream_cache::lookup(&format!("repology:{name}"), || {
            crate::upstream::latest_on_repology(name)
        })
    };
    let (hits_before, fetched_before) = upstream_cache::stats();
    let lookups = Lookups {
        fetch_pypi_latest: &fetch_pypi_latest,
        fetch_repo_latest: &fetch_repo_latest,
        fetch_index_latest: &fetch_index_latest,
        fetch_repology_latest: &fetch_repology_latest,
    };

    let total = names.len();
    for (i, n) in names.iter().enumerate() {
        emit.reporter.report(&Event::Progress {
            step: emit.step,
            message: format!("checking {n} ({}/{total})", i + 1),
        });
        if !on(classify(n, &fetch_spec, &lookups)) {
            break;
        }
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

/// Specs a plan has just read, by (project, package), for [`planned_spec`].
pub(super) static PLANNED_SPECS: Mutex<Option<HashMap<(String, String), String>>> =
    Mutex::new(None);

pub(super) fn stash_spec(project: &str, pkg: &str, spec: Option<&String>) {
    if let Some(spec) = spec {
        PLANNED_SPECS
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get_or_insert_with(HashMap::new)
            .insert((project.to_string(), pkg.to_string()), spec.clone());
    }
}

/// [`spec_for_package`] for a project that is only read during this run: the
/// spec a plan fetched a moment ago is handed over, once, instead of fetching
/// it again. Taken rather than kept, so a later read, after the package has
/// been changed, always asks OBS.
pub fn planned_spec(project: &str, pkg: &str, emit: Emit<'_>) -> Result<String, ReadError> {
    let kept = PLANNED_SPECS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_mut()
        .and_then(|m| m.remove(&(project.to_string(), pkg.to_string())));
    match kept {
        Some(spec) => Ok(spec),
        None => spec_for_package(project, pkg, emit),
    }
}
