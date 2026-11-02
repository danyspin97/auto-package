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
    for ecosystem in ecosystems(&header, service_git) {
        match lookup(&ecosystem, lookups) {
            Ok(v) => {
                found = Some((ecosystem, v));
                break;
            }
            Err(failure) => {
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
                upstream_tag: upstream_tag.clone(),
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
    let decision = match compare_versions(&latest, tag_version, scheme) {
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
                upstream_tag,
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

/// The series a package is pinned to by its name: `fmt-9`, `boost-1.85` and
/// `wlroots-0_15` name `[9]`, `[1, 85]` and `[0, 15]`. Without a dash the
/// number needs an underscore (`pipewire0_2`), or a soname (`libfoo2`) would
/// pin. The number must also start the packaged version, or it is part of the
/// name (`foo-2` packaging 1.4) and pins nothing.
pub(super) fn series_of(rpm_name: &str, version: &str) -> Option<Vec<u64>> {
    let tail = rpm_name.trim_end_matches(|c: char| c.is_ascii_digit() || c == '.' || c == '_');
    let suffix = &rpm_name[tail.len()..];
    let dashed = tail.ends_with('-');
    if suffix.is_empty() || suffix.starts_with(['.', '_']) || suffix.ends_with(['.', '_']) {
        return None;
    }
    if !dashed && !suffix.contains('_') {
        return None;
    }
    let parts: Vec<&str> = suffix.split(['.', '_']).collect();
    let series: Vec<u64> = parts.iter().filter_map(|p| p.parse().ok()).collect();
    let packaged = version::leading_numbers(version);
    (series.len() == parts.len()
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
    let fetch_gem_latest = |gem: &str| {
        upstream_cache::lookup(&format!("rubygems:{gem}"), || {
            crate::upstream::latest_on_rubygems(gem)
        })
    };
    let fetch_cpan_latest = |dist: &str| {
        upstream_cache::lookup(&format!("cpan:{dist}"), || {
            crate::upstream_cpan::latest_on_metacpan(dist)
        })
    };
    let fetch_service = |rpm_name: &str| osc::cat_file(project, rpm_name, "_service", emit);
    let (hits_before, fetched_before) = upstream_cache::stats();
    let fetch_has_service = |pkg: &str| suse::project_has_service(project, pkg, emit);
    let fetch_release_date = |url: &str| snapshot::cached_release_date(url);
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
