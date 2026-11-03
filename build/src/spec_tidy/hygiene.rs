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
    Defattr,
    Ldconfig,
}

/// The `%defattr` of a `%files` list that only says the default: root owns
/// the files and rpm sets the modes from the build.
const DEFAULT_DEFATTR: &[&str] = &["%defattr(-,root,root)", "%defattr(-,root,root,-)"];

/// Whether `line` sets a file mode of its own: a `%defattr` that is not one of
/// the default forms, or an `%attr`. The default `%defattr` after one of these
/// changes the modes of the files that follow, so it is kept.
fn sets_mode(line: &str) -> bool {
    (line.trim_start().starts_with("%defattr(") && !DEFAULT_DEFATTR.contains(&line.trim_end()))
        || line.contains("%attr(")
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

/// A `%post` or `%postun` header, with the arguments that name its subpackage
/// and whether it is `-p /sbin/ldconfig`.
struct Scriptlet {
    index: usize,
    depth: i32,
    post: bool,
    name: Option<String>,
    ldconfig: bool,
}

fn scriptlet_of(lines: &[&str], index: usize, depth: i32) -> Scriptlet {
    let args: Vec<&str> = lines[index].split_whitespace().skip(1).collect();
    let (name, rest) = match args.as_slice() {
        ["-n", name, rest @ ..] => (Some(name.to_string()), rest),
        rest => (None, rest),
    };
    Scriptlet {
        index,
        depth,
        post: first_word(lines[index]) == "%post",
        name,
        ldconfig: rest == ["-p", "/sbin/ldconfig"],
    }
}

/// Whether the body of the header at `i` is blank.
fn body_is_blank(lines: &[&str], i: usize) -> bool {
    lines[i + 1..body_end(lines, i)]
        .iter()
        .all(|l| l.trim().is_empty())
}

/// A `%post` and a `%postun` that only run `/sbin/ldconfig` for the same
/// subpackage: current targets run ldconfig from a file trigger, so both go and
/// nothing replaces them.
fn ldconfig_constructs(lines: &[&str], scriptlets: &[Scriptlet]) -> Vec<Construct> {
    let mut out = Vec::new();
    for post in scriptlets
        .iter()
        .filter(|s| s.post && s.ldconfig && s.depth == 0)
    {
        let same = |s: &&Scriptlet| s.name == post.name;
        let posts = scriptlets.iter().filter(|s| s.post).filter(same).count();
        let unposts: Vec<&Scriptlet> = scriptlets.iter().filter(|s| !s.post).filter(same).collect();
        let [postun] = unposts.as_slice() else {
            continue;
        };
        if posts != 1
            || !postun.ldconfig
            || postun.depth != 0
            || !body_is_blank(lines, post.index)
            || !body_is_blank(lines, postun.index)
        {
            continue;
        }
        out.push(Construct {
            pass: Pass::Ldconfig,
            line: post.index + 1,
            what: format!(
                "the `%post` (line {}) and `%postun` (line {}) that only run /sbin/ldconfig",
                post.index + 1,
                postun.index + 1
            ),
            fix: "delete both: a file trigger runs ldconfig".to_string(),
            edits: removal_edits(
                lines,
                &[
                    post.index..post.index + 1,
                    postun.index..body_end(lines, postun.index),
                ],
            ),
        });
    }
    out
}

/// The edits that delete the line ranges `removed`, plus the blank line after a
/// removed run when the line before it is blank (or the run starts the file): the
/// deletion never leaves two blank lines in a row.
fn removal_edits(lines: &[&str], removed: &[std::ops::Range<usize>]) -> Vec<LineEdit> {
    let mut gone: std::collections::BTreeSet<usize> = removed.iter().cloned().flatten().collect();
    loop {
        let mut next = None;
        let mut prev: Option<usize> = None;
        for &i in &gone {
            if prev.is_none_or(|p| p + 1 != i) {
                // `i` starts a run: find its end.
                let end = (i..).find(|j| !gone.contains(j)).unwrap_or(i);
                let blank_before = i == 0 || lines[i - 1].trim().is_empty();
                if blank_before && end < lines.len() && lines[end].trim().is_empty() {
                    next = Some(end);
                    break;
                }
            }
            prev = Some(i);
        }
        match next {
            Some(i) => {
                gone.insert(i);
            }
            None => break,
        }
    }
    let mut edits: Vec<LineEdit> = Vec::new();
    for i in gone {
        match edits.last_mut() {
            Some(e) if e.end == i => e.end = i + 1,
            _ => edits.push(LineEdit {
                start: i,
                end: i + 1,
                with: Vec::new(),
            }),
        }
    }
    edits
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

/// Whether `%ldconfig_scriptlets` is defined for the build repository
/// `repository`: Tumbleweed, Slowroll, Factory, Leap 16 and SLE 16. Any other
/// name is taken as not defining it (an older Leap or SLE, or one unknown).
pub fn ldconfig_macro_defined(repository: &str) -> bool {
    const KNOWN: &[&str] = &[
        "Tumbleweed",
        "Slowroll",
        "Factory",
        "Leap_16",
        "SLE-16",
        "SLE_16",
    ];
    KNOWN.iter().any(|known| repository.contains(known))
}

/// Every obsolete construct of `spec` that a pass can remove. Lines inside
/// an `%if` block are not reported: the passes leave them. The
/// ldconfig pair is reported only when `ldconfig_macro` is set
/// (see [`ldconfig_macro_defined`]).
pub fn constructs(spec: &str, ldconfig_macro: bool) -> Vec<Construct> {
    let lines: Vec<&str> = spec.lines().collect();
    let mut out = Vec::new();
    let mut depth = 0i32;
    // The preamble is the tags above the first section, and the tags of each
    // `%package`; a `%description`, `%files` or other section ends it.
    let mut in_preamble = true;
    // A `%define` or `%global` continued by a trailing backslash: its lines
    // are macro text, so an `%if` in them opens no block.
    let mut in_macro = false;
    let mut section = "";
    // Whether the current section has set a file mode (see `sets_mode`).
    let mut mode_set = false;
    let mut scriptlets = Vec::new();
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
            section = word;
            mode_set = false;
            if depth == 0 {
                match word {
                    "%clean" => out.extend(clean_construct(&lines, i)),
                    "%install" => out.extend(install_construct(&lines, i)),
                    _ => {}
                }
            }
            if matches!(word, "%post" | "%postun") {
                scriptlets.push(scriptlet_of(&lines, i, depth));
            }
            continue;
        }
        // A mode set in a conditional may apply, so it counts as well.
        if section == "%files" && sets_mode(line) {
            mode_set = true;
        }
        if depth != 0 {
            continue;
        }
        if section == "%files" && DEFAULT_DEFATTR.contains(&line.trim_end()) {
            if !mode_set {
                out.push(Construct {
                    pass: Pass::Defattr,
                    line: i + 1,
                    what: "the default `%defattr` of a `%files` list".to_string(),
                    fix: format!("delete the line `{}`", line.trim()),
                    edits: vec![LineEdit {
                        start: i,
                        end: i + 1,
                        with: Vec::new(),
                    }],
                });
            }
            continue;
        }
        if !in_preamble {
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
    if ldconfig_macro {
        out.extend(ldconfig_constructs(&lines, &scriptlets));
    }
    out
}

/// The spec with every construct of `pass` removed, `None` when it has none.
/// The ldconfig pass runs whenever it is asked: the caller that
/// knows the build target decides whether to ask.
fn apply(spec: &str, pass: Pass) -> Option<String> {
    let mut edits: Vec<LineEdit> = constructs(spec, true)
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

/// The hand-written `%post` and `%postun` pair that only runs `/sbin/ldconfig`
/// for one subpackage, dropped: no `%ldconfig_scriptlets` replaces it.
pub fn drop_ldconfig_pair(spec: &str) -> Option<String> {
    apply(spec, Pass::Ldconfig)
}

/// Every pass above, in order, on `spec`. The text is `spec` itself when
/// none of them applies. The ldconfig pass runs only when
/// `ldconfig_macro` is set (see [`ldconfig_macro_defined`]).
pub fn drop_obsolete_constructs(spec: &str, ldconfig_macro: bool) -> String {
    let passes: [fn(&str) -> Option<String>; 5] = [
        drop_group_tags,
        drop_buildroot_tags,
        drop_clean_sections,
        drop_install_buildroot_rm,
        drop_default_defattr,
    ];
    let mut text = passes
        .iter()
        .fold(spec.to_string(), |text, pass| pass(&text).unwrap_or(text));
    if ldconfig_macro && let Some(next) = drop_ldconfig_pair(&text) {
        text = next;
    }
    text
}

/// Whether `spec` has a hand-written `%post`/`%postun` pair that the
/// ldconfig pass would drop, which stays because the target
/// does not define the macro.
pub fn ldconfig_left(spec: &str, ldconfig_macro: bool) -> bool {
    !ldconfig_macro
        && constructs(spec, true)
            .iter()
            .any(|c| c.pass == Pass::Ldconfig)
}

/// The default `%defattr(-,root,root)` lines of `%files`, dropped. Other
/// modes are kept: they say something the default does not.
pub fn drop_default_defattr(spec: &str) -> Option<String> {
    apply(spec, Pass::Defattr)
}

#[cfg(test)]
mod tests;
