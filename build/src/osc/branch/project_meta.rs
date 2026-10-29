//! Project meta: the repositories and paths of a project, reading and writing its
//! meta document, and the build paths added to it.

use super::*;
use crate::osc::error::*;
use crate::osc::names::*;
use crate::osc::repo_paths::*;

/// One `<repository>` of a project meta.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepoDef {
    pub name: String,
    /// `(project, repository)` build paths, in order.
    pub paths: Vec<(String, String)>,
    pub arches: Vec<String>,
}

/// What `update` needs to know about a project meta.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ProjectInfo {
    /// The `kind` attribute: absent for a standard project, `maintenance`,
    /// `maintenance_incident` or `maintenance_release` otherwise.
    pub kind: Option<String>,
    pub locked: bool,
    pub repositories: Vec<RepoDef>,
}

/// Byte ranges `(start, end)` of every `<repository>` element in `xml`, end
/// exclusive and past the closing tag.
pub(super) fn repository_spans(xml: &str) -> Vec<(usize, usize)> {
    let mut spans = Vec::new();
    let mut from = 0;
    while let Some(rel) = xml[from..].find("<repository") {
        let start = from + rel;
        let after = start + "<repository".len();
        // `<repositoryfoo` is some other element: only whitespace after the
        // name makes it this one.
        if !xml[after..].starts_with(|c: char| c.is_whitespace()) {
            from = after;
            continue;
        }
        let Some(open_end) = xml[start..].find('>').map(|i| start + i + 1) else {
            break;
        };
        let end = if xml[..open_end].ends_with("/>") {
            open_end
        } else {
            match xml[open_end..].find("</repository>") {
                Some(i) => open_end + i + "</repository>".len(),
                None => break,
            }
        };
        spans.push((start, end));
        from = end;
    }
    spans
}

/// Parsing half of the project-meta read, split out so it is testable
/// without shelling out to `osc`.
pub fn parse_project_info(xml: &str) -> ProjectInfo {
    let kind = xml
        .find("<project ")
        .and_then(|i| xml[i..].find('>').map(|e| &xml[i..i + e]))
        .and_then(|tag| crate::obs_xml::attr(tag, "kind"))
        .filter(|k| !k.is_empty() && k != "standard");
    let locked = xml
        .find("<lock>")
        .and_then(|i| xml[i..].find("</lock>").map(|e| &xml[i..i + e]))
        .is_some_and(|block| block.contains("<enable"));
    let repositories = repository_spans(xml)
        .into_iter()
        .filter_map(|(s, e)| {
            let block = &xml[s..e];
            let open = &block[..block.find('>')? + 1];
            let name = crate::obs_xml::attr(open, "name")?;
            let arches = block
                .split("<arch>")
                .skip(1)
                .filter_map(|c| c.split("</arch>").next())
                .map(|a| crate::obs_xml::unescape(a.trim()))
                .collect();
            Some(RepoDef {
                name,
                paths: parse_project_repo_paths(block),
                arches,
            })
        })
        .collect();
    ProjectInfo {
        kind,
        locked,
        repositories,
    }
}

/// The meta XML of `project`, or `None` when the project does not exist.
pub fn get_project_meta(project: &str, emit: Emit<'_>) -> Result<Option<String>, Error> {
    check_obs_name("project", project)?;
    let cmd = format!("meta prj {project}");
    let out = proc::capture_with_timeout(
        "osc",
        &["meta", "prj", project],
        &anywhere(),
        emit,
        OSC_QUERY_TIMEOUT,
    )
    .map_err(during(cmd.clone()))?;
    if out.success() {
        return Ok(Some(out.stdout));
    }
    if is_not_found(&out.stderr) || out.stderr.contains("unknown_project") {
        return Ok(None);
    }
    Err(Error::OscFailed {
        cmd,
        code: out.code,
        stderr: out.stderr,
    })
}

