//! The checks of `lint`: what is wrong with a package that is
//! already in a project, judged from its spec, its build result and the
//! rpmlint block of its build log.
//!
//! Nothing here touches OBS. The CLI fetches the inputs and acts on the
//! findings: the ones [`tidy`] can fix are committed as they are, the rest go
//! to the ordinary pipeline.

use std::collections::BTreeMap;

use crate::osc::BuildStatus;
use crate::spec_tidy;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Kind {
    /// The latest build did not succeed.
    BuildBroken,
    /// An rpmlint error in the latest build.
    RpmlintError,
    /// An rpmlint warning in the latest build.
    RpmlintWarning,
    /// A style slip [`tidy`] corrects.
    SpecStyle,
    /// An obsolete spec construct [`tidy`] removes: one finding per construct.
    SpecHygiene,
    /// A comment that repeats an earlier comment of the spec. Only reported.
    DuplicateComment,
    /// A comment that restates its line or the packaging tool.
    TrivialComment,
    /// The spec has no `%check`.
    NoCheck,
    /// Tests were dropped from `%check` to get the package to build.
    DroppedTests,
    /// An rpmlintrc line [`crate::rpmlintrc_lint::tidy`] removes.
    RpmlintrcStyle,
    /// An rpmlintrc line that convention 39 does not allow, or a filter
    /// without its reason.
    RpmlintrcHygiene,
    /// An rpmlintrc named `<name>.rpmlintrc`. Only reported: the name is in
    /// the spec's `Source:` line.
    RpmlintrcName,
    /// An rpmlintrc that no `Source:` line of the spec names: the publish
    /// plan leaves it out at commit. Only reported.
    RpmlintrcUnnamed,
    /// An upstream patch whose header or spec comment lacks its provenance
    /// (convention 111). Lint writes the header and reports the status; tidy
    /// commits it with no build and no `.changes` bullet.
    PatchProvenance,
    /// A carried patch whose upstream request is merged and in the packaged
    /// release: lint removes it only with the removal proof (the pipeline
    /// checks it); without the proof it stays with a warning.
    PatchUpstreamMerged,
    /// A carried patch whose open upstream request moved with different code.
    /// Only reported: lint never replaces a patch; `update` takes the request's
    /// revision for a patch that fails to apply.
    PatchUpstreamMoved,
    /// A carried patch whose upstream request is merged in a newer release.
    /// Only reported: the fix is a newer version.
    PatchUpstreamReleased,
    /// A carried patch whose upstream request was closed without a merge.
    /// Only reported: a maintainer decides.
    PatchUpstreamClosed,
    /// A carried patch whose upstream state could not be read, or cannot be
    /// compared. Only reported.
    PatchUpstreamNotChecked,
}

