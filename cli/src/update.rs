//! The update command: the in-place update of a project, the outcome messages
//! of a package, and the command that drives them.

use super::*;

/// `update --in-place`: rebuild the packages of PROJECT that have a newer upstream
/// release in PROJECT itself, a project the user owns. Nothing is branched or
/// submitted.
pub(super) fn update_in_place(
    a: update_project::UpdateProjectArgs,
    summary: &mut Option<String>,
) -> color_eyre::Result<()> {
    let update_project::UpdateProjectArgs {
        allow_host,
        source: project,
        only,
        exclude,
        work_dir,
        notes_dir,
        distro,
        arch,
        log,
        tui: enable_tui,
        dry_run: check,
        max_attempts,
        budget_usd,
        no_agent,
        unsafe_resolve,
        jobs,
        regen,
        limits,
        ..
    } = a;
    limits.apply(&work_dir)?;
    ap_build::sources::set_scratch_dir(work_dir.join("cache"));
    let distro = choose_distro(distro.as_deref(), &project, &arch);
    ap_build::discovery::set_agent_enabled(!no_agent);
    if !no_agent {
        ap_build::agent::ensure_logged_in().map_err(|e| color_eyre::eyre::eyre!(e))?;
    }
    ap_ecosystem::pip::set_unsafe_resolve(unsafe_resolve);
    ap_build::package_notes::set_notes_dir(std::path::absolute(&notes_dir)?);

    let level: LogLevel = log
        .parse()
        .map_err(|e: String| color_eyre::eyre::eyre!(e))?;

    // Same per-project-arch subdir layout as Build (see that arm for
    // the rationale): a single `--work-dir` can hold state for multiple OBS
    // projects (and arches) side by side.
    std::fs::create_dir_all(&work_dir)?;
    let project_dir = work_dir.join(format!(
        "{}-{}",
        ap_core::run::sanitize_name(&project),
        ap_core::run::sanitize_name(&arch),
    ));
    reject_symlink_project_dir(&project_dir)?;
    std::fs::create_dir_all(&project_dir)?;
    let _project_lock = ProjectLock::acquire(&project_dir)?;
    let work_dir = project_dir.canonicalize()?;

    let rpms_dir = work_dir.join("rpms");
    std::fs::create_dir_all(&rpms_dir)?;
    let state_root = work_dir.join("state");
    std::fs::create_dir_all(&state_root)?;

    let source_urls = Arc::new(
        ap_build::sources::SourceMap::load(&work_dir).map_err(|e| color_eyre::eyre::eyre!(e))?,
    );
    let python_flavors = Arc::new(
        ap_build::flavors::PythonFlavors::load(&work_dir)
            .map_err(|e| color_eyre::eyre::eyre!(e))?,
    );

    let run_dir = RunDir::allocate(&work_dir)?;
    let memory = std::sync::Arc::new(MemorySink::new());
    // See the matching guard in `Commands::Build` for the rationale
    // (short version: the TUI render thread is detached, process
    // exit kills it before its own terminal teardown runs, leaving
    // raw mode and the alternate screen stuck on). Declared BEFORE
    // `tui_ctrl` so it drops LAST.
    let mut _terminal_restore = enable_tui.then(tui::TerminalGuard::new);
    let tui_ctrl = start_tui(
        enable_tui,
        memory.clone(),
        tui::RunScope {
            project: project.clone(),
            targets: only.to_vec(),
        },
        false,
    );
    if tui_ctrl.is_none() {
        _terminal_restore = None;
    }
    ap_core::sink::set_quiet(tui_ctrl.is_some());
    ap_core::sink::set_diagnostics_file(&run_dir.diagnostics());
    ap_core::sink::set_log_level(level);
    tui::set_explain_dir(work_dir.clone());

    let mut sinks: Vec<Box<dyn ap_core::sink::Reporter>> = Vec::new();
    // Always attached: quiet while the TUI owns the terminal, and it starts
    // printing if the TUI dies mid-run (see `tui::PlainOutput`).
    sinks.push(Box::new(tui::PlainOutput::new(HumanSink::new(level))));
    sinks.push(Box::new(JsonlSink::open(&run_dir.jsonl())?));
    sinks.push(Box::new(memory.clone()));

    let base_reporter: Arc<dyn Reporter + Send + Sync> = Arc::new(FanoutReporter::new(sinks));
    let reporter: Arc<dyn Reporter + Send + Sync> = match &tui_ctrl {
        Some(t) => t.tap(base_reporter.clone(), None),
        None => base_reporter.clone(),
    };
    ap_core::sink::set_reporter(reporter.clone());
    if let Some(t) = &tui_ctrl {
        t.set_recorder(base_reporter.clone());
    }

    install_interrupt_handler(tui_ctrl.is_some(), memory.clone())?;

    let argv: Vec<String> = std::env::args().collect();
    reporter.report(&Event::RunStarted {
        run_id: run_dir.run_id.clone(),
        argv,
        work_dir: work_dir.clone(),
        build: env!("AP_BUILD_REVISION").to_string(),
    });
    allow_host.record(reporter.as_ref());

    // Fail fast if the OBS project has no `<repository>` entries — otherwise
    // every per-package `osc build` would fail downstream with "no
    // repositories defined" after we had already checked each one out.
    match ap_build::osc::project_has_repositories(
        &project,
        ap_build::proc::Emit::run(reporter.as_ref()),
    ) {
        Ok(true) => {}
        Ok(false) => {
            return Err(color_eyre::eyre::eyre!(
                "OBS project '{project}' has no build repositories defined — \
                 configure at least one (e.g. openSUSE_Tumbleweed → openSUSE:Factory) \
                 in its project meta before building."
            ));
        }
        Err(e) => {
            return Err(color_eyre::eyre::eyre!(
                "could not read OBS meta for project '{project}' to verify its \
                 build repositories: {e}"
            ));
        }
    }

    let start = Instant::now();
    let result = cmd_update(
        &project,
        &only,
        &exclude,
        &work_dir,
        &state_root,
        &rpms_dir,
        &distro,
        &arch,
        max_attempts,
        budget_usd,
        no_agent,
        make_semaphore(jobs),
        check,
        regen,
        source_urls,
        python_flavors,
        &run_dir,
        reporter.clone(),
        base_reporter.clone(),
        tui_ctrl.clone(),
    );
    if let Some(t) = &tui_ctrl {
        t.work_finished();
    }

    let outcome = if result.is_ok() {
        RunOutcome::Success
    } else {
        RunOutcome::Failed
    };
    reporter.report(&Event::RunFinished {
        outcome,
        elapsed: start.elapsed(),
        usage: memory.usage(),
    });
    // Built as text now and printed by `main`, once the TUI has
    // restored the terminal; printed here it would be wiped.
    *summary = Some(print_run_summary(
        &memory,
        &project,
        &distro,
        &arch,
        &state_root,
    ));

    match &tui_ctrl {
        Some(t) => {
            t.wait_closed();
            tui_run_exit(t.all_settled(), t.failure(), result, Vec::new())?;
        }
        None => result?,
    }
    Ok(())
}

