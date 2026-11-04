//! The rpmlintrc files of a package, judged by `lint` (convention 39): only
//! `addFilter(...)` lines, each with its `# reason:` comment directly above,
//! no `from Config import *` and no `setBadness`, named `<name>-rpmlintrc`.
//!
//! Only reported here: nothing rewrites the file yet.

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
