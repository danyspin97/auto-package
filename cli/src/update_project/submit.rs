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
    /// The run checked the Factory ports, so their results gate a submit (see
    /// `gate_counts_ports`).
    pub(super) include_ports: bool,
    /// The packages whose other-architecture check ran (see [`ArchChecked`]).
    pub(super) arch_checked: ArchChecked,
    /// Whether the ready gate judges a request (see `ready::ReadyCheck`).
    pub(super) ready: ready::ReadyCheck,
    /// The run's other-architecture deadline (see `ArchDeadline`).
    pub(super) arch_deadline: Arc<ArchDeadline>,
    /// The one bounded wait of the submits made before any build of the run
    /// ended (a TUI submit of an earlier run), started by the first of them.
    pub(super) untied_wait: std::sync::OnceLock<ArchDeadline>,
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

    /// Takes an item that ended Skipped out of the offers: it is not submitted.
    pub(super) fn forget(&self, item: &Item) {
        self.items
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .remove(&osc::fuzzy_key(&item.pkg));
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
        let own = self
            .untied_wait
            .get_or_init(|| batch_deadline(&self.arch_deadline, Instant::now()));
        let arch_wait = if self.arch_checked_for(&item.pkg) {
            ArchWait::Awaited
        } else {
            ArchWait::Until(submit_deadline(&self.arch_deadline, own))
        };
        // The rows offered are this run's items that were not forgotten (a
        // Skipped item is); whether the build succeeded is read again inside.
        match prepare_request(
            &self.branch,
            self.distro.as_deref(),
            &self.arch,
            &self.state_root,
            &item,
            &ItemOutcome::Updated,
            self.allow_arch,
            self.include_ports,
            arch_wait,
            self.reporter.as_ref(),
            None,
            self.ready,
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

/// The deadline of a submit wait not tied to a build of the run: one bound,
/// the run's `--arch-wait-minutes` (see `ArchDeadline::bound`), from `at`.
pub(super) fn batch_deadline(run: &ArchDeadline, at: Instant) -> ArchDeadline {
    ArchDeadline::starting_at(run.bound(), at)
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
    // The packages not checked wait for one deadline: the run's once a build
    // of the run ended, else one bound for this batch (not one per package).
    let batch = batch_deadline(&ctx.arch_deadline, Instant::now());
    let wait = submit_deadline(&ctx.arch_deadline, &batch);
    let mut pending: Vec<(usize, osc::SubmitRequest)> = Vec::new();
    for (idx, r) in results.iter_mut().enumerate() {
        let checked = ctx
            .arch_checked
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .contains(&r.item.pkg);
        match prepare_request(
            branch,
            a.distro.as_deref(),
            &a.arch,
            &ctx.state_root,
            &r.item,
            &r.outcome,
            a.allow_arch_failures,
            super::arch::gate_counts_ports(a),
            if checked {
                ArchWait::Awaited
            } else {
                ArchWait::Until(wait)
            },
            ctx.reporter.as_ref(),
            None,
            ready::check_for(a),
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

    // A dependency's request is made before the request of what requires it,
    // so that request can name it.
    let order = requires_order(&items);
    let mut slots: Vec<Option<(usize, osc::SubmitRequest)>> =
        pending.into_iter().map(Some).collect();
    let pending: Vec<(usize, osc::SubmitRequest)> =
        order.into_iter().filter_map(|k| slots[k].take()).collect();
    let mut created: Vec<(String, String)> = Vec::new();
    for (idx, mut sr) in pending {
        sr.description = name_created(&sr.description, &results[idx].item.requires, &created);
        results[idx].sr = match osc::create_submit_request(&sr, emit) {
            Ok(id) => {
                created.push((sr.source_package.clone(), id.clone()));
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

/// The order to create the requests of `items` in, as indices into `items`:
/// each item after the items its `requires` name (by package), so that a
/// request can name the request of a dependency it requires. Otherwise the
/// order given is kept. A cycle does not hang: an item is visited once.
pub(super) fn requires_order(items: &[&Item]) -> Vec<usize> {
    fn visit(i: usize, items: &[&Item], seen: &mut [bool], out: &mut Vec<usize>) {
        if std::mem::replace(&mut seen[i], true) {
            return;
        }
        for r in &items[i].requires {
            if let Some(j) = items.iter().position(|o| o.pkg == r.pkg) {
                visit(j, items, seen, out);
            }
        }
        out.push(i);
    }
    let mut seen = vec![false; items.len()];
    let mut out = Vec::with_capacity(items.len());
    for i in 0..items.len() {
        visit(i, items, &mut seen, &mut out);
    }
    out
}

/// `description` with each required update that it names by its branch
/// package named by its request instead, when `created` (package, request id)
/// has one for it.
pub(super) fn name_created(
    description: &str,
    requires: &[up::RequiredUpdate],
    created: &[(String, String)],
) -> String {
    let mut out = description.to_string();
    for r in requires.iter().filter(|r| r.request.is_none()) {
        let Some((_, id)) = created.iter().find(|(pkg, _)| *pkg == r.pkg) else {
            continue;
        };
        let with_id = up::RequiredUpdate {
            request: Some(id.clone()),
            ..r.clone()
        };
        out = out.replace(&r.line(), &with_id.line());
    }
    out
}

/// The packages among `pkgs` whose change needs a review (it has risk flags
/// recorded), with those flags' lines, in the order given.
pub(crate) fn review_gate(state_root: &Path, pkgs: &[&str]) -> ReviewGate {
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
pub(crate) struct ReviewGate {
    /// The packages that need a review, with each risk's line.
    pub(crate) needing: Vec<(String, Vec<String>)>,
    /// The packages whose recorded state cannot be read, with why. Nothing is
    /// known about them, so `--yes` does not go ahead for them either.
    pub(crate) unreadable: Vec<String>,
}

/// What `--yes` refuses: when it is given and any package needs a review or
/// has a state that cannot be read, the message first, then each such package
/// and its flags or its error. `None` when the requests may go on (no `--yes`,
/// or nothing needs a review and every state was read).
pub(crate) fn yes_refusal(yes: bool, gate: &ReviewGate) -> Option<Vec<String>> {
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

#[cfg(test)]
mod name_created_tests {
    use super::*;

    fn libbar(request: Option<&str>) -> up::RequiredUpdate {
        up::RequiredUpdate {
            pkg: "libbar".into(),
            branch_package: "home:u:branches:B/libbar".into(),
            requirement: "libbar-devel >= 2".into(),
            origin: "devel:libraries".into(),
            origin_version: "1".into(),
            request: request.map(str::to_string),
        }
    }

    #[test]
    fn a_request_created_in_the_batch_replaces_the_branch_package() {
        let text = format!("Update libfoo to 2 (was 1).\n\n{}", libbar(None).line());
        let created = [("libbar".to_string(), "77".to_string())];
        let out = name_created(&text, &[libbar(None)], &created);
        assert!(
            out.contains("Requires the update of libbar (request #77)"),
            "{out}"
        );
        assert!(!out.contains("branch package"), "{out}");
        // Nothing created for it: the branch package stays.
        assert_eq!(name_created(&text, &[libbar(None)], &[]), text);
    }
}

#[cfg(test)]
mod requires_order_tests {
    use super::*;

    fn item(pkg: &str, requires: &[&str]) -> Item {
        Item {
            pkg: pkg.into(),
            origin: "Base:System".into(),
            ecosystem: ap_build::update::Ecosystem::GitRepo {
                url: "https://example.org/x".into(),
            },
            old_version: "1".into(),
            new_version: "2".into(),
            upstream_tag: None,
            held_version: None,
            dependency_of: None,
            requires: requires
                .iter()
                .map(|pkg| up::RequiredUpdate {
                    pkg: pkg.to_string(),
                    branch_package: format!("home:u:branches:B/{pkg}"),
                    requirement: format!("{pkg}-devel >= 2"),
                    origin: "devel:libraries".into(),
                    origin_version: "1".into(),
                    request: None,
                })
                .collect(),
            findings: Vec::new(),
        }
    }

    fn order(items: &[Item]) -> Vec<&str> {
        let refs: Vec<&Item> = items.iter().collect();
        requires_order(&refs)
            .into_iter()
            .map(|i| items[i].pkg.as_str())
            .collect()
    }

    #[test]
    fn a_dependency_comes_before_what_requires_it_even_when_listed_after() {
        // app needs libfoo, libfoo needs libbar; listed the other way round.
        let items = [
            item("app", &["libfoo"]),
            item("libfoo", &["libbar"]),
            item("libbar", &[]),
        ];
        assert_eq!(order(&items), ["libbar", "libfoo", "app"]);
    }

    #[test]
    fn items_nothing_requires_keep_their_order() {
        let items = [item("a", &[]), item("b", &[]), item("c", &[])];
        assert_eq!(order(&items), ["a", "b", "c"]);
        let items = [
            item("a", &[]),
            item("app", &["lib"]),
            item("b", &[]),
            item("lib", &[]),
        ];
        assert_eq!(order(&items), ["a", "lib", "app", "b"]);
    }

    #[test]
    fn a_cycle_is_ordered_once_each() {
        let items = [item("a", &["b"]), item("b", &["a"])];
        let mut got = order(&items);
        got.sort();
        assert_eq!(got, ["a", "b"]);
    }

    #[test]
    fn a_requirement_on_a_package_not_pending_is_ignored() {
        let items = [item("app", &["elsewhere"]), item("b", &[])];
        assert_eq!(order(&items), ["app", "b"]);
    }
}
