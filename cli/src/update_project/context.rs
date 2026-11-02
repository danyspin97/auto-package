use super::*;

/// Everything a run produced, kept outside the `Result` so the summary is
/// printed even when the run fails part-way.
#[derive(Default)]
pub(super) struct Report {
    pub(super) results: Vec<ItemResult>,
    pub(super) paths: Vec<PathAddition>,
    /// Capabilities with no provider in a repository the branch builds in.
    pub(super) requirements: Vec<MissingRequirement>,
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
}

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
