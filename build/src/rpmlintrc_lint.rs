//! The rpmlintrc files of a package, judged by `lint` (convention 39): only
//! `addFilter(...)` lines, each with its `# reason:` comment directly above,
//! no `from Config import *` and no `setBadness`, named `<name>-rpmlintrc`.
//!
//! The only fix is a text one: [`tidy`] removes the `from Config import *`
//! line. The filters are never rewritten.

use crate::quality::{Finding, Kind};

/// The import rpmlint 2.x does not need in an rpmlintrc.
const CONFIG_IMPORT: &str = "from Config import *";

/// The rpmlintrc files a spec names as a `Source:`, the ones `lint` reads.
pub fn spec_rpmlintrc_files(spec: &str) -> Vec<String> {
    crate::spec::declared_source_files(spec)
        .0
        .into_iter()
        .filter(|f| f.ends_with("rpmlintrc"))
        .collect()
}

/// `text` without its `from Config import *` lines, each with the blank
/// line that follows it. Nothing else changes.
pub fn tidy(text: &str) -> String {
    let lines: Vec<&str> = text.split_inclusive('\n').collect();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while i < lines.len() {
        if lines[i].trim() == CONFIG_IMPORT {
            i += 1;
            if lines.get(i).is_some_and(|l| l.trim().is_empty()) {
                i += 1;
            }
            continue;
        }
        out.push_str(lines[i]);
        i += 1;
    }
    out
}

/// The changelog bullet for the rpmlintrc part of a tidy commit.
pub const BULLET: &str = "Drop `from Config import *` from the rpmlintrc";

/// What is wrong with the rpmlintrc `name` that holds `text`.
pub fn audit(name: &str, text: &str) -> Vec<Finding> {
    let mut out = Vec::new();
    if let Some(base) = name.strip_suffix(".rpmlintrc") {
        out.push(Finding {
            kind: Kind::RpmlintrcName,
            detail: format!(
                "{name}: convention 39 names it {base}-rpmlintrc; renaming changes the \
                 spec's Source line, so it is not done by lint"
            ),
        });
    }
    if let Some(n) = text.lines().position(|l| l.trim() == CONFIG_IMPORT) {
        out.push(Finding {
            kind: Kind::RpmlintrcStyle,
            detail: format!(
                "{name}: line {}: `{CONFIG_IMPORT}`; fix: delete the line",
                n + 1
            ),
        });
    }
    let import = format!("(`{CONFIG_IMPORT}`)");
    for problem in crate::pipeline::rpmlintrc_guard::rpmlintrc_problems(None, text) {
        // Said above, as a style finding.
        if !problem.contains(&import) {
            out.push(Finding {
                kind: Kind::RpmlintrcHygiene,
                detail: format!("{name}: {problem}"),
            });
        }
    }
    out
}

#[cfg(test)]
mod tests;
