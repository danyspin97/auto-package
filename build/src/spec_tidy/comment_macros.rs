//! Macros in the comment lines of a spec, escaped before the first build.
//!
//! RPM expands a macro in a `#` comment when it parses the spec, so
//! `# Installs into %{_bindir}` is rewritten by rpm, and rpmlint reports the
//! comment (`macro-in-comment`). The pass doubles the lone `%` of each such
//! macro: `%%` is a literal percent to rpm. Only comment lines are touched.
//! Anything that is not a comment in rpm's eyes (a `#` after code, a `%`
//! inside a here-document, a `#` inside a macro body or a continued line) is
//! left as it is, and [`has_macro`] still sees what it leaves.

use super::{first_word, is_section};

/// Sections whose bodies are shell scripts, where a here-document is read.
const SCRIPT_SECTIONS: &[&str] = &[
    "%prep",
    "%conf",
    "%build",
    "%install",
    "%check",
    "%generate_buildrequires",
    "%pre",
    "%post",
    "%preun",
    "%postun",
    "%pretrans",
    "%posttrans",
    "%preuntrans",
    "%postuntrans",
    "%verifyscript",
    "%ldconfig_scriptlets",
];

/// Prefixes of the trigger and file trigger sections, which are scripts too.
const TRIGGER_PREFIXES: &[&str] = &["%trigger", "%filetrigger", "%transfiletrigger"];

/// Whether a `%` followed by `next` starts a macro: a letter, `_`, `{`, `(`,
/// `?` or `!`. The one place the rule lives; the escape and the check both
/// read it.
pub fn starts_macro(next: Option<char>) -> bool {
    next.is_some_and(|c| c.is_alphabetic() || matches!(c, '_' | '{' | '(' | '?' | '!'))
}

/// Byte offsets of the `%` signs in `line` that start a macro. A `%%` pair is
/// a literal percent: both signs are skipped as a pair.
fn macro_starts(line: &str) -> Vec<usize> {
    let mut starts = Vec::new();
    let mut chars = line.char_indices().peekable();
    while let Some((at, c)) = chars.next() {
        if c != '%' {
            continue;
        }
        match chars.peek().map(|&(_, n)| n) {
            Some('%') => {
                chars.next();
            }
            next if starts_macro(next) => starts.push(at),
            _ => {}
        }
    }
    starts
}

/// Whether `line` holds a macro that rpm would expand.
pub fn has_macro(line: &str) -> bool {
    !macro_starts(line).is_empty()
}

/// `line` with each macro's `%` doubled.
fn escape_line(line: &str) -> String {
    let starts = macro_starts(line);
    let mut out = String::with_capacity(line.len() + starts.len());
    let mut last = 0;
    for at in starts {
        out.push_str(&line[last..at]);
        out.push_str("%%");
        last = at + 1;
    }
    out.push_str(&line[last..]);
    out
}

/// A comment line in rpm's reading: its first non-blank character is `#`.
fn is_comment(line: &str) -> bool {
    line.trim_start().starts_with('#')
}

/// The terminator word of a here-document that `line` opens (`<<WORD`,
/// `<<'WORD'`, `<<"WORD"`, `<<-WORD`), and whether it is the `<<-` form whose
/// terminator may be indented. `None` when the line opens none.
fn heredoc_opened(line: &str) -> Option<(String, bool)> {
    let at = line.find("<<")?;
    let mut rest = &line[at + 2..];
    let dash = rest.starts_with('-');
    if dash {
        rest = &rest[1..];
    }
    let quote = rest.chars().next().filter(|c| *c == '\'' || *c == '"');
    if quote.is_some() {
        rest = &rest[1..];
    }
    let word: String = rest
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
        .collect();
    if word.is_empty() || word.starts_with(|c: char| c.is_ascii_digit()) {
        return None;
    }
    if let Some(q) = quote {
        if !rest[word.len()..].starts_with(q) {
            return None;
        }
    }
    Some((word, dash))
}

