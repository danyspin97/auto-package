//! Build flags of a branched package: the `<build>`, `<publish>`, `<debuginfo>`
//! and `<useforbuild>` elements of a package meta. Each says, per repository and
//! architecture, what OBS builds, publishes or keeps debug info for. A branch
//! starts without them, so a repository its origin deliberately leaves out gets
//! built in the branch and fails there. These functions copy the origin's flags
//! onto the branch package and leave the rest of the branch meta alone.

use super::*;
use crate::osc::branch::project_meta::write_meta;
use crate::osc::error::*;
use crate::osc::names::*;
use crate::osc::query::*;
use quick_xml::Reader;
use quick_xml::events::{BytesStart, Event};

/// The flag kinds, in the order a meta lists them.
///
/// Copy rule: for each kind the origin defines (at least one element of that
/// name), the branch's elements of that kind are replaced by the origin's. A
/// kind the origin does not define is left exactly as the branch has it, so a
/// `<disable>` a user added to the branch survives unless the origin speaks
/// for that kind.
pub const FLAG_KINDS: [&str; 4] = ["build", "publish", "debuginfo", "useforbuild"];

/// One element directly under the root of a meta document, by byte range
/// (end exclusive, past the closing tag).
#[derive(Debug)]
struct Child {
    name: String,
    start: usize,
    end: usize,
}

/// The root element of a meta document and its direct children.
#[derive(Debug, Default)]
struct Layout {
    root: String,
    children: Vec<Child>,
    /// Where the first child starts, for its indentation.
    first_child: Option<usize>,
    /// Where the root's closing tag starts; `None` for an empty root element.
    close_at: Option<usize>,
}

fn tag_name(e: &BytesStart) -> Result<String, String> {
    Ok(e.name().as_ref().to_string())
}

/// The element structure of a meta document, or why it is not well formed.
fn layout(xml: &str) -> Result<Layout, String> {
    let mut reader = Reader::from_str(xml);
    let mut out = Layout::default();
    let mut depth = 0usize;
    // The depth-1 element still open, with its start.
    let mut open: Option<(String, usize)> = None;
    let mut seen_root = false;
    loop {
        let before = reader.buffer_position() as usize;
        let event = reader
            .read_event()
            .map_err(|e| format!("malformed XML: {e}"))?;
        let after = reader.buffer_position() as usize;
        match event {
            Event::Start(e) => {
                let name = tag_name(&e)?;
                match depth {
                    0 => {
                        if std::mem::replace(&mut seen_root, true) {
                            return Err("more than one root element".into());
                        }
                        out.root = name;
                    }
                    1 => {
                        out.first_child.get_or_insert(before);
                        open = Some((name, before));
                    }
                    _ => {}
                }
                depth += 1;
            }
            Event::Empty(e) => {
                let name = tag_name(&e)?;
                match depth {
                    0 => {
                        if std::mem::replace(&mut seen_root, true) {
                            return Err("more than one root element".into());
                        }
                        out.root = name;
                    }
                    1 => {
                        out.first_child.get_or_insert(before);
                        out.children.push(Child {
                            name,
                            start: before,
                            end: after,
                        });
                    }
                    _ => {}
                }
            }
            Event::End(_) => {
                depth = depth
                    .checked_sub(1)
                    .ok_or("a closing tag with nothing open")?;
                match depth {
                    0 => out.close_at = Some(before),
                    1 => {
                        if let Some((name, start)) = open.take() {
                            out.children.push(Child {
                                name,
                                start,
                                end: after,
                            });
                        }
                    }
                    _ => {}
                }
            }
            Event::Eof => break,
            _ => {}
        }
    }
    if depth != 0 || !seen_root {
        return Err("the document is not complete".into());
    }
    Ok(out)
}

/// A flag block as a value that ignores the order of its children: the start
/// tag of the block, then the token list of each direct child, sorted. A child
/// the origin lists in another order is the same child.
fn canonical_block(block: &str) -> Result<(Vec<String>, Vec<Vec<String>>), String> {
    let lay = layout(block)?;
    let head_end = lay
        .children
        .first()
        .map_or(lay.close_at.unwrap_or(block.len()), |c| c.start);
    let head = canonical(&block[..head_end])?;
    let mut members = lay
        .children
        .iter()
        .map(|c| canonical(&block[c.start..c.end]))
        .collect::<Result<Vec<_>, _>>()?;
    members.sort();
    Ok((head, members))
}

