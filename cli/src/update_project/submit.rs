use super::*;

/// The TUI's way to read diffs and submit. Every package that goes to the
/// build is registered here with its origin, and the request is made by
/// [`prepare_request`] and `osc::create_submit_request`, the code `--submit`
/// runs, so the answer cannot differ between the two.
pub(super) struct SubmitBackend {
    pub(super) branch: String,
    pub(super) distro: Option<String>,
    pub(super) arch: String,
    /// Where each package's state, with its fix-loop flags, is kept.
    pub(super) state_root: PathBuf,
    /// `--yes` was given: a request for a change that needs a review is refused.
    pub(super) yes: bool,
    /// `--allow-arch-failures` was given (see `prepare_request`).
    pub(super) allow_arch: bool,
    /// The packages whose other-architecture check ran (see [`ArchChecked`]).
    pub(super) arch_checked: ArchChecked,
    pub(super) reporter: Arc<dyn Reporter + Send + Sync>,
    pub(super) items: std::sync::Mutex<std::collections::HashMap<String, Item>>,
    /// `pkg: SR #id` for each request made from the TUI, for the run summary.
    pub(super) created: std::sync::Mutex<Vec<String>>,
}

impl SubmitBackend {
    pub(super) fn register(&self, item: &Item) {
        self.items
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .insert(osc::fuzzy_key(&item.pkg), item.clone());
    }

    fn item(&self, row: &str) -> Result<Item, String> {
        self.items
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .get(&osc::fuzzy_key(row))
            .cloned()
            .ok_or_else(|| format!("{row} is not a package of this run's branch project"))
    }

    fn request(&self, row: &str) -> Result<(Item, osc::SubmitRequest), tui::SubmitFailure> {
        let item = self.item(row).map_err(tui::SubmitFailure::Other)?;
        let gate = review_gate(&self.state_root, &[item.pkg.as_str()]);
        if let Some(lines) = yes_refusal(self.yes, &gate) {
            return Err(tui::SubmitFailure::Other(lines.join("; ")));
        }
        // The TUI only offers rows that built; OBS is asked again inside.
        match prepare_request(
            &self.branch,
            self.distro.as_deref(),
            &self.arch,
            &self.state_root,
            &item,
            &ItemOutcome::Updated,
            self.allow_arch,
            self.arch_checked_for(&item.pkg),
            self.reporter.as_ref(),
            None,
        ) {
            Ok(sr) => Ok((item, sr)),
            Err(outcome) => Err(submit_failure(outcome)),
        }
    }

    fn arch_checked_for(&self, pkg: &str) -> bool {
        self.arch_checked
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .contains(pkg)
    }

    pub(super) fn created_lines(&self) -> Vec<String> {
        self.created
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
    }
}

/// Why a request was not made, as the TUI shows it.
pub(super) fn submit_failure(outcome: SrOutcome) -> tui::SubmitFailure {
    match outcome {
        SrOutcome::Existing(id) => tui::SubmitFailure::Existing(id),
        SrOutcome::NotEligible(why) => tui::SubmitFailure::Other(format!("not eligible: {why}")),
        SrOutcome::Failed(why) => tui::SubmitFailure::Other(why),
        SrOutcome::Declined => tui::SubmitFailure::Other("declined".to_string()),
        SrOutcome::NeedsReview => {
            tui::SubmitFailure::Other("needs a review: run without --yes to review it".to_string())
        }
        SrOutcome::Created(id) => tui::SubmitFailure::Other(format!("already created: {id}")),
        SrOutcome::NotRequested => tui::SubmitFailure::Other("not requested".to_string()),
    }
}

impl tui::PackageBackend for SubmitBackend {
    fn diff(&self, row: &str) -> Result<String, String> {
        let item = self.item(row)?;
        rdiff(
            &item.origin,
            &item.pkg,
            &self.branch,
            self.reporter.as_ref(),
        )
    }

