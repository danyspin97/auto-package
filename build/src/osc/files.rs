//! Files of a package on OBS: the file listing, the spec file name and the
//! file reads.

use super::*;
use crate::osc::query::*;

/// Enumerate the files committed to an OBS package.
///
/// Returns `None` when `osc ls` fails (project/package doesn't exist, auth
/// broken) rather than panicking, same as [`cat_file`]: the update sweep
/// uses that `None` to skip the package and record a SpecFetchFailed instead
/// of killing the whole scan.
///
/// Used so the update sweep can discover the real `.spec` filename rather
/// than guess `<pkg>.spec` — AppCo's versioned-library convention gives one
/// OBS package per upstream version (`pytorch+2.4`, `pytorch+2.5`), each
/// holding a plain `<base-name>.spec` (`pytorch.spec`) that has no relation
/// to the OBS package name, so the guess is not enough on its own.
pub fn list_package_files(project: &str, pkg: &str, emit: Emit<'_>) -> Option<Vec<String>> {
    if ![project, pkg].iter().all(|n| is_valid_obs_name(n)) {
        return None;
    }
    let out = osc_query(&["ls", project, pkg], emit, &format!("ls {project}/{pkg}"))?;
    if !out.success() {
        return None;
    }
    Some(
        out.stdout
            .lines()
            .map(|l| l.trim().to_string())
            .filter(|l| !l.is_empty())
            .collect(),
    )
}

/// The spec file of OBS package `pkg` given its file listing: the one `*.spec`
/// it holds. A package with several, or none, falls back to `<pkg>.spec`.
pub(super) fn spec_file_name(files: &[String], pkg: &str) -> String {
    let specs: Vec<&String> = files.iter().filter(|f| f.ends_with(".spec")).collect();
    match specs.as_slice() {
        [only] => only.to_string(),
        _ => format!("{pkg}.spec"),
    }
}

/// The spec file of OBS package `pkg` in `project`, found from its listing (see
/// [`spec_file_name`]). `<pkg>.spec` when the listing cannot be read.
pub(super) fn package_spec_name(project: &str, pkg: &str, emit: Emit<'_>) -> String {
    match list_package_files(project, pkg, emit) {
        Some(files) => spec_file_name(&files, pkg),
        None => format!("{pkg}.spec"),
    }
}

/// Fetch a single file's contents from an OBS package without checking out.
///
/// Returns `None` when the package doesn't exist, the file isn't in it, or
/// `osc cat` otherwise fails — the update sweep uses that `None` to skip the
/// package instead of blowing up the whole run on a stray missing spec.
/// A file name passed to `osc` as a positional argument. A leading `-` would
/// be read as an option, and a `/` would leave the package directory.
pub(super) fn is_safe_file_arg(name: &str) -> bool {
    !name.is_empty() && !name.starts_with('-') && !name.contains('/') && !name.contains('\0')
}

pub fn cat_file(project: &str, pkg: &str, filename: &str, emit: Emit<'_>) -> Option<String> {
    if ![project, pkg].iter().all(|n| is_valid_obs_name(n)) || !is_safe_file_arg(filename) {
        return None;
    }
    let out = osc_query(
        &["cat", project, pkg, filename],
        emit,
        &format!("cat {project}/{pkg} {filename}"),
    )?;
    if !out.success() {
        return None;
    }
    Some(out.stdout)
}
