//! Whether two versions of a spec differ only in their comments.
//!
//! A comment is a `#` line rpm reads as one (see `comment_macros`), except a
//! directive: OBS reads `#!BuildIgnore: foo` and `# nodebuginfo` as build
//! settings, so a change to one of those is a change to the build. Blank lines
//! at the end of a spec are not compared.

use super::comment_macros::comment_lines;

/// The words OBS reads from a `# word` line.
const DIRECTIVE_WORDS: &[&str] = &[
    "needsbinariesforbuild",
    "needsrootforbuild",
    "needssslcertforbuild",
    "norootforbuild",
    "nodebuginfo",
    "usedforbuild",
];

/// Whether `line` is an OBS or rpmbuild directive rather than a plain comment:
/// `#!` followed by a word (`#!BuildIgnore: foo`, `#!ForceMultiVersion`), or
/// `#` with optional blanks and one of [`DIRECTIVE_WORDS`], in any case.
pub fn is_directive_comment(line: &str) -> bool {
    let Some(rest) = line.trim_start().strip_prefix('#') else {
        return false;
    };
    if let Some(after_bang) = rest.strip_prefix('!') {
        return after_bang.starts_with(|c: char| c.is_ascii_alphabetic());
    }
    let rest = rest.trim_start_matches([' ', '\t']);
    let word_len = rest
        .find(|c: char| !c.is_ascii_alphanumeric())
        .unwrap_or(rest.len());
    let (word, tail) = rest.split_at(word_len);
    let word = word.to_ascii_lowercase();
    (tail.is_empty() || tail.starts_with(char::is_whitespace))
        && DIRECTIVE_WORDS.contains(&word.as_str())
}

/// The lines of `spec` that are comments: rpm's comment lines that are not
/// directives, as `(line, is_comment)` pairs over `split('\n')`.
fn classify(lines: &[&str]) -> Vec<bool> {
    comment_lines(lines)
        .into_iter()
        .zip(lines)
        .map(|(real, line)| real && !is_directive_comment(line))
        .collect()
}

/// The spec's lines that are not comments, with the blank lines at the end
/// dropped.
fn code(spec: &str) -> Vec<&str> {
    let lines: Vec<&str> = spec.split('\n').collect();
    let comments = classify(&lines);
    let mut code: Vec<&str> = lines
        .iter()
        .zip(&comments)
        .filter(|(_, comment)| !**comment)
        .map(|(line, _)| *line)
        .collect();
    while code.last().is_some_and(|l| l.trim().is_empty()) {
        code.pop();
    }
    code
}

/// The text of the comment lines of `spec`, in order, each with its blanks at
/// the ends dropped and every run of blanks inside it collapsed to one space.
fn comment_texts(spec: &str) -> Vec<String> {
    let lines: Vec<&str> = spec.split('\n').collect();
    let comments = classify(&lines);
    lines
        .iter()
        .zip(&comments)
        .filter(|(_, comment)| **comment)
        .map(|(line, _)| line.split_whitespace().collect::<Vec<_>>().join(" "))
        .collect()
}

/// Whether `a` and `b` are the same spec once every comment line and the blank
/// lines at the end are dropped. Line endings are compared as they are: a CRLF
/// spec is not the LF one.
pub fn same_without_comments(a: &str, b: &str) -> bool {
    code(a) == code(b)
}

