use super::*;

/// Starts dependency builds: each runs on its own thread, reported as soon as
/// it is done, and settles its row when it finishes.
pub(super) struct DepSpawner<'a> {
    pub(super) run_dir: &'a RunDir,
    pub(super) reporter: &'a Arc<dyn Reporter + Send + Sync>,
    pub(super) tui_ctrl: &'a Option<tui::TuiController>,
    pub(super) pkg_reporter: &'a dyn Fn(&str) -> Arc<dyn Reporter + Send + Sync>,
    pub(super) sem: &'a Option<Semaphore>,
    pub(super) dep_drops: &'a DepDrops,
}

impl DepSpawner<'_> {
    /// Start `dep_config` for `dep`. `None` when its run directory cannot be
    /// created; the dependency is then dropped with that reason.
    pub(super) fn spawn(
        &self,
        dep_config: PipelineConfig,
        dep: String,
    ) -> Option<(
        String,
        String,
        std::thread::JoinHandle<PipelineResult>,
        String,
    )> {
        let dep_rpm = dep_config.rpm_name.clone();
        let dep_pkg_run = self.run_dir.pkg(&dep_rpm);
        if let Err(e) = dep_pkg_run.ensure() {
            self.dep_drops.dropped(
                self.reporter.as_ref(),
                &dep,
                format!(
                    "its run directory {} could not be created: {e}",
                    dep_pkg_run.path.display()
                ),
            );
            return None;
        }
        self.reporter.report(&Event::TargetSelected {
            pkg: dep_rpm.clone(),
            version: dep_config.version.clone(),
            reason: SelectionReason::FirstUnbuiltDependency,
            blocked_by: vec![],
            from_version: None,
        });
        // A failed request under the dependency's name is settled by this
        // row (see `withdraw_settled_failures`). It is not a request, so
        // a retry of this row must not resend the name (see `request_names`).
        if let Some(t) = &self.tui_ctrl {
            t.note_label(&dep_rpm, &dep);
        }
        let dep_version = dep_config.version.clone();
        let r = (self.pkg_reporter)(&dep_rpm);
        let sem2 = self.sem.clone();
        let finished_name = dep_rpm.clone();
        let finished_version = dep_version.clone();
        let handle = std::thread::spawn(move || {
            let _permit = Permit::acquire(&sem2);
            // A panic is a failure of this row, reported like any other
            // outcome, so the row settles and the run sees it finish.
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                run_package(&dep_config, &dep_pkg_run, r.as_ref())
            }))
            .unwrap_or_else(|p| PipelineResult::Failed {
                rpm_name: finished_name.clone(),
                reason: format!("build thread panicked: {}", panic_message(p.as_ref())),
                kind: FailureKind::Other,
                remedy: None,
            });
            r.report(&Event::PackageFinished {
                pkg: finished_name,
                version: finished_version,
                outcome: package_outcome(&result),
            });
            result
        });
        Some((dep_rpm, dep_version, handle, dep))
    }
}