/// The content of one element as a token list, so two elements compare equal
/// when they differ only in whitespace between tags, attribute order or quote
/// style. Text is trimmed; an entity reference inside text is kept as written.
/// Flag blocks carry no text: CDATA and comments are ignored (`_ => {}`), so
/// they do not make two blocks differ.
fn canonical(block: &str) -> Result<Vec<String>, String> {
    let mut reader = Reader::from_str(block);
    let mut out = Vec::new();
    let mut text = String::new();
    let flush = |text: &mut String, out: &mut Vec<String>| {
        let t = text.trim();
        if !t.is_empty() {
            out.push(format!("#{t}"));
        }
        text.clear();
    };
    loop {
        let event = reader
            .read_event()
            .map_err(|e| format!("malformed XML: {e}"))?;
        match event {
            Event::Start(e) => {
                flush(&mut text, &mut out);
                out.push(open_token(&e)?);
            }
            Event::Empty(e) => {
                flush(&mut text, &mut out);
                out.push(open_token(&e)?);
                out.push(format!("</{}>", tag_name(&e)?));
            }
            Event::End(e) => {
                flush(&mut text, &mut out);
                out.push(format!("</{}>", e.name().as_ref()));
            }
            Event::Text(t) => {
                text.push_str(&t);
            }
            Event::GeneralRef(r) => {
                text.push_str(&format!("&{};", &*r));
            }
            Event::Eof => break,
            _ => {}
        }
    }
    flush(&mut text, &mut out);
    Ok(out)
}

/// The start tag as a token: its name and its attributes sorted by name, with
/// values unescaped.
fn open_token(e: &BytesStart) -> Result<String, String> {
    let name = tag_name(e)?;
    let mut attrs = Vec::new();
    for attr in e.attributes() {
        let attr = attr.map_err(|e| format!("malformed attribute: {e}"))?;
        let key = attr.key.as_ref();
        let value = attr
            .normalized_value(quick_xml::XmlVersion::Implicit1_0)
            .map_err(|e| format!("malformed attribute: {e}"))?;
        attrs.push(format!("{key}=\"{value}\""));
    }
    attrs.sort();
    Ok(format!("<{name} {}>", attrs.join(" ")))
}

/// The indentation of the first child of the root, or two spaces when there
/// is none or it does not start its line.
fn child_indent(xml: &str, layout: &Layout) -> String {
    let Some(at) = layout.first_child else {
        return "  ".into();
    };
    let line_start = xml[..at].rfind('\n').map_or(0, |i| i + 1);
    let lead = &xml[line_start..at];
    if lead.trim().is_empty() {
        lead.to_string()
    } else {
        "  ".into()
    }
}

/// What [`merge_build_flags`] decided.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FlagMerge {
    /// The origin defines no flag kind: nothing to copy.
    NoFlags,
    /// Every kind the origin defines already matches the branch.
    Equal,
    /// The branch meta with the origin's flags written in, and the kinds that
    /// changed. Everything else in the branch meta is byte for byte as it was.
    Changed {
        xml: String,
        kinds: Vec<&'static str>,
    },
}

/// One edit of the branch meta: replace `start..end` with `text`.
struct Edit {
    start: usize,
    end: usize,
    text: String,
}

