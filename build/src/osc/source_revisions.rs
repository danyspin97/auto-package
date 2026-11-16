//! The revisions of every package of a project in one OBS answer
//! (`/source/<project>?view=info`), for the spec cache's keys.
//!
//! The answer's `srcmd5` is the expanded directory revision of each package,
//! the value `/source/<project>/<package>?expand=1` gives for it. An entry that
//! carries an `<error>` (a broken link) is left out, and so is one without a
//! `.spec` file of its own: a caller asks for those package by package.

use super::*;
use crate::osc::query::*;
use std::collections::HashMap;

/// The expanded revision of each package of `project` that holds a `.spec`
/// file, from one OBS request. `None` when OBS cannot answer.
pub fn project_spec_revisions(project: &str) -> Option<HashMap<String, String>> {
    if !is_valid_obs_name(project) {
        return None;
    }
    let xml = osc_api(&format!("/source/{}?view=info", enc(project)))?;
    Some(parse_spec_revisions(&xml))
}

/// The `package` → `srcmd5` pairs of a `view=info` answer, for the entries that
/// hold a `.spec` file and no error.
pub fn parse_spec_revisions(xml: &str) -> HashMap<String, String> {
    let mut out = HashMap::new();
    let mut rest = xml;
    while let Some(start) = rest.find("<sourceinfo ") {
        rest = &rest[start..];
        let Some(tag_end) = rest.find('>') else {
            break;
        };
        let tag = &rest[..tag_end];
        let (body, next) = if tag.ends_with('/') {
            ("", &rest[tag_end + 1..])
        } else {
            let after = &rest[tag_end + 1..];
            match after.find("</sourceinfo>") {
                Some(end) => (&after[..end], &after[end + "</sourceinfo>".len()..]),
                None => (after, ""),
            }
        };
        rest = next;
        if body.contains("<error") || !has_spec_file(body) {
            continue;
        }
        if let (Some(package), Some(srcmd5)) = (
            crate::obs_xml::attr(tag, "package"),
            crate::obs_xml::attr(tag, "srcmd5"),
        ) && !srcmd5.is_empty()
        {
            out.insert(package, srcmd5);
        }
    }
    out
}

/// Whether an entry's body names a `.spec` file among its `<filename>`s.
fn has_spec_file(body: &str) -> bool {
    body.split("<filename>")
        .skip(1)
        .filter_map(|s| s.split("</filename>").next())
        .any(|name| name.trim().ends_with(".spec"))
}

#[cfg(test)]
#[path = "source_revisions_tests.rs"]
mod tests;
