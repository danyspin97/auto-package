use super::*;

/// Why a package's spec could not be read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReadError {
    /// OBS answered and the package has no spec file. Not retried.
    NoSpec,
    /// OBS could not be asked or did not answer (also: the run was
    /// interrupted); the text says which step failed.
    Failed(String),
    /// The package is a `_link` whose target is gone: the text is the report
    /// for the maintainer (see `osc::broken_link_report`).
    NeedsMaintainer(String),
}

impl std::fmt::Display for ReadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ReadError::NoSpec => write!(f, "the package has no spec file"),
            ReadError::Failed(why) => write!(f, "the spec could not be read: {why}"),
            ReadError::NeedsMaintainer(why) => write!(f, "needs the maintainer: {why}"),
        }
    }
}

impl std::error::Error for ReadError {}

impl From<ReadError> for SkipReason {
    fn from(e: ReadError) -> Self {
        match e {
            ReadError::NoSpec => SkipReason::NoSpec,
            ReadError::Failed(why) => SkipReason::ReadFailed(why),
            ReadError::NeedsMaintainer(why) => SkipReason::NeedsMaintainer(why),
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
        &|| osc::list_package_files_checked(project, pkg, emit),
        &|file| osc::cat_file_checked(project, pkg, file, emit),
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
    .map_err(|e| match e {
        // The report names the link's target and the request that deleted it,
        // read from OBS here; the plain reason stays when that cannot be read.
        ReadError::NeedsMaintainer(fallback) => {
            ReadError::NeedsMaintainer(osc::broken_link_report(project, pkg, &fallback, emit))
        }
        other => other,
    })
}

/// The error a permanent failure of an osc read is: a `_link` that cannot be
/// expanded goes to the maintainer, the rest is a failed read.
fn permanent_error(osc_says: &str, reason: String) -> ReadError {
    if osc_says.contains("cannot be expanded") {
        ReadError::NeedsMaintainer(reason)
    } else {
        ReadError::Failed(reason)
    }
}

/// How often a read is tried, and the wait before attempt `n` (2, 3, ...).
pub(super) const READ_ATTEMPTS: u32 = 3;

pub(super) fn read_backoff(attempt: u32) -> std::time::Duration {
    std::time::Duration::from_secs(5 * u64::from(attempt - 1))
}

/// Why a failed `osc ls` or `osc cat` of `pkg` will fail again, as the reason
/// to report, or `None` when the failure may pass. `osc_says` is osc's error
/// text.
///
/// Permanent: a `_link` that cannot be expanded (its target is missing or the
/// link conflicts; only the maintainer can fix it), a name OBS cannot hold, a
/// 401 or 403 (the credentials do not change within a run), and a 404 that OBS
/// gives for a package or project that is not there (`unknown_package`,
/// `unknown_project`). A bare 400 or 404 is not: a failing backend answers the
/// same way. A 404 for a listed file is judged by [`read_spec`].
pub(super) fn permanent_listing_failure(pkg: &str, osc_says: &str) -> Option<String> {
    if osc::text_is_transient(osc_says) {
        return None;
    }
    if osc_says.contains("is not a valid OBS name") {
        return Some(format!("{pkg} is not a valid OBS name; not retried"));
    }
    let first = osc_says
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("");
    if let Some(at) = osc_says.find("cannot be expanded") {
        let detail = osc_says[at + "cannot be expanded".len()..]
            .trim_start_matches(':')
            .lines()
            .map(str::trim)
            .take_while(|l| !l.contains("repairlink"))
            .filter(|l| !l.is_empty())
            .collect::<Vec<_>>()
            .join(" ");
        let detail = if detail.is_empty() {
            first.to_string()
        } else {
            detail
        };
        return Some(format!(
            "the _link of {pkg} cannot be expanded ({detail}); the maintainer must relink or unlink it"
        ));
    }
    if let Some(status @ (401 | 403)) = osc::http_status(osc_says) {
        return Some(format!(
            "OBS refused access to {pkg} (HTTP {status}): the package may be read-protected or your credentials may not allow it; not retried"
        ));
    }
    let lower = osc_says.to_lowercase();
    let missing = lower.contains("unknown_package") || lower.contains("unknown_project");
    if osc_says.contains("HTTP Error 404") && missing {
        return Some(format!(
            "{pkg} does not exist on OBS ({first}); not retried"
        ));
    }
    None
}

/// [`spec_for_package`] with OBS behind closures: `list` is the file listing
/// (osc's error text when it failed), `cat` a file's text, `incident` the read
/// of the newest incident copy when there is one, and `pause` waits and says
/// whether to go on.
pub(super) fn read_spec(
    pkg: &str,
    list: &dyn Fn() -> Result<Vec<String>, String>,
    cat: &dyn Fn(&str) -> Result<String, String>,
    incident: &dyn Fn() -> Option<Result<String, ReadError>>,
    pause: &dyn Fn(std::time::Duration) -> bool,
) -> Result<String, ReadError> {
    let mut why = String::from("no attempt was made");
    let mut cat_404s = 0;
    for attempt in 1..=READ_ATTEMPTS {
        if attempt > 1 && !pause(read_backoff(attempt)) {
            return Err(ReadError::Failed("interrupted".into()));
        }
        let files = match list() {
            Ok(files) => files,
            Err(osc_says) => {
                if let Some(reason) = permanent_listing_failure(pkg, &osc_says) {
                    return Err(permanent_error(&osc_says, reason));
                }
                let first = osc_says.lines().map(str::trim).find(|l| !l.is_empty());
                why = format!(
                    "listing the package's files (osc ls) failed: {}",
                    first.unwrap_or("no output")
                );
                continue;
            }
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
            Ok(spec) => return Ok(spec),
            Err(osc_says) => {
                let first = osc_says.lines().map(str::trim).find(|l| !l.is_empty());
                if osc_says.contains("HTTP Error 404") {
                    // A listed file that answers 404 is gone, or OBS erred once:
                    // a second 404 ends the read, and so does a listing that no
                    // longer names the file.
                    cat_404s += 1;
                    let gone = matches!(list(), Ok(files) if !files.contains(&picked));
                    if gone || cat_404s > 1 {
                        return Err(ReadError::Failed(format!(
                            "{picked} of {pkg} answers 404 ({}): {}",
                            if gone {
                                "no longer listed"
                            } else {
                                "again after a retry"
                            },
                            first.unwrap_or("no output")
                        )));
                    }
                } else if let Some(reason) = permanent_listing_failure(pkg, &osc_says) {
                    return Err(permanent_error(&osc_says, reason));
                }
                why = format!(
                    "reading {picked} (osc cat) failed: {}",
                    first.unwrap_or("no output")
                );
            }
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
