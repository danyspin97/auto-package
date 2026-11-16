//! Binaries and build logs: fetching a build log, the rebuild request, and the
//! RPM counts of a build.

use super::*;
use crate::osc::poll::*;

/// Fetch the OBS build log into `log_path`.
///
/// Runs `osc buildlog <project> <package> <repo> <arch>` and writes
/// stdout to `log_path`. Returns `Ok` even when the exit code is non-zero
/// if some output was captured (partial logs are still useful).
pub fn fetch_buildlog(
    project: &str,
    pkg_name: &str,
    repo: &str,
    arch: &str,
    log_path: &Path,
    emit: Emit<'_>,
) -> Result<(), Error> {
    check_obs_name("project", project)?;
    check_obs_name("pkg_name", pkg_name)?;
    check_obs_name("repo", repo)?;
    check_obs_name("arch", arch)?;
    let out = proc::capture_with_timeout(
        "osc",
        &["buildlog", project, pkg_name, repo, arch],
        &anywhere(),
        emit,
        OSC_ARTIFACT_TIMEOUT,
    )
    .map_err(during(format!(
        "buildlog {project} {pkg_name} {repo} {arch}"
    )))?;

    proc::write_log(log_path, out.stdout.as_bytes())?;

    if !out.success() && out.stdout.is_empty() {
        return Err(Error::OscFailed {
            cmd: format!("buildlog {project} {pkg_name} {repo} {arch}"),
            code: out.code,
            stderr: out.stderr,
        });
    }
    Ok(())
}

/// Ask OBS to build `pkg_name` again from the sources it already has, and wait
/// (at most 90 s) until its status is no longer a final one. Without that wait
/// the build wait that follows would read the previous build's verdict, which
/// matches the current sources, as the answer. Returns whether the new build
/// was seen to start.
pub fn rebuild(
    project: &str,
    pkg_name: &str,
    repo: &str,
    arch: &str,
    emit: Emit<'_>,
) -> Result<bool, Error> {
    check_obs_name("project", project)?;
    check_obs_name("pkg_name", pkg_name)?;
    check_obs_name("repo", repo)?;
    check_obs_name("arch", arch)?;
    let out = proc::capture_with_timeout(
        "osc",
        &["rebuild", project, pkg_name, repo, arch],
        &anywhere(),
        emit,
        OSC_QUERY_TIMEOUT,
    )
    .map_err(during(format!(
        "rebuild {project} {pkg_name} {repo} {arch}"
    )))?;
    if !out.success() {
        return Err(Error::OscFailed {
            cmd: format!("rebuild {project} {pkg_name} {repo} {arch}"),
            code: out.code,
            stderr: out.stderr,
        });
    }
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(90) && !crate::proc::shutting_down() {
        if get_build_status(project, pkg_name, repo, arch).is_ok_and(|s| !s.is_terminal()) {
            return Ok(true);
        }
        sleep_interruptible(Duration::from_secs(5));
    }
    Ok(false)
}

/// Download built binaries from OBS into `dest_dir`.
///
/// How many of the binary RPMs OBS lists for `pkg_name` are now in `rpms_dir`.
///
/// Used after `osc getbinaries` to confirm it actually produced artifacts
/// rather than silently exiting 0 with an empty binary set (a misconfigured
/// publishing setup for the repo/arch pair). The RPMs are matched by the
/// names OBS lists, NOT by the source package name: a multi-flavor Python
/// package `python-events` ships `python313-Events-…rpm`, which no
/// `python-events-` prefix would ever match. If the listing cannot be read,
/// fall back to counting the RPMs of `pkg` written since `since` (the start of
/// this package's download), see [`count_package_rpms_since`].
pub(super) fn downloaded_rpm_count(
    listing: Option<&[String]>,
    rpms_dir: &std::path::Path,
    pkg: &str,
    since: SystemTime,
) -> usize {
    match listing {
        Some(names) => names.iter().filter(|n| rpms_dir.join(n).is_file()).count(),
        None => count_package_rpms_since_in_dir(rpms_dir, pkg, since),
    }
}

