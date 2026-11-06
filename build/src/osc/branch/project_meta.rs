//! Project meta: the repositories and paths of a project, reading and writing its
//! meta document, and the build paths added to it.

use super::*;
use crate::osc::error::*;
use crate::osc::names::*;
use crate::osc::query::*;
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

/// One `<repository>` element of a project meta, by byte range.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepoElement {
    pub name: String,
    /// The element's byte range, end exclusive, past the closing tag.
    pub start: usize,
    pub end: usize,
    /// Where its start tag ends (`end` too when it is self-closing).
    pub open_end: usize,
    pub self_closing: bool,
    /// The architectures it lists, in order.
    pub arches: Vec<String>,
    /// The byte range of its last `<arch>` element.
    pub last_arch: Option<(usize, usize)>,
    /// Where its closing tag starts; `None` when it is self-closing.
    pub close_at: Option<usize>,
}

fn repo_element(xml: &str, before: usize, after: usize, self_closing: bool) -> RepoElement {
    let open = &xml[before..after];
    RepoElement {
        name: crate::obs_xml::attr(open, "name").unwrap_or_default(),
        start: before,
        end: after,
        open_end: after,
        self_closing,
        arches: Vec::new(),
        last_arch: None,
        close_at: None,
    }
}

/// The `<repository>` elements that are direct children of the root of `xml`,
/// found by parsing it: a comment or text that looks like one is not an
/// element. A document that is not well formed is an error.
pub fn repository_elements(xml: &str) -> Result<Vec<RepoElement>, String> {
    use quick_xml::Reader;
    use quick_xml::events::Event;
    let mut reader = Reader::from_str(xml);
    let mut out = Vec::new();
    let mut depth = 0usize;
    // The repository element still open (its children are at depth 2).
    let mut open: Option<RepoElement> = None;
    // Where the text of the `<arch>` element being read starts, and the element.
    let mut arch_text: Option<(usize, usize)> = None;
    loop {
        let before = reader.buffer_position() as usize;
        let event = reader
            .read_event()
            .map_err(|e| format!("malformed XML: {e}"))?;
        let after = reader.buffer_position() as usize;
        match event {
            Event::Start(e) => {
                match (depth, e.name().as_ref()) {
                    (1, "repository") => open = Some(repo_element(xml, before, after, false)),
                    (2, "arch") if open.is_some() => arch_text = Some((before, after)),
                    _ => {}
                }
                depth += 1;
            }
            Event::Empty(e) => match (depth, e.name().as_ref()) {
                (1, "repository") => out.push(repo_element(xml, before, after, true)),
                (2, "arch") => {
                    if let Some(r) = open.as_mut() {
                        r.last_arch = Some((before, after));
                    }
                }
                _ => {}
            },
            Event::End(e) => {
                depth = depth
                    .checked_sub(1)
                    .ok_or("a closing tag with nothing open")?;
                match (depth, e.name().as_ref()) {
                    (2, "arch") => {
                        if let (Some((_, text_at)), Some(r)) = (arch_text.take(), open.as_mut()) {
                            let text = xml[text_at..before].trim();
                            r.arches.push(crate::obs_xml::unescape(text));
                            r.last_arch = Some((text_at - "<arch>".len(), after));
                        }
                    }
                    (1, "repository") => {
                        if let Some(mut r) = open.take() {
                            r.close_at = Some(before);
                            r.end = after;
                            out.push(r);
                        }
                    }
                    _ => {}
                }
            }
            Event::Eof => break,
            _ => {}
        }
    }
    if depth != 0 {
        return Err("the document is not complete".into());
    }
    Ok(out)
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
    let repositories = repository_elements(xml)
        .unwrap_or_default()
        .into_iter()
        .filter(|r| !r.name.is_empty())
        .map(|r| RepoDef {
            paths: parse_project_repo_paths(&xml[r.start..r.end]),
            name: r.name,
            arches: r.arches,
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
    let out = capture_query(
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
    write_meta(&["meta", "prj", project], xml, emit)
}

/// The lock every read-modify-write of a project meta takes, in this process:
/// an edit reads the meta, changes it and writes it back while the guard is
/// held, so two such edits never interleave and lose one another's change.
/// Read the meta again inside the guard, not before it.
pub(super) static PROJECT_META_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[cfg(test)]
pub(crate) fn project_meta_lock_is_free() -> bool {
    PROJECT_META_LOCK.try_lock().is_ok()
}

/// Hold the project-meta lock until the guard is dropped.
pub fn lock_project_meta() -> std::sync::MutexGuard<'static, ()> {
    PROJECT_META_LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

/// Replace a meta document through `osc`: `args` names the meta (`meta prj P`
/// or `meta pkg P PKG`), and `xml` is written to a private file that `-F`
/// reads.
pub(super) fn write_meta(args: &[&str], xml: &str, emit: Emit<'_>) -> Result<(), Error> {
    let mut file = tempfile::Builder::new()
        .prefix("ap-meta-")
        .suffix(".xml")
        .tempfile_in(proc::private_dir()?)?;
    std::io::Write::write_all(&mut file, xml.as_bytes())?;
    let path = file.path().to_string_lossy().into_owned();
    let cmd = format!("{} -F", args.join(" "));
    let mut argv: Vec<&str> = args.to_vec();
    argv.extend(["-F", &path]);
    let out = proc::capture_with_timeout("osc", &argv, &anywhere(), emit, OSC_QUERY_TIMEOUT)
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
        let (start, end) = repository_elements(&xml)?
            .into_iter()
            .find(|r| r.name == add.repository)
            .map(|r| (r.start, r.end))
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

/// `xml` with `rebuild="local"` on every `<repository>`, or `None` when each
/// already has it. A repository that names another `rebuild` mode gets `local`
/// in place of it. Its other attributes, the other repositories and the
/// order of the attributes are left as they are. `block` is not touched.
pub fn with_local_rebuild(xml: &str) -> Option<String> {
    let mut out = xml.to_string();
    let mut changed = false;
    // Back to front, so earlier offsets stay valid.
    for r in repository_elements(xml).ok()?.into_iter().rev() {
        let tag = &out[r.start..r.open_end];
        match crate::obs_xml::attr(tag, "rebuild").as_deref() {
            Some("local") => continue,
            Some(_) => {
                let at = r.start + tag.find("rebuild=")? + "rebuild=".len();
                let quote = out[at..].chars().next()?;
                let value = at + 1;
                let end = value + out[value..].find(quote)?;
                out.replace_range(value..end, "local");
            }
            None => {
                let gt = r.start + tag.rfind('>')?;
                let at = if out[..gt].ends_with('/') { gt - 1 } else { gt };
                let trimmed = out[..at].trim_end().len();
                out.insert_str(trimmed, " rebuild=\"local\"");
            }
        }
        changed = true;
    }
    changed.then_some(out)
}

/// Make the repositories of `project` rebuild only for their own sources (see
/// [`with_local_rebuild`]). A project with nothing to change is not written.
pub fn rebuild_locally(project: &str, emit: Emit<'_>) -> Result<(), Error> {
    let _meta = lock_project_meta();
    let Some(xml) = get_project_meta(project, emit)? else {
        return Ok(());
    };
    match with_local_rebuild(&xml) {
        Some(edited) => set_project_meta(project, &edited, emit),
        None => Ok(()),
    }
}
