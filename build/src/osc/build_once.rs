//! One attempt at a build on OBS: the request, the wait and the result it
//! returns.

use super::*;

pub(super) fn obs_build_once(
    project: &str,
    pkg_name: &str,
    repo: &str,
    arch: &str,
    log_path: &Path,
    rpms_dir: &Path,
    emit: Emit<'_>,
) -> Result<(), Error> {
    let reporter = emit.reporter;
    let step = emit.step;
    // The caller has committed already: no `_result` document fetched before
    // this may be used for the status (see `get_build_status_after_commit`).
    let committed = Instant::now();
    ensure_builds_for(project, repo, arch)?;
    const MAX_WAIT: Duration = Duration::from_secs(4 * 3600);
    // Time spent `blocked` (waiting for other packages to build) does not
    // count against MAX_WAIT while the blocker is visibly moving, i.e. its
    // details changed within BLOCK_STALL; the wait as a whole is still
    // bounded by MAX_TOTAL_WAIT.
    const BLOCK_STALL: Duration = Duration::from_secs(600);
    const MAX_TOTAL_WAIT: Duration = Duration::from_secs(6 * 3600);

    let start = Instant::now();
    let mut prev_status = String::new();

    // A single `osc` hiccup (network blip, OBS 5xx, rate limit) during a
    // build that can run for many minutes must not abort the wait and throw
    // the build away: tolerate a few consecutive poll failures, then give up.
    const MAX_CONSECUTIVE_POLL_ERRORS: u32 = 8;
    let mut poll_errors: u32 = 0;
    // Polls in a row that saw the same status; the delay between polls grows
    // with it (see `poll_delay`).
    let mut unchanged_polls: u32 = 0;

    // The previous mechanism only ever broke silence for one specific case
    // (an `Unknown("stale: ...")` verdict). A plain non-terminal status
    // (`building`, `scheduled`, ...) that simply never changes again —
    // because the XML `_result` document this reads is serving a stale
    // snapshot while OBS has actually moved on — produced no `Progress`
    // event (nothing to report: the string didn't change) and no `Warning`
    // either, so the wait looked indistinguishable from a healthy long
    // build for up to the full `MAX_WAIT`. Reported live: the TUI sat on an
    // unchanging "building" for 1000+ seconds while OBS itself showed
    // nothing in progress.
    const NO_PROGRESS_WARNING: Duration = Duration::from_secs(120);
    let mut last_change = Instant::now();
    let mut last_warning = Instant::now();
    let mut last_recheck: Option<Instant> = None;
    let mut last_table_label = String::new();
    const UNRESOLVABLE_CONFIRM: Duration = Duration::from_secs(15);
    let mut unresolvable_since: Option<Instant> = None;
    // A repository/arch the project does not build for has no result at all,
    // which would otherwise read as "not started yet" until MAX_WAIT. A
    // freshly committed package usually shows up within seconds, but a busy
    // scheduler can take minutes: after this long the project meta is
    // consulted again, and only a repository/arch it lacks ends the wait.
    const NO_RESULTS_LIMIT: Duration = Duration::from_secs(600);
    let mut no_results_since: Option<Instant> = None;
    // A built verdict whose job history cannot be read at all is accepted
    // after this long (see `obs_status::accept_unconfirmed`).
    const UNCONFIRMED_LIMIT: Duration = Duration::from_secs(300);
    let mut unconfirmed_since: Option<Instant> = None;
    let mut blocked_credit = Duration::ZERO;
    let mut last_tick = Instant::now();
    let mut last_block_change = Instant::now();
    let mut was_blocked = false;

    let status = loop {
        if crate::proc::shutting_down() {
            return Err(Error::OscFailed {
                cmd: format!("wait {project}/{pkg_name}"),
                code: -1,
                stderr: "interrupted".to_string(),
            });
        }
        let tick = last_tick.elapsed();
        last_tick = Instant::now();
        if was_blocked && last_block_change.elapsed() < BLOCK_STALL {
            blocked_credit += tick;
        }
        let waited = start.elapsed();
        if waited.saturating_sub(blocked_credit) >= MAX_WAIT || waited >= MAX_TOTAL_WAIT {
            // Which limit tripped: the total one only when it did so alone.
            let limit = if waited >= MAX_TOTAL_WAIT {
                MAX_TOTAL_WAIT
            } else {
                MAX_WAIT
            };
            return Err(Error::OscFailed {
                cmd: format!("wait {project}/{pkg_name}"),
                code: -1,
                stderr: format!(
                    "timed out after {}s (limit {}s) waiting for OBS build (last status: {})",
                    waited.as_secs(),
                    limit.as_secs(),
                    if prev_status.is_empty() {
                        "none"
                    } else {
                        &prev_status
                    }
                ),
            });
        }

        let status = match get_build_status_after_commit(project, pkg_name, repo, arch, committed) {
            Ok(s) => {
                poll_errors = 0;
                s
            }
            Err(e) => {
                poll_errors += 1;
                if poll_errors >= MAX_CONSECUTIVE_POLL_ERRORS {
                    return Err(Error::OscFailed {
                        cmd: format!("wait {project}/{pkg_name} ({repo}/{arch})"),
                        code: -1,
                        stderr: format!(
                            "gave up after {poll_errors} consecutive failed status polls; last error: {e}"
                        ),
                    });
                }
                reporter.report(&Event::Warning {
                    step,
                    message: format!(
                        "OBS status poll failed ({poll_errors}/{MAX_CONSECUTIVE_POLL_ERRORS}), retrying: {e}"
                    ),
                });
                sleep_interruptible(poll_delay(unchanged_polls, poll_errors, jitter()));
                continue;
            }
        };
        if status == BuildStatus::Unknown("no results".to_string()) {
            let since = *no_results_since.get_or_insert_with(Instant::now);
            if since.elapsed() >= NO_RESULTS_LIMIT {
                // The project meta decides: a repository/arch it does not
                // build is a dead end, anything else is a slow scheduler
                // and MAX_WAIT bounds the wait.
                ensure_builds_for(project, repo, arch)?;
                reporter.report(&Event::Warning {
                    step,
                    message: format!(
                        "OBS has reported no build result for {pkg_name} on {repo}/{arch} \
                         for {}s although {project} builds for it; the scheduler is slow, \
                         still waiting",
                        since.elapsed().as_secs()
                    ),
                });
                no_results_since = Some(Instant::now());
            }
        } else {
            no_results_since = None;
        }
        // A verdict no job history could confirm (empty or evicted history,
        // failing API calls) is not worth waiting on forever: settle for it.
        let status = match obs_status::accept_unconfirmed(&status) {
            Some(accepted) => {
                let since = *unconfirmed_since.get_or_insert_with(Instant::now);
                if since.elapsed() >= UNCONFIRMED_LIMIT {
                    reporter.report(&Event::Warning {
                        step,
                        message: format!(
                            "no OBS job history to confirm the {accepted} result for \
                             {pkg_name} after {}s; accepting it unconfirmed",
                            since.elapsed().as_secs()
                        ),
                    });
                    accepted
                } else {
                    status
                }
            }
            None => {
                unconfirmed_since = None;
                status
            }
        };
        let status_str = status.to_string();
        // What blocks or delays the package is part of the status: a change
        // in it is progress worth showing, and keeps the polling brisk.
        let label = match status {
            BuildStatus::Blocked | BuildStatus::Scheduled => {
                match build_details(project, pkg_name, repo, arch) {
                    Some(d) => format!("{status_str}: {d}"),
                    None => status_str.clone(),
                }
            }
            _ => status_str.clone(),
        };
        was_blocked = status == BuildStatus::Blocked;

        let mut status = status;
        if label == prev_status {
            unchanged_polls = unchanged_polls.saturating_add(1);
        } else {
            unchanged_polls = 0;
        }
        if label != prev_status {
            reporter.report(&Event::Progress {
                step,
                message: format!("OBS build status: {label}"),
            });
            if was_blocked && resets_block_timer(&prev_status) {
                last_block_change = Instant::now();
            }
            prev_status = label;
            last_change = Instant::now();
            last_warning = Instant::now();
            last_recheck = None;
        } else if !status.is_terminal() {
            // Non-terminal and unchanged: make sure the user can always
            // tell this is still alive and polling, not silently wedged.
            if last_warning.elapsed() >= NO_PROGRESS_WARNING {
                reporter.report(&Event::Warning {
                    step,
                    message: format!(
                        "OBS build status still {prev_status} after {}s with no change",
                        last_change.elapsed().as_secs()
                    ),
                });
                last_warning = Instant::now();
            }
            // An independent cross-check, via a different OBS endpoint than
            // whichever one produced `status` (see
            // `get_build_status_from_results_table`'s doc comment) — catches
            // one of them silently serving a stale snapshot while the other
            // has already moved on. Once per stuck period, or, while the
            // status only says the newest sources are not built yet, soon
            // and then every minute (see `recheck_due`).
            if recheck_due(
                obs_status::awaiting_new_revision(&status),
                last_change.elapsed(),
                last_recheck.map(|t| t.elapsed()),
            ) {
                last_recheck = Some(Instant::now());
                if let Ok(table_status) = get_build_status_from_results_table(
                    project,
                    pkg_name,
                    repo,
                    arch,
                    Emit::quiet(),
                ) {
                    let table_str = table_status.to_string();
                    match obs_status::cross_check(&status, &table_status) {
                        obs_status::CrossCheck::Agrees | obs_status::CrossCheck::Ignore => {}
                        obs_status::CrossCheck::NewRevisionSeen => {
                            // The scheduler works on the new revision: the
                            // wait is progressing, so it polls briskly again.
                            unchanged_polls = 0;
                            if table_str != last_table_label {
                                reporter.report(&Event::Progress {
                                    step,
                                    message: format!(
                                        "OBS has taken up the new revision: {table_str}"
                                    ),
                                });
                                last_table_label = table_str;
                            }
                        }
                        obs_status::CrossCheck::Adopt => {
                            reporter.report(&Event::Warning {
                                step,
                                message: format!(
                                    "OBS status disagreement after {}s of no change: the API \
                                     document says {status_str}, but `osc results` says \
                                     {table_str} — trusting the independent read",
                                    last_change.elapsed().as_secs()
                                ),
                            });
                            reporter.report(&Event::Progress {
                                step,
                                message: format!("OBS build status: {table_str}"),
                            });
                            prev_status = table_str;
                            last_change = Instant::now();
                            last_warning = Instant::now();
                            last_recheck = None;
                            unchanged_polls = 0;
                            status = table_status;
                        }
                    }
                }
            }
        }

        // `unresolvable` is only the scheduler's current view: a dependency
        // published a moment later turns it back into `scheduled`. Accept it
        // as the answer only once a later poll agrees (UNRESOLVABLE_CONFIRM is
        // one poll interval, so a genuine verdict costs a single extra poll).
        if status == BuildStatus::Unresolvable {
            let since = *unresolvable_since.get_or_insert_with(Instant::now);
            if since.elapsed() >= UNRESOLVABLE_CONFIRM {
                break status;
            }
        } else {
            unresolvable_since = None;
            if status.is_terminal() {
                break status;
            }
        }

        // `stale: <verdict>` is not a final answer: that terminal result is
        // from a source revision the server may already have superseded, so
        // keep polling. Only a verdict confirmed for the current sources
        // (see `obs_status::resolve`) ends the wait; if none arrives, the
        // MAX_WAIT check above reports a timeout error.
        sleep_interruptible(poll_delay(
            wait_polls(unchanged_polls, obs_status::awaiting_new_revision(&status)),
            0,
            jitter(),
        ));
    };

    // Fetch the log regardless of outcome — partial logs help diagnose failures.
    // A missing or stale log leaves the failure without its cause, so say so.
    // Announce the command that really writes `log_path`; the polling above
    // goes through the API and runs no `osc results --watch`.
    //
    // A multibuild package whose own entry built but whose flavor did not has
    // the flavor's log as the evidence: the package's would show a success and
    // send the fix agent after a failure it cannot see.
    let log_pkg = if status.is_success() {
        pkg_name.to_string()
    } else {
        failing_result(project, pkg_name, repo, arch, &status)
    };
    if log_pkg != pkg_name {
        reporter.report(&Event::Warning {
            step,
            message: format!(
                "{pkg_name} itself built, but its multibuild flavor {log_pkg} did not: \
                 the build log is {log_pkg}'s"
            ),
        });
    }
    reporter.report(&Event::Command {
        step,
        program: "osc".to_string(),
        argv: vec![
            "buildlog".to_string(),
            project.to_string(),
            log_pkg.clone(),
            repo.to_string(),
            arch.to_string(),
        ],
        cwd: anywhere(),
        log: Some(log_path.to_path_buf()),
    });
    if let Err(e) = fetch_buildlog(project, &log_pkg, repo, arch, log_path, Emit::quiet()) {
        reporter.report(&Event::Warning {
            step,
            message: format!("could not fetch the OBS build log for {log_pkg}: {e}"),
        });
    }
    let log_bytes = log_path.metadata().map(|m| m.len()).unwrap_or(0);
    let elapsed = start.elapsed();

    if status.is_success() {
        // Download the built RPMs. A succeeded status followed by a failing
        // getbinaries must not still return success with the RPM directory
        // empty, so this call is fatal: a succeeded OBS build without any
        // locally-downloaded artifacts is NOT a success a dependent
        // build can rely on — `rpm_satisfies_dep` can't find anything,
        // the dep is reported unresolvable even though OBS says the
        // source is green, and the dependent spends attempts chasing a
        // nonexistent problem. Promote this to fatal: if OBS says
        // succeeded but we cannot pull the binaries, surface that as a
        // build failure.
        //
        // We also verify that at least one `.rpm` landed under
        // `rpms_dir` after the fetch — `osc getbinaries` can exit 0
        // with no files written when the project is misconfigured
        // (empty binary set for this repo/arch pair), which the warning
        // version silently papered over.
        let fetch_result = download_built_rpms(project, pkg_name, repo, arch, rpms_dir, emit);
        let downloaded_count = fetch_result.as_ref().copied().unwrap_or(0);
        if let Err(e) = fetch_result {
            reporter.report(&Event::CommandDone {
                step,
                code: 1,
                elapsed,
                stdout_bytes: log_bytes,
                stderr_bytes: 0,
            });
            let advice = if e.is_disk_full() {
                // The build is fine; only this machine could not take the files.
                "The build itself succeeded: free disk space and run again."
            } else {
                "A green source without downloadable artifacts is not a usable \
                 build — a dependent package would see every BuildRequires as \
                 unresolvable. Re-check the OBS project's binary publishing config \
                 for repo={repo}/arch={arch}."
            };
            return Err(Error::NotActionable(format!(
                "OBS reported {pkg_name} {DOWNLOAD_FAILED}: {e}. {}",
                advice.replace("{repo}", repo).replace("{arch}", arch)
            )));
        }
        if downloaded_count == 0 {
            reporter.report(&Event::CommandDone {
                step,
                code: 1,
                elapsed,
                stdout_bytes: log_bytes,
                stderr_bytes: 0,
            });
            return Err(Error::NotActionable(format!(
                "OBS reported {pkg_name} succeeded and `osc getbinaries` exited 0, \
                 but no .rpm files appeared under {}. The project's binary \
                 publishing for repo={repo}/arch={arch} is probably misconfigured \
                 (empty binary set) — check the OBS <publish>/<binaries> settings.",
                rpms_dir.display()
            )));
        }
        reporter.report(&Event::CommandDone {
            step,
            code: 0,
            elapsed,
            stdout_bytes: log_bytes,
            stderr_bytes: 0,
        });
        return Ok(());
    }

    reporter.report(&Event::CommandDone {
        step,
        code: 1,
        elapsed,
        stdout_bytes: log_bytes,
        stderr_bytes: 0,
    });

    Err(match status {
        BuildStatus::Unresolvable => {
            let (names, detail) = current_unresolvable(project, &log_pkg, repo, arch, log_path);
            Error::UnresolvableDeps { names, detail }
        }
        BuildStatus::Broken if !broken_is_confirmed(log_path) => {
            Error::NotActionable(BROKEN_UNCONFIRMED.to_string())
        }
        BuildStatus::Broken => {
            Error::NotActionable(match build_details(project, &log_pkg, repo, arch) {
                Some(d) => format!("{BROKEN_MESSAGE}: {d}"),
                None => BROKEN_MESSAGE.to_string(),
            })
        }
        BuildStatus::Disabled => Error::Excluded(format!(
            "OBS: build is disabled for {repo}/{arch} in the project configuration"
        )),
        BuildStatus::Excluded => Error::Excluded(format!(
            "OBS: build is excluded by OBS for {repo}/{arch} (project configuration or the \
             spec's ExclusiveArch)"
        )),
        _ => {
            // The log is the only evidence for what went wrong. One that
            // cannot be read, or is empty (a failed `osc buildlog` leaves an
            // empty file behind), is an environment problem: classifying it
            // would send the package to the fix agent as a BuildFailed.
            let log_text = usable_build_log(log_path)?;
            match classify_build_failure(&log_text) {
                FailureKind::NoRepositories => Error::NotActionable(
                    "OBS project has no build repositories — configure them in OBS first"
                        .to_string(),
                ),
                FailureKind::NotWorkingCopy => {
                    Error::NotActionable("osc checkout is not server-backed".to_string())
                }
                FailureKind::UnresolvableDeps { names, detail } => {
                    Error::UnresolvableDeps { names, detail }
                }
                FailureKind::BuildFailed => Error::BuildFailed {
                    spec: package_spec_name(project, pkg_name, emit),
                    code: 1,
                    log: log_path.to_owned(),
                },
            }
        }
    })
}