impl Kind {
    pub fn label(self) -> &'static str {
        match self {
            Kind::BuildBroken => "build",
            Kind::RpmlintError => "rpmlint-error",
            Kind::RpmlintWarning => "rpmlint-warning",
            Kind::SpecStyle => "style",
            Kind::SpecHygiene => "hygiene",
            Kind::DuplicateComment => "duplicate-comment",
            Kind::TrivialComment => "trivial-comment",
            Kind::NoCheck => "no-check",
            Kind::DroppedTests => "dropped-tests",
            Kind::RpmlintrcStyle => "rpmlintrc-style",
            Kind::RpmlintrcHygiene => "rpmlintrc",
            Kind::RpmlintrcName => "rpmlintrc-name",
            Kind::RpmlintrcUnnamed => "rpmlintrc-unnamed",
            Kind::PatchProvenance => "patch-provenance",
            Kind::PatchUpstreamMerged => "patch-upstream-merged",
            Kind::PatchUpstreamMoved => "patch-upstream-moved",
            Kind::PatchUpstreamReleased => "patch-upstream-released",
            Kind::PatchUpstreamClosed => "patch-upstream-closed",
            Kind::PatchUpstreamNotChecked => "patch-upstream-not-checked",
        }
    }

    /// Whether [`tidy`] fixes it without a build or a model.
    pub fn is_mechanical(self) -> bool {
        matches!(
            self,
            Kind::SpecStyle | Kind::SpecHygiene | Kind::RpmlintrcStyle | Kind::PatchProvenance
        )
    }

    /// Whether the finding is about a carried patch of an upstream request
    /// (see `patch_provenance`): the agent is not asked to act on it.
    pub fn is_provenance(self) -> bool {
        matches!(
            self,
            Kind::PatchProvenance
                | Kind::PatchUpstreamMerged
                | Kind::PatchUpstreamMoved
                | Kind::PatchUpstreamReleased
                | Kind::PatchUpstreamClosed
                | Kind::PatchUpstreamNotChecked
        )
    }

    /// Whether the finding is only reported: nothing fixes it by itself and
    /// it does not send the package to the pipeline.
    pub fn is_report_only(self) -> bool {
        matches!(
            self,
            Kind::DuplicateComment
                | Kind::TrivialComment
                | Kind::RpmlintrcName
                | Kind::RpmlintrcUnnamed
                | Kind::PatchUpstreamMoved
                | Kind::PatchUpstreamReleased
                | Kind::PatchUpstreamClosed
                | Kind::PatchUpstreamNotChecked
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finding {
    pub kind: Kind,
    pub detail: String,
}

/// Every deterministic fix of `lint`: the obsolete constructs first, so that a
/// `%clean` is gone before the `%check` it could be moved with, then the style
/// passes in the order the pipeline applies them. `ldconfig_macro` says whether the
/// target runs ldconfig from a file trigger (see `hygiene::ldconfig_macro_defined`).
pub fn tidy(spec: &str, ldconfig_macro: bool) -> String {
    style(&spec_tidy::hygiene::drop_obsolete_constructs(
        spec,
        ldconfig_macro,
    ))
}

/// The style passes of [`tidy`].
fn style(spec: &str) -> String {
    let spec = spec_tidy::unwrap_python_expand_block(spec);
    let spec = spec_tidy::move_check_before_files(&spec);
    let spec = spec_tidy::add_noarch_to_pure_python(&spec);
    let spec = spec_tidy::use_version_macro(&spec);
    spec_tidy::drop_unneeded_wheel_build_requires(&spec)
}

/// The changelog bullet of the commit that [`tidy`] makes of `spec`: the
/// style fixes as lint has always named them, the obsolete constructs, or
/// both in one bullet. Empty when `tidy` changes nothing.
pub fn tidy_bullet(spec: &str, ldconfig_macro: bool) -> &'static str {
    let hygiene = spec_tidy::hygiene::drop_obsolete_constructs(spec, ldconfig_macro);
    let style_fixed = style(&hygiene) != hygiene;
    match (hygiene != spec, style_fixed) {
        (true, true) => {
            "Tidy the spec: section order, noarch, %{version}; clean up obsolete spec constructs"
        }
        (true, false) => "Clean up obsolete spec constructs",
        (false, true) => "Tidy the spec: section order, noarch, %{version}",
        (false, false) => "",
    }
}

type StylePass = fn(&str) -> String;

/// What is wrong with `spec` on its own: the style passes that would change
/// it, a missing `%check`, and tests the pipeline dropped. The obsolete constructs
/// are judged for a target with `ldconfig_macro` or without it.
pub fn audit_spec(spec: &str, ldconfig_macro: bool) -> Vec<Finding> {
    let mut out = Vec::new();
    let passes: [(&str, StylePass); 5] = [
        (
            "%{python_expand #} wrapper",
            spec_tidy::unwrap_python_expand_block,
        ),
        (
            "%check after the scriptlets or %files",
            spec_tidy::move_check_before_files,
        ),
        (
            "pure Python without BuildArch: noarch",
            spec_tidy::add_noarch_to_pure_python,
        ),
        (
            "literal version instead of %{version}",
            spec_tidy::use_version_macro,
        ),
        (
            "wheel install with unused Python BuildRequires",
            spec_tidy::drop_unneeded_wheel_build_requires,
        ),
    ];
    for (what, pass) in passes {
        if pass(spec) != spec {
            out.push(Finding {
                kind: Kind::SpecStyle,
                detail: what.to_string(),
            });
        }
    }
    for c in spec_tidy::hygiene::constructs(spec, ldconfig_macro) {
        out.push(Finding {
            kind: Kind::SpecHygiene,
            detail: format!("line {}: {}; fix: {}", c.line, c.what, c.fix),
        });
    }
    out.extend(spec_tidy::duplicate_comments::findings(spec));
    if !spec.lines().any(|l| l.trim_end() == "%check") {
        out.push(Finding {
            kind: Kind::NoCheck,
            detail: "the spec has no %check".to_string(),
        });
    }
    let dropped = crate::deselect::dropped_count(spec);
    if dropped > 0 {
        out.push(Finding {
            kind: Kind::DroppedTests,
            detail: format!("{dropped} failing test(s) dropped from %check"),
        });
    }
    out
}

/// The build result as a finding, `None` when it is fine. A package still
/// building, or one that is disabled or excluded, is not a quality problem.
pub fn audit_status(status: &BuildStatus) -> Option<Finding> {
    match status {
        BuildStatus::Failed
        | BuildStatus::Unresolvable
        | BuildStatus::Broken
        | BuildStatus::Unknown(_) => Some(Finding {
            kind: Kind::BuildBroken,
            detail: format!("the latest build is {status}"),
        }),
        _ => None,
    }
}

/// The rpmlint lines of a build log, one finding per tag with how often it
/// was reported and the first line that reported it. Errors come first.
pub fn rpmlint_findings(log: &str) -> Vec<Finding> {
    // tag -> (is_error, count, first line)
    let mut tags: BTreeMap<String, (bool, usize, String)> = BTreeMap::new();
    for line in log.lines() {
        let Some((is_error, rest)) = [(true, ": E: "), (false, ": W: ")]
            .iter()
            .find_map(|(e, m)| line.split_once(m).map(|(_, r)| (*e, r)))
        else {
            continue;
        };
        let Some(tag) = rest.split_whitespace().next() else {
            continue;
        };
        // The log's own timestamp prefix (`[  123s] `) is not part of the line.
        let shown = line.rsplit_once("] ").map_or(line, |(_, l)| l).trim();
        let entry = tags
            .entry(tag.to_string())
            .or_insert((is_error, 0, shown.to_string()));
        entry.1 += 1;
    }
    let mut found: Vec<(bool, String, usize, String)> = tags
        .into_iter()
        .map(|(tag, (e, n, line))| (e, tag, n, line))
        .collect();
    found.sort_by_key(|(e, tag, ..)| (!*e, tag.clone()));
    found
        .into_iter()
        .map(|(is_error, tag, count, line)| Finding {
            kind: if is_error {
                Kind::RpmlintError
            } else {
                Kind::RpmlintWarning
            },
            detail: format!("{tag} x{count}: {line}"),
        })
        .collect()
}

/// Whether anything is left for the pipeline once [`tidy`] has run.
pub fn needs_pipeline(findings: &[Finding]) -> bool {
    findings
        .iter()
        .any(|f| !f.kind.is_mechanical() && !f.kind.is_report_only())
}

/// One package's findings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackageAudit {
    pub pkg: String,
    pub findings: Vec<Finding>,
}

