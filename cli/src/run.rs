use super::*;

pub(super) fn run(cli: Cli, summary: &mut Option<String>) -> color_eyre::Result<()> {
    match cli.command {
        Commands::Build {
            name,
            version,
            project,
            work_dir,
            notes_dir,
            distro,
            arch,
            log,
            tui: enable_tui,
            max_attempts,
            budget_usd,
            target,
            force,
            regen,
            agent_backend,
            no_agent,
            unsafe_resolve,
            local,
            local_opts,
            cleanup,
            jobs,
            limits,
            min_free_gb,
            nvidia,
            nvidia_redist,
            url,
            source_url,
            sweep,
            mut allow_host,
        } => {
            allow_host.apply()?;
            // `--regen` means "start over": without this, an already-built
            // package returned AlreadyBuilt (state.json / OBS-green fast paths,
            // `select_target`) before the regeneration ever ran, so `--regen`
            // alone was a silent no-op and needed `--force` alongside it.
            let force = force || regen;

            // `--no-agent` promises no model calls at all, including the
            // web-search fallback that locates an upstream for unknown names.
            ap_build::discovery::set_agent_enabled(!no_agent);
            if !no_agent {
                ap_build::agent::ensure_logged_in().map_err(crate::session::AgentUnavailable)?;
            }
            ap_ecosystem::pip::set_unsafe_resolve(unsafe_resolve);
            local_opts.apply(local)?;
            limits.apply(&work_dir)?;
            ap_build::sources::set_scratch_dir(work_dir.join("cache"));
            ap_build::package_notes::set_notes_dir(std::path::absolute(&notes_dir)?);
            let distro = choose_distro(distro.as_deref(), &project, &arch);

            // `build https://github.com/owner/repo` — the URL is the source and
            // the repository name is the package name. `--url` does the same
            // thing but lets the package be named separately.
            let request =
                name_clash::parse_request(&name).map_err(|e| color_eyre::eyre::eyre!(e))?;
            let pypi = request.pypi;
            let (name, source_repo) = match (request.repo, url) {
                (Some(_), Some(u)) => {
                    return Err(color_eyre::eyre::eyre!(
                        "the package name is already a URL ({name}); drop --url {u}"
                    ));
                }
                (repo, u) => (request.name, repo.or(u)),
            };
            if pypi && source_repo.is_some() {
                return Err(color_eyre::eyre::eyre!(
                    "`pypi:{name}` is the PyPI project and takes no repository; drop --url"
                ));
            }
            validate_package_name(&name).map_err(|e| color_eyre::eyre::eyre!(e))?;

            // Both of these are silent-wrong-behaviour traps otherwise: a
            // mistyped level would quietly log at info, and an unsupported
            // backend would quietly run the CLI one.
            let level: LogLevel = log
                .parse()
                .map_err(|e: String| color_eyre::eyre::eyre!(e))?;
            if agent_backend != "cli" {
                return Err(color_eyre::eyre::eyre!(
                    "unsupported --agent-backend '{agent_backend}'; only 'cli' is available"
                ));
            }

            // `--work-dir` is the root; each OBS project lives in its own
            // subdir inside it (`<work-dir>/<sanitized-project>-<arch>/`),
            // so one root can hold state for several projects (and now
            // several arches) side by side. `--cleanup` scopes to just
            // this project-arch's subdir.
            //
            // The arch suffix keeps architectures apart: running with
            // `--arch x86_64` and then `--arch aarch64` under the same
            // `--work-dir` must not share state (RPMs, source checkouts,
            // state.json).
            //
            // Guarded by an ownership marker (`.auto-package`) written at
            // the project_dir root on first creation. `--cleanup` refuses
            // to recursively remove a project_dir that does not carry the
            // marker — this stops a mistyped `--work-dir` pointing at an
            // unrelated directory (or a stray symlink into one) from being
            // wiped out just because `--cleanup` was passed. For existing
            // project_dirs with no marker, the user has to add it manually
            // if they want the clean-up to proceed.
            let project_dir_name = format!(
                "{}-{}",
                ap_core::run::sanitize_name(&project),
                ap_core::run::sanitize_name(&arch),
            );
            let project_dir = work_dir.join(&project_dir_name);
            // The source overrides as they stand once this command's own
            // `--source-url` values are in. A source given for the name says
            // which project it is, so the clash check below does not ask.
            let mut source_urls = ap_build::sources::SourceMap::load(&project_dir)
                .map_err(|e| color_eyre::eyre::eyre!(e))?;
            for arg in &source_url {
                let (pkg, repo) = ap_build::sources::SourceMap::parse_arg(arg, &name)
                    .map_err(|e| color_eyre::eyre::eyre!(e))?;
                source_urls.insert(&pkg, &repo);
            }
            // A bare name is PyPI's unless a repository of that name is another
            // project: the user decides. Checked before anything is created.
            let nvidia_given = nvidia.is_some() || nvidia_redist;
            if !pypi
                && crate::name_clash::applies(
                    &name,
                    source_repo.is_some(),
                    nvidia_given,
                    source_urls.get(&name).is_some(),
                )
            {
                crate::name_clash::check(&name, &crate::name_clash::WarningsToStderr)?;
            }
            const OWNERSHIP_MARKER: &str = ".auto-package";
            reject_symlink_project_dir(&project_dir)?;
            // Only a directory this run creates is ours to mark. Marking one that
            // already existed would let a later --cleanup delete the user's files.
            let created_now = !project_dir.exists();
            std::fs::create_dir_all(&project_dir)?;
            // Taken before --cleanup touches anything: a second run for this
            // project must not delete state under one that is still building.
            let _project_lock = ProjectLock::acquire(&project_dir)?;
            // `--nvidia`: the flag, else what this work directory used before.
            let nvidia_choice = nvidia.or(nvidia_redist.then_some(NvidiaMode::Redist));
            if cleanup && !created_now {
                let marker = project_dir.join(OWNERSHIP_MARKER);
                if !marker.exists() {
                    return Err(color_eyre::eyre::eyre!(
                        "--cleanup refuses to remove {} because it has no \
                         `.auto-package` ownership marker — this directory \
                         may not have been created by auto-package. Delete \
                         it manually if that is wrong, or `touch {}/.auto-package` \
                         first to accept that --cleanup can recursively \
                         remove it on this and later runs.",
                        project_dir.display(),
                        project_dir.display(),
                    ));
                }
                // Only rebuildable caches go; `runs/`, `state/`, `ledger.json`
                // and the osc checkouts stay (see `cleanup`).
                cleanup::run(&project_dir)?;
            }
            // Runs after the --cleanup refusal above, so a refused run does not
            // change the process-wide NVIDIA mode or print its warnings.
            let nvidia_mode = apply_nvidia_mode(&project_dir, nvidia_choice, cleanup)?;
            if created_now {
                // Non-fatal: without the marker, --cleanup just refuses later.
                if let Err(e) = ap_core::fsx::write_atomic(&project_dir.join(OWNERSHIP_MARKER), b"")
                {
                    ap_core::diag!(
                        "warning: could not write the ownership marker in {}: {e}; \
                         --cleanup will refuse this directory",
                        project_dir.display()
                    );
                }
            }

            // Canonicalize so no relative path can reach a subprocess whose
            // cwd is the osc checkout.
            let project_dir = project_dir.canonicalize()?;

            write_nvidia_mode(&project_dir, nvidia_mode, nvidia_choice.is_some())?;
            let rpms_dir = project_dir.join("rpms");
            std::fs::create_dir_all(&rpms_dir)?;
            let state_root = project_dir.join("state");
            std::fs::create_dir_all(&state_root)?;

            // Source overrides: whatever is on disk, plus anything given on
            // this command line (read above), written straight back so the
            // answer is only ever given once.
            if !source_url.is_empty() {
                source_urls
                    .save(&project_dir)
                    .map_err(|e| color_eyre::eyre::eyre!(e))?;
            }
            let source_urls = Arc::new(source_urls);

            // Which Python flavors this project builds for, read once from
            // `<project-dir>/flavors.toml` if present.
            let python_flavors = Arc::new(
                ap_build::flavors::PythonFlavors::load(&project_dir)
                    .map_err(|e| color_eyre::eyre::eyre!(e))?,
            );

            init_disk_guard(&project_dir, min_free_gb, &project, &distro, &arch);
            let run_dir = RunDir::allocate(&project_dir)?;
            let work_dir = project_dir;

            // Built before the TUI so it can be handed straight to
            // `TuiController::new` — the TUI's raw-mode terminal swallows a
            // real SIGINT (Ctrl-C only reaches it as a keypress), so it has
            // to call `handle_interrupt` itself with the same usage data
            // rather than relying on the `ctrlc` handler below ever firing.
            let memory = std::sync::Arc::new(MemorySink::new());

            // Declared BEFORE `tui_ctrl` so Rust's reverse-of-declaration
            // local-drop order runs it LAST — after `tui_ctrl` and every
            // reporter Arc has dropped (closing every sender into the render
            // thread) and after the render thread's own in-thread teardown
            // had its chance to run. The render thread is detached: on
            // process exit Rust kills it before its own teardown can
            // complete, leaving raw mode and the alternate screen stuck on
            // in the user's shell. This main-thread guard runs on every
            // exit path (normal return, `?` early return, panic unwinding)
            // and the extra teardown is idempotent.
            let mut _terminal_restore = enable_tui.then(tui::TerminalGuard::new);

            // The TUI takes over the terminal, so text output is dropped in
            // favor of it; jsonl/memory logging are unaffected either way.
            // `set_quiet` also silences the ad hoc `diag!` lines scattered
            // through resolution/search code that don't go through a
            // `Reporter` at all — left unguarded they'd print straight over
            // the TUI's alternate screen.
            let tui_ctrl = start_tui(
                enable_tui,
                memory.clone(),
                tui::RunScope {
                    project: project.clone(),
                    targets: vec![target.clone().unwrap_or_else(|| name.clone())],
                    jobs,
                },
                true,
            );
            if tui_ctrl.is_none() {
                _terminal_restore = None;
            }
            ap_core::sink::set_quiet(tui_ctrl.is_some());
            ap_core::sink::set_diagnostics_file(&run_dir.diagnostics());
            ap_core::sink::set_log_level(level);
            tui::set_explain_dir(work_dir.clone());

            let mut sinks: Vec<Box<dyn ap_core::sink::Reporter>> = Vec::new();
            // Always attached: quiet while the TUI owns the terminal, and it
            // starts printing if the TUI dies mid-run (see `tui::PlainOutput`).
            sinks.push(Box::new(tui::PlainOutput::new(HumanSink::new(level))));

            // Always add JSONL logging
            sinks.push(Box::new(JsonlSink::open(&run_dir.jsonl())?));
            sinks.push(Box::new(memory.clone()));

            let base_reporter: Arc<dyn Reporter + Send + Sync> =
                Arc::new(FanoutReporter::new(sinks));
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
            // In TUI mode the user can add more root packages at runtime
            // (`n` key). Each becomes an independent `cmd_build` on its own
            // thread, sharing the reporter/TUI/run directory; overlapping
            // dependencies are serialised per package inside `run_package`.
            // The dispatcher below ends when the TUI exits (its channel
            // disconnects), so quitting the TUI never leaves it waiting.
            let add_rx = tui_ctrl.as_ref().and_then(|t| t.take_add_requests());
            // One semaphore for the whole run: `--jobs` caps concurrent package
            // builds across the root build and every TUI-added root.
            let sem = make_semaphore(jobs);
            // Roots added in the TUI run on their own threads; their failures
            // are collected here so they reach the outcome and exit code. Each
            // is keyed by the name it was requested under, so a retry that
            // builds the package clears the failure it follows, and a later
            // build of that package as a dependency clears it too (see
            // `withdraw_settled`).
            let added_failures: Mutex<Vec<AddFailure>> = Mutex::new(Vec::new());
            let result = {
                let sem = &sem;
                let added_failures = &added_failures;
                let memory = &memory;
                let (project, work_dir, state_root, rpms_dir) =
                    (&project, &work_dir, &state_root, &rpms_dir);
                let (distro, arch, run_dir) = (&distro, &arch, &run_dir);
                let (source_urls, python_flavors) = (&source_urls, &python_flavors);
                let (reporter, base_reporter, tui_ctrl) = (&reporter, &base_reporter, &tui_ctrl);
                std::thread::scope(|scope| {
                    if let Some(add_rx) = add_rx {
                        scope.spawn(move || {
                            for req in add_rx {
                                scope.spawn(move || {
                                    // A retry is sent under the name its row was
                                    // requested with. The row's own key is an RPM name,
                                    // which a build cannot take back (`python-foo` is
                                    // `foo` on PyPI).
                                    let (raw, retry_of) = request_names(req, |row| {
                                        tui_ctrl.as_ref().and_then(|t| t.origin_of(row))
                                    });
                                    let label = raw
                                        .clone()
                                        .or_else(|| retry_of.clone())
                                        .unwrap_or_default();
                                    // Watches whether the build selected anything to start.
                                    let watch = Arc::new(StartWatch::new(reporter.clone()));
                                    // The package's own outcome is reported to the
                                    // base reporter, so it is read from the run's
                                    // record: events from before this retry are not its.
                                    let finished_before = memory.finished().len();
                                    let r = match &raw {
                                        None => Err(color_eyre::eyre::eyre!(NO_REQUEST_NAME)),
                                        Some(raw) => name_clash::admit(
                                            raw,
                                            source_urls,
                                            nvidia_given,
                                            reporter.as_ref(),
                                        )
                                        .and_then(|req| {
                                            catch_panic(|| {
                                                cmd_build(
                                                    &req.name,
                                                    None,
                                                    project,
                                                    work_dir,
                                                    state_root,
                                                    rpms_dir,
                                                    distro,
                                                    arch,
                                                    max_attempts,
                                                    budget_usd,
                                                    None,
                                                    false, // force: reuse anything already built
                                                    false, // regen
                                                    no_agent,
                                                    sem.clone(),
                                                    false, // sweep
                                                    req.repo,
                                                    source_urls.clone(),
                                                    python_flavors.clone(),
                                                    run_dir,
                                                    watch.clone(),
                                                    base_reporter.clone(),
                                                    tui_ctrl.clone(),
                                                    None,
                                                    req.pypi.then(|| raw.clone()),
                                                )
                                            })
                                        }),
                                    };
                                    reporter.report(&match &r {
                                        Ok(()) => {
                                            // The package is built, so an earlier
                                            // failure of the same request is fixed.
                                            added_failures
                                                .lock()
                                                .unwrap_or_else(|p| p.into_inner())
                                                .retain(|f| f.label != label);
                                            Event::Progress {
                                                step: None,
                                                message: format!("{label}: build finished"),
                                            }
                                        }
                                        Err(e) => {
                                            added_failures
                                                .lock()
                                                .unwrap_or_else(|p| p.into_inner())
                                                .push(AddFailure {
                                                    label: label.clone(),
                                                    message: name_clash::tui_failure(&label, &e),
                                                    after: memory.finished().len(),
                                                });
                                            Event::Warning {
                                                step: None,
                                                message: name_clash::tui_failure(&label, &e),
                                            }
                                        }
                                    });
                                    // A retry that fails without its own row having
                                    // finished would leave the row marked as retrying
                                    // for good (its own worker reports it, but a retry
                                    // may only have started a dependency). Report it
                                    // failed here. A row the retry did finish keeps
                                    // its own outcome.
                                    // A retry that succeeded without starting anything
                                    // (the package was already built) settles its row
                                    // as already built, instead of leaving it marked
                                    // as retrying.
                                    match (&r, retry_of) {
                                        (Err(e), Some(row))
                                            if !finished_since(
                                                &memory.finished(),
                                                finished_before,
                                                &row,
                                            ) =>
                                        {
                                            let reason = if watch.started() {
                                                format!("retry failed: {e:#}")
                                            } else {
                                                format!("retry did not start: {e:#}")
                                            };
                                            reporter.report(&Event::PackageFinished {
                                                pkg: row,
                                                version: String::new(),
                                                outcome: PackageOutcome::Failed {
                                                    reason,
                                                    disk_full: false,
                                                },
                                            });
                                        }
                                        (Ok(()), Some(row)) if !watch.started() => {
                                            reporter.report(&Event::PackageFinished {
                                                pkg: row,
                                                version: String::new(),
                                                outcome: PackageOutcome::AlreadyBuilt,
                                            });
                                        }
                                        _ => {}
                                    }
                                    // Only now, with its last event reported, does the
                                    // TUI stop counting this request as work.
                                    if let Some(t) = tui_ctrl {
                                        t.work_finished();
                                    }
                                });
                            }
                        });
                    }
                    // A panic here must become an `Err`: the scope also joins the
                    // add-request dispatcher, which only ends when the TUI quits,
                    // and the panic text would be invisible in the alternate screen.
                    let first = catch_panic(|| {
                        cmd_build(
                            &name,
                            version.as_deref(),
                            project,
                            work_dir,
                            state_root,
                            rpms_dir,
                            distro,
                            arch,
                            max_attempts,
                            budget_usd,
                            target.as_deref(),
                            force,
                            regen,
                            no_agent,
                            sem.clone(),
                            sweep,
                            source_repo,
                            source_urls.clone(),
                            python_flavors.clone(),
                            run_dir,
                            reporter.clone(),
                            base_reporter.clone(),
                            tui_ctrl.clone(),
                            None,
                            name_clash::root_text(&name, pypi),
                        )
                    });
                    // The TUI stays open after the root build until it finds
                    // nothing left to do or is quit, and `result?` below only
                    // runs after that — so a failure (e.g. pip finding no
                    // version for this interpreter) must be shown here, not
                    // left looking like a hang. What to do next is the TUI's to
                    // say once it knows whether everything is built.
                    if tui_ctrl.is_some()
                        && let Err(e) = &first
                    {
                        reporter.report(&Event::Warning {
                            step: None,
                            message: format!("Root build of {name} failed: {e:#}"),
                        });
                    }
                    if let Some(t) = tui_ctrl {
                        t.work_finished();
                        // RunFinished says how the run went, so it waits for the
                        // requests the TUI added as well. It must not wait for the
                        // dispatcher above: that only ends once the TUI has closed,
                        // and the TUI needs RunFinished to close.
                        t.wait_until_idle();
                    }
                    let failed = first.is_err() || {
                        let mut failures = added_failures.lock().unwrap_or_else(|p| p.into_inner());
                        withdraw_settled(&mut failures, memory, tui_ctrl);
                        !failures.is_empty()
                    };
                    // Sum usage from all AgentReply events recorded during the run.
                    reporter.report(&Event::RunFinished {
                        outcome: if failed {
                            RunOutcome::Failed
                        } else {
                            RunOutcome::Success
                        },
                        elapsed: start.elapsed(),
                        usage: memory.usage(),
                    });
                    first
                })
            };

            // Every add request has returned by now (the scope joined them), so
            // this is the run's final word on which failures still stand.
            let mut failures = added_failures
                .into_inner()
                .unwrap_or_else(|p| p.into_inner());
            withdraw_settled(&mut failures, &memory, &tui_ctrl);
            let added_failures: Vec<String> = failures.into_iter().map(|f| f.message).collect();
            // Built as text now and printed by `main`, once the TUI has
            // restored the terminal; printed here it would be wiped.
            *summary = Some(print_run_summary(
                &memory,
                &project,
                &distro,
                &arch,
                &state_root,
            ));

            // Only the TUI takes add requests, so without it there are no
            // added failures to report.
            if let Some(t) = &tui_ctrl {
                // A settled run closes itself after a grace period, so its
                // verdict is only final once the TUI has closed.
                t.wait_closed();
                tui_run_exit(t.all_settled(), t.failure(), result, added_failures)?;
            } else {
                result?;
            }
        }
        Commands::Fix(args) => fix_cmd::run(args, summary)?,
        Commands::Hosts { command } => allow_host::hosts_command(&command)?,
        Commands::Keyring { command } => match command {
            keyring::KeyringCommand::Accept(args) => keyring::accept_command(&args)?,
        },
        Commands::Lint(mut args) => {
            args.allow_host.apply()?;
            args.improve = true;
            update_project::run(args, summary)?
        }
        Commands::Notes { name, notes_dir } => crate::notes_cmd::notes_command(&name, &notes_dir)?,
        Commands::Explain(args) => explain_cmd::run(&args)?,
        Commands::Review(_) => unreachable!("handled in main"),
        Commands::Baseline { command } => baseline_command(command)?,
        Commands::EditBytes(args) => edit_bytes::run(&args)?,
        Commands::Update(mut args) => {
            args.allow_host.apply()?;
            update_project::run(args, summary)?
        }
    }

    Ok(())
}