/// Copy the origin's flag kinds into the branch meta, by the rule in
/// [`FLAG_KINDS`]. Both documents must be package metas, and the branch meta
/// must have a root element to put a missing flag in. A malformed document
/// is an error and nothing is produced.
pub fn merge_build_flags(origin_xml: &str, branch_xml: &str) -> Result<FlagMerge, String> {
    let origin = layout(origin_xml).map_err(|e| format!("origin meta: {e}"))?;
    let branch = layout(branch_xml).map_err(|e| format!("branch meta: {e}"))?;
    if origin.root != "package" || branch.root != "package" {
        return Err("not a package meta".into());
    }
    let indent = child_indent(branch_xml, &branch);
    let mut edits = Vec::new();
    let mut kinds = Vec::new();
    // Blocks of kinds the branch lacks, in FLAG_KINDS order.
    let mut absent = Vec::new();
    let mut origin_defines_any = false;
    for kind in FLAG_KINDS {
        let theirs: Vec<&Child> = origin.children.iter().filter(|c| c.name == kind).collect();
        if theirs.is_empty() {
            continue;
        }
        origin_defines_any = true;
        let ours: Vec<&Child> = branch.children.iter().filter(|c| c.name == kind).collect();
        let theirs_text: Vec<&str> = theirs.iter().map(|c| &origin_xml[c.start..c.end]).collect();
        let ours_text: Vec<&str> = ours.iter().map(|c| &branch_xml[c.start..c.end]).collect();
        let same = theirs_text.len() == ours_text.len()
            && theirs_text
                .iter()
                .zip(&ours_text)
                .try_fold(true, |same, (t, o)| {
                    Ok::<_, String>(same && canonical_block(t)? == canonical_block(o)?)
                })?;
        if same {
            continue;
        }
        kinds.push(kind);
        let joined = theirs_text.join(&format!("\n{indent}"));
        match ours.split_first() {
            Some((first, rest)) => {
                edits.push(Edit {
                    start: first.start,
                    end: first.end,
                    text: joined,
                });
                // A removed block takes the whitespace before it with it.
                for c in rest {
                    edits.push(Edit {
                        start: branch_xml[..c.start].trim_end().len(),
                        end: c.end,
                        text: String::new(),
                    });
                }
            }
            None => absent.push(joined),
        }
    }
    if !origin_defines_any {
        return Ok(FlagMerge::NoFlags);
    }
    if kinds.is_empty() {
        return Ok(FlagMerge::Equal);
    }
    if !absent.is_empty() {
        let close = branch
            .close_at
            .ok_or("the branch meta is an empty element, with nowhere to put the flags")?;
        let line_start = branch_xml[..close].rfind('\n').map_or(0, |i| i + 1);
        let (at, text) = if line_start > 0 && branch_xml[line_start..close].trim().is_empty() {
            // The closing tag has its own line: the flags go on lines of their own.
            let text: String = absent.iter().map(|b| format!("{indent}{b}\n")).collect();
            (line_start, text)
        } else {
            let text: String = absent.iter().map(|b| format!("\n{indent}{b}")).collect();
            (close, text)
        };
        edits.push(Edit {
            start: at,
            end: at,
            text,
        });
    }
    // Back to front, so the offsets of the edits still to apply stay valid.
    edits.sort_by(|a, b| b.start.cmp(&a.start));
    let mut xml = branch_xml.to_string();
    for e in edits {
        xml.replace_range(e.start..e.end, &e.text);
    }
    layout(&xml).map_err(|e| format!("the merged meta does not parse: {e}"))?;
    Ok(FlagMerge::Changed { xml, kinds })
}

/// What [`sync_build_flags`] did to the branch package.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FlagSync {
    /// The origin defines no flags: the branch is unchanged.
    NoFlags,
    /// The branch already has the origin's flags: nothing was written.
    Equal,
    /// The branch meta was written with the origin's flags of these kinds.
    Copied(Vec<&'static str>),
}

/// Copy the build flags of `origin_project/pkg` onto `target_project/pkg`:
/// both metas are read, and the branch meta is written only when it changes.
pub fn sync_build_flags(
    origin_project: &str,
    target_project: &str,
    pkg: &str,
    emit: Emit<'_>,
) -> Result<FlagSync, Error> {
    check_obs_name("project", origin_project)?;
    check_obs_name("project", target_project)?;
    check_obs_name("package", pkg)?;
    let origin = api_get(
        &format!("/source/{origin_project}/{}/_meta", enc(pkg)),
        emit,
    )?;
    let branch = api_get(
        &format!("/source/{target_project}/{}/_meta", enc(pkg)),
        emit,
    )?;
    let merged = merge_build_flags(&origin, &branch).map_err(|why| {
        Error::NotActionable(format!(
            "cannot copy the build flags of {origin_project}/{pkg} to {target_project}: {why}"
        ))
    })?;
    match merged {
        FlagMerge::NoFlags => Ok(FlagSync::NoFlags),
        FlagMerge::Equal => Ok(FlagSync::Equal),
        FlagMerge::Changed { xml, kinds } => {
            write_meta(&["meta", "pkg", target_project, pkg], &xml, emit)?;
            Ok(FlagSync::Copied(kinds))
        }
    }
}