/// The error that ends a build when packages were given up on: each one with
/// what actually blocked it, since "after N failed attempts" is false for a
/// package given up on because the packages it needs cannot be built, and says
/// nothing about which of them or why.
pub(super) fn build_incomplete_message(
    give_ups: &std::collections::BTreeMap<String, String>,
) -> String {
    let mut out = format!(
        "build incomplete — gave up on {} package(s), blocking everything left in the \
         closure that needs them:",
        give_ups.len()
    );
    for (pkg, why) in give_ups {
        let mut lines = why.lines();
        out.push_str(&format!("\n  - {pkg}: {}", lines.next().unwrap_or("")));
        for line in lines {
            out.push_str(&format!("\n    {}", line.trim_start()));
        }
    }
    out
}

/// The version a package was last built at here, when its last outcome was a
/// success. Read from its state file, so it does not depend on OBS indexing.
///
/// This is the only place that parses state.json for the queue. It reads the
/// two fields as loose JSON instead of going through `PkgState` because
/// `PkgState::load` needs a `Workspace` for one exact directory, while the
/// directory here is found by fuzzy name, and `load` also rewrites a corrupt
/// file aside, which a read-only queue check must not do.
pub(super) fn built_locally(state_root: &Path, rpm_name: &str) -> Option<String> {
    // The RPM spelling of a PyPI name is only a guess (`_` may or may not become
    // `-`), so match the real state directories by fuzzy key instead of building
    // one path from the guess.
    let wanted = ap_build::osc::fuzzy_key(rpm_name);
    let dir = std::fs::read_dir(state_root)
        .ok()?
        .filter_map(|e| e.ok())
        .find(|e| ap_build::osc::fuzzy_key(&e.file_name().to_string_lossy()) == wanted)?;
    let state: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(dir.path().join("state.json")).ok()?).ok()?;
    // A local build is available only to a local run: OBS never has it, so a
    // normal run must not drop the dependency from its queue for it.
    let accepted = match state["last_outcome"].as_str() {
        Some("success") => true,
        Some(ap_build::pipeline::LOCAL_SUCCESS) => ap_build::osc::local_build().is_some(),
        _ => false,
    };
    if !accepted {
        return None;
    }
    state["last_built_version"].as_str().map(str::to_string)
}