/// The depth of open macro bodies (`%{...}`, `%{lua:...}`) after `line`, given
/// the depth before it. A `%{` opens a body; the braces inside it count.
fn brace_depth_after(line: &str, depth: usize) -> usize {
    let mut depth = depth;
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        if depth == 0 {
            if c == '%' {
                match chars.peek().copied() {
                    Some('{') => {
                        chars.next();
                        depth = 1;
                    }
                    Some('%') => {
                        chars.next();
                    }
                    _ => {}
                }
            }
        } else {
            match c {
                '{' => depth += 1,
                '}' => depth -= 1,
                _ => {}
            }
        }
    }
    depth
}

/// Escape the macros in the comment lines of `spec`, returning the new text
/// and the 1-based numbers of the lines that changed.
///
/// A comment line is left alone when rpm would not read it as a comment or
/// would read it as expanded text: inside `%changelog` or `%description`,
/// inside a here-document of a script section, inside an open `%{...}` body,
/// or continuing a line that ends in `\`. The pass is idempotent.
pub fn escape_comment_macros(spec: &str) -> (String, Vec<usize>) {
    let mut out = String::with_capacity(spec.len() + 16);
    let mut changed = Vec::new();
    let mut skip_section = false;
    let mut script = false;
    // The terminator word and the `<<-` flag of the open here-document.
    let mut heredoc: Option<(String, bool)> = None;
    let mut depth = 0usize;
    let mut continued = false;
    let lines: Vec<&str> = spec.split('\n').collect();
    for (i, &line) in lines.iter().enumerate() {
        if i > 0 {
            out.push('\n');
        }
        if is_section(line) {
            let name = first_word(line);
            skip_section = matches!(name, "%changelog" | "%description");
            script = SCRIPT_SECTIONS.contains(&name)
                || TRIGGER_PREFIXES.iter().any(|p| name.starts_with(p));
            heredoc = None;
            depth = 0;
            out.push_str(line);
        } else {
            let comment = is_comment(line);
            let inside = skip_section || depth > 0 || continued || heredoc.is_some();
            if comment && !inside {
                let escaped = escape_line(line);
                if escaped != line {
                    changed.push(i + 1);
                }
                out.push_str(&escaped);
            } else {
                out.push_str(line);
            }
            let closes = heredoc.as_ref().map(|(word, dash)| {
                let t = if *dash { line.trim_start() } else { line };
                t.trim_end() == word.as_str()
            });
            match closes {
                Some(true) => heredoc = None,
                Some(false) => {}
                None if script && !comment && !skip_section => heredoc = heredoc_opened(line),
                None => {}
            }
            depth = brace_depth_after(line, depth);
        }
        continued = line.trim_end().ends_with('\\');
    }
    (out, changed)
}

