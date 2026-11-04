//! The spec lint of the request: the findings of `quality::audit_spec` that
//! the new spec has and the base does not, so only what the request adds is
//! reported.

use super::{Finding, Severity, ids};
use crate::quality::{Kind, audit_spec};

/// A finding's detail without its `line N: ` prefix, and the line.
fn split_line(detail: &str) -> (Option<usize>, &str) {
    let Some(rest) = detail.strip_prefix("line ") else {
        return (None, detail);
    };
    match rest.split_once(": ") {
        Some((n, text)) if n.chars().all(|c| c.is_ascii_digit()) => (n.parse().ok(), text),
        _ => (None, detail),
    }
}

/// What identifies a finding across the two sides: its text with the digits
/// left out, since the lines it names move when the request adds one.
fn key(text: &str) -> String {
    text.chars().filter(|c| !c.is_ascii_digit()).collect()
}

/// The findings for the spec `file` of `package`. `base` is the target's
/// spec, `None` for a package the target does not have yet. `ldconfig_macro`
/// is whether the target defines `%ldconfig_scriptlets`.
pub fn spec_findings(
    package: &str,
    file: &str,
    base: Option<&str>,
    new: &str,
    ldconfig_macro: bool,
) -> Vec<Finding> {
    let at = |f: Finding| f.in_file(file, None);
    if crate::spec::parse_header(new).is_none() {
        return vec![at(Finding::check(
            ids::SPEC_UNPARSABLE,
            Severity::Blocker,
            package,
            &format!("{file} has no readable Name: and Version:"),
        ))];
    }
    let mut out = Vec::new();
    let unfinished = |s: &str| crate::osc::unfinished_spec_reason(s);
    if let Some(why) = unfinished(new).filter(|_| base.is_none_or(|b| unfinished(b).is_none())) {
        out.push(at(Finding::check(
            ids::SPEC_UNFINISHED,
            Severity::Blocker,
            package,
            &format!("{file} is an unfinished skeleton: {why}"),
        )));
    }
    if crate::spec::parse_header(new).is_some_and(|h| h.version.contains('%')) {
        out.push(at(Finding::check(
            ids::SPEC_VERSION_MACRO_UNEXPANDED,
            Severity::Warning,
            package,
            "Version: still holds a macro after expanding the spec's defines",
        )));
    }
    let new_found = audit_spec(new, ldconfig_macro);
    let base_found = base.map(|b| audit_spec(b, ldconfig_macro));
    let known = |kind: Kind, text: &str| {
        base_found.as_ref().is_some_and(|b| {
            b.iter()
                .any(|f| f.kind == kind && key(split_line(&f.detail).1) == key(text))
        })
    };
    for f in &new_found {
        let (line, text) = split_line(&f.detail);
        if known(f.kind, text) {
            continue;
        }
        let (id, severity) = match f.kind {
            Kind::SpecStyle => (ids::SPEC_STYLE, Severity::Note),
            Kind::SpecHygiene => (ids::SPEC_HYGIENE, Severity::Warning),
            // A package the target lacks has no %check to lose.
            Kind::NoCheck if base.is_some() => (ids::SPEC_NO_CHECK, Severity::Warning),
            _ => continue,
        };
        out.push(Finding::check(id, severity, package, text).in_file(file, line));
    }
    let dropped = |s: &str| crate::deselect::dropped_count(s);
    let (before, now) = (base.map_or(0, dropped), dropped(new));
    if now > before {
        out.push(at(Finding::check(
            ids::SPEC_DROPPED_TESTS,
            Severity::Warning,
            package,
            &format!("tests dropped from %check: {before} before, {now} now"),
        )));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const OLD: &str = include_str!("../testdata/request_review/cracklib_old.spec");
    const NEW: &str = include_str!("../testdata/request_review/cracklib_new.spec");

    fn ids_of(f: &[Finding]) -> Vec<&'static str> {
        f.iter().map(|f| f.id).collect()
    }

    #[test]
    fn a_finding_both_sides_have_is_not_reported() {
        // Whatever the old spec has (Group: in dict-small) stays quiet.
        let same = spec_findings("cracklib", "cracklib.spec", Some(OLD), OLD, true);
        assert!(same.is_empty(), "{same:?}");
        let request = spec_findings("cracklib", "cracklib.spec", Some(OLD), NEW, true);
        assert!(request.is_empty(), "{request:?}");
    }

    #[test]
    fn a_removed_check_section_is_a_warning() {
        let without = OLD.replace("\n%check\n", "\n%checkx\n");
        let found = spec_findings("p", "p.spec", Some(OLD), &without, true);
        assert!(ids_of(&found).contains(&ids::SPEC_NO_CHECK), "{found:?}");
        // Without a base there is no %check to lose.
        let found = spec_findings("p", "p.spec", None, &without, true);
        assert!(!ids_of(&found).contains(&ids::SPEC_NO_CHECK));
        // Both sides without one: not new.
        let found = spec_findings("p", "p.spec", Some(&without), &without, true);
        assert!(!ids_of(&found).contains(&ids::SPEC_NO_CHECK));
    }

    #[test]
    fn dropped_tests_are_counted_on_both_sides() {
        let with = |n: usize| {
            let mut s = OLD.replace("\n%check\n", "\n%check\n# evidence: first\n");
            for i in 1..n {
                s = s.replace(
                    "# evidence: first\n",
                    &format!("# evidence: first\n# evidence: e{i}\n"),
                );
            }
            s
        };
        let found = spec_findings("p", "p.spec", Some(&with(1)), &with(2), true);
        let d = found.iter().find(|f| f.id == ids::SPEC_DROPPED_TESTS);
        assert!(d.unwrap().message.contains("1 before, 2 now"), "{found:?}");
        let found = spec_findings("p", "p.spec", Some(&with(2)), &with(2), true);
        assert!(!ids_of(&found).contains(&ids::SPEC_DROPPED_TESTS));
    }

    #[test]
    fn a_todo_placeholder_is_an_unfinished_spec() {
        let skeleton = "Name: x\nVersion: 1\nRelease: 0\nSummary: s\nLicense: MIT\n\
                        \n%install\n# TODO: install\n\n%files\n%license LICENSE\n";
        let found = spec_findings("x", "x.spec", None, skeleton, true);
        assert!(ids_of(&found).contains(&ids::SPEC_UNFINISHED), "{found:?}");
        assert_eq!(found[0].severity, Severity::Blocker);
    }

    #[test]
    fn a_spec_without_name_and_version_is_unparsable() {
        let found = spec_findings("x", "x.spec", Some(OLD), "garbage\n", true);
        assert_eq!(ids_of(&found), [ids::SPEC_UNPARSABLE]);
    }

    #[test]
    fn a_hygiene_finding_carries_its_line_number() {
        let new = OLD.replace("%changelog", "%clean\nrm -rf %{buildroot}\n\n%changelog");
        let found = spec_findings("p", "p.spec", Some(OLD), &new, true);
        let h = found.iter().find(|f| f.id == ids::SPEC_HYGIENE);
        let h = h.unwrap_or_else(|| panic!("{found:?}"));
        assert!(h.line.is_some_and(|l| l > 1));
        assert_eq!(h.file.as_deref(), Some("p.spec"));
        assert!(!h.message.starts_with("line "));
    }

    #[test]
    fn a_version_that_keeps_a_macro_is_a_warning() {
        let new = NEW.replace("Version:        2.10.4", "Version:        %{unknown_ver}");
        let found = spec_findings("p", "p.spec", Some(OLD), &new, true);
        assert!(ids_of(&found).contains(&ids::SPEC_VERSION_MACRO_UNEXPANDED));
    }
}