/// Replace the meta of `project` with `xml`.
pub fn set_project_meta(project: &str, xml: &str, emit: Emit<'_>) -> Result<(), Error> {
    check_obs_name("project", project)?;
    crate::osc::invalidate_repo_paths(project);
    let mut file = tempfile::Builder::new()
        .prefix("ap-prj-meta-")
        .suffix(".xml")
        .tempfile_in(proc::private_dir()?)?;
    std::io::Write::write_all(&mut file, xml.as_bytes())?;
    let path = file.path().to_string_lossy().into_owned();
    let cmd = format!("meta prj {project} -F");
    let out = proc::capture_with_timeout(
        "osc",
        &["meta", "prj", project, "-F", &path],
        &anywhere(),
        emit,
        OSC_QUERY_TIMEOUT,
    )
    .map_err(during(cmd.clone()))?;
    if !out.success() {
        return Err(Error::OscFailed {
            cmd,
            code: out.code,
            stderr: out.stderr,
        });
    }
    Ok(())
}

/// A build path to add to one repository of a project.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PathAddition {
    /// The repository of the project being edited.
    pub repository: String,
    pub project: String,
    /// The repository of `project` the path points at.
    pub path_repository: String,
}

impl PathAddition {
    fn element(&self) -> String {
        format!(
            "<path project=\"{}\" repository=\"{}\"/>",
            crate::obs_xml::escape(&self.project),
            crate::obs_xml::escape(&self.path_repository)
        )
    }
}

/// The whitespace leading the line `pos` is on.
fn line_indent(xml: &str, pos: usize) -> &str {
    let line_start = xml[..pos].rfind('\n').map_or(0, |i| i + 1);
    let line = &xml[line_start..pos];
    &line[..line.len() - line.trim_start().len()]
}

/// Add build paths to a project meta, after the paths a repository already
/// has (a path listed earlier wins when two provide the same package, so an
/// added one only ever supplies what the existing ones lack).
///
/// A path the repository already lists is left alone, so running this again
/// changes nothing. Returns the new XML and the additions actually made. A
/// repository the meta does not have is an error.
pub fn add_paths_to_meta(
    xml: &str,
    additions: &[PathAddition],
) -> Result<(String, Vec<PathAddition>), String> {
    let mut xml = xml.to_string();
    let mut made = Vec::new();
    for add in additions {
        let (start, end) = repository_spans(&xml)
            .into_iter()
            .find(|&(s, _)| {
                let open_end = xml[s..].find('>').map_or(xml.len(), |i| s + i + 1);
                crate::obs_xml::attr(&xml[s..open_end], "name").as_deref()
                    == Some(add.repository.as_str())
            })
            .ok_or_else(|| format!("the project meta has no repository {}", add.repository))?;
        let block = &xml[start..end];
        if parse_project_repo_paths(block)
            .contains(&(add.project.clone(), add.path_repository.clone()))
        {
            continue;
        }
        let element = add.element();
        let open_end = start + block.find('>').unwrap_or(0) + 1;
        let repo_indent = line_indent(&xml, start).to_string();

        if xml[..open_end].ends_with("/>") {
            // `<repository name="x"/>` has no body to insert into.
            let open = xml[start..open_end].trim_end_matches("/>").trim_end();
            let rewritten =
                format!("{open}>\n{repo_indent}  {element}\n{repo_indent}</repository>");
            xml.replace_range(start..end, &rewritten);
        } else if let Some(rel) = block.rfind("<path ") {
            // Right after the last existing path.
            let at = start + rel;
            let close = xml[at..].find('>').map_or(end, |i| at + i + 1);
            let indent = line_indent(&xml, at).to_string();
            let rest = if indent.is_empty() && !xml[..at].ends_with('\n') {
                // Inline XML: no line to copy the indentation from.
                element
            } else {
                format!("\n{indent}{element}")
            };
            xml.insert_str(close, &rest);
        } else {
            // Paths come before arches in the schema; with neither, before
            // the closing tag.
            let anchor = block
                .find("<arch>")
                .map_or(end - "</repository>".len(), |rel| start + rel);
            let own_line = xml[..anchor]
                .rfind('\n')
                .is_some_and(|nl| xml[nl + 1..anchor].trim().is_empty());
            if own_line {
                let line_start = xml[..anchor].rfind('\n').unwrap_or(0) + 1;
                let indent = if xml[anchor..].starts_with("</repository>") {
                    format!("{repo_indent}  ")
                } else {
                    line_indent(&xml, anchor).to_string()
                };
                xml.insert_str(line_start, &format!("{indent}{element}\n"));
            } else {
                xml.insert_str(anchor, &element);
            }
        }
        made.push(add.clone());
    }
    Ok((xml, made))
}
