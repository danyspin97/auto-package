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
    /// The spec has no `%check`.
    NoCheck,
    /// Tests were dropped from `%check` to get the package to build.
    DroppedTests,
}

impl Kind {
    pub fn label(self) -> &'static str {
        match self {
            Kind::BuildBroken => "build",
            Kind::RpmlintError => "rpmlint-error",
            Kind::RpmlintWarning => "rpmlint-warning",
            Kind::SpecStyle => "style",
            Kind::NoCheck => "no-check",
            Kind::DroppedTests => "dropped-tests",
        }
    }

    /// Whether [`tidy`] fixes it without a build or a model.
    pub fn is_mechanical(self) -> bool {
        self == Kind::SpecStyle
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finding {
    pub kind: Kind,
    pub detail: String,
}

/// Every deterministic style pass, in the order the pipeline applies them.
pub fn tidy(spec: &str) -> String {
    let spec = spec_tidy::unwrap_python_expand_block(spec);
    let spec = spec_tidy::move_check_before_files(&spec);
    let spec = spec_tidy::add_noarch_to_pure_python(&spec);
    let spec = spec_tidy::use_version_macro(&spec);
    spec_tidy::drop_unneeded_wheel_build_requires(&spec)
}

type StylePass = fn(&str) -> String;

/// What is wrong with `spec` on its own: the style passes that would change
/// it, a missing `%check`, and tests the pipeline dropped.
pub fn audit_spec(spec: &str) -> Vec<Finding> {
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
    findings.iter().any(|f| !f.kind.is_mechanical())
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
    fn a_spec_with_a_check_has_no_missing_check_finding() {
        assert!(!audit_spec(GOOD).iter().any(|f| f.kind == Kind::NoCheck));
    }

    #[test]
    fn a_spec_without_check_is_reported() {
        let spec = GOOD.replace("%check\n%pytest\n\n", "");
        let f = audit_spec(&spec);
        assert!(f.iter().any(|f| f.kind == Kind::NoCheck));
    }

    #[test]
    fn check_after_files_is_a_style_finding_that_tidy_fixes() {
        let spec =
            "Name: x\nVersion: 1\n\n%install\ntrue\n\n%files\n/a\n\n%check\ntrue\n\n%changelog\n";
        let f = audit_spec(spec);
        assert!(f.iter().any(|f| f.kind == Kind::SpecStyle));
        let tidied = tidy(spec);
        assert!(
            !audit_spec(&tidied)
                .iter()
                .any(|f| f.detail.starts_with("%check after"))
        );
    }

    #[test]
    fn tidy_is_idempotent() {
        let spec =
            "Name: x\nVersion: 1\n\n%install\ntrue\n\n%files\n/a\n\n%check\ntrue\n\n%changelog\n";
        let once = tidy(spec);
        assert_eq!(tidy(&once), once);
    }

    #[test]
    fn dropped_tests_are_counted() {
        let spec = format!("{GOOD}\n# evidence: FAILED t.py::a\n# evidence: FAILED t.py::b\n");
        let f = audit_spec(&spec);
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
}
