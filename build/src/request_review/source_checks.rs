//! The files of the request: that every source and patch the spec names is
//! in the package, and what is left over.

use std::collections::{BTreeMap, HashSet};

use super::gather::Side;
use super::{Finding, Severity, ids};
use crate::risk_flags::patch_names;
use crate::spec::{conditional_source_files, declared_source_files};

/// Suffixes of a source archive.
const ARCHIVE_SUFFIXES: [&str; 11] = [
    ".tar.gz", ".tar.bz2", ".tar.xz", ".tar.zst", ".tgz", ".zip", ".gem", ".crate", ".obscpio",
    ".tar", ".tbz2",
];

/// Suffixes of an editor or patch tool's leftover.
const LEFTOVER_SUFFIXES: [&str; 6] = [".orig", ".rej", "~", ".swp", ".bak", ".spec.new"];

/// Files the tool's own runs leave in a checkout.
const TOOL_ARTIFACTS: [&str; 4] = ["series-rebase", "state.json", "notes.md", "prompt.md"];

/// Whether a file the request adds looks like a leftover of a tool or an
/// editor. `had_series` is whether the target has a `series` file.
pub(crate) fn is_leftover_name(name: &str, had_series: bool) -> bool {
    let artifact = TOOL_ARTIFACTS.contains(&name) || (name == "series" && !had_series);
    artifact || name.starts_with(".#") || LEFTOVER_SUFFIXES.iter().any(|s| name.ends_with(s))
}

fn is_archive(name: &str) -> bool {
    ARCHIVE_SUFFIXES.iter().any(|s| name.ends_with(s))
}

/// The names the package lists, where a `_service:<service>:<file>` entry
/// stands for `<file>`.
fn listed(side: &Side) -> HashSet<String> {
    side.listing
        .files
        .iter()
        .flat_map(|f| {
            let own = f.name.clone();
            let produced = f
                .name
                .strip_prefix("_service:")
                .and_then(|r| r.rsplit_once(':'))
                .map(|(_, file)| file.to_string());
            [Some(own), produced].into_iter().flatten()
        })
        .collect()
}

/// The source findings. `added` are the file names the request adds.
/// `archives` maps a downloaded archive to the files it holds (its top
/// directory cut off, as `risk_flags::archive_files` gives them).
pub fn source_findings(
    package: &str,
    base: &Side,
    new: &Side,
    added: &[String],
    archives: &BTreeMap<String, Vec<String>>,
) -> Vec<Finding> {
    let Some(new_spec) = new.spec() else {
        return Vec::new();
    };
    let spec_name = new.spec_name.as_deref().unwrap_or("the spec");
    let base_spec = base.spec().unwrap_or("");
    let (declared, unexpanded) = declared_source_files(new_spec);
    let (declared_before, unexpanded_before) = declared_source_files(base_spec);
    let patches = patch_names(new_spec);
    let present = listed(new);
    let has_service = new.lists("_service");
    let conditional = conditional_source_files(new_spec);
    let mk =
        |id, sev, file: &str, msg: &str| Finding::check(id, sev, package, msg).in_file(file, None);
    let mut out = Vec::new();
    for name in declared.iter().chain(&patches) {
        if present.contains(name) {
            continue;
        }
        let (severity, why) = if has_service {
            (
                Severity::Warning,
                "; a source service may produce it at build time",
            )
        } else if conditional.contains(name) {
            (
                Severity::Warning,
                "; its Source line is inside a conditional that may not hold",
            )
        } else {
            (Severity::Blocker, "")
        };
        out.push(mk(
            ids::SOURCE_MISSING,
            severity,
            name,
            &format!("{name} is named by the spec and is not in the package{why}"),
        ));
    }
    for raw in unexpanded.iter().filter(|u| !unexpanded_before.contains(u)) {
        out.push(mk(
            ids::SOURCE_UNEXPANDED,
            Severity::Note,
            spec_name,
            &format!("Source {raw} holds a macro that cannot be expanded: its file is not checked"),
        ));
    }
    for name in &declared_before {
        if is_archive(name) && !declared.contains(name) && present.contains(name) {
            out.push(mk(
                ids::SOURCE_OLD_ARCHIVE_KEPT,
                Severity::Warning,
                name,
                &format!("{name} is no longer a Source of the spec and is still in the package"),
            ));
        }
    }
    let had_series = base.lists("series");
    for name in added {
        if is_leftover_name(name, had_series) {
            out.push(mk(
                ids::FILES_LEFTOVER,
                Severity::Warning,
                name,
                &format!("{name} looks like a leftover of a tool or an editor"),
            ));
        }
    }
    for (name, files) in archives {
        let vendor = super::downloads::is_vendor(name);
        if vendor && files.iter().any(|f| f.starts_with("target/")) {
            out.push(mk(
                ids::FILES_LEFTOVER,
                Severity::Warning,
                name,
                &format!("{name} holds a target/ directory: a build directory was packed with it"),
            ));
        }
    }
    let referenced: HashSet<&String> = declared.iter().chain(&patches).collect();
    for name in added {
        let known = name.ends_with(".spec")
            || name.ends_with(".changes")
            || name.ends_with("rpmlintrc")
            || name.ends_with(".keyring")
            || name.starts_with("_service")
            || name == "_multibuild";
        let leftover = out
            .iter()
            .any(|f| f.id == ids::FILES_LEFTOVER && f.file.as_deref() == Some(name));
        if !known && !leftover && !referenced.contains(name) && !referenced_by_service(new, name) {
            out.push(mk(
                ids::FILES_UNREFERENCED,
                Severity::Note,
                name,
                &format!("{name} is added and no Source, Patch or service names it"),
            ));
        }
    }
    out
}

/// Whether the `_service` file names `file`.
fn referenced_by_service(side: &Side, file: &str) -> bool {
    side.service().is_some_and(|s| s.contains(file))
}
