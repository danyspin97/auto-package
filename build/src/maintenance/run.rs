use super::*;

/// `update --maintenance` / `lint --maintenance` with the [`StreamCheck`] of
/// the options (see [`run_with`]).
pub fn run(opts: &Options, host: &mut Host<'_>) -> Result<(), String> {
    let check = stream_check_for(opts);
    run_with(opts, host, check.as_ref())
}

/// The streams of `chosen` that every package's verdict leaves out.
pub fn streams_to_build(chosen: &[String], per_package: &[Vec<(String, Affected)>]) -> Vec<String> {
    chosen
        .iter()
        .filter(|s| {
            !per_package.iter().all(|v| {
                v.iter()
                    .any(|(cs, a)| cs == *s && matches!(a, Affected::NotAffected(_)))
            })
        })
        .cloned()
        .collect()
}

/// Build the change in a branch of every chosen code stream that `check` does
/// not rule out, each from its own package, and send the packages that built
/// to the maintenance project, after showing the plan and asking (unless
/// `yes`). A dry run builds nothing, asks nothing and changes nothing on OBS.
/// Every code stream ends in a [`StreamReport`], printed per package.
pub fn run_with(
    opts: &Options,
    host: &mut Host<'_>,
    check: &dyn StreamCheck,
) -> Result<(), String> {
    if opts.packages.is_empty() {
        return Err("a maintenance request is per package: pass --only PKG[,PKG...]".to_string());
    }
    validate_mode(
        opts.mode,
        &opts.backports,
        opts.backport_repo.as_deref(),
        opts.no_bug_ref,
        &opts.packages,
    )?;
    let refs = parse_bug_refs(&opts.bugs, opts.no_bug_ref)?;
    for w in bug_ref_warnings(&refs) {
        (host.warn)(&w);
    }
    let job: Option<Arc<backport::Job>> = if opts.mode == Mode::Backport {
        let job = backport::prepare(&opts.backports, opts.backport_repo.as_deref(), &refs)?;
        for p in &job.patches {
            (host.say)(&format!(
                "backport {}: {} (from {})",
                p.file_name, p.reason, p.origin
            ));
        }
        Some(Arc::new(job))
    } else {
        None
    };
    let mode_text = mode_line(opts.mode, job.as_deref());
    let emit = Emit::quiet();
    let mut per_package = Vec::new();
    let mut git: Vec<Vec<GitStream>> = Vec::new();
    for pkg in &opts.packages {
        per_package.push(maintained_codestreams(pkg, emit).map_err(|e| format!("{pkg}: {e}"))?);
        git.push(git_maintained(pkg, emit).unwrap_or_else(|e| {
            (host.warn)(&format!(
                "{pkg}: could not check for code streams maintained in git: {e}"
            ));
            Vec::new()
        }));
    }
    for (pkg, g) in opts.packages.iter().zip(&git) {
        for s in g {
            (host.say)(&format!(
                "{pkg}: {s} is maintained in git (SLFO), which is not supported yet: left out"
            ));
        }
    }
    let chosen = choose(opts, &per_package, host)?;
    if opts.branch_project.is_some() && chosen.len() > 1 {
        return Err(
            "--branch-project names one project, but each code stream is built in its own: drop it, or pick one code stream"
                .to_string(),
        );
    }

    // Which code streams need the fix (the --affected-by seam).
    let fix = FixQuery {
        refs: &refs,
        sources: &opts.backports,
    };
    let affected: Vec<Vec<(String, Affected)>> = opts
        .packages
        .iter()
        .map(|pkg| verdicts(check, pkg, &chosen, &fix))
        .collect();
    let mut reports: Vec<Vec<StreamReport>> = affected
        .iter()
        .map(|v| {
            v.iter()
                .filter_map(|(s, a)| match a {
                    Affected::NotAffected(why) => Some(StreamReport {
                        stream: s.clone(),
                        outcome: StreamOutcome::NotAffected(why.clone()),
                    }),
                    _ => None,
                })
                .collect()
        })
        .collect();
    for (r, g) in reports.iter_mut().zip(&git) {
        r.extend(g.iter().map(|s| StreamReport {
            stream: s.to_string(),
            outcome: StreamOutcome::GitMaintained,
        }));
    }
    let chosen = streams_to_build(&chosen, &affected);
    let print_summaries = |reports: &[Vec<StreamReport>]| {
        for (pkg, r) in opts.packages.iter().zip(reports) {
            for l in summary_lines(pkg, r) {
                (host.say)(&l);
            }
        }
    };
    if chosen.is_empty() {
        (host.say)("no code stream is left to build: the fix affects none of them");
        print_summaries(&reports);
        return Ok(());
    }

    let user = osc::current_user(emit).ok();
    let target_project = maintenance_project(emit).map_err(|e| e.to_string())?;

    let mut targets: Vec<Target> = Vec::new();
    for project in &chosen {
        let branch = match (&opts.branch_project, &user) {
            (Some(b), _) => b.clone(),
            (None, Some(u)) => branch_project_for(u, project),
            (None, None) => {
                return Err(
                    "could not read the OBS user to name the branch project; pass --branch-project"
                        .to_string(),
                );
            }
        };
        let listed = osc::try_list_project_packages(project, emit)?;
        let source = if opts.packages.iter().all(|p| listed.contains(p)) {
            project.clone()
        } else {
            let meta = osc::get_project_meta(project, emit)
                .map_err(|e| format!("could not read the meta of {project}: {e}"))?
                .unwrap_or_default();
            match planning_source(project, false, &meta) {
                Some(s) => {
                    (host.say)(&format!(
                        "{project} has no update of the package yet; browsing {s}, which it links to"
                    ));
                    s
                }
                None => {
                    return Err(format!(
                        "{project} does not have the package and links to no project that does"
                    ));
                }
            }
        };
        targets.push(Target {
            project: project.clone(),
            source,
            branch,
        });
    }

    // Each code stream is built from its own package; a failed one is noted
    // and the others go on.
    let mut build_failed: Vec<(String, String)> = Vec::new();
    for t in &targets {
        (host.say)(&format!(
            "code stream {} ({}): building in {} from {}",
            t.project,
            opts.mode.name(),
            t.branch,
            t.source
        ));
        let bj = BuildJob {
            source: &t.source,
            branch: &t.branch,
            release: &t.project,
            mode: opts.mode,
            backport: job.as_ref(),
        };
        if let Err(e) = (host.build)(&bj) {
            (host.warn)(&format!("{}: {e}", t.project));
            build_failed.push((t.project.clone(), e));
        }
    }
    // With one package the build's error is that package's; with several, its
    // state on OBS decides per package below.
    let failed_build = |t: &Target| -> Option<String> {
        if opts.packages.len() != 1 {
            return None;
        }
        build_failed
            .iter()
            .find(|(p, _)| *p == t.project)
            .map(|(_, e)| e.clone())
    };
    let bullets = backport_bullets(job.as_deref(), &refs);

    if opts.dry_run {
        (host.say)(&format!(
            "dry run: no maintenance request is created; each package that builds would go to {target_project}:"
        ));
        for (i, pkg) in opts.packages.iter().enumerate() {
            let mut ok: Vec<&Target> = Vec::new();
            for t in &targets {
                match failed_build(t) {
                    Some(e) => reports[i].push(StreamReport {
                        stream: t.project.clone(),
                        outcome: StreamOutcome::Failed(e),
                    }),
                    None => {
                        reports[i].push(StreamReport {
                            stream: t.project.clone(),
                            outcome: StreamOutcome::Ready(format!(
                                "would be built ({})",
                                opts.mode.name()
                            )),
                        });
                        ok.push(t);
                    }
                }
            }
            let entry = if bullets.is_empty() {
                format!("<the newest .changes entry, with {}>", refs_suffix(&refs))
            } else {
                bullets
                    .iter()
                    .map(|(_, b)| b.clone())
                    .collect::<Vec<_>>()
                    .join("\n")
            };
            let req = MaintenanceRequest {
                target_project: target_project.clone(),
                actions: ok
                    .iter()
                    .map(|t| IncidentAction {
                        source_project: t.branch.clone(),
                        source_package: pkg.clone(),
                        release_project: t.project.clone(),
                    })
                    .collect(),
                sourceupdate: ok
                    .iter()
                    .all(|t| sourceupdate_for(&t.branch, user.as_deref()).is_some())
                    .then(|| "cleanup".to_string()),
                description: request_description(pkg, &mode_text, &[("".into(), Some(entry))], &[]),
            };
            for l in plan_lines(&req, &refs, &mode_text, &[], &left_out(&reports[i])) {
                (host.say)(&l);
            }
        }
        print_summaries(&reports);
        return Ok(());
    }

    let mut failures: Vec<String> = Vec::new();
    for (i, pkg) in opts.packages.iter().enumerate() {
        let mut ready: Vec<Ready> = Vec::new();
        for t in &targets {
            let outcome = match failed_build(t) {
                Some(e) => Err(StreamOutcome::Failed(e)),
                None => check_stream(t, pkg, &refs, opts, job.as_deref()),
            };
            match outcome {
                Ok(r) => ready.push(r),
                Err(o) => {
                    (host.say)(&format!(
                        "no maintenance request for {pkg} in {}: {}",
                        t.project,
                        match &o {
                            StreamOutcome::Failed(w)
                            | StreamOutcome::LeftOut(w)
                            | StreamOutcome::NotAffected(w)
                            | StreamOutcome::Ready(w) => w.as_str(),
                            StreamOutcome::GitMaintained => "maintained in git",
                        }
                    ));
                    reports[i].push(StreamReport {
                        stream: t.project.clone(),
                        outcome: o,
                    });
                }
            }
        }
        if ready.is_empty() {
            continue;
        }
        let entries: Vec<(String, Option<String>)> = ready
            .iter()
            .map(|r| (r.action.release_project.clone(), r.entry.clone()))
            .collect();
        let notes: Vec<(String, String)> = ready
            .iter()
            .map(|r| (r.action.release_project.clone(), r.note.clone()))
            .collect();
        let sourceupdate = ready
            .iter()
            .all(|r| sourceupdate_for(&r.action.source_project, user.as_deref()).is_some())
            .then(|| "cleanup".to_string());
        let req = MaintenanceRequest {
            target_project: target_project.clone(),
            actions: ready.iter().map(|r| r.action.clone()).collect(),
            sourceupdate,
            description: request_description(pkg, &mode_text, &entries, &refs),
        };
        for l in plan_lines(&req, &refs, &mode_text, &notes, &left_out(&reports[i])) {
            (host.say)(&l);
        }
        for r in &ready {
            if r.amended.is_some() {
                (host.say)(&format!(
                    "  the changelog bullets and references are added to the newest entry of {}/{} first",
                    r.action.source_project, pkg
                ));
            }
        }
        // `--yes` approves without asking, so a package whose change needs a
        // review (or whose recorded state cannot be read) gets no request.
        let blockers = if opts.yes {
            let roots: Vec<(String, PathBuf)> = ready
                .iter()
                .map(|r| {
                    let branch = r.action.source_project.clone();
                    let root = branch_state_root(&opts.work_dir, &branch, &opts.arch);
                    (branch, root)
                })
                .collect();
            yes_blockers(&roots, pkg)
        } else {
            Vec::new()
        };
        if !blockers.is_empty() {
            (host.warn)(&format!(
                "--yes refuses to create the maintenance request for {pkg}: it needs a review; \
                 run without --yes to review it"
            ));
            for line in &blockers {
                (host.warn)(&format!("  {line}"));
            }
            for r in &ready {
                reports[i].push(StreamReport {
                    stream: r.action.release_project.clone(),
                    outcome: StreamOutcome::Failed(
                        "--yes refuses to submit a change that needs a review".into(),
                    ),
                });
            }
            continue;
        }
        if !opts.yes && !(host.confirm)(&format!("Create the maintenance request for {pkg}?")) {
            (host.say)(&format!("{pkg}: no maintenance request (declined)"));
            for r in &ready {
                reports[i].push(StreamReport {
                    stream: r.action.release_project.clone(),
                    outcome: StreamOutcome::LeftOut("the request was declined".into()),
                });
            }
            continue;
        }
        // A code stream whose changelog cannot be amended is left out; the
        // others still go.
        let mut sent: Vec<Ready> = Vec::new();
        let mut bases: Vec<PathBuf> = Vec::new();
        for r in ready {
            if let Some((name, _)) = &r.amended {
                match commit_refs(&r.action.source_project, pkg, name, opts, &refs, &bullets) {
                    Ok(base) => bases.extend(base),
                    Err(e) => {
                        reports[i].push(StreamReport {
                            stream: r.action.release_project.clone(),
                            outcome: StreamOutcome::Failed(format!(
                                "could not add the changelog bullets and references in {}: {e}",
                                r.action.source_project
                            )),
                        });
                        continue;
                    }
                }
            }
            sent.push(r);
        }
        if sent.is_empty() {
            continue;
        }
        let req = MaintenanceRequest {
            actions: sent.iter().map(|r| r.action.clone()).collect(),
            ..req
        };
        match create_request(&req, emit) {
            Ok(id) => {
                for base in &bases {
                    let _ = std::fs::remove_dir_all(base);
                }
                // The request text carries the flags from now on.
                for r in &sent {
                    let root =
                        branch_state_root(&opts.work_dir, &r.action.source_project, &opts.arch);
                    let _ = crate::fix_flags::clear_submitted(&root, pkg);
                    let _ = crate::risk_flags::clear_submitted(&root, pkg);
                }
                (host.say)(&format!(
                    "created maintenance request {id} for {pkg} to {}",
                    req.target_project
                ));
                for r in &sent {
                    reports[i].push(StreamReport {
                        stream: r.action.release_project.clone(),
                        outcome: StreamOutcome::Ready(format!("{} (request {id})", r.note)),
                    });
                }
            }
            Err(e) => {
                let not_restored = undo_refs(&bases).map_or(String::new(), |why| {
                    format!("; the OBS baseline was NOT restored: {why}")
                });
                for r in &sent {
                    reports[i].push(StreamReport {
                        stream: r.action.release_project.clone(),
                        outcome: StreamOutcome::Failed(format!(
                            "the maintenance request could not be created: {e}{not_restored}"
                        )),
                    });
                }
            }
        }
    }
    print_summaries(&reports);
    for (pkg, r) in opts.packages.iter().zip(&reports) {
        for rep in r {
            if let StreamOutcome::Failed(why) = &rep.outcome {
                failures.push(format!("{pkg} in {}: {why}", rep.stream));
            }
        }
    }
    if failures.is_empty() {
        Ok(())
    } else {
        Err(failures.join("; "))
    }
}
