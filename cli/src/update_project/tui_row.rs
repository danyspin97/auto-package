//! The TUI row of a package whose build is handed over: queued at once, and
//! closed by the run when its build ends without closing it.

use super::*;
use ap_core::event::{PackageOutcome, SelectionReason};
use ap_core::sink::NullReporter;
use std::sync::atomic::{AtomicBool, Ordering};

/// The row `cmd_build` reports `item` under: the pipeline's name of a PyPI
/// package, the lowercased name of a repository or release one. `None` for a
/// package the pipeline cannot build, which never gets a row.
pub(crate) fn row_name(item: &Item) -> Option<String> {
    match &item.ecosystem {
        Ecosystem::Pypi { pypi_name } => Some(osc::rpm_name(pypi_name)),
        Ecosystem::GitRepo { .. } | Ecosystem::Index { .. } => Some(item.pkg.to_lowercase()),
        Ecosystem::Repology { .. } | Ecosystem::Rubygems { .. } | Ecosystem::Cpan { .. } => None,
    }
}

/// A reporter that reaches the TUI only: the plain output and the run log
/// do not list a package before its build starts.
fn tui_only(ctx: &Ctx<'_>) -> Option<Arc<dyn Reporter + Send + Sync>> {
    let t = ctx.tui_ctrl.as_ref()?;
    Some(t.tap(Arc::new(NullReporter), None))
}

/// Show `item` as queued under `row` in the TUI.
pub(super) fn queue_row(ctx: &Ctx<'_>, row: &str, item: &Item) {
    if let Some(r) = tui_only(ctx) {
        r.report(&Event::TargetSelected {
            pkg: row.to_string(),
            version: item.new_version.clone(),
            reason: SelectionReason::RequestedRoot,
            blocked_by: vec![],
            from_version: Some(item.old_version.clone()),
        });
    }
}

/// Close `row` in the TUI with what its item ended with.
pub(super) fn close_row(ctx: &Ctx<'_>, row: &str, item: &Item, outcome: &ItemOutcome) {
    if let Some(r) = tui_only(ctx) {
        r.report(&Event::PackageFinished {
            pkg: row.to_string(),
            version: item.new_version.clone(),
            outcome: package_outcome(outcome),
        });
    }
}

/// The row's outcome for an item's.
pub(super) fn package_outcome(outcome: &ItemOutcome) -> PackageOutcome {
    match outcome {
        ItemOutcome::Updated => PackageOutcome::Built,
        ItemOutcome::AlreadyUpdated => PackageOutcome::AlreadyBuilt,
        ItemOutcome::Skipped(reason) => PackageOutcome::Skipped {
            reason: reason.clone(),
        },
        ItemOutcome::Failed(reason) => PackageOutcome::Failed {
            reason: reason.clone(),
            disk_full: false,
        },
    }
}

/// Passes every event on and notes whether the build closed `row`.
pub(super) struct FinishWatch {
    inner: Arc<dyn Reporter + Send + Sync>,
    pub(super) row: String,
    finished: AtomicBool,
}

impl FinishWatch {
    pub(super) fn new(inner: Arc<dyn Reporter + Send + Sync>, row: String) -> Self {
        FinishWatch {
            inner,
            row,
            finished: AtomicBool::new(false),
        }
    }

    pub(super) fn finished(&self) -> bool {
        self.finished.load(Ordering::SeqCst)
    }
}

impl Reporter for FinishWatch {
    fn report(&self, event: &Event) {
        if let Event::PackageFinished { pkg, .. } = event
            && *pkg == self.row
        {
            self.finished.store(true, Ordering::SeqCst);
        }
        self.inner.report(event);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(pkg: &str, ecosystem: Ecosystem) -> Item {
        Item {
            pkg: pkg.to_string(),
            origin: "devel:languages:python".to_string(),
            ecosystem,
            old_version: "1.0".to_string(),
            new_version: "2.0".to_string(),
            upstream_tag: None,
            dependency_of: None,
            findings: vec![],
        }
    }

    #[test]
    fn the_row_is_named_as_the_build_names_it() {
        let pypi = item(
            "python-APScheduler",
            Ecosystem::Pypi {
                pypi_name: "APScheduler".to_string(),
            },
        );
        assert_eq!(
            row_name(&pypi).as_deref(),
            Some(osc::rpm_name("APScheduler").as_str())
        );
        let repo = item(
            "Foo",
            Ecosystem::GitRepo {
                url: "https://github.com/x/foo".to_string(),
            },
        );
        assert_eq!(row_name(&repo).as_deref(), Some("foo"));
    }

    #[test]
    fn the_watch_notes_only_its_own_row_and_passes_everything_on() {
        let sink = Arc::new(MemorySink::new());
        let watch = FinishWatch::new(sink.clone(), "foo".to_string());
        let finished = |pkg: &str| Event::PackageFinished {
            pkg: pkg.to_string(),
            version: "2.0".to_string(),
            outcome: PackageOutcome::Built,
        };
        watch.report(&finished("bar"));
        assert!(!watch.finished());
        watch.report(&finished("foo"));
        assert!(watch.finished());
        assert_eq!(sink.finished().len(), 2);
    }

    #[test]
    fn a_row_ends_as_its_item_did() {
        assert!(matches!(
            package_outcome(&ItemOutcome::Updated),
            PackageOutcome::Built
        ));
        assert!(matches!(
            package_outcome(&ItemOutcome::Failed("x".into())),
            PackageOutcome::Failed {
                disk_full: false,
                ..
            }
        ));
        assert!(matches!(
            package_outcome(&ItemOutcome::Skipped("x".into())),
            PackageOutcome::Skipped { .. }
        ));
    }
}