/// The `.rpm` binaries OBS lists for `pkg_name` in `repo`/`arch`, or `None`
/// when the listing cannot be read.
pub(super) fn binary_rpm_listing(
    project: &str,
    pkg_name: &str,
    repo: &str,
    arch: &str,
) -> Option<Vec<String>> {
    osc_api(&format!(
        "/build/{}/{}/{}/{}",
        enc(project),
        enc(repo),
        enc(arch),
        enc(pkg_name)
    ))
    .map(|xml| obs_status::parse_binary_rpms(&xml))
}

/// The package name of an RPM file name: `python313-foo-1.0-2.1.noarch.rpm`
/// is `python313-foo`.
pub(super) fn rpm_base_name(file: &str) -> Option<&str> {
    let (rest, _arch) = file.strip_suffix(".rpm")?.rsplit_once('.')?;
    let (rest, _release) = rest.rsplit_once('-')?;
    let (name, _version) = rest.rsplit_once('-')?;
    Some(name)
}

/// Delete the RPMs in `rpms_dir` that an earlier build of the same package
/// left behind: files named like one of `current` (same package name),
/// including a `current` file itself, so that a download which wrote nothing
/// is not mistaken for one that did by a stale file of the same name.
/// `rpms_dir` is shared by every package, so they would otherwise pile up and
/// satisfy a dependency with an old version.
pub(super) fn remove_superseded_rpms(rpms_dir: &Path, current: &[String]) {
    let names: std::collections::HashSet<&str> =
        current.iter().filter_map(|f| rpm_base_name(f)).collect();
    let Ok(entries) = fs::read_dir(rpms_dir) else {
        return;
    };
    for entry in entries.filter_map(|e| e.ok()) {
        let file = entry.file_name().to_string_lossy().into_owned();
        if file.ends_with(".src.rpm") {
            continue;
        }
        if rpm_base_name(&file).is_some_and(|n| names.contains(n))
            && let Err(e) = fs::remove_file(entry.path())
        {
            crate::diag_failure!("  cannot remove the stale {file}: {e}");
        }
    }
}

/// The result a non-successful `status` of `pkg_name` comes from: the package
/// itself, or the `<pkg>:<flavor>` multibuild entry that failed while the
/// package built (`python-mcp:test`'s `%check`). The status of a multibuild
/// package is the worst of its entries, so the log that explains it is that
/// entry's, not the package's own, which can be a clean success.
pub(super) fn failing_result(
    project: &str,
    pkg_name: &str,
    repo: &str,
    arch: &str,
    status: &BuildStatus,
) -> String {
    if ![project, pkg_name, repo, arch]
        .iter()
        .all(|n| is_valid_obs_name(n))
    {
        return pkg_name.to_string();
    }
    osc_api(&format!(
        "/build/{}/_result?package={}&repository={}&arch={}&multibuild=1",
        enc(project),
        enc(pkg_name),
        enc(repo),
        enc(arch)
    ))
    .and_then(|xml| failing_entry_name(&xml, repo, arch, pkg_name, status))
    .unwrap_or_else(|| pkg_name.to_string())
}

/// [`failing_result`] over a `_result` document: the package if its own entry
/// has `status`, else the first flavor entry that does.
pub(super) fn failing_entry_name(
    xml: &str,
    repo: &str,
    arch: &str,
    pkg_name: &str,
    status: &BuildStatus,
) -> Option<String> {
    let base = obs_status::find_entry(xml, repo, arch, pkg_name);
    std::iter::once(base)
        .flatten()
        .chain(obs_status::flavor_entries(xml, repo, arch, pkg_name))
        .find(|e| obs_status::resolve_lenient(e) == *status)
        .map(|e| e.pkg)
        .filter(|name| is_valid_obs_name(name))
}

/// The `<pkg>:<flavor>` names of `pkg_name`'s multibuild flavors built for
/// `repo`/`arch`; empty for a plain package or when the listing cannot be read.
pub(super) fn multibuild_flavors(
    project: &str,
    pkg_name: &str,
    repo: &str,
    arch: &str,
) -> Vec<String> {
    if ![project, pkg_name, repo, arch]
        .iter()
        .all(|n| is_valid_obs_name(n))
    {
        return Vec::new();
    }
    osc_api(&format!(
        "/build/{}/_result?package={}&repository={}&arch={}&multibuild=1",
        enc(project),
        enc(pkg_name),
        enc(repo),
        enc(arch)
    ))
    .map(|xml| {
        obs_status::flavor_entries(&xml, repo, arch, pkg_name)
            .into_iter()
            .filter(|e| {
                is_valid_obs_name(&e.pkg)
                    && !matches!(
                        obs_status::resolve_lenient(e),
                        BuildStatus::Excluded | BuildStatus::Disabled
                    )
            })
            .map(|e| e.pkg)
            .collect()
    })
    .unwrap_or_default()
}

