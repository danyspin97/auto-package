use super::*;

/// The submit request for `item`, after every check that decides whether one
/// may be made: the update succeeded, the package built in the branch project
/// at the new version, and no request from the branch is open already. The
/// reason it may not be made comes back as the `SrOutcome` to record. Shared by
/// `--submit` and the TUI's submit keys, so both ask exactly the same.
pub(super) fn prepare_request(
    branch: &str,
    distro: Option<&str>,
    arch: &str,
    state_root: &Path,
    item: &Item,
    outcome: &ItemOutcome,
    reporter: &(dyn Reporter + Send + Sync),
) -> Result<osc::SubmitRequest, SrOutcome> {
    let emit = Emit::run(reporter);
    let say = |m: String| {
        reporter.report(&Event::Progress {
            step: None,
            message: m,
        })
    };
    let warn = |m: String| {
        reporter.report(&Event::Warning {
            step: None,
            message: m,
        })
    };
    let updated = *outcome == ItemOutcome::Updated;
    // Read before the build status and the spec are checked, so the request is
    // pinned to a revision at least as old as what was checked.
    let source_rev = if updated {
        match osc::source_srcmd5(branch, &item.pkg, emit) {
            Ok(rev) => Some(rev),
            Err(e) => {
                warn(format!(
                    "no request for {}: could not read its revision: {e}",
                    item.pkg
                ));
                return Err(SrOutcome::Failed(format!(
                    "could not read the revision: {e}"
                )));
            }
        }
    } else {
        None
    };
    let build = updated
        .then(|| {
            osc::get_overall_build_status(
                branch,
                &item.pkg,
                &crate::choose_distro(distro, branch, arch),
                arch,
            )
            // Name the repository/arch that blocks, not just its status.
            .map(|(status, at)| match (&status, at) {
                (osc::BuildStatus::Succeeded, _) | (_, None) => status,
                (_, Some(at)) => osc::BuildStatus::Unknown(format!("{status} on {at}")),
            })
            .map_err(|e| {
                warn(format!(
                    "could not read the build status of {}: {e}",
                    item.pkg
                ))
            })
            .ok()
        })
        .flatten();
    let spec_version = updated
        .then(|| update::spec_for_package(branch, &item.pkg, emit).ok())
        .flatten()
        .and_then(|spec| ap_build::spec::parse_header(&spec))
        .map(|h| h.version);
    if let Err(why) = up::sr_eligibility(item, outcome, build.as_ref(), spec_version.as_deref()) {
        say(format!("no request for {}: {why}", item.pkg));
        return Err(SrOutcome::NotEligible(why));
    }
    match osc::open_submit_requests(branch, &item.pkg, &item.origin, emit) {
        Ok(open) => {
            if let Some(first) = open.first() {
                say(format!(
                    "no request for {}: request {} from {branch} to {} is already open",
                    item.pkg, first.id, item.origin
                ));
                return Err(SrOutcome::Existing(first.id.clone()));
            }
        }
        Err(e) => {
            warn(format!(
                "no request for {}: could not check for an open one: {e}",
                item.pkg
            ));
            return Err(SrOutcome::Failed(format!(
                "could not check for an open request: {e}"
            )));
        }
    }
    Ok(osc::SubmitRequest {
        source_project: branch.to_string(),
        source_package: item.pkg.clone(),
        target_project: item.origin.clone(),
        target_package: item.pkg.clone(),
        description: up::sr_description(
            item,
            newest_changes(branch, &item.pkg, emit).as_deref(),
            &ap_build::fix_flags::of_package(state_root, &item.pkg),
            &ap_build::risk_flags::of_package(state_root, &item.pkg),
        ),
        source_rev,
    })
}

/// How long `osc rdiff` may take.
const RDIFF_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(180);

/// The difference between a package in the project it was branched from and
/// in the branch project, as `osc rdiff` prints it.
pub(super) fn rdiff(
    origin: &str,
    pkg: &str,
    branch: &str,
    reporter: &(dyn Reporter + Send + Sync),
) -> Result<String, String> {
    for name in [origin, pkg, branch] {
        if !osc::is_valid_obs_name(name) {
            return Err(format!("{name:?} is not a valid OBS name"));
        }
    }
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/"));
    let out = ap_build::proc::capture_with_timeout(
        "osc",
        &["rdiff", origin, pkg, branch],
        &cwd,
        Emit::run(reporter),
        RDIFF_TIMEOUT,
    )
    .map_err(|e| format!("osc rdiff {origin} {pkg} {branch}: {e}"))?;
    if out.success() {
        Ok(out.stdout)
    } else {
        let why = out.stderr.trim();
        Err(format!(
            "osc rdiff {origin} {pkg} {branch} failed: {}",
            if why.is_empty() { "no output" } else { why }
        ))
    }
}