    fn preview(&self, row: &str) -> Result<tui::SubmitPreview, tui::SubmitFailure> {
        let (item, sr) = self.request(row)?;
        Ok(tui::SubmitPreview {
            summary: up::sr_confirmation_lines(&self.branch, &[&item])
                .pop()
                .unwrap_or_default(),
            source: format!("{}/{}", sr.source_project, sr.source_package),
            target: format!("{}/{}", sr.target_project, sr.target_package),
            description: sr.description,
        })
    }

    fn submit(&self, row: &str) -> Result<String, tui::SubmitFailure> {
        let (_, sr) = self.request(row)?;
        match osc::create_submit_request(&sr, Emit::run(self.reporter.as_ref())) {
            Ok(id) => {
                forget_submitted_flags(&self.state_root, &sr.source_package);
                self.reporter.report(&Event::Progress {
                    step: None,
                    message: format!(
                        "created request {id}: {}/{} -> {}/{}",
                        sr.source_project, sr.source_package, sr.target_project, sr.target_package
                    ),
                });
                self.created
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .push(format!(
                        "{}: request {id} to {}",
                        sr.source_package, sr.target_project
                    ));
                Ok(id)
            }
            Err(e) => {
                self.reporter.report(&Event::Warning {
                    step: None,
                    message: format!(
                        "could not create the request for {}: {e}",
                        sr.source_package
                    ),
                });
                Err(tui::SubmitFailure::Other(e.to_string()))
            }
        }
    }
}

/// Drops the fix-loop and risk flags of a package whose request was created:
/// the request text carries them from then on. A failure keeps them, which is
/// the safe side: they are shown again rather than lost.
fn forget_submitted_flags(state_root: &Path, pkg: &str) {
    let _ = ap_build::fix_flags::clear_submitted(state_root, pkg);
    let _ = ap_build::risk_flags::clear_submitted(state_root, pkg);
}

/// Create a submit request, to its origin project, for each package that
/// built at the new version in the branch project.
///
/// Conservative at every step: a package whose build or version cannot be
/// confirmed gets no request; one with an open request from the branch
/// project already gets none either (and a request list that cannot be read
/// counts as unknown, not as empty); and nothing is created before the list
/// is shown and confirmed, unless `--yes`.
pub(super) fn submit_requests(
    a: &UpdateProjectArgs,
    branch: &str,
    results: &mut [ItemResult],
    ctx: &Ctx<'_>,
) -> color_eyre::Result<()> {
    let emit = Emit::run(ctx.reporter.as_ref());
    let mut pending: Vec<(usize, osc::SubmitRequest)> = Vec::new();
    for (idx, r) in results.iter_mut().enumerate() {
        match prepare_request(
            branch,
            a.distro.as_deref(),
            &a.arch,
            &ctx.state_root,
            &r.item,
            &r.outcome,
            a.allow_arch_failures,
            ctx.arch_checked
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .contains(&r.item.pkg),
            ctx.reporter.as_ref(),
            None,
        ) {
            Ok(sr) => pending.push((idx, sr)),
            Err(sr) => r.sr = sr,
        }
    }
    if pending.is_empty() {
        return Ok(());
    }

    let items: Vec<&Item> = pending.iter().map(|(i, _)| &results[*i].item).collect();
    ctx.say(format!("submit requests to create ({}):", items.len()));
    for (line, item) in up::sr_confirmation_lines(branch, &items).iter().zip(&items) {
        ctx.say(format!("  {line}"));
        // What the change adds that needs a look, under the request it is in.
        for risk in ap_build::risk_flags::lines_of_package(&ctx.state_root, &item.pkg) {
            ctx.say(format!("      {risk}"));
        }
    }
    // `--yes` approves the batch without asking, so a batch with a change that
    // needs a review is refused whole: nothing is submitted.
    let names: Vec<&str> = pending
        .iter()
        .map(|(i, _)| results[*i].item.pkg.as_str())
        .collect();
    if let Some(lines) = yes_refusal(a.yes, &review_gate(&ctx.state_root, &names)) {
        for (idx, _) in &pending {
            results[*idx].sr = SrOutcome::NeedsReview;
        }
        for line in &lines {
            ctx.warn(line.clone());
        }
        return Err(refused_by_yes(&lines));
    }
    if !a.yes && !confirm(&format!("Create {} submit request(s)?", pending.len()), ctx) {
        for (idx, _) in &pending {
            results[*idx].sr = SrOutcome::Declined;
        }
        return Ok(());
    }

    for (idx, sr) in pending {
        results[idx].sr = match osc::create_submit_request(&sr, emit) {
            Ok(id) => {
                ctx.say(format!(
                    "created request {id}: {}/{} -> {}/{}",
                    sr.source_project, sr.source_package, sr.target_project, sr.target_package
                ));
                forget_submitted_flags(&ctx.state_root, &sr.source_package);
                SrOutcome::Created(id)
            }
            Err(e) => {
                ctx.warn(format!(
                    "could not create the request for {}: {e}",
                    sr.source_package
                ));
                SrOutcome::Failed(e.to_string())
            }
        };
    }
    Ok(())
}

