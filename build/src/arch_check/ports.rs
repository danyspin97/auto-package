//! The Factory ports: `openSUSE:Factory:<name>` projects that build Factory's
//! sources for an architecture Factory itself does not build. A branch gets a
//! repository per port, with a path to the port project's build repository,
//! and a project-level disable, so that a package builds there only once it
//! enables the repository in its own build flags.

use super::{ArchState, classify, missing_from_details};
use crate::osc::{BuildStatus, child_indent, layout, open_tags};

/// One port: the repository a branch adds, the project it paths to, and the
/// architecture it builds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Port {
    pub repo: &'static str,
    pub project: &'static str,
    pub arch: &'static str,
}

/// The ports that can be checked: each builds one architecture that Factory
/// does not. armv7l has no port, so it cannot be checked.
pub const PORTS: &[Port] = &[
    Port {
        repo: "openSUSE_Factory_zSystems",
        project: "openSUSE:Factory:zSystems",
        arch: "s390x",
    },
    Port {
        repo: "openSUSE_Factory_PowerPC",
        project: "openSUSE:Factory:PowerPC",
        arch: "ppc64le",
    },
    Port {
        repo: "openSUSE_Factory_RISCV",
        project: "openSUSE:Factory:RISCV",
        arch: "riscv64",
    },
];

/// The repository of each port project that its packages are built in.
pub const PORT_BUILD_REPO: &str = "standard";

/// The port a repository of a branch is, if it is one.
pub fn port_for_repo(repo: &str) -> Option<&'static Port> {
    PORTS.iter().find(|p| p.repo == repo)
}

/// Whether `repo` is the repository of a port.
pub fn is_port_repo(repo: &str) -> bool {
    port_for_repo(repo).is_some()
}

/// The baseline of a port's result: the repository of Factory's port project
/// that the result is compared with.
pub fn baseline_key(repo: &str, arch: &str) -> (String, String) {
    match port_for_repo(repo) {
        Some(_) => (PORT_BUILD_REPO.to_string(), arch.to_string()),
        None => (repo.to_string(), arch.to_string()),
    }
}

/// The ports whose architecture is in `wanted` (every port when `None`).
pub fn ports_wanted(wanted: Option<&[String]>) -> Vec<Port> {
    PORTS
        .iter()
        .filter(|p| wanted.is_none_or(|w| w.iter().any(|a| a == p.arch)))
        .copied()
        .collect()
}

/// Add `port` to a project meta: its repository, when the meta has none of that
/// name, and a project-level `<disable>` of it in the `<build>` flags, when
/// those lack one. Each part already there is left alone, so a second run
/// changes nothing. Returns the new XML and whether anything changed.
pub fn add_port_to_meta(xml: &str, port: &Port) -> Result<(String, bool), String> {
    let esc = crate::obs_xml::escape;
    let mut xml = xml.to_string();
    let mut changed = false;
    let has_repo = crate::osc::repository_elements(&xml)?
        .iter()
        .any(|r| r.name == port.repo);
    if !has_repo {
        let lay = layout(&xml)?;
        let close = lay
            .close_at
            .ok_or("the project meta is an empty element, with nowhere to put a repository")?;
        let element = format!(
            "<repository name=\"{}\"><path project=\"{}\" repository=\"{PORT_BUILD_REPO}\"/><arch>{}</arch></repository>",
            esc(port.repo),
            esc(port.project),
            esc(port.arch)
        );
        let indent = child_indent(&xml, &lay);
        insert_before(&mut xml, close, &indent, &element);
        changed = true;
    }
    let disable = format!("<disable repository=\"{}\"/>", esc(port.repo));
    let lay = layout(&xml)?;
    match lay.children.iter().find(|c| c.name == "build") {
        Some(build) => {
            let block = xml[build.start..build.end].to_string();
            let disabled = open_tags(&block, "disable")?
                .iter()
                .any(|t| crate::obs_xml::attr(t, "repository").as_deref() == Some(port.repo));
            if !disabled {
                if block.ends_with("/>") {
                    let ind = line_indent(&xml, build.start).to_string();
                    let rewritten = format!("<build>\n{ind}  {disable}\n{ind}</build>");
                    xml.replace_range(build.start..build.end, &rewritten);
                } else {
                    let close = build.start
                        + block
                            .rfind("</build>")
                            .ok_or("the project's build flags are not closed")?;
                    let ind = format!("{}  ", line_indent(&xml, build.start));
                    insert_before(&mut xml, close, &ind, &disable);
                }
                changed = true;
            }
        }
        None => {
            let close = lay
                .close_at
                .ok_or("the project meta is an empty element, with nowhere to put build flags")?;
            let indent = child_indent(&xml, &lay);
            let block = format!("<build>\n{indent}  {disable}\n{indent}</build>");
            insert_before(&mut xml, close, &indent, &block);
            changed = true;
        }
    }
    Ok((xml, changed))
}

/// Insert `element` on a line of its own, indented by `indent`, before the tag
/// at `at`, when that tag starts its line; otherwise right before it.
fn insert_before(xml: &mut String, at: usize, indent: &str, element: &str) {
    let line_start = xml[..at].rfind('\n').map_or(0, |i| i + 1);
    if xml[line_start..at].trim().is_empty() {
        xml.insert_str(line_start, &format!("{indent}{element}\n"));
    } else {
        xml.insert_str(at, element);
    }
}

/// The whitespace leading the line `pos` is on.
fn line_indent(xml: &str, pos: usize) -> &str {
    let line_start = xml[..pos].rfind('\n').map_or(0, |i| i + 1);
    let line = &xml[line_start..pos];
    &line[..line.len() - line.trim_start().len()]
}

/// The state of a result in `repo`: a port's is judged as a port's (see
/// [`classify_port`]), any other as an ordinary one.
pub fn state_in(repo: &str, status: &BuildStatus, details: Option<&str>) -> ArchState {
    if is_port_repo(repo) {
        classify_port(status, details)
    } else {
        classify(status, details)
    }
}

/// The state of a port's result. A dependency missing on the port makes it not
/// testable, as does a broken result; a result still going is not final. A
/// failure is left for [`judge_port`] to judge against Factory.
pub fn classify_port(status: &BuildStatus, details: Option<&str>) -> ArchState {
    match classify(status, details) {
        ArchState::Unresolvable { .. } => {
            let names = missing_from_details(details.unwrap_or(""));
            ArchState::NotTestable {
                detail: (!names.is_empty())
                    .then(|| format!("nothing provides {}", names.join(", "))),
            }
        }
        ArchState::Broken => ArchState::NotTestable {
            detail: Some("broken".to_string()),
        },
        ArchState::Pending(what) => ArchState::NotFinal(what),
        other => other,
    }
}

/// A port's failure judged against Factory's result for the same arch:
/// `baseline` is that result's state, `None` when Factory has no result of the
/// package there. Returns the state and whether Factory fails too. A failure
/// Factory builds is the update's (it blocks); one Factory fails too does not;
/// one Factory has no result of is `NoBaseline`, which blocks.
pub fn judge_port(state: ArchState, baseline: Option<&ArchState>) -> (ArchState, bool) {
    match state {
        ArchState::Failed { detail } => match baseline {
            None => (ArchState::NoBaseline { detail }, false),
            Some(b) if b.is_failure() => (ArchState::Failed { detail }, true),
            Some(ArchState::Succeeded) => (ArchState::Failed { detail }, false),
            Some(_) => (ArchState::NotFinal("baseline building".to_string()), false),
        },
        other => (other, false),
    }
}
