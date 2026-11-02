//! Obsolete spec constructs that `lint` removes from an existing spec.
//!
//! An update keeps the maintainer's recipe (convention 86) and does not run
//! these passes. Each pass is pure: `None` when the spec holds none of its
//! constructs, so the text is then left byte-identical. Lines inside an `%if`
//! block are never touched, and neither is a comment or `%changelog`.

use super::{first_word, is_section};

/// The constructs one pass removes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pass {
    Group,
    BuildRoot,
}

/// Lines `start..end` (0-based, `end` exclusive) replaced by `with`.
#[derive(Debug, Clone)]
struct LineEdit {
    start: usize,
    end: usize,
    with: Vec<String>,
}

/// One obsolete construct: where it is, what it is and the exact fix.
#[derive(Debug, Clone)]
pub struct Construct {
    pub pass: Pass,
    /// 1-based line of the construct's first line.
    pub line: usize,
    pub what: String,
    pub fix: String,
    edits: Vec<LineEdit>,
}

/// Whether `line` starts a section: the headers of [`super::is_section`] and
/// `%clean`, which the other passes need as a section end.
fn is_header(line: &str) -> bool {
    is_section(line) || first_word(line) == "%clean"
}

/// Every obsolete construct of `spec` that a pass can remove. Lines inside
/// an `%if` block are not reported: the passes leave them.
pub fn constructs(spec: &str) -> Vec<Construct> {
    let lines: Vec<&str> = spec.lines().collect();
    let mut out = Vec::new();
    let mut depth = 0i32;
    // The preamble is the tags above the first section, and the tags of each
    // `%package`; a `%description`, `%files` or other section ends it.
    let mut in_preamble = true;
    // A `%define` or `%global` continued by a trailing backslash: its lines
    // are macro text, so an `%if` in them opens no block.
    let mut in_macro = false;
    for (i, &line) in lines.iter().enumerate() {
        let word = first_word(line);
        let continued = line.trim_end().ends_with('\\');
        if in_macro {
            in_macro = continued;
            continue;
        }
        if matches!(word, "%define" | "%global") && continued {
            in_macro = true;
            continue;
        }
        if line.trim_start().starts_with('#') {
            continue;
        }
        if word == "%changelog" {
            break;
        }
        if word.starts_with("%if") {
            depth += 1;
            continue;
        }
        if word == "%endif" {
            depth -= 1;
            continue;
        }
        if is_header(line) {
            in_preamble = word == "%package";
            continue;
        }
        if depth != 0 || !in_preamble {
            continue;
        }
        let trimmed = line.trim_start();
        if trimmed.starts_with("Group:") {
            out.push(Construct {
                pass: Pass::Group,
                line: i + 1,
                what: "the `Group:` tag".to_string(),
                fix: format!("delete the line `{}`", line.trim()),
                edits: vec![LineEdit {
                    start: i,
                    end: i + 1,
                    with: Vec::new(),
                }],
            });
        } else if trimmed.starts_with("BuildRoot:") {
            out.push(Construct {
                pass: Pass::BuildRoot,
                line: i + 1,
                what: "the `BuildRoot:` tag".to_string(),
                fix: format!("delete the line `{}`", line.trim()),
                edits: vec![LineEdit {
                    start: i,
                    end: i + 1,
                    with: Vec::new(),
                }],
            });
        }
    }
    out
}

/// The spec with every construct of `pass` removed, `None` when it has none.
fn apply(spec: &str, pass: Pass) -> Option<String> {
    let mut edits: Vec<LineEdit> = constructs(spec)
        .into_iter()
        .filter(|c| c.pass == pass)
        .flat_map(|c| c.edits)
        .collect();
    if edits.is_empty() {
        return None;
    }
    edits.sort_by_key(|e| e.start);
    // Each piece keeps its own terminator, so a CRLF spec stays CRLF.
    let lines: Vec<&str> = spec.split_inclusive('\n').collect();
    let mut out = splice(&lines, &edits).concat();
    // A spec without a final newline gets none from the edit either.
    if !spec.ends_with('\n') && out.ends_with('\n') {
        out.pop();
        if out.ends_with('\r') {
            out.pop();
        }
    }
    Some(out)
}

