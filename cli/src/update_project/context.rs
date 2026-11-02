use super::*;

/// Everything a run produced, kept outside the `Result` so the summary is
/// printed even when the run fails part-way.
#[derive(Default)]
pub(super) struct Report {
    pub(super) results: Vec<ItemResult>,
    pub(super) paths: Vec<PathAddition>,
    /// Capabilities with no provider in a repository the branch builds in.
    pub(super) requirements: Vec<MissingRequirement>,
    /// The other-architecture results of the updated packages.
    pub(super) arch: Vec<ap_build::arch_check::ArchRow>,
}

/// What the build needs from `run`'s setup.
pub(super) struct Ctx<'a> {
    pub(super) work_dir: PathBuf,
    pub(super) state_root: PathBuf,
    pub(super) rpms_dir: PathBuf,
    pub(super) source_urls: Arc<ap_build::sources::SourceMap>,
    pub(super) python_flavors: Arc<ap_build::flavors::PythonFlavors>,
    pub(super) run_dir: &'a RunDir,
    pub(super) reporter: Arc<dyn Reporter + Send + Sync>,
    pub(super) base_reporter: Arc<dyn Reporter + Send + Sync>,
    pub(super) tui_ctrl: Option<tui::TuiController>,
    pub(super) sem: Option<Semaphore>,
    /// Reads diffs and makes submit requests for the TUI (see [`SubmitBackend`]).
    pub(super) backend: Option<Arc<SubmitBackend>>,
    /// The packages whose other-architecture check ran in this run: their
    /// results were already awaited, so a submit does not wait for them again.
    pub(super) arch_checked: ArchChecked,
}

/// The packages the arch check ran for, shared with the TUI's submit backend.
pub(super) type ArchChecked = Arc<std::sync::Mutex<std::collections::HashSet<String>>>;

impl Ctx<'_> {
    pub(super) fn say(&self, message: impl Into<String>) {
        self.reporter.report(&Event::Progress {
            step: None,
            message: message.into(),
        });
    }

    pub(super) fn warn(&self, message: impl Into<String>) {
        self.reporter.report(&Event::Warning {
            step: None,
            message: message.into(),
        });
    }
}
