//! The tidy pass that puts the licence header on a spec that has none.

use super::header::spec_header;

/// `spec` with the header of `package` in front when it has none. A spec
/// whose preamble already has a comment that names a copyright or the package is
/// returned as it is, whatever the holder or year; so is one that starts with
/// a `#!` line. The line endings and a byte order mark of the spec are kept.
pub fn add_spec_header(spec: &str, package: &str, year: i64) -> String {
    let (bom, body) = match spec.strip_prefix('\u{feff}') {
        Some(rest) => ("\u{feff}", rest),
        None => ("", spec),
    };
    let body = body.trim_start_matches(['\r', '\n']);
    if body.starts_with("#!") || has_header(body) {
        return spec.to_string();
    }
    let mut header = spec_header(package, year);
    if spec.contains("\r\n") {
        header = header.replace('\n', "\r\n");
    }
    format!("{bom}{header}{body}")
}

/// Section headers; the preamble ends at the first one.
const SECTIONS: [&str; 17] = [
    "%package",
    "%description",
    "%prep",
    "%build",
    "%install",
    "%check",
    "%files",
    "%changelog",
    "%pre",
    "%post",
    "%preun",
    "%postun",
    "%pretrans",
    "%posttrans",
    "%triggerin",
    "%generate_buildrequires",
    "%conf",
];

/// Whether the preamble of `body` carries a licence header. The preamble
/// runs up to the first tag or section line; comments, blank lines and
/// macro lines (`%define`, `%global`, `%bcond`, `%if` and the like, with
/// their continuation lines) may precede or follow the header in it.
fn has_header(body: &str) -> bool {
    let mut continued = false;
    for line in body.lines().map(str::trim) {
        let was_continued = std::mem::replace(&mut continued, line.ends_with('\\'));
        if was_continued || line.is_empty() {
            continue;
        }
        if line.starts_with('#') {
            if is_header_comment(line) {
                return true;
            }
        } else if !line.starts_with('%') || is_section(line) {
            return false;
        }
    }
    false
}

fn is_section(line: &str) -> bool {
    let word = line.split_whitespace().next().unwrap_or("");
    SECTIONS.contains(&word)
}

/// Whether a comment line names a copyright or the package of the spec.
fn is_header_comment(line: &str) -> bool {
    let l = line.to_lowercase();
    l.contains("copyright")
        || l.contains("(c)")
        || l.contains('\u{a9}')
        || l.contains("spec file for package")
}

#[cfg(test)]
mod tests {
    use super::*;

    const BODY: &str = "Name:           x\nVersion:        1\n";

    #[test]
    fn a_headerless_spec_gets_the_header_with_the_given_year() {
        let out = add_spec_header(BODY, "x", 2031);
        assert!(out.starts_with(
            "#\n# spec file for package x\n#\n# Copyright (c) 2031 SUSE LLC and contributors\n"
        ));
        assert!(
            out.contains("https://bugs.opensuse.org/\n#\n\n\nName:"),
            "{out}"
        );
        assert!(out.ends_with(BODY));
    }

    #[test]
    fn adding_twice_changes_nothing_more() {
        let once = add_spec_header(BODY, "x", 2031);
        assert_eq!(add_spec_header(&once, "x", 2040), once);
    }

    #[test]
    fn an_existing_header_is_kept_whatever_its_holder_or_year() {
        for holder in [
            "Copyright (c) 2019 Jane Doe",
            "Copyright (c) 2023 SUSE LLC and contributors",
            "Copyright (c) 2012 SUSE LINUX Products GmbH, Nuernberg, Germany.",
            "Copyright (C) 1999 Someone",
        ] {
            let spec = format!("#\n# spec file for package x\n#\n# {holder}\n#\n\n{BODY}");
            assert_eq!(add_spec_header(&spec, "x", 2031), spec);
        }
        let only_copyright = format!("# Copyright 2020 Acme\n\n{BODY}");
        assert_eq!(add_spec_header(&only_copyright, "x", 2031), only_copyright);
    }

    #[test]
    fn a_shebang_spec_is_left_alone() {
        let spec = format!("#!/bin/sh\n{BODY}");
        assert_eq!(add_spec_header(&spec, "x", 2031), spec);
    }

    #[test]
    fn leading_blank_lines_bom_and_crlf_are_handled() {
        let out = add_spec_header("\n\nName: x\n", "x", 2031);
        assert!(out.starts_with("#\n# spec file"));
        assert!(out.ends_with("\n\n\nName: x\n"));
        let out = add_spec_header("\u{feff}Name: x\n", "x", 2031);
        assert!(out.starts_with("\u{feff}#\n# spec file"));
        let crlf = add_spec_header("Name: x\r\nVersion: 1\r\n", "x", 2031);
        assert!(crlf.starts_with("#\r\n# spec file"));
        assert!(!crlf.replace("\r\n", "").contains('\n'), "{crlf:?}");
        assert_eq!(add_spec_header(&crlf, "x", 2040), crlf);
    }

    #[test]
    fn a_non_header_comment_does_not_count() {
        let out = add_spec_header("# norootforbuild\nName: x\n", "x", 2044);
        assert!(out.contains("Copyright (c) 2044 SUSE LLC and contributors"));
        assert!(out.ends_with("# norootforbuild\nName: x\n"));
    }

    #[test]
    fn a_header_after_macro_lines_is_found() {
        let header = "#\n# spec file for package x\n#\n# Copyright (c) 2020 SUSE LLC\n#\n\n";
        for pre in [
            "%define a 1\n",
            "# norootforbuild\n",
            "%bcond_with foo\n%bcond_without bar\n%bcond tests 1\n",
            "%global a b\\\n  c\n%if 0%{?suse_version}\n%define d 2\n%else\n%define d 3\n%endif\n",
        ] {
            let spec = format!("{pre}{header}{BODY}");
            assert_eq!(add_spec_header(&spec, "x", 2031), spec, "{pre}");
        }
    }

    #[test]
    fn other_copyright_marks_count() {
        for line in [
            "# Copyright (C) 1999 Someone",
            "# (c) 2001 Acme",
            "# \u{a9} 2020 Acme",
        ] {
            let spec = format!("%define a 1\n{line}\n\n{BODY}");
            assert_eq!(add_spec_header(&spec, "x", 2031), spec, "{line}");
        }
    }

    #[test]
    fn comments_after_the_first_tag_do_not_count() {
        let spec = "Name: x\n# Copyright 2020 Acme\n%description\n# (c) y\n";
        let out = add_spec_header(spec, "x", 2031);
        assert!(out.starts_with("#\n# spec file for package x\n"));
        assert!(out.ends_with(spec));
        let in_section = "%description\n# copyright\n";
        assert!(add_spec_header(in_section, "x", 2031).starts_with("#\n# spec file"));
    }
}