/// The outcome a finished package reports, shared by every build path.
pub(super) fn package_outcome(result: &PipelineResult) -> PackageOutcome {
    match result {
        PipelineResult::Built { .. } if ap_build::osc::local_build().is_some() => {
            PackageOutcome::BuiltLocally
        }
        PipelineResult::Built { .. } => PackageOutcome::Built,
        PipelineResult::AlreadyBuilt { .. } => PackageOutcome::AlreadyBuilt,
        PipelineResult::MissingDeps { deps, .. } => {
            PackageOutcome::MissingDeps { deps: deps.clone() }
        }
        PipelineResult::Failed {
            reason,
            kind: FailureKind::NeedsMaintainer,
            ..
        } => PackageOutcome::NeedsMaintainer {
            reason: reason.clone(),
        },
        // Still a failure, but the summary says the URL is gone upstream.
        PipelineResult::Failed {
            reason,
            kind: FailureKind::BrokenOutside,
            ..
        } => PackageOutcome::Failed {
            reason: format!("broken outside the tool: {reason}"),
            disk_full: false,
        },
        PipelineResult::Failed {
            reason,
            kind: FailureKind::Excluded,
            ..
        } => PackageOutcome::Skipped {
            reason: reason.clone(),
        },
        PipelineResult::Failed { reason, kind, .. } => PackageOutcome::Failed {
            reason: reason.clone(),
            disk_full: *kind == FailureKind::DiskFull,
        },
    }
}

