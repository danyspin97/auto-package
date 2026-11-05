use super::*;

/// Whether `pkg` in the branch of `t` may go into a request; how the stream
/// ended otherwise.
pub(super) fn check_stream(
    t: &Target,
    pkg: &str,
    refs: &[String],
    opts: &Options,
    job: Option<&backport::Job>,
) -> Result<Ready, StreamOutcome> {
    let emit = Emit::quiet();
    let repo = osc::choose_repository(&t.branch, opts.distro.as_deref(), &opts.arch).0;
    match osc::get_overall_build_status(&t.branch, pkg, &repo, &opts.arch) {
        Ok((osc::BuildStatus::Succeeded, _)) => {}
        Ok((status, at)) => {
            let at = at.map(|a| format!(" on {a}")).unwrap_or_default();
            return Err(StreamOutcome::Failed(format!(
                "its build in {} is {status}{at}, not succeeded",
                t.branch
            )));
        }
        Err(e) => {
            return Err(StreamOutcome::Failed(format!(
                "could not read its build status in {}: {e}",
                t.branch
            )));
        }
    }
    let Some((name, text)) = read_changes(&t.branch, pkg, emit) else {
        return Err(StreamOutcome::Failed(format!(
            "could not read its .changes in {}",
            t.branch
        )));
    };
    let entry = newest_entry_body(&text);
    let origin_entry = read_changes(&t.project, pkg, emit).and_then(|(_, o)| newest_entry_body(&o));
    if entry.is_some() && entry == origin_entry {
        return Err(StreamOutcome::LeftOut(format!(
            "{} has no change of its own over {}",
            t.branch, t.project
        )));
    }
    let branch_spec = crate::update::spec_for_package(&t.branch, pkg, emit).ok();
    let branch_version = branch_spec
        .as_deref()
        .and_then(crate::spec::parse_header)
        .map(|h| h.version);
    let note = match opts.mode {
        Mode::Update => format!(
            "updated to {}",
            branch_version.as_deref().unwrap_or("an unread version")
        ),
        Mode::Backport | Mode::Lint => {
            let release_version =
                spec_version(&t.project, pkg, emit).or_else(|| spec_version(&t.source, pkg, emit));
            let version = version_kept(release_version.as_deref(), branch_version.as_deref())
                .map_err(StreamOutcome::Failed)?;
            match job {
                Some(job) => {
                    let spec = branch_spec.unwrap_or_default();
                    let mut notes = Vec::new();
                    for p in &job.patches {
                        let committed = osc::cat_file(&t.branch, pkg, &p.file_name, emit);
                        match backport::classify(p, committed.as_deref(), &spec) {
                            backport::Applied::Missing(why) => {
                                return Err(StreamOutcome::Failed(format!(
                                    "the backport did not go in: {why}"
                                )));
                            }
                            applied => notes.push(format!("{} {applied}", p.file_name)),
                        }
                    }
                    format!("{version}: {}", notes.join("; "))
                }
                None => format!("packaging fixes at {version}"),
            }
        }
    };
    match open_incident(&t.branch, pkg, &t.project, emit) {
        Ok(Some(why)) => return Err(StreamOutcome::LeftOut(why)),
        Ok(None) => {}
        Err(e) => {
            return Err(StreamOutcome::Failed(format!(
                "could not check for an earlier request: {e}"
            )));
        }
    }
    let amended = amend_changes(&text, refs, &backport_bullets(job, refs));
    let entry = amended.as_deref().map_or(entry, newest_entry_body);
    Ok(Ready {
        action: IncidentAction {
            source_project: t.branch.clone(),
            source_package: pkg.to_string(),
            release_project: t.project.clone(),
        },
        amended: amended.map(|a| (name, a)),
        entry,
        note,
    })
}