/// The audit as text: a line per package with its findings, then totals.
pub fn report_lines(audits: &[PackageAudit]) -> Vec<String> {
    let mut out = Vec::new();
    let mut totals: BTreeMap<Kind, usize> = BTreeMap::new();
    let mut clean = 0;
    for a in audits {
        if a.findings.is_empty() {
            clean += 1;
            continue;
        }
        out.push(a.pkg.clone());
        for f in &a.findings {
            *totals.entry(f.kind).or_default() += 1;
            out.push(format!("  {}: {}", f.kind.label(), f.detail));
        }
    }
    out.push(format!(
        "{} package(s) checked, {clean} without findings",
        audits.len()
    ));
    for (kind, n) in totals {
        out.push(format!("  {}: {n}", kind.label()));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const GOOD: &str = "Name: python-foo\nVersion: 1.0\nSource: foo-%{version}.tar.gz\n\n%prep\n%autosetup\n\n%build\n%pyproject_wheel\n\n%install\n%pyproject_install\n\n%check\n%pytest\n\n%files\n%license LICENSE\n\n%changelog\n";

    #[test]
    fn a_repeated_comment_is_a_finding_that_tidy_does_not_fix() {
        let spec = "%check\n# the text tests need a system font installed\n:\n# the text tests need a system font installed\n";
        let found = audit_spec(spec, false);
        let f: Vec<_> = found
            .iter()
            .filter(|f| f.kind == Kind::DuplicateComment)
            .collect();
        assert_eq!(f.len(), 1);
        assert!(f[0].detail.starts_with("line 4: "), "{}", f[0].detail);
        assert!(!Kind::DuplicateComment.is_mechanical());
        assert!(Kind::DuplicateComment.is_report_only());
        let only: Vec<_> = found
            .into_iter()
            .filter(|f| f.kind == Kind::DuplicateComment)
            .collect();
        assert!(!needs_pipeline(&only));
    }

    #[test]
    fn a_spec_with_a_check_has_no_missing_check_finding() {
        assert!(
            !audit_spec(GOOD, true)
                .iter()
                .any(|f| f.kind == Kind::NoCheck)
        );
    }

    #[test]
    fn a_spec_without_check_is_reported() {
        let spec = GOOD.replace("%check\n%pytest\n\n", "");
        let f = audit_spec(&spec, true);
        assert!(f.iter().any(|f| f.kind == Kind::NoCheck));
    }

    #[test]
    fn check_after_files_is_a_style_finding_that_tidy_fixes() {
        let spec =
            "Name: x\nVersion: 1\n\n%install\ntrue\n\n%files\n/a\n\n%check\ntrue\n\n%changelog\n";
        let f = audit_spec(spec, true);
        assert!(f.iter().any(|f| f.kind == Kind::SpecStyle));
        let tidied = tidy(spec, true);
        assert!(
            !audit_spec(&tidied, true)
                .iter()
                .any(|f| f.detail.starts_with("%check after"))
        );
    }

    #[test]
    fn tidy_is_idempotent() {
        let spec =
            "Name: x\nVersion: 1\n\n%install\ntrue\n\n%files\n/a\n\n%check\ntrue\n\n%changelog\n";
        let once = tidy(spec, true);
        assert_eq!(tidy(&once, true), once);
    }

    #[test]
    fn dropped_tests_are_counted() {
        let spec = format!("{GOOD}\n# evidence: FAILED t.py::a\n# evidence: FAILED t.py::b\n");
        let f = audit_spec(&spec, true);
        let d = f.iter().find(|f| f.kind == Kind::DroppedTests).unwrap();
        assert!(d.detail.starts_with("2 "), "{}", d.detail);
    }

    #[test]
    fn a_failed_build_is_a_finding_a_building_one_is_not() {
        assert_eq!(
            audit_status(&BuildStatus::Failed).unwrap().kind,
            Kind::BuildBroken
        );
        assert!(audit_status(&BuildStatus::Unresolvable).is_some());
        assert!(audit_status(&BuildStatus::Succeeded).is_none());
        assert!(audit_status(&BuildStatus::Building).is_none());
        assert!(audit_status(&BuildStatus::Excluded).is_none());
    }

    #[test]
    fn rpmlint_lines_are_grouped_by_tag_with_errors_first() {
        let log = "\
[  10s] python-foo.noarch: W: no-documentation
[  10s] python-foo.noarch: W: no-documentation
[  10s] python-foo.noarch: E: devel-file-in-non-devel-package /usr/include/a.h
[  10s] python-foo.noarch: W: spelling-error Summary(en_US) x -> y
[  11s] unrelated line
";
        let f = rpmlint_findings(log);
        assert_eq!(f.len(), 3);
        assert_eq!(f[0].kind, Kind::RpmlintError);
        assert!(
            f[0].detail
                .starts_with("devel-file-in-non-devel-package x1")
        );
        assert_eq!(f[1].kind, Kind::RpmlintWarning);
        assert!(
            f[1].detail.starts_with("no-documentation x2"),
            "{}",
            f[1].detail
        );
        assert!(f[1].detail.contains("python-foo.noarch: W:"));
        assert!(!f[1].detail.contains("[  10s]"));
    }

    #[test]
    fn a_log_without_rpmlint_lines_has_no_findings() {
        assert!(rpmlint_findings("building...\nfinished\n").is_empty());
    }

    #[test]
    fn only_style_findings_are_mechanical() {
        let style = Finding {
            kind: Kind::SpecStyle,
            detail: String::new(),
        };
        let warn = Finding {
            kind: Kind::RpmlintWarning,
            detail: String::new(),
        };
        assert!(!needs_pipeline(std::slice::from_ref(&style)));
        assert!(needs_pipeline(&[style, warn]));
    }

    #[test]
    fn the_report_lists_findings_and_totals() {
        let audits = vec![
            PackageAudit {
                pkg: "a".into(),
                findings: vec![Finding {
                    kind: Kind::NoCheck,
                    detail: "the spec has no %check".into(),
                }],
            },
            PackageAudit {
                pkg: "b".into(),
                findings: vec![],
            },
        ];
        let lines = report_lines(&audits);
        assert_eq!(lines[0], "a");
        assert_eq!(lines[1], "  no-check: the spec has no %check");
        assert!(lines.contains(&"2 package(s) checked, 1 without findings".to_string()));
        assert!(lines.contains(&"  no-check: 1".to_string()));
    }

    /// The VulkanMemoryAllocator spec of X11:Wayland as it is in the project.
    const VMA: &str = include_str!("testdata/spec/vulkanmemoryallocator.spec");
    /// The hyprlang spec of X11:Wayland: `Group:` tags and a hand-written
    /// `%post`/`%postun` pair for libhyprlang2.
    const HYPRLANG: &str = include_str!("testdata/spec/hyprlang.spec");

    /// A spec with every obsolete construct `lint` removes, and a version to
    /// check that `lint` leaves it alone.
    const LEGACY: &str = "Name:           libfoo\nVersion:        1.2.3\nRelease:        0\n\
        Summary:        Foo\nLicense:        MIT\nGroup:          Development/Libraries/C\n\
        BuildRoot:      %{_tmppath}/%{name}\nURL:            https://example.org/foo\n\
        Source0:        %{name}-%{version}.tar.gz\nBuildRequires:  cmake\n\n%description\nFoo.\n\n\
        %package -n libfoo1\nSummary:        Library\nGroup:          System/Libraries\n\
        %description -n libfoo1\nFoo.\n\n%prep\n%autosetup\n\n%build\n%cmake\n%cmake_build\n\n\
        %install\nrm -rf %{buildroot}\n%cmake_install\n\n%check\n%ctest\n\n\
        %post   -n libfoo1 -p /sbin/ldconfig\n%postun -n libfoo1 -p /sbin/ldconfig\n\n\
        %clean\nrm -rf %{buildroot}\n\n%files\n%defattr(-,root,root)\n%license LICENSE\n\n\
        %files -n libfoo1\n%{_libdir}/libfoo.so.*\n\n%changelog\n";

    fn hygiene_findings(spec: &str) -> Vec<Finding> {
        audit_spec(spec, true)
            .into_iter()
            .filter(|f| f.kind == Kind::SpecHygiene)
            .collect()
    }

    #[test]
    fn each_obsolete_construct_is_one_hygiene_finding_with_its_line_and_fix() {
        let f = hygiene_findings(LEGACY);
        assert_eq!(f.len(), 7, "{f:#?}");
        assert!(f.iter().all(|f| f.detail.starts_with("line ")));
        assert!(f.iter().any(|f| f.detail.starts_with("line 6: ")
            && f.detail.contains("fix: delete the line `Group:")));
        assert!(!needs_pipeline(&audit_spec(LEGACY, true)));
    }

    #[test]
    fn lint_tidy_removes_the_obsolete_constructs_and_keeps_the_version() {
        let tidied = tidy(LEGACY, true);
        for gone in ["Group:", "BuildRoot:", "%clean", "%defattr", "rm -rf"] {
            assert!(!tidied.contains(gone), "{gone} is left:\n{tidied}");
        }
        assert!(!tidied.contains("ldconfig"), "{tidied}");
        assert!(tidied.contains("Version:        1.2.3\n"));
        assert!(hygiene_findings(&tidied).is_empty());
        assert_eq!(tidy(&tidied, true), tidied);
    }

    #[test]
    fn the_vulkan_group_line_is_the_only_change_tidy_makes() {
        let expected = VMA.replace("Group:          Development/Libraries/C and C++\n", "");
        assert_eq!(tidy(VMA, true), expected);
        assert_eq!(tidy(&expected, true), expected);
        assert!(hygiene_findings(VMA).len() == 1);
    }

    #[test]
    fn the_hyprlang_pair_and_its_group_tags_are_dropped() {
        let expected = HYPRLANG
            .replace("Group:          System/Libraries\n", "")
            .replace("Group:          Development/Libraries/Other\n", "")
            .replace(
                "%post   -n lib%{name}%{sover} -p /sbin/ldconfig\n\
                 %postun -n lib%{name}%{sover} -p /sbin/ldconfig\n\n",
                "",
            );
        assert_eq!(tidy(HYPRLANG, true), expected);
        assert_eq!(tidy(&expected, true), expected);
    }

    #[test]
    fn a_group_inside_a_conditional_is_left_and_not_a_finding() {
        let spec = "Name: x\nVersion: 1.2.3\n%if 0%{?suse_version} < 1315\nGroup: Other\n%endif\n\
                    %check\ntrue\n%files\n/a\n%changelog\n";
        assert!(hygiene_findings(spec).is_empty());
        assert_eq!(tidy(spec, true), spec);
    }

    #[test]
    fn a_hand_written_ldconfig_pair_is_fixed_only_for_a_target_with_the_macro() {
        let spec = "%post -p /sbin/ldconfig\n%postun -p /sbin/ldconfig\n%files\n/a\n";
        let hygiene = |ldconfig| {
            audit_spec(spec, ldconfig)
                .iter()
                .filter(|f| f.kind == Kind::SpecHygiene)
                .count()
        };
        assert_eq!(hygiene(true), 1);
        assert_eq!(hygiene(false), 0);
        assert_eq!(tidy(spec, false), spec);
        assert!(!tidy(spec, true).contains("ldconfig"));
        assert!(spec_tidy::hygiene::ldconfig_left(spec, false));
        assert!(!spec_tidy::hygiene::ldconfig_left(spec, true));
    }

    #[test]
    fn the_changelog_bullet_names_what_tidy_changed() {
        assert_eq!(
            tidy_bullet(LEGACY, true),
            "Clean up obsolete spec constructs"
        );
        assert_eq!(tidy_bullet(VMA, true), "Clean up obsolete spec constructs");
        let style =
            "Name: x\nVersion: 1\n\n%install\ntrue\n\n%files\n/a\n\n%check\ntrue\n\n%changelog\n";
        assert_eq!(
            tidy_bullet(style, true),
            "Tidy the spec: section order, noarch, %{version}"
        );
        let both = style.replace("Name: x\n", "Name: x\nGroup: Other\n");
        assert_eq!(
            tidy_bullet(&both, true),
            "Tidy the spec: section order, noarch, %{version}; clean up obsolete spec constructs"
        );
        assert_eq!(tidy_bullet(&tidy(&both, true), true), "");
        assert_eq!(tidy_bullet("Name: x\nVersion: 1\n%changelog\n", true), "");
    }
}