/// Scan `project` for packages with a newer upstream release, print the plan,
/// and (unless `--check`) hand each candidate to the ordinary build pipeline
/// pinned to its new version.
///
/// `--force` is always passed to the per-package build (the whole point is
/// to rebuild at a new version, so state.json's "already built at <old>"
/// check must not short-circuit). `--regen` is passed through from the
/// update command's own `--regen`: default off, so a bump preserves the
/// existing recipe's hand-tuned decisions (patches, BuildRequires choices,
/// `%check` skips); the user opts in when upstream has changed build system
/// or dependency layout enough that reusing the old recipe is worse than
/// starting over. Reusing the recipe by default keeps its recipe-specific work.
///
/// Rebuilds run sequentially by default (`--jobs 1`) because each one already
/// kicks off its own dep-closure concurrency inside `cmd_build`; stacking
/// multiple top-level updates on top of that would multiply, not add. A user
/// who knows their machine can take it can raise `--jobs`.
#[allow(clippy::too_many_arguments)]
fn cmd_update(
    project: &str,
    only: &[String],
    exclude: &[String],
    work_dir: &Path,
    state_root: &Path,
    rpms_dir: &Path,
    distro: &str,
    arch: &str,
    max_attempts: usize,
    budget_usd: f64,
    no_agent: bool,
    sem: Option<Semaphore>,
    check: bool,
    regen: bool,
    source_urls: Arc<ap_build::sources::SourceMap>,
    python_flavors: Arc<ap_build::flavors::PythonFlavors>,
    run_dir: &RunDir,
    reporter: Arc<dyn Reporter + Send + Sync>,
    base_reporter: Arc<dyn Reporter + Send + Sync>,
    tui_ctrl: Option<tui::TuiController>,
) -> color_eyre::Result<()> {
    // The project may call the distribution by another name than `--distro`.
    let distro = &ap_build::osc::resolve_repository(project, distro, arch);
    let sweep = Sweep::start(project, only, exclude, reporter.clone());
    // `--check` only reports the plan: the sweep runs to its end and nothing
    // is built.
    let updates: Box<dyn Iterator<Item = ap_build::update::Candidate>> = if check {
        for _ in sweep.updates.iter() {}
        Box::new(std::iter::empty())
    } else {
        Box::new(sweep.updates.iter())
    };
    let mut failures: Vec<(String, String)> = Vec::new();
    for c in updates {
        // The package exists under this name: the pipeline must not rename it
        // after what it makes of the source (a C project that ships Python
        // bindings reads as `python-<name>`).
        let existing = update_project::BranchBuild {
            rpm_name: Some(ap_build::update_project::branch_name(&c.rpm_name).to_string()),
            resolver: None,
            improve: None,
            from_version: None,
            upstream_tag: None,
            update: true,
            bug_fix: None,
        };
        let ap_build::update::Decision::Update {
            ecosystem,
            old_version,
            new_version,
            upstream_tag,
        } = c.decision
        else {
            continue;
        };
        // Found by a lookup the pipeline cannot build from: already reported
        // in the plan.
        if !ecosystem.can_build() {
            continue;
        }
        // The build pipeline wants a plain "name" positional and an optional
        // source-repo URL. For a PyPI update that's just the PyPI name; for a
        // git-forge update it's the repo's final path segment (what
        // `repo_package_name` would compute for the CLI's own URL arg), and
        // we also hand `source_repo` back in so the pipeline fetches the
        // tarball from the forge rather than guessing a PyPI release.
        let (name, source_repo) = match &ecosystem {
            ap_build::update::Ecosystem::Pypi { pypi_name } => (pypi_name.clone(), None),
            ap_build::update::Ecosystem::GitRepo { url } => {
                (repo_package_name(url), Some(url.clone()))
            }
            // A release directory: the spec's own `Source:` names the archive
            // (and its signature) at the new version, so the build is handed the
            // address of the release it packages, under the package's own name.
            ap_build::update::Ecosystem::Index { url } => (
                c.rpm_name.clone(),
                Some(url.replace(&old_version, &new_version)),
            ),
            _ => continue,
        };
        reporter.report(&Event::Progress {
            step: None,
            message: format!("updating {name} to {new_version}"),
        });
        // `--force` always on so the state.json "already built at <old>"
        // check doesn't short-circuit a rebuild at the new version.
        // `--regen` is a passthrough from the caller: off by default so a
        // bump preserves the existing recipe (see this function's doc
        // comment for why that is the safer default), on when the user
        // asked for it. `sweep` stays off: an `update` of package X must
        // not opportunistically start rebuilding every other stuck
        // package in the project.
        let existing = update_project::BranchBuild {
            from_version: Some(old_version),
            upstream_tag,
            ..existing
        };
        let r = cmd_build(
            &name,
            Some(&new_version),
            project,
            work_dir,
            state_root,
            rpms_dir,
            distro,
            arch,
            max_attempts,
            budget_usd,
            None,
            true,  // force
            regen, // passthrough from Update's own --regen
            no_agent,
            sem.clone(),
            false, // sweep: never from inside an update
            source_repo,
            source_urls.clone(),
            python_flavors.clone(),
            run_dir,
            reporter.clone(),
            base_reporter.clone(),
            tui_ctrl.clone(),
            Some(&existing),
        );
        if let Err(e) = r {
            failures.push((name, e.to_string()));
        }
    }

    let plan = sweep.finish().map_err(|e| color_eyre::eyre::eyre!("{e}"))?;
    if plan.total() == 0 {
        reporter.report(&Event::Progress {
            step: None,
            message: format!("no package of {project} to update (is it empty, or do --only and --exclude leave nothing?)"),
        });
    } else if plan.n_update == 0 && !check {
        reporter.report(&Event::Progress {
            step: None,
            message: "nothing to update".into(),
        });
    }

    match failures.first() {
        Some((pkg, err)) => Err(color_eyre::eyre::eyre!(
            "{} update(s) failed; first: {pkg} ({err})",
            failures.len()
        )),
        None => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_build_excluded_for_its_arch_is_a_skip_with_its_reason() {
        let result = PipelineResult::Failed {
            rpm_name: "lsvpd".into(),
            reason: "excluded".into(),
            kind: FailureKind::Excluded,
        };
        assert!(matches!(
            package_outcome(&result),
            PackageOutcome::Skipped { reason } if reason == "excluded"
        ));
        let failed = PipelineResult::Failed {
            rpm_name: "lsvpd".into(),
            reason: "boom".into(),
            kind: FailureKind::Other,
        };
        assert!(matches!(
            package_outcome(&failed),
            PackageOutcome::Failed { .. }
        ));
    }
}
