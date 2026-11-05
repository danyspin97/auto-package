use super::*;
use ap_build::arch_check::GateVerdict;

/// The submit request for `item`, after every check that decides whether one
/// may be made: the update succeeded, the package built in the branch project
/// at the new version, and no request from the branch is open already. The
/// reason it may not be made comes back as the `SrOutcome` to record. Shared by
/// `--submit` and the TUI's submit keys, so both ask exactly the same.
/// `superseding` is the open request a new one replaces (see
/// [`first_open_request`]); `None` for every other caller.
pub(crate) fn prepare_request(
    branch: &str,
    distro: Option<&str>,
    arch: &str,
    state_root: &Path,
    item: &Item,
    outcome: &ItemOutcome,
    allow_arch: bool,
    arch_waited: bool,
    reporter: &(dyn Reporter + Send + Sync),
    superseding: Option<&str>,
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
    let updated = matches!(outcome, ItemOutcome::Updated | ItemOutcome::AlreadyUpdated);
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
    let primary_repo = crate::choose_distro(distro, branch, arch);
    // The other architectures were awaited by the arch check when it ran: then
    // their results are read once, not waited for again.
    let overall_wait = if arch_waited {
        std::time::Duration::ZERO
    } else {
        osc::OVERALL_MAX_WAIT
    };
    let overall = updated
        .then(|| {
            osc::get_overall_build_status_within(
                branch,
                &item.pkg,
                &primary_repo,
                arch,
                overall_wait,
            )
            .map_err(|e| {
                warn(format!(
                    "could not read the build status of {}: {e}",
                    item.pkg
                ))
            })
            .ok()
        })
        .flatten();
    let build = match overall {
        // A failure on another architecture is judged by the arch gate: the
        // origin's own failures do not count, and `--allow-arch-failures` lets
        // the rest through.
        // Also on success: a port result that fails is not in the status.
        Some((status, _)) if is_arch_failure(&status) || status.is_success() => {
            let primary = (primary_repo.as_str(), arch);
            let failed = is_arch_failure(&status);
            match arch_gate(
                branch,
                &item.origin,
                &item.pkg,
                primary,
                allow_arch,
                failed,
                emit,
            ) {
                Ok(GateVerdict::Pass { notes }) => {
                    notes.into_iter().for_each(&say);
                    Some(osc::BuildStatus::Succeeded)
                }
                Ok(GateVerdict::Refuse(why)) => {
                    say(format!("no request for {}: {why}", item.pkg));
                    return Err(SrOutcome::NotEligible(why));
                }
                Err(e) => {
                    warn(format!(
                        "could not read the arch results of {}: {e}",
                        item.pkg
                    ));
                    None
                }
            }
        }
        // Name the repository/arch that blocks, not just its status.
        Some((status, at)) => Some(match (&status, at) {
            (osc::BuildStatus::Succeeded, _) | (_, None) => status,
            (_, Some(at)) => osc::BuildStatus::Unknown(format!("{status} on {at}")),
        }),
        None => None,
    };
    let spec_version = updated
        .then(|| update::spec_for_package(branch, &item.pkg, emit).ok())
        .flatten()
        .and_then(|spec| ap_build::spec::parse_header(&spec))
        .map(|h| h.version);
    if let Err(why) = up::sr_eligibility(item, outcome, build.as_ref(), spec_version.as_deref()) {
        say(format!("no request for {}: {why}", item.pkg));
        return Err(SrOutcome::NotEligible(why));
    }
    // A branch whose sources are those of its origin has nothing to submit: the
    // origin is the revision the branch was made from, so equal sources mean no
    // change (a tidy that changed nothing, or a header the revision refused).
    if let Some(branch_rev) = source_rev.as_deref() {
        let origin = match osc::source_srcmd5(&item.origin, &item.pkg, emit) {
            Ok(rev) => OriginRev::Rev(rev),
            Err(e) if is_missing(&e.to_string()) => OriginRev::Missing,
            Err(e) => OriginRev::Unreadable(e.to_string()),
        };
        match origin_guard(Some(branch_rev), &origin) {
            OriginGuard::Pass => {}
            OriginGuard::NoChange => {
                let why = "the branch has no change against its origin".to_string();
                say(format!("no request for {}: {why}", item.pkg));
                return Err(SrOutcome::NotEligible(why));
            }
            OriginGuard::Unreadable(e) => {
                warn(format!(
                    "no request for {}: could not read its origin's revision: {e}",
                    item.pkg
                ));
                return Err(SrOutcome::Failed(format!(
                    "could not read the origin's revision: {e}"
                )));
            }
        }
    }
    match osc::open_submit_requests(branch, &item.pkg, &item.origin, emit) {
        Ok(open) => {
            if let Some(first) = first_open_request(&open, superseding) {
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

/// The first open request from the branch that a new request does not
/// replace. `superseding` is the request the new one supersedes: it stays open
/// until OBS is told, so it is not a duplicate.
pub(super) fn first_open_request<'a>(
    open: &'a [osc::OpenRequest],
    superseding: Option<&str>,
) -> Option<&'a osc::OpenRequest> {
    open.iter().find(|r| Some(r.id.as_str()) != superseding)
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

/// Whether a blocking overall status is a failure on some architecture.
fn is_arch_failure(status: &osc::BuildStatus) -> bool {
    matches!(
        status,
        osc::BuildStatus::Failed | osc::BuildStatus::Unresolvable | osc::BuildStatus::Broken
    )
}

/// The arch gate of a submit: the verdict on `pkg`'s results in the branch,
/// each failure compared with its baseline (see `gate_inputs`). `failed` is
/// whether the overall build status is a failure.
fn arch_gate(
    branch: &str,
    origin: &str,
    pkg: &str,
    primary: (&str, &str),
    allow: bool,
    failed: bool,
    emit: Emit<'_>,
) -> Result<GateVerdict, String> {
    let obs = ap_build::arch_check::LiveArchObs { emit };
    let rows = ap_build::arch_check::gate_inputs(&obs, branch, origin, pkg)?;
    Ok(ap_build::arch_check::gate_verdict(
        pkg, primary, &rows, allow, failed,
    ))
}

/// The origin's revision of a package, as the guard reads it.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum OriginRev {
    Rev(String),
    /// The origin has no such package: a new one, which has nothing to compare.
    Missing,
    Unreadable(String),
}

/// What the origin guard decides for a branch.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum OriginGuard {
    Pass,
    /// The branch's sources are its origin's: nothing to submit.
    NoChange,
    Unreadable(String),
}

/// The origin guard (see `prepare_request`): a branch whose revision equals its
/// origin's has no change; a new package (no origin) and a branch whose
/// revision is not known pass.
pub(super) fn origin_guard(branch_rev: Option<&str>, origin: &OriginRev) -> OriginGuard {
    match (branch_rev, origin) {
        (None, _) | (_, OriginRev::Missing) => OriginGuard::Pass,
        (Some(_), OriginRev::Unreadable(e)) => OriginGuard::Unreadable(e.clone()),
        (Some(b), OriginRev::Rev(o)) if !b.is_empty() && b == o => OriginGuard::NoChange,
        (Some(_), OriginRev::Rev(_)) => OriginGuard::Pass,
    }
}

/// Whether an osc error says the package is not there (a new package).
fn is_missing(error: &str) -> bool {
    let lower = error.to_ascii_lowercase();
    lower.contains("404") || lower.contains("not found")
}

#[cfg(test)]
mod origin_guard_tests {
    use super::*;

    #[test]
    fn the_guard_refuses_only_an_equal_revision() {
        let rev = |s: &str| OriginRev::Rev(s.to_string());
        assert_eq!(
            origin_guard(Some("abc"), &rev("abc")),
            OriginGuard::NoChange
        );
        assert_eq!(origin_guard(Some("abc"), &rev("abd")), OriginGuard::Pass);
        assert_eq!(origin_guard(None, &rev("abc")), OriginGuard::Pass);
    }

    #[test]
    fn a_new_package_is_not_refused_and_an_unreadable_origin_is() {
        assert_eq!(
            origin_guard(Some("abc"), &OriginRev::Missing),
            OriginGuard::Pass
        );
        assert_eq!(
            origin_guard(Some("abc"), &OriginRev::Unreadable("timeout".into())),
            OriginGuard::Unreadable("timeout".into())
        );
        assert!(is_missing("osc: package not found (404)"));
        assert!(!is_missing("timeout"));
    }
}