/// Whether the file name `file` is a binary RPM of package `pkg`: `<pkg>-` and
/// then `<version>-<release>.<arch>.rpm`, the same shape `sibling_rpms` takes
/// (a version starting with a digit, exactly two `-` fields after the name).
/// A sibling whose name extends `pkg` (`pkg-libs-1.0-1.x86_64.rpm`) does not
/// match, and neither does a `.src.rpm`.
pub(super) fn is_package_rpm(file: &str, pkg: &str) -> bool {
    if file.ends_with(".src.rpm") {
        return false;
    }
    let Some(rest) = file.strip_prefix(pkg).and_then(|r| r.strip_prefix('-')) else {
        return false;
    };
    let Some(stem) = rest.strip_suffix(".rpm") else {
        return false;
    };
    match stem.split('-').collect::<Vec<_>>().as_slice() {
        [version, release_arch] => {
            version.starts_with(|c: char| c.is_ascii_digit())
                && release_arch
                    .rsplit_once('.')
                    .is_some_and(|(release, arch)| !release.is_empty() && !arch.is_empty())
        }
        _ => false,
    }
}

/// How many of `files` (name, modification time; `None` when unknown) are
/// binary RPMs of `pkg` modified at or after `since`. Pure: the caller reads
/// the directory.
pub(super) fn count_package_rpms_since(
    files: &[(String, Option<SystemTime>)],
    pkg: &str,
    since: SystemTime,
) -> usize {
    files
        .iter()
        .filter(|(name, modified)| {
            is_package_rpm(name, pkg) && modified.is_some_and(|m| m >= since)
        })
        .count()
}

/// [`count_package_rpms_since`] over the files currently in `rpms_dir`.
/// Returns 0 when the directory doesn't exist or can't be read.
pub(super) fn count_package_rpms_since_in_dir(
    rpms_dir: &std::path::Path,
    pkg: &str,
    since: SystemTime,
) -> usize {
    let Ok(iter) = std::fs::read_dir(rpms_dir) else {
        return 0;
    };
    let files: Vec<(String, Option<SystemTime>)> = iter
        .flatten()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_file()))
        .map(|e| {
            (
                e.file_name().to_string_lossy().into_owned(),
                e.metadata().and_then(|m| m.modified()).ok(),
            )
        })
        .collect();
    count_package_rpms_since(&files, pkg, since)
}

/// Runs `osc getbinaries --destdir=<dest> <project> <package> <repo> <arch>`.
/// Errors are non-fatal: the caller logs a warning and continues.
pub fn get_binaries(
    project: &str,
    pkg_name: &str,
    repo: &str,
    arch: &str,
    dest_dir: &Path,
    emit: Emit<'_>,
) -> Result<(), Error> {
    check_obs_name("project", project)?;
    check_obs_name("pkg_name", pkg_name)?;
    check_obs_name("repo", repo)?;
    check_obs_name("arch", arch)?;
    fs::create_dir_all(dest_dir)?;
    let dest = dest_dir.to_string_lossy();
    let out = proc::capture_with_timeout(
        "osc",
        &[
            "getbinaries",
            &format!("--destdir={dest}"),
            project,
            pkg_name,
            repo,
            arch,
        ],
        &anywhere(),
        emit,
        OSC_ARTIFACT_TIMEOUT,
    )
    .map_err(during(format!(
        "getbinaries {project} {pkg_name} {repo} {arch}"
    )))?;

    if !out.success() {
        return Err(Error::OscFailed {
            cmd: format!("getbinaries {project} {pkg_name} {repo} {arch}"),
            code: out.code,
            stderr: out.stderr,
        });
    }
    Ok(())
}
