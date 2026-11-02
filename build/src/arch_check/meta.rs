//! The one write the arch check makes: adding architectures to repositories
//! of the user's own branch project, under the project-meta lock.

use super::{ArchAddition, ArchObs};
use crate::osc::{ProjectInfo, lock_project_meta, parse_project_info, repository_elements};

/// Refuse any project but a `home:` one. Only the branch project, which the
/// user owns, may be written to by the arch check.
pub fn ensure_branch_project(project: &str) -> Result<(), String> {
    if project.starts_with("home:") {
        Ok(())
    } else {
        Err(format!(
            "{project} is not a home: project; the arch check writes only to the branch project"
        ))
    }
}

/// Add architectures to a project meta, one `<arch>` per addition, after the
/// architectures the repository already lists. An architecture the repository
/// has already is left alone, so a second run changes nothing. Returns the new
/// XML and the additions actually made. A repository the meta does not have is
/// an error.
pub fn add_arches_to_meta(
    xml: &str,
    additions: &[ArchAddition],
) -> Result<(String, Vec<ArchAddition>), String> {
    let mut xml = xml.to_string();
    let mut made = Vec::new();
    for add in additions {
        let repo = repository_elements(&xml)?
            .into_iter()
            .find(|r| r.name == add.repository)
            .ok_or_else(|| format!("the project meta has no repository {}", add.repository))?;
        if repo.arches.contains(&add.arch) {
            continue;
        }
        let element = format!("<arch>{}</arch>", crate::obs_xml::escape(&add.arch));
        let repo_indent = line_indent(&xml, repo.start).to_string();
        if repo.self_closing {
            // `<repository name="x"/>` has no body yet.
            let open = xml[repo.start..repo.open_end]
                .trim_end_matches("/>")
                .trim_end();
            let rewritten =
                format!("{open}>\n{repo_indent}  {element}\n{repo_indent}</repository>");
            xml.replace_range(repo.start..repo.end, &rewritten);
        } else if let Some((arch_start, arch_end)) = repo.last_arch {
            // After the last existing arch, on a line of its own when it has one.
            let own_line = xml[..arch_start]
                .rfind('\n')
                .is_some_and(|nl| xml[nl + 1..arch_start].trim().is_empty());
            let text = if own_line {
                format!("\n{}{element}", line_indent(&xml, arch_start))
            } else {
                element.clone()
            };
            xml.insert_str(arch_end, &text);
        } else if let Some(close) = repo.close_at {
            let line_start = xml[..close].rfind('\n').map_or(0, |i| i + 1);
            let own_line = xml[line_start..close].trim().is_empty();
            let text = if own_line {
                format!("  {element}\n{repo_indent}")
            } else {
                element.clone()
            };
            xml.insert_str(close, &text);
        } else {
            return Err(format!(
                "the repository {} of the project meta is not closed",
                add.repository
            ));
        }
        made.push(add.clone());
    }
    Ok((xml, made))
}

/// The whitespace leading the line `pos` is on.
fn line_indent(xml: &str, pos: usize) -> &str {
    let line_start = xml[..pos].rfind('\n').map_or(0, |i| i + 1);
    let line = &xml[line_start..pos];
    &line[..line.len() - line.trim_start().len()]
}

/// Edit the branch project `project` on OBS. Under the project-meta lock the
/// meta is read and parsed, `plan` picks the additions from that fresh copy,
/// `announce` is told what is about to be added, and the meta is written back
/// only when something changed. A project that is not a `home:` one is refused
/// before any OBS call. Returns the additions made.
pub fn edit_branch_meta<O: ArchObs + ?Sized>(
    obs: &O,
    project: &str,
    plan: impl FnOnce(&ProjectInfo) -> Vec<ArchAddition>,
    announce: impl FnOnce(&[ArchAddition]),
) -> Result<Vec<ArchAddition>, String> {
    ensure_branch_project(project)?;
    let _lock = lock_project_meta();
    let xml = obs
        .project_meta(project)?
        .ok_or_else(|| format!("the branch project {project} does not exist"))?;
    let planned = plan(&parse_project_info(&xml));
    if planned.is_empty() {
        return Ok(Vec::new());
    }
    let (edited, made) = add_arches_to_meta(&xml, &planned)?;
    if made.is_empty() {
        return Ok(made);
    }
    announce(&made);
    obs.set_project_meta(project, &edited)?;
    Ok(made)
}
