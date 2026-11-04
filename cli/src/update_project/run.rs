use super::*;

pub fn run(a: UpdateProjectArgs, summary: &mut Option<String>) -> color_eyre::Result<()> {
    deprecated::check_args(&a).map_err(|e| eyre!("{e}"))?;
    if a.local && (a.maintenance.is_some() || a.maintenance_list) {
        return Err(eyre!(
            "--local builds on this machine: it cannot be combined with --maintenance, which sends the change to OBS"
        ));
    }
    a.local_opts.apply(a.local)?;
    if a.maintenance.is_some() || a.maintenance_list {
        return run_maintenance(a, summary);
    }
    if !a.maintenance_source
        && (a.maintenance_mode.is_some()
            || !a.backport.is_empty()
            || a.backport_repo.is_some()
            || a.affected_by.is_some())
    {
        return Err(eyre!(
            "--maintenance-mode, --backport, --backport-repo and --affected-by are for --maintenance runs"
        ));
    }
    if a.in_place && (a.submit || a.branch_project.is_some()) {
        return Err(eyre!(
            "--in-place changes {} itself: there is no branch to name and no request to submit",
            a.source
        ));
    }
    if a.in_place && !a.improve && (a.update_deps || a.follow_devel) {
        return Err(eyre!(
            "--update-deps and --follow-devel branch other projects: they cannot be combined with --in-place"
        ));
    }
    if a.improve && a.update_deps {
        return Err(eyre!("--update-deps is only for update"));
    }
    if let Err(e) = arch::wanted_arches(&a) {
        return Err(eyre!("--arches: {e}"));
    }
    ap_build::branch_revision::validate_args(
        &a.only,
        a.revision.as_deref(),
        a.to_version.as_deref(),
        a.in_place,
        a.improve,
    )
    .map_err(|e| eyre!("{e}"))?;
    ap_ecosystem::pip::set_unsafe_resolve(a.unsafe_resolve);
    std::fs::create_dir_all(&a.work_dir)?;
    ap_build::upstream_cache::set_dir(std::path::absolute(&a.work_dir)?);
    ap_build::upstream_cache::set_refresh(a.refresh);
    if a.in_place && !a.improve {
        return crate::update_in_place(a, summary);
    }
    let branch_project = match &a.branch_project {
        _ if a.in_place => a.source.clone(),
        Some(b) => b.clone(),
        None => {
            let user = osc::current_user(Emit::quiet()).map_err(|e| eyre!("{e}"))?;
            osc::default_branch_project(&user, &a.source)
        }
    };
    for (what, name) in [("source", &a.source), ("branch", &branch_project)] {
        if !osc::is_valid_obs_name(name) {
            return Err(eyre!("{name:?} is not a valid {what} project name"));
        }
    }
    if branch_project == a.source && !a.in_place {
        return Err(eyre!(
            "the branch project must not be the source project: {} is never written to",
            a.source
        ));
    }
    if a.tui && a.submit && !a.yes && !a.dry_run {
        return Err(eyre!(
            "--submit asks for confirmation on the terminal, which the TUI owns: pass --yes, or drop --submit \
             and press S (or A) in the TUI to submit with a confirmation"
        ));
    }

    ap_build::discovery::set_agent_enabled(!a.no_agent);
    if !a.no_agent {
        ap_build::agent::ensure_logged_in().map_err(|e| eyre!(e))?;
    }
    ap_build::package_notes::set_notes_dir(std::path::absolute(&a.notes_dir)?);
    let level: LogLevel = a
        .log
        .parse()
        .map_err(|e: String| color_eyre::eyre::eyre!(e))?;

    // The pipeline's state is per project, so the branch project is the one
    // that names the work directory (same layout as `update`).
    std::fs::create_dir_all(&a.work_dir)?;
    let project_dir = a.work_dir.join(format!(
        "{}-{}",
        ap_core::run::sanitize_name(&branch_project),
        ap_core::run::sanitize_name(&a.arch),
    ));
    reject_symlink_project_dir(&project_dir)?;
    std::fs::create_dir_all(&project_dir)?;
    let _project_lock = ProjectLock::acquire(&project_dir)?;
    let work_dir = project_dir.canonicalize()?;
    crate::init_disk_guard(
        &work_dir,
        a.min_free_gb,
        &branch_project,
        &osc::choose_repository(&branch_project, a.distro.as_deref(), &a.arch).0,
        &a.arch,
    );
    let nvidia_choice = a
        .nvidia
        .or(a.nvidia_redist.then_some(crate::NvidiaMode::Redist));
    let nvidia_mode = crate::apply_nvidia_mode(&work_dir, nvidia_choice, false)?;
    crate::write_nvidia_mode(&work_dir, nvidia_mode, nvidia_choice.is_some())?;
    let rpms_dir = work_dir.join("rpms");
    std::fs::create_dir_all(&rpms_dir)?;
    let state_root = work_dir.join("state");
    std::fs::create_dir_all(&state_root)?;

    let source_urls =
        Arc::new(ap_build::sources::SourceMap::load(&work_dir).map_err(|e| eyre!(e))?);
    let python_flavors =
        Arc::new(ap_build::flavors::PythonFlavors::load(&work_dir).map_err(|e| eyre!(e))?);

    let run_dir = RunDir::allocate(&work_dir)?;
    let memory = Arc::new(MemorySink::new());
    // Declared before `tui_ctrl` so it drops last (see `Commands::Build`).
    let mut _terminal_restore = a.tui.then(tui::TerminalGuard::new);
    let tui_ctrl = crate::start_tui(
        a.tui,
        memory.clone(),
        tui::RunScope {
            project: branch_project.clone(),
            targets: a.only.clone(),
        },
        false,
    );
    if tui_ctrl.is_none() {
        _terminal_restore = None;
    }
    ap_core::sink::set_quiet(tui_ctrl.is_some());
    ap_core::sink::set_diagnostics_file(&run_dir.diagnostics());
    ap_core::sink::set_log_level(level);

    let mut sinks: Vec<Box<dyn Reporter>> = Vec::new();
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

    reporter.report(&Event::RunStarted {
        run_id: run_dir.run_id.clone(),
        argv: std::env::args().collect(),
        work_dir: work_dir.clone(),
        build: env!("AP_BUILD_REVISION").to_string(),
    });
    a.allow_host.record(reporter.as_ref());

    let arch_checked: ArchChecked = Default::default();
    let ctx = Ctx {
        work_dir,
        state_root,
        rpms_dir,
        source_urls,
        python_flavors,
        run_dir: &run_dir,
        reporter: reporter.clone(),
        base_reporter,
        tui_ctrl,
        sem: make_semaphore(a.jobs),
        backend: None,
        arch_checked: arch_checked.clone(),
    };
    // A dry run builds nothing, so there is nothing to diff or submit.
    let ctx = match (&ctx.tui_ctrl, a.dry_run) {
        (Some(t), false) => {
            let backend = Arc::new(SubmitBackend {
                branch: branch_project.clone(),
                distro: a.distro.clone(),
                arch: a.arch.clone(),
                state_root: ctx.state_root.clone(),
                yes: a.yes,
                allow_arch: a.allow_arch_failures,
                arch_checked: arch_checked.clone(),
                reporter: reporter.clone(),
                items: Default::default(),
                created: Default::default(),
            });
            // Left open once everything is built, so a request can still be
            // made; with `--submit` the run makes them itself and ends as before.
            t.set_backend(backend.clone(), !a.submit);
            Ctx {
                backend: Some(backend),
                ..ctx
            }
        }
        _ => ctx,
    };

    let start = Instant::now();
    let mut report = Report::default();
    let result = cmd_update_project(&a, &branch_project, &ctx, &mut report);
    if let Some(t) = &ctx.tui_ctrl {
        t.work_finished();
    }
    let failed = up::any_failed(&report.results);

    reporter.report(&Event::RunFinished {
        outcome: if result.is_ok() && !failed {
            RunOutcome::Success
        } else {
            RunOutcome::Failed
        },
        elapsed: start.elapsed(),
        usage: memory.usage(),
    });
    if !a.dry_run {
        *summary = Some(format!(
            "{}{}{}{}",
            print_run_summary(
                &memory,
                &branch_project,
                &osc::choose_repository(&branch_project, a.distro.as_deref(), &a.arch).0,
                &a.arch,
                &ctx.state_root
            ),
            up::summary_text(
                &branch_project,
                &report.results,
                &report.paths,
                &report.requirements,
                &report.maintainer,
                a.submit
            ),
            up::requested_section(&report.requested),
            up::arch_section(&report.arch),
        ));
    }

    let failures = if failed {
        up::failure_lines(&report.results)
    } else {
        Vec::new()
    };
    match &ctx.tui_ctrl {
        Some(t) => {
            t.wait_closed();
            // Requests made in the TUI after the run ended are not in the summary built above.
            let from_tui = ctx
                .backend
                .as_ref()
                .map(|b| b.created_lines())
                .unwrap_or_default();
            if let Some(text) = summary.as_mut()
                && !from_tui.is_empty()
            {
                text.push_str(&format!(
                    "submit requests made from the TUI ({}):\n  {}\n",
                    from_tui.len(),
                    from_tui.join("\n  ")
                ));
            }
            crate::tui_run_exit(t.all_settled(), t.failure(), result, failures)
        }
        None => {
            result?;
            if failures.is_empty() {
                Ok(())
            } else {
                Err(eyre!(failures.join("; ")))
            }
        }
    }
}

/// The packages whose names `only` and `exclude` allow, as the listing spells
/// them.
pub fn wanted_by(only: &[String], exclude: &[String], listed: &str) -> bool {
    let pkg = up::branch_name(listed);
    (only.is_empty() || up::name_listed(only, pkg)) && !up::name_listed(exclude, pkg)
}
