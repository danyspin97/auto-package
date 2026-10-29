use super::*;

/// Why a package's spec could not be read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReadError {
    /// OBS answered and the package has no spec file. Not retried.
    NoSpec,
    /// OBS could not be asked or did not answer (also: the run was
    /// interrupted); the text says which step failed.
    Failed(String),
}

impl std::fmt::Display for ReadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ReadError::NoSpec => write!(f, "the package has no spec file"),
            ReadError::Failed(why) => write!(f, "the spec could not be read: {why}"),
        }
    }
}

impl std::error::Error for ReadError {}

impl From<ReadError> for SkipReason {
    fn from(e: ReadError) -> Self {
        match e {
            ReadError::NoSpec => SkipReason::NoSpec,
            ReadError::Failed(why) => SkipReason::ReadFailed(why),
        }
    }
}

/// Fetch the primary spec from an OBS package, picking the right filename
/// out of whatever `osc ls` returns.
///
/// OBS answers slowly or not at all now and then (a sweep of hundreds of
/// packages saw `osc ls`/`osc cat` time out), so a failed read is repeated a
/// few times (waiting between them, and giving up when the run is
/// interrupted) before the package is given up on as [`ReadError::Failed`]. A
/// package with no spec at all is [`ReadError::NoSpec`]: not a failed read, and
/// not retried.
pub fn spec_for_package(project: &str, pkg: &str, emit: Emit<'_>) -> Result<String, ReadError> {
    read_spec(
        pkg,
        &|| osc::list_package_files(project, pkg, emit),
        &|file| osc::cat_file(project, pkg, file, emit),
        // An update project keeps a released package's sources in its newest
        // incident copy (`pkg.NNNNN`), the plain one empty.
        &|| {
            crate::maintenance::newest_incident_package(project, pkg, emit)
                .map(|incident| spec_for_package(project, &incident, emit))
        },
        &|wait| {
            osc::sleep_interruptible(wait);
            !crate::proc::shutting_down()
        },
    )
}

/// How often a read is tried, and the wait before attempt `n` (2, 3, ...).
pub(super) const READ_ATTEMPTS: u32 = 3;

pub(super) fn read_backoff(attempt: u32) -> std::time::Duration {
    std::time::Duration::from_secs(5 * u64::from(attempt - 1))
}

/// [`spec_for_package`] with OBS behind closures: `list` is the file listing
/// (`None` when it failed), `cat` a file's text, `incident` the read of the
/// newest incident copy when there is one, and `pause` waits and says whether
/// to go on.
pub(super) fn read_spec(
    pkg: &str,
    list: &dyn Fn() -> Option<Vec<String>>,
    cat: &dyn Fn(&str) -> Option<String>,
    incident: &dyn Fn() -> Option<Result<String, ReadError>>,
    pause: &dyn Fn(std::time::Duration) -> bool,
) -> Result<String, ReadError> {
    let mut why = String::from("no attempt was made");
    for attempt in 1..=READ_ATTEMPTS {
        if attempt > 1 && !pause(read_backoff(attempt)) {
            return Err(ReadError::Failed("interrupted".into()));
        }
        let Some(files) = list() else {
            why = "listing the package's files (osc ls) failed".into();
            continue;
        };
        let Some(picked) = pick_spec_filename(pkg, &files) else {
            if files.is_empty()
                && let Some(read) = incident()
            {
                return read;
            }
            return Err(ReadError::NoSpec);
        };
        match cat(&picked) {
            Some(spec) => return Ok(spec),
            None => why = format!("reading {picked} (osc cat) failed"),
        }
    }
    Err(ReadError::Failed(format!(
        "{why}, {READ_ATTEMPTS} attempts"
    )))
}

/// Pure, testable half of [`spec_for_package`]'s filename picker.
///
/// Preference order:
/// 1. `<base>.spec`, where `<base>` is the OBS package name with any
///    `+<version>` suffix stripped (AppCo convention: `pytorch+2.4` →
///    `pytorch.spec`).
/// 2. `<pkg>.spec` verbatim (the common non-AppCo case).
/// 3. Alphabetical first, so multibuild flavors or alternative spec names
///    still produce a stable choice across runs.
///
/// Returns `None` only when no `.spec` file is present at all.
pub fn pick_spec_filename(pkg: &str, files: &[String]) -> Option<String> {
    let mut specs: Vec<&String> = files.iter().filter(|f| f.ends_with(".spec")).collect();
    if specs.is_empty() {
        return None;
    }
    specs.sort();
    let bare_spec = format!(
        "{}.spec",
        pkg.split_once('+').map(|(n, _)| n).unwrap_or(pkg),
    );
    let pkg_spec = format!("{pkg}.spec");
    if let Some(s) = specs.iter().find(|s| ***s == bare_spec) {
        return Some((*s).clone());
    }
    if let Some(s) = specs.iter().find(|s| ***s == pkg_spec) {
        return Some((*s).clone());
    }
    specs.first().map(|s| (*s).clone())
}