/// The terminator of a line of [`apply`]: CRLF, LF, or none for a last line.
fn terminator(line: &str) -> &'static str {
    if line.ends_with("\r\n") {
        "\r\n"
    } else if line.ends_with('\n') {
        "\n"
    } else {
        ""
    }
}

/// `lines` (each with its terminator) with `edits` (sorted, not overlapping)
/// applied. A removal that takes blank lines out from between two sections
/// keeps one blank line there, so the section that follows still has its
/// separator.
fn splice(lines: &[&str], edits: &[LineEdit]) -> Vec<String> {
    let mut out: Vec<String> = Vec::with_capacity(lines.len());
    let mut pos = 0;
    for e in edits {
        if e.start < pos {
            continue;
        }
        let mut start = e.start;
        // A removal at the end of the spec takes the blank lines before it too.
        if e.end == lines.len() {
            while start > pos && lines[start - 1].trim().is_empty() {
                start -= 1;
            }
        }
        out.extend(lines[pos..start].iter().map(|l| l.to_string()));
        let eol = terminator(lines[e.start]);
        let mut with: Vec<String> = e.with.iter().map(|w| format!("{w}{eol}")).collect();
        let removed_blank = lines[start..e.end].iter().any(|l| l.trim().is_empty());
        if e.with.is_empty()
            && removed_blank
            && start > 0
            && !lines[start - 1].trim().is_empty()
            && e.end < lines.len()
        {
            with.push(eol.to_string());
        }
        out.extend(with);
        pos = e.end;
    }
    out.extend(lines[pos..].iter().map(|l| l.to_string()));
    out
}

/// The `Group:` tags of the preamble, which rpm no longer reads, dropped.
pub fn drop_group_tags(spec: &str) -> Option<String> {
    apply(spec, Pass::Group)
}