/// The packages among `pkgs` whose change needs a review (it has risk flags
/// recorded), with those flags' lines, in the order given.
pub(super) fn review_gate(state_root: &Path, pkgs: &[&str]) -> ReviewGate {
    let mut gate = ReviewGate::default();
    for pkg in pkgs {
        match ap_build::risk_flags::try_of_package(state_root, pkg) {
            Ok(flags) => {
                let lines: Vec<String> = flags.iter().map(|f| f.line()).collect();
                if !lines.is_empty() {
                    gate.needing.push((pkg.to_string(), lines));
                }
            }
            Err(e) => gate.unreadable.push(format!("{pkg}: {e}")),
        }
    }
    gate
}

/// What the review gate found among the packages of a batch.
#[derive(Debug, Default, PartialEq)]
pub(super) struct ReviewGate {
    /// The packages that need a review, with each risk's line.
    pub(super) needing: Vec<(String, Vec<String>)>,
    /// The packages whose recorded state cannot be read, with why. Nothing is
    /// known about them, so `--yes` does not go ahead for them either.
    pub(super) unreadable: Vec<String>,
}

/// What `--yes` refuses: when it is given and any package needs a review or
/// has a state that cannot be read, the message first, then each such package
/// and its flags or its error. `None` when the requests may go on (no `--yes`,
/// or nothing needs a review and every state was read).
pub(super) fn yes_refusal(yes: bool, gate: &ReviewGate) -> Option<Vec<String>> {
    if !yes || (gate.needing.is_empty() && gate.unreadable.is_empty()) {
        return None;
    }
    let mut lines = Vec::new();
    if !gate.needing.is_empty() {
        lines.push(format!(
            "--yes refuses to submit: {} package(s) need a review, nothing was submitted; \
             run without --yes to review them",
            gate.needing.len()
        ));
        for (pkg, risks) in &gate.needing {
            lines.push(format!("  {pkg}"));
            lines.extend(risks.iter().map(|r| format!("    {r}")));
        }
    }
    if !gate.unreadable.is_empty() {
        lines.push(format!(
            "--yes refuses to submit: the recorded state of {} package(s) cannot be read, \
             nothing was submitted; run without --yes to look at them",
            gate.unreadable.len()
        ));
        lines.extend(gate.unreadable.iter().map(|u| format!("  {u}")));
    }
    Some(lines)
}

/// The error of a batch that `--yes` refused (`lines` from `yes_refusal`): a
/// `NotSubmitted`, so `main` exits 5 and nothing was submitted.
pub(super) fn refused_by_yes(lines: &[String]) -> color_eyre::Report {
    crate::session::NotSubmitted(lines[0].clone()).into()
}
