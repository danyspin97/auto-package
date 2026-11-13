//! The shape of the braced `%{python_expand ...}` blocks an edit leaves in a
//! spec (convention 19 (section 119)). The origin's block is the maintainer's
//! recipe: an edit may change a line inside it, but it may not remove a block,
//! merge two or split one, or re-emit its lines as `%python_expand` lines. The
//! blocks are counted per RPM section; a line inside a block is not looked at.

use super::nested_python_expand::brace_depth;

/// The RPM section headers a block can sit under.
const SECTIONS: &[&str] = &[
    "prep",
    "generate_buildrequires",
    "build",
    "install",
    "check",
    "clean",
    "pre",
    "post",
    "preun",
    "postun",
    "pretrans",
    "posttrans",
    "verifyscript",
    "triggerprein",
    "triggerin",
    "triggerun",
    "triggerpostun",
    "filetriggerin",
    "filetriggerun",
    "filetriggerpostun",
    "transfiletriggerin",
    "transfiletriggerun",
    "transfiletriggerpostun",
    "files",
    "package",
    "description",
    "changelog",
    "sepolicy",
];

/// A braced `%{python_expand ...}` opener: the section it sits under and its
/// line, trimmed at the end.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BracedBlock {
    pub section: String,
    pub first_line: String,
}

/// The braced `%{python_expand ...}` openers of `spec`, in order. A block ends
/// where its braces balance, so a `%{...}` macro in its body does not end it.
pub fn braced_python_expand_blocks(spec: &str) -> Vec<BracedBlock> {
    let mut found = Vec::new();
    let mut section = String::new();
    let mut depth = 0;
    for line in spec.lines() {
        if depth > 0 {
            depth = brace_depth(line, depth);
            continue;
        }
        let trimmed = line.trim_start();
        if trimmed.starts_with("%{python_expand ") {
            found.push(BracedBlock {
                section: section.clone(),
                first_line: trimmed.trim_end().to_string(),
            });
            depth = brace_depth(trimmed, 0);
            continue;
        }
        if let Some(name) = section_name(line) {
            section = name;
        }
    }
    found
}

/// The first lines of the origin's blocks an edit reshapes: in each section
/// where the origin has braced blocks, an edit that leaves a different number
/// of them is a reshape. The named lines are the origin's blocks whose opener
/// the edit no longer has, or the section's first block when every opener
/// survives (a split). Empty when no block changed shape.
pub fn reshaped_python_expand_blocks(before: &str, after: &str) -> Vec<String> {
    let origin = braced_python_expand_blocks(before);
    let edited = braced_python_expand_blocks(after);
    let mut named = Vec::new();
    let mut sections: Vec<&str> = origin.iter().map(|b| b.section.as_str()).collect();
    sections.sort_unstable();
    sections.dedup();
    for section in sections {
        let theirs: Vec<&BracedBlock> = origin.iter().filter(|b| b.section == section).collect();
        let ours: Vec<&BracedBlock> = edited.iter().filter(|b| b.section == section).collect();
        if theirs.len() == ours.len() {
            continue;
        }
        let gone: Vec<String> = theirs
            .iter()
            .filter(|b| !ours.iter().any(|o| o.first_line == b.first_line))
            .map(|b| b.first_line.clone())
            .collect();
        if gone.is_empty() {
            named.push(theirs[0].first_line.clone());
        } else {
            named.extend(gone);
        }
    }
    named
}

/// The RPM section `line` opens, when it is a section header (`%install`,
/// `%files -n foo`, `%package devel`).
fn section_name(line: &str) -> Option<String> {
    let word = line.split_whitespace().next()?;
    let name = word.strip_prefix('%')?;
    SECTIONS.contains(&name).then(|| name.to_string())
}
