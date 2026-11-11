use super::*;

/// The item of a maintenance backport for a candidate of the plan: the
/// package at the version it has (`old_version == new_version`), whatever
/// upstream released since.
pub(super) fn backport_item(origin: &str, c: &update::Candidate) -> Result<Item, String> {
    let pkg = up::branch_name(&c.rpm_name);
    let (ecosystem, version) = match &c.decision {
        update::Decision::Update {
            ecosystem,
            old_version,
            ..
        } => (ecosystem, old_version),
        update::Decision::UpToDate { ecosystem, version } => (ecosystem, version),
        update::Decision::Skipped(why) => {
            return Err(format!(
                "{pkg}: cannot be rebuilt for the backport: {why} (the pipeline needs to know its source)"
            ));
        }
    };
    if !ecosystem.can_build() {
        return Err(format!(
            "{pkg}: its source is known only through the {} lookup, which the pipeline cannot rebuild from",
            ecosystem.tag()
        ));
    }
    Ok(Item {
        pkg: pkg.to_string(),
        origin: origin.to_string(),
        ecosystem: ecosystem.clone(),
        old_version: version.clone(),
        new_version: version.clone(),
        upstream_tag: None,
        held_version: None,
        dependency_of: None,
        requires: Vec::new(),
        findings: Vec::new(),
    })
}

/// The packages of `--only` in the source of a backport run, each at its own
/// version.
pub(super) fn backport_items(a: &UpdateProjectArgs, emit: Emit<'_>) -> Result<Vec<Item>, String> {
    let candidates = update::plan_where(&a.source, &|n| wanted_by(&a.only, &a.exclude, n), emit)?;
    let mut items: Vec<Item> = Vec::new();
    for c in &candidates {
        let item = backport_item(&a.source, c)?;
        if !items.iter().any(|i| i.pkg == item.pkg) {
            items.push(item);
        }
    }
    if items.is_empty() {
        return Err(format!("{} has none of {}", a.source, a.only.join(", ")));
    }
    Ok(items)
}

/// Put the backports into the checkout the pipeline builds `item` from (the
/// pipeline checks out into `<work dir>/<project>/<package>` and keeps files it
/// does not track; one the spec does not name is not committed).
pub(super) fn stage_backport(
    branch: &str,
    item: &Item,
    job: &ap_build::backport::Job,
    ctx: &Ctx<'_>,
) -> Result<(), String> {
    let emit = Emit::run(ctx.reporter.as_ref());
    let dir = osc::checkout(branch, &item.pkg, &ctx.work_dir, emit).map_err(|e| e.to_string())?;
    // A file of the same name in the code stream itself is not ours to replace.
    let release_files = osc::list_package_files(&item.origin, &item.pkg, emit).unwrap_or_default();
    for note in ap_build::backport::backport_patches(&dir, job, &release_files)? {
        ctx.say(format!("{}: {note}", item.pkg));
    }
    Ok(())
}

/// `--maintenance` and `--maintenance-list`: the ordinary run once per code
/// stream, with the update project as PROJECT and its own branch project, then
/// the maintenance requests (see `ap_build::maintenance`).
pub(super) fn run_maintenance(
    a: UpdateProjectArgs,
    summary: &mut Option<String>,
) -> color_eyre::Result<()> {
    use ap_build::maintenance;
    let say = |m: &str| println!("{m}");
    let warn = |m: &str| ap_core::sink::eprint_coloured(&format!("warning: {m}"));
    // Without a terminal the maintenance run says itself what to pass instead.
    let ask = |q: &str| ask_line(q, &|_| {});
    let confirm = |q: &str| confirm_with(q, &warn);
    let mut nothing = |_: &maintenance::BuildJob<'_>| Ok(());
    if a.maintenance_list {
        let host = maintenance::Host {
            say: &say,
            warn: &warn,
            ask: &ask,
            confirm: &confirm,
            build: &mut nothing,
        };
        return maintenance::list(&a.only, &host).map_err(|e| eyre!(e));
    }
    if a.submit || a.in_place {
        return Err(eyre!(
            "--maintenance sends the change to the maintenance project: it cannot be combined with --submit or --in-place"
        ));
    }
    let mode =
        maintenance::Mode::parse(a.maintenance_mode.as_deref(), a.improve).map_err(|e| eyre!(e))?;
    if mode == maintenance::Mode::Backport && (a.to_version.is_some() || a.revision.is_some()) {
        return Err(eyre!(
            "--to-version and --revision pick a version; a backport keeps each code stream's own: \
             pass --maintenance-mode update for a version update"
        ));
    }
    let codestreams = a.maintenance.clone().unwrap_or_default();
    if !codestreams.is_empty() && !codestreams.iter().any(|c| c == &a.source || c == "all") {
        say(&format!(
            "--maintenance: the code streams are the sources; {} is not browsed",
            a.source
        ));
    }
    let opts = maintenance::Options {
        packages: a.only.clone(),
        codestreams,
        branch_project: a.branch_project.clone(),
        bugs: a.bug.clone(),
        no_bug_ref: a.no_bug_ref,
        dry_run: a.dry_run,
        yes: a.yes,
        distro: a.distro.clone(),
        arch: a.arch.clone(),
        checkout_dir: std::path::absolute(a.work_dir.join("maintenance"))?,
        work_dir: std::path::absolute(&a.work_dir)?,
        mode,
        backports: a.backport.clone(),
        backport_repo: a.backport_repo.clone(),
        affected_by: a.affected_by.clone(),
    };
    let mut summaries: Vec<String> = Vec::new();
    // Each code stream is its own ordinary run, from its own package.
    let mut build = |job: &maintenance::BuildJob<'_>| -> Result<(), String> {
        let mut one = a.clone();
        one.source = job.source.to_string();
        one.branch_project = Some(job.branch.to_string());
        one.maintenance = None;
        one.maintenance_source = true;
        one.backport_job = job.backport.cloned();
        let mut s = None;
        let result = run(one, &mut s);
        summaries.extend(s);
        result.map_err(|e| e.to_string())
    };
    let mut host = maintenance::Host {
        say: &say,
        warn: &warn,
        ask: &ask,
        confirm: &confirm,
        build: &mut build,
    };
    let result = maintenance::run(&opts, &mut host);
    if !summaries.is_empty() {
        *summary = Some(summaries.join("\n"));
    }
    result.map_err(|e| match e {
        maintenance::RunError::Refused(message) => crate::session::NotSubmitted(message).into(),
        maintenance::RunError::Failed(message) => eyre!(message),
    })
}