/// Whether the comment lines of `a` and `b` differ: a line added or removed, or
/// one whose text changed (blanks at its ends, and runs of blanks inside it,
/// do not count).
pub fn comment_text_differs(a: &str, b: &str) -> bool {
    comment_texts(a) != comment_texts(b)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_comment_only_change_leaves_the_code_the_same() {
        let before = "Name: a\n# old %{_bindir}\n%build\n";
        let after = "Name: a\n# new text\n%build\n";
        assert!(same_without_comments(before, after));
        assert!(comment_text_differs(before, after));
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
    fn a_trailing_blank_or_final_newline_edit_is_not_a_comment_change() {
        let spec = "Name: a\n# c\n%build\n";
        assert!(same_without_comments(spec, "Name: a\n# c\n%build"));
        assert!(!comment_text_differs(spec, "Name: a\n# c\n%build\n\n\n"));
        assert!(!comment_text_differs(spec, "Name: a\n# c\n%build"));
    }

    #[test]
    fn a_whitespace_only_change_in_a_comment_is_not_a_comment_change() {
        assert!(!comment_text_differs("# c\nName: a\n", "#  c  \nName: a\n"));
        assert!(!comment_text_differs(
            "# c\r\nName: a\r\n",
            "# c\nName: a\n"
        ));
        assert!(comment_text_differs("# c\n", "# d\n"));
        assert!(comment_text_differs("# c\n", ""));
    }

    #[test]
    fn a_hash_line_in_a_changelog_or_description_is_text_not_a_comment() {
        assert!(!same_without_comments(
            "%changelog\n# x\n* a\n",
            "%changelog\n* a\n"
        ));
        assert!(!same_without_comments(
            "%changelog\n* a\n# b\n",
            "%changelog\n* a\n"
        ));
        assert!(!same_without_comments(
            "%description\n# x\n%build\n",
            "%description\n%build\n"
        ));
        assert!(same_without_comments(
            "%changelog\n* a\n",
            "%changelog\n* a\n\n"
        ));
    }

    #[test]
    fn a_hash_line_continuing_a_macro_or_here_document_is_code() {
        assert!(!same_without_comments(
            "%define x \\\n# a\n%global y 1\n",
            "%define x \\\n# b\n%global y 1\n"
        ));
        assert!(!same_without_comments(
            "%{lua:\n# a\n}\n",
            "%{lua:\n# b\n}\n"
        ));
        assert!(!same_without_comments(
            "%install\ncat <<EOF\n# a\nEOF\n",
            "%install\ncat <<EOF\n# b\nEOF\n"
        ));
        assert!(!same_without_comments(
            "%install\ncat << EOF\n#!/usr/bin/python3\nEOF\n",
            "%install\ncat << EOF\nEOF\n"
        ));
        assert!(!same_without_comments(
            "%build\nx=%(\n# a\n)\n",
            "%build\nx=%(\n# b\n)\n"
        ));
    }

    #[test]
    fn a_hash_after_code_or_a_conditional_is_not_a_comment_line() {
        assert!(!same_without_comments(
            "%define x a # b\n",
            "%define x a # c\n"
        ));
        assert!(!same_without_comments(
            "%{!?x:%define y 1}\n",
            "%{!?x:%define y 2}\n"
        ));
        assert!(same_without_comments(
            "%{!?x:%define y 1}\n",
            "%{!?x:%define y 1}\n# c\n"
        ));
    }

    #[test]
    fn line_endings_are_compared_as_they_are() {
        assert!(same_without_comments(
            "Name: a\r\n# c\r\n%build\r\n",
            "Name: a\r\n# d\r\n%build\r\n"
        ));
        assert!(!same_without_comments(
            "Name: a\r\n%build\r\n",
            "Name: a\n%build\n"
        ));
    }

    #[test]
    fn every_directive_form_is_code_not_a_comment() {
        for line in [
            "#!BuildIgnore: foo",
            "#!BuildConstraint: hardware:memory 8",
            "#!ForceMultiVersion",
            "#!CreateArchivePackage",
            "#!BuildTarget: x",
            "# needsrootforbuild",
            "# needssslcertforbuild",
            "# norootforbuild",
            "# nodebuginfo",
            "# needsbinariesforbuild",
            "# usedforbuild",
            "#NoDebugInfo",
            "#   NeedsRootForBuild",
        ] {
            assert!(is_directive_comment(line), "{line}");
            let added = format!("Name: a\n{line}\n%build\n");
            assert!(
                !same_without_comments("Name: a\n%build\n", &added),
                "{line}"
            );
            assert!(
                !same_without_comments(&added, "Name: a\n%build\n"),
                "{line}"
            );
        }
    }

    #[test]
    fn a_plain_comment_is_still_comment_only() {
        for line in [
            "# plain comment",
            "#!/usr/bin/python3",
            "# needs a root",
            "#norootforbuild-notes",
            "# the build is usedforbuilds",
        ] {
            assert!(!is_directive_comment(line), "{line}");
        }
        assert!(same_without_comments(
            "Name: a\n# plain\n%build\n",
            "Name: a\n# other\n%build\n"
        ));
        assert!(same_without_comments(
            "Name: a\n# ok\n%build\n",
            "Name: a\n#!/usr/bin/x\n%build\n"
        ));
    }
}