/// The `BuildRoot:` tags of the preamble, which rpm ignores, dropped.
pub fn drop_buildroot_tags(spec: &str) -> Option<String> {
    apply(spec, Pass::BuildRoot)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The VulkanMemoryAllocator spec of X11:Wayland, with its `Group:` line.
    const VMA: &str = "Name:           VulkanMemoryAllocator\nVersion:        3.4.0\n\
        Release:        0\nSummary:        Vulkan memory allocation library\n\
        License:        MIT\nGroup:          Development/Libraries/C and C++\n\
        URL:            https://github.com/GPUOpen-LibrariesAndSDKs/VulkanMemoryAllocator\n\
        BuildRequires:  cmake\nBuildArch:      noarch\n\n%description\nA library.\n\n\
        %package devel\nSummary:        Vulkan memory allocation library\n\
        Requires:       vulkan-headers >= 1.3\n\n%description devel\nA library.\n\n\
        %prep\n%autosetup\n\n%build\n%cmake\n%cmake_build\n\n%install\n%cmake_install\n\n\
        %files devel\n%license LICENSE.txt\n\n%changelog\n";

    #[test]
    fn the_group_line_of_the_preamble_goes_and_nothing_else_moves() {
        let out = drop_group_tags(VMA).unwrap();
        assert_eq!(
            out,
            VMA.replace("Group:          Development/Libraries/C and C++\n", "")
        );
        assert_eq!(drop_group_tags(&out), None);
    }

    #[test]
    fn a_group_in_a_subpackage_preamble_goes_too() {
        let spec = "Name: x\n%package devel\nGroup: Development/Libraries/C\nSummary: s\n\
                    %description devel\nd\n%files devel\n/a\n%changelog\n";
        assert_eq!(
            drop_group_tags(spec).unwrap(),
            "Name: x\n%package devel\nSummary: s\n%description devel\nd\n%files devel\n/a\n%changelog\n"
        );
    }

    #[test]
    fn a_group_inside_a_conditional_is_left() {
        let spec = "Name: x\n%if 0%{?suse_version} < 1315\nGroup: System/Libraries\n%endif\n\
                    %if 0%{?fedora}\n%else\nGroup: Other\n%endif\n";
        assert_eq!(drop_group_tags(spec), None);
    }

    #[test]
    fn a_group_after_a_conditional_is_dropped_and_the_block_stays() {
        let spec = "%if 0%{?a}\nRequires: b\n%endif\nGroup: System/Libraries\nSummary: s\n";
        assert_eq!(
            drop_group_tags(spec).unwrap(),
            "%if 0%{?a}\nRequires: b\n%endif\nSummary: s\n"
        );
    }

    #[test]
    fn a_group_text_in_a_description_comment_or_changelog_is_left() {
        let spec = "Name: x\n# Group: not a tag\n%description\nGroup: text\n\
                    %changelog\n- Group: changed\n";
        assert_eq!(drop_group_tags(spec), None);
    }

    #[test]
    fn a_spec_without_a_group_is_none() {
        assert_eq!(drop_group_tags("Name: x\nSummary: s\n"), None);
        assert_eq!(drop_group_tags(""), None);
    }

    #[test]
    fn a_group_at_the_end_of_the_file_without_a_newline_is_dropped() {
        assert_eq!(drop_group_tags("Name: x\nGroup: y").unwrap(), "Name: x");
    }

    #[test]
    fn the_buildroot_tag_goes_from_the_preamble_and_a_conditional_keeps_it() {
        let spec = "Name: x\nBuildRoot:   %{_tmppath}/%{name}\nSummary: s\n\
                    %package devel\nBuildRoot: %{_tmppath}/%{name}-devel\n\
                    %description devel\nd\n";
        let out = drop_buildroot_tags(spec).unwrap();
        assert_eq!(
            out,
            "Name: x\nSummary: s\n%package devel\n%description devel\nd\n"
        );
        assert_eq!(drop_buildroot_tags(&out), None);
        let kept = "%if 0%{?a}\nBuildRoot: x\n%endif\n";
        assert_eq!(drop_buildroot_tags(kept), None);
    }

    #[test]
    fn a_buildroot_in_a_description_is_left() {
        let spec = "%description\nBuildRoot: text\n";
        assert_eq!(drop_buildroot_tags(spec), None);
    }

    #[test]
    fn an_if_inside_a_multi_line_macro_does_not_open_a_block() {
        let spec = "%define m() \\\n%if 0%{?a} x %endif\nName: x\nGroup: y\n%changelog\n";
        assert_eq!(
            drop_group_tags(spec).unwrap(),
            "%define m() \\\n%if 0%{?a} x %endif\nName: x\n%changelog\n"
        );
        let block = "%global m \\\n%if 0%{?a}\\\n  x \\\n%endif\nName: x\nGroup: y\n";
        assert_eq!(
            drop_group_tags(block).unwrap(),
            "%global m \\\n%if 0%{?a}\\\n  x \\\n%endif\nName: x\n"
        );
    }

    #[test]
    fn a_crlf_spec_keeps_its_line_endings_when_a_pass_applies() {
        let spec = "Name: x\r\nGroup: y\r\nSummary: s\r\n%description\r\nd\r\n";
        assert_eq!(
            drop_group_tags(spec).unwrap(),
            "Name: x\r\nSummary: s\r\n%description\r\nd\r\n"
        );
        let tags = "Name: x\r\nGroup: y\r\nBuildRoot: z\r\n%description\r\nd\r\n";
        assert_eq!(
            drop_buildroot_tags(tags).unwrap(),
            "Name: x\r\nGroup: y\r\n%description\r\nd\r\n"
        );
    }

    #[test]
    fn a_crlf_spec_without_a_construct_is_none_and_a_missing_final_newline_stays_missing() {
        assert_eq!(drop_group_tags("Name: x\r\nSummary: s\r\n"), None);
        assert_eq!(drop_group_tags("Name: x\r\nGroup: y").unwrap(), "Name: x");
    }
}
