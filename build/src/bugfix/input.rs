use std::path::Path;

use crate::maintenance::{bug_ref_warnings, is_cve, parse_bug_ref};

/// The longest `--description`.
pub const MAX_DESCRIPTION_BYTES: usize = 4 * 1024;

/// The largest `--bug-file`.
pub const MAX_BUG_FILE_BYTES: u64 = 64 * 1024;

/// A bug report the user pasted into a file (`--bug-file`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReportFile {
    pub path: String,
    pub text: String,
}

/// What `fix` was told about the bug, checked.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct BugInput {
    /// References in SUSE spelling, each once, in the order given.
    pub refs: Vec<String>,
    pub description: Option<String>,
    pub report_file: Option<ReportFile>,
    /// Valid but unusual input, to say once.
    pub warnings: Vec<String>,
}

fn split_refs(given: &[String]) -> impl Iterator<Item = &str> {
    given
        .iter()
        .flat_map(|g| g.split(','))
        .map(str::trim)
        .filter(|g| !g.is_empty())
}

/// Read a `--bug-file`: a regular file of at most [`MAX_BUG_FILE_BYTES`].
pub fn read_report_file(path: &Path) -> Result<ReportFile, String> {
    let meta =
        std::fs::metadata(path).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    if !meta.is_file() {
        return Err(format!("{} is not a file", path.display()));
    }
    if meta.len() > MAX_BUG_FILE_BYTES {
        return Err(format!(
            "{} is larger than {} KiB: keep the part that describes the bug",
            path.display(),
            MAX_BUG_FILE_BYTES / 1024
        ));
    }
    let bytes = std::fs::read(path).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    Ok(ReportFile {
        path: path.display().to_string(),
        text: String::from_utf8_lossy(&bytes).into_owned(),
    })
}

/// Check `--bug`, `--cve`, `--description` and `--bug-file`. At least one is
/// required. `--bug` takes any reference (a CVE too), `--cve` only a CVE.
pub fn parse_bug_input(
    bugs: &[String],
    cves: &[String],
    description: Option<&str>,
    bug_file: Option<&Path>,
) -> Result<BugInput, String> {
    let mut refs: Vec<String> = Vec::new();
    for g in split_refs(bugs) {
        let r = parse_bug_ref(g)?;
        if !refs.contains(&r) {
            refs.push(r);
        }
    }
    for g in split_refs(cves) {
        let r = parse_bug_ref(g)?;
        if !is_cve(&r) {
            return Err(format!(
                "--cve takes a CVE (CVE-YYYY-NNNN), not {g:?}: use --bug"
            ));
        }
        if !refs.contains(&r) {
            refs.push(r);
        }
    }
    let description = description.map(str::trim).filter(|d| !d.is_empty());
    if let Some(d) = description
        && d.len() > MAX_DESCRIPTION_BYTES
    {
        return Err(format!(
            "--description is {} bytes, over {MAX_DESCRIPTION_BYTES}: put the report in a file and pass --bug-file",
            d.len()
        ));
    }
    let report_file = bug_file.map(read_report_file).transpose()?;
    if refs.is_empty() && description.is_none() && report_file.is_none() {
        return Err(
            "say what to fix: pass --bug bsc#NNNNNNN, --cve CVE-YYYY-NNNN, --description TEXT or --bug-file FILE"
                .to_string(),
        );
    }
    let warnings = bug_ref_warnings(&refs);
    Ok(BugInput {
        refs,
        description: description.map(str::to_string),
        report_file,
        warnings,
    })
}