/// Whether `a` and `b` are the same spec once every `#` comment line and the
/// blank lines at the end are dropped.
pub fn same_without_comments(a: &str, b: &str) -> bool {
    fn code(spec: &str) -> Vec<&str> {
        let mut lines: Vec<&str> = spec.lines().filter(|l| !is_comment(l)).collect();
        while lines.last().is_some_and(|l| l.trim().is_empty()) {
            lines.pop();
        }
        lines
    }
    code(a) == code(b)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn escaped(spec: &str) -> String {
        escape_comment_macros(spec).0
    }

    #[test]
    fn the_jay_comment_gets_its_macro_percent_doubled() {
        let spec = "Name: jay\n# Installs the crate's binaries into %{_bindir}\n%install\n";
        let (out, lines) = escape_comment_macros(spec);
        assert_eq!(
            out,
            "Name: jay\n# Installs the crate's binaries into %%{_bindir}\n%install\n"
        );
        assert_eq!(lines, vec![2]);
    }

    #[test]
    fn every_macro_start_character_is_escaped() {
        let spec = "# %_libdir %(uname) %?x %!?y %a %{b}\n";
        assert_eq!(
            escaped(spec),
            "# %%_libdir %%(uname) %%?x %%!?y %%a %%{b}\n"
        );
    }

    #[test]
    fn a_double_percent_pair_is_copied_as_it_is() {
        assert_eq!(escaped("# 100%% of %%{x}\n"), "# 100%% of %%{x}\n");
        // The pair is a literal percent; the lone macro after it is escaped.
        assert_eq!(escaped("# %%%{x}\n"), "# %%%%{x}\n");
    }

    #[test]
    fn a_percent_that_starts_no_macro_is_left_alone() {
        let spec = "# 50% of the time, 100% done, a %- and 5 %\n";
        assert_eq!(escaped(spec), spec);
        assert!(!has_macro("# 50% of the time %- %"));
    }

    #[test]
    fn a_conditional_or_macro_line_is_not_a_comment_and_stays() {
        let spec = "%if 0%{?fedora}\n%{?x}\n%endif\n";
        assert_eq!(escaped(spec), spec);
    }

    #[test]
    fn a_comment_that_starts_with_a_conditional_is_escaped() {
        assert_eq!(escaped("#%if 0%{?x}\n"), "#%%if 0%%{?x}\n");
        assert_eq!(escaped("# %{?x}\n"), "# %%{?x}\n");
    }

    #[test]
    fn a_hash_that_is_not_the_first_character_is_not_a_comment() {
        let spec = "%build\necho $# %{_bindir}\nx=${#arr[@]} # %{_bindir}\n\techo a # b %{x}\n";
        assert_eq!(escaped(spec), spec);
    }

    #[test]
    fn an_indented_comment_is_escaped() {
        assert_eq!(escaped("  \t# a %{x}\n"), "  \t# a %%{x}\n");
    }

    #[test]
    fn a_comment_continuing_a_backslash_line_is_left_alone() {
        let spec = "%define cmd \\\n# not a comment %{_bindir}\n%global y 1\n";
        assert_eq!(escaped(spec), spec);
    }

    #[test]
    fn a_comment_inside_a_lua_or_macro_body_is_left_alone() {
        let spec = "%{lua:\n# lua text %{_bindir}\nprint(1)\n}\n# after %{_bindir}\n";
        assert_eq!(
            escaped(spec),
            "%{lua:\n# lua text %{_bindir}\nprint(1)\n}\n# after %%{_bindir}\n"
        );
    }

    #[test]
    fn a_multi_line_macro_body_with_braces_closes_before_the_next_comment() {
        let spec = "%{lua:\nlocal t = {1, 2}\n}\n# after %{_bindir}\n";
        assert_eq!(
            escaped(spec),
            "%{lua:\nlocal t = {1, 2}\n}\n# after %%{_bindir}\n"
        );
    }

    #[test]
    fn changelog_and_description_are_left_alone() {
        let spec = "%description\n# text %{_bindir}\n%changelog\n# log %{_bindir}\n* x\n";
        assert_eq!(escaped(spec), spec);
    }

    #[test]
    fn a_here_document_in_a_script_is_left_alone_until_its_terminator() {
        let spec = "%install\ncat <<'EOF' > x\n# kept %{_bindir}\nEOF\n# after %{_bindir}\n";
        assert_eq!(
            escaped(spec),
            "%install\ncat <<'EOF' > x\n# kept %{_bindir}\nEOF\n# after %%{_bindir}\n"
        );
    }

    #[test]
    fn a_dash_here_document_ends_at_an_indented_terminator() {
        let spec = "%build\ncat <<-END\n\t# kept %{x}\n\tEND\n# after %{x}\n";
        assert_eq!(
            escaped(spec),
            "%build\ncat <<-END\n\t# kept %{x}\n\tEND\n# after %%{x}\n"
        );
    }

    #[test]
    fn a_quoted_or_bare_here_document_word_is_read() {
        for open in ["<<EOF", "<<\"EOF\"", "<<'EOF'"] {
            let spec = format!("%install\ncat {open}\n# kept %{{x}}\nEOF\n");
            assert_eq!(escaped(&spec), spec, "{open}");
        }
    }

    #[test]
    fn a_here_document_is_not_opened_outside_a_script_section() {
        let spec = "%files\ncat <<EOF\n# out %{x}\n";
        assert_eq!(escaped(spec), "%files\ncat <<EOF\n# out %%{x}\n");
    }

    #[test]
    fn a_here_string_or_shift_opens_no_here_document() {
        let spec = "%build\necho <<< x\n# after %{x}\n";
        assert_eq!(escaped(spec), "%build\necho <<< x\n# after %%{x}\n");
        assert_eq!(heredoc_opened("x=$((1<<2))"), None);
    }

    #[test]
    fn the_pass_is_idempotent() {
        let spec = "# a %{_bindir} %%{x} %_libdir\n#%if 0\n%build\n# b %(x)\n";
        let once = escape_comment_macros(spec);
        let twice = escape_comment_macros(&once.0);
        assert_eq!(twice.0, once.0);
        assert!(twice.1.is_empty());
    }

    #[test]
    fn a_comment_only_change_leaves_the_code_the_same() {
        let before = "Name: a\n# old %{_bindir}\n%build\n";
        let after = "Name: a\n# new text\n%build\n";
        assert!(same_without_comments(before, after));
    }

    #[test]
    fn a_code_change_is_not_the_same_without_comments() {
        let before = "Name: a\n# c\n%build\n";
        assert!(!same_without_comments(before, "Name: b\n%build\n"));
        assert!(!same_without_comments(before, "Name: a\n%build\necho x\n"));
    }

    #[test]
    fn trailing_blank_lines_do_not_count() {
        assert!(same_without_comments(
            "Name: a\n%build\n",
            "Name: a\n%build\n\n\n"
        ));
        assert!(same_without_comments("Name: a\n# c\n\n", "Name: a\n"));
        assert!(!same_without_comments(
            "Name: a\n\n%build\n",
            "Name: a\n%build\n"
        ));
    }

    #[test]
    fn the_check_sees_a_macro_the_escape_skips_and_not_an_escaped_one() {
        assert!(has_macro("# 50% of %{_bindir}"));
        assert!(!has_macro("# 50%% of %%{_bindir}"));
        let (out, _) = escape_comment_macros("# 50% of %{_bindir}\n");
        assert!(!has_macro(out.trim_end()));
        assert_eq!(
            crate::spec::first_macro_in_comment("# 50% of %{_bindir}\n"),
            Some((1, "# 50% of %{_bindir}".to_string()))
        );
    }

    #[test]
    fn a_realistic_spec_gets_only_its_preamble_and_install_comments_escaped() {
        let spec = "\
Name:           jay
Version:        0.1.0
Release:        0
# Installs the crate's binaries into %{_bindir}
Summary:        A compositor

%description
# Not a comment, a description line about %{x}.

%prep
%autosetup
# Patches live in %{_sourcedir}
cat <<'EOF' > gen.sh
# generated %{_bindir}
EOF

%build
%cargo_build

%install
# Installs the crate's binaries into %{_bindir}
install -D target/release/jay %{buildroot}%{_bindir}/jay
echo $# %{_bindir}

%files
%{_bindir}/jay

%changelog
# old entry %{_bindir}
* Mon Jan 01 2026 Someone - 0.1.0-0
- Initial
";
        let expected = spec
            .replace(
                "# Installs the crate's binaries into %{_bindir}\nSummary",
                "# Installs the crate's binaries into %%{_bindir}\nSummary",
            )
            .replace(
                "# Patches live in %{_sourcedir}",
                "# Patches live in %%{_sourcedir}",
            )
            .replace(
                "# Installs the crate's binaries into %{_bindir}\ninstall",
                "# Installs the crate's binaries into %%{_bindir}\ninstall",
            );
        let (out, lines) = escape_comment_macros(spec);
        assert_eq!(out, expected);
        assert_eq!(lines, vec![4, 12, 21]);
        assert_eq!(escape_comment_macros(&out).0, out);
        assert!(same_without_comments(spec, &out));
    }
}