/// Commit the backport bullets and the references into the branch's `.changes`.
/// Returns the baseline kept for the commit and the checkout it was saved from
/// (see [`undo_refs`]), `None` when nothing was committed. A commit that fails is undone on OBS here.
pub(super) fn commit_refs(
    branch: &str,
    pkg: &str,
    name: &str,
    opts: &Options,
    refs: &[String],
    bullets: &[(String, String)],
) -> Result<Option<(PathBuf, PathBuf)>, String> {
    let _obs_write = proc::obs_write();
    let emit = Emit::quiet();
    let dir = osc::checkout(branch, pkg, &opts.checkout_dir, emit).map_err(|e| e.to_string())?;
    let path = dir.join(name);
    let text = std::fs::read_to_string(&path)
        .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    let Some(amended) = amend_changes(&text, refs, bullets) else {
        return Ok(None);
    };
    let baseline = osc::begin_baseline(
        &opts.checkout_dir.join(osc::BASELINE_ROOT),
        &dir,
        branch,
        pkg,
        emit,
    )
    .map_err(|e| format!("cannot keep the OBS baseline of {branch}: {e}"))?;
    std::fs::write(&path, amended).map_err(|e| format!("cannot write {}: {e}", path.display()))?;
    osc::note_attempt(&baseline, &[name.to_string()]).map_err(|e| e.to_string())?;
    let committed = osc::commit_attempt(
        &baseline,
        &dir,
        &format!(
            "Add the bug references {} (auto-package maintenance)",
            refs.join(", ")
        ),
        &[name.to_string()],
        None,
        emit,
    );
    match committed {
        Ok(()) => Ok(Some((baseline, dir))),
        Err(e) => match osc::restore_baseline(&baseline, &dir, emit) {
            Ok(()) => Err(e.to_string()),
            Err(why) => Err(format!("{e}; the OBS baseline was NOT restored: {why}")),
        },
    }
}

/// The risk lines `--yes` must not submit past for `pkg`, read from the state
/// of each `(branch, state root)` it was built in: what the recorded state says
/// needs a review, or why that state cannot be read. A state that is missing
/// is a blocker too: the build that would have recorded the risks is the one
/// whose request is about to go.
pub(super) fn yes_blockers(roots: &[(String, PathBuf)], pkg: &str) -> Vec<String> {
    let mut out = Vec::new();
    for (branch, root) in roots {
        match crate::risk_flags::try_of_package(root, pkg) {
            Ok(flags) => out.extend(flags.iter().map(crate::risk_flags::RiskFlag::line)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => out.push(format!(
                "no recorded state for {pkg} in {branch}: whether its change needs a review is not known"
            )),
            Err(e) => out.push(format!(
                "its recorded state in {branch} cannot be read: {e}"
            )),
        }
    }
    out
}

/// The workspace state directory of the update run that built `branch` in
/// `work_dir` (`<work_dir>/<branch>-<arch>/state`, as `update` names it).
pub fn branch_state_root(work_dir: &std::path::Path, branch: &str, arch: &str) -> PathBuf {
    work_dir
        .join(format!(
            "{}-{}",
            ap_core::run::sanitize_name(branch),
            ap_core::run::sanitize_name(arch),
        ))
        .join("state")
}

/// Put back the OBS baselines of branches whose references were committed, for
/// a maintenance request that could not be created: the references must not
/// stay on the branches without their request. Returns what could not go back.
pub(super) fn undo_refs(bases: &[(PathBuf, PathBuf)]) -> Option<String> {
    let failed: Vec<String> = bases
        .iter()
        .filter_map(|(baseline, checkout)| {
            osc::restore_baseline(baseline, checkout, Emit::quiet()).err()
        })
        .collect();
    (!failed.is_empty()).then(|| failed.join("; "))
}

/// The [`StreamCheck`] of a run: `--affected-by` when given, else none.
pub fn stream_check_for(opts: &Options) -> Box<dyn StreamCheck> {
    match &opts.affected_by {
        Some(program) => Box::new(CommandCheck {
            program: program.clone(),
            timeout: AFFECTED_BY_TIMEOUT,
        }),
        None => Box::new(NoStreamCheck),
    }
}
