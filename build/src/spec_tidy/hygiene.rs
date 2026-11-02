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
    Clean,
    InstallRm,
}

/// The lines that only remove the build root, which rpm does before `%install`.
const BUILDROOT_RM: &[&str] = &["rm -rf %{buildroot}", "rm -rf $RPM_BUILD_ROOT"];

fn is_buildroot_rm(line: &str) -> bool {
    BUILDROOT_RM.contains(&line.trim_end())
}

/// The index of the first header after `i`, or the end of the spec.
fn body_end(lines: &[&str], i: usize) -> usize {
    lines[i + 1..]
        .iter()
        .position(|l| is_header(l))
        .map_or(lines.len(), |p| i + 1 + p)
}

/// A `%clean` section whose body is only blank lines and the build root
/// removal: rpm does that itself, so the whole section goes.
fn clean_construct(lines: &[&str], i: usize) -> Option<Construct> {
    let end = body_end(lines, i);
    let body = &lines[i + 1..end];
    if !body
        .iter()
        .all(|l| l.trim().is_empty() || is_buildroot_rm(l))
    {
        return None;
    }
    let what = if body.iter().any(|l| is_buildroot_rm(l)) {
        "a `%clean` section that only removes the build root"
    } else {
        "an empty `%clean` section"
    };
    let fix = if end == i + 1 {
        "delete the line `%clean`".to_string()
    } else {
        format!("delete the section, lines {}-{end}", i + 1)
    };
    Some(Construct {
        pass: Pass::Clean,
        line: i + 1,
        what: what.to_string(),
        fix,
        edits: vec![LineEdit {
            start: i,
            end,
            with: Vec::new(),
        }],
    })
}

/// A `rm -rf %{buildroot}` that is the first line of `%install`.
fn install_construct(lines: &[&str], i: usize) -> Option<Construct> {
    let rm = lines.get(i + 1).filter(|l| is_buildroot_rm(l))?;
    Some(Construct {
        pass: Pass::InstallRm,
        line: i + 2,
        what: "the build root removal as the first line of `%install`".to_string(),
        fix: format!("delete the line `{}`", rm.trim()),
        edits: vec![LineEdit {
            start: i + 1,
            end: i + 2,
            with: Vec::new(),
        }],
    })
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
            if depth == 0 {
                match word {
                    "%clean" => out.extend(clean_construct(&lines, i)),
                    "%install" => out.extend(install_construct(&lines, i)),
                    _ => {}
                }
            }
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

/// The `%clean` sections that only remove the build root, and an empty
/// `%clean` header, dropped. A `%clean` with other commands stays.
pub fn drop_clean_sections(spec: &str) -> Option<String> {
    apply(spec, Pass::Clean)
}

/// The `rm -rf %{buildroot}` that starts `%install`, dropped.
pub fn drop_install_buildroot_rm(spec: &str) -> Option<String> {
    apply(spec, Pass::InstallRm)
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

    #[test]
    fn a_clean_that_only_removes_the_build_root_goes_with_its_separator() {
        let spec = "%install\nmake\n\n%clean\nrm -rf %{buildroot}\n\n%files\n/a\n";
        assert_eq!(
            drop_clean_sections(spec).unwrap(),
            "%install\nmake\n\n%files\n/a\n"
        );
        let env = "%install\nmake\n%clean\nrm -rf $RPM_BUILD_ROOT\n\n%files\n/a\n";
        assert_eq!(
            drop_clean_sections(env).unwrap(),
            "%install\nmake\n\n%files\n/a\n"
        );
    }

    #[test]
    fn an_empty_clean_header_goes_even_with_nothing_after_it() {
        assert_eq!(
            drop_clean_sections("%install\nmake\n%clean\n\n%files\n/a\n").unwrap(),
            "%install\nmake\n\n%files\n/a\n"
        );
        assert_eq!(
            drop_clean_sections("%install\nmake\n\n%clean\n").unwrap(),
            "%install\nmake\n"
        );
        assert_eq!(
            drop_clean_sections("%install\nmake\n%clean\n%changelog\n- x\n").unwrap(),
            "%install\nmake\n%changelog\n- x\n"
        );
    }

    #[test]
    fn a_clean_at_the_end_of_the_file_keeps_no_trailing_blank() {
        assert_eq!(
            drop_clean_sections("%install\nmake\n\n%clean\nrm -rf %{buildroot}\n").unwrap(),
            "%install\nmake\n"
        );
    }

    #[test]
    fn a_clean_with_other_commands_a_comment_or_a_conditional_stays() {
        for body in [
            "rm -rf %{buildroot}\necho done\n",
            "# keep the tree\nrm -rf %{buildroot}\n",
            "%if 0%{?a}\nrm -rf %{buildroot}\n%endif\n",
            "rm -rf %{buildroot}/cache\n",
            "rm -rf %{buildroot} # all\n",
            "rm -fr %{buildroot}\n",
        ] {
            let spec = format!("%install\nmake\n%clean\n{body}%files\n/a\n");
            assert_eq!(drop_clean_sections(&spec), None, "{body}");
        }
    }

    #[test]
    fn a_clean_inside_a_conditional_is_left() {
        let spec = "%if 0%{?a}\n%clean\nrm -rf %{buildroot}\n%endif\n%files\n/a\n";
        assert_eq!(drop_clean_sections(spec), None);
    }

    #[test]
    fn a_trigger_ends_a_clean_section_and_its_body_is_not_part_of_it() {
        let spec = "%install\nmake\n%clean\nrm -rf %{buildroot}\n%triggerin -- x\necho\n";
        assert_eq!(
            drop_clean_sections(spec).unwrap(),
            "%install\nmake\n%triggerin -- x\necho\n"
        );
    }

    #[test]
    fn the_build_root_removal_that_starts_install_goes() {
        let spec = "%install\nrm -rf %{buildroot}\nmake install\n%files\n/a\n";
        assert_eq!(
            drop_install_buildroot_rm(spec).unwrap(),
            "%install\nmake install\n%files\n/a\n"
        );
        let env = "%install\nrm -rf $RPM_BUILD_ROOT\n%cmake_install\n";
        assert_eq!(
            drop_install_buildroot_rm(env).unwrap(),
            "%install\n%cmake_install\n"
        );
    }

    #[test]
    fn a_build_root_removal_that_is_not_the_first_line_stays() {
        for spec in [
            "%install\n\nrm -rf %{buildroot}\nmake\n",
            "%install\nmake\nrm -rf %{buildroot}\n",
            "%install\nrm -rf %{buildroot}/x\n",
            "%install\n%if 0%{?a}\nrm -rf %{buildroot}\n%endif\n",
        ] {
            assert_eq!(drop_install_buildroot_rm(spec), None, "{spec}");
        }
    }

    #[test]
    fn the_removals_are_idempotent_and_do_not_touch_each_other() {
        let spec =
            "%install\nrm -rf %{buildroot}\nmake\n\n%clean\nrm -rf %{buildroot}\n\n%files\n/a\n";
        let once = drop_install_buildroot_rm(spec).unwrap();
        assert_eq!(
            drop_clean_sections(spec).unwrap(),
            "%install\nrm -rf %{buildroot}\nmake\n\n%files\n/a\n"
        );
        let twice = drop_clean_sections(&once).unwrap();
        assert_eq!(twice, "%install\nmake\n\n%files\n/a\n");
        assert_eq!(drop_install_buildroot_rm(&twice), None);
        assert_eq!(drop_clean_sections(&twice), None);
    }
}
