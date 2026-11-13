//! The audit as text: each package with its findings and its pre-pass
//! findings, then the totals.

use std::collections::BTreeMap;

use super::{Finding, Kind};

/// One package's findings. `pre_pass` is what the review of its unpublished
/// branch work found before the build (one line each): not findings of its
/// spec, but a package with one is not counted as without findings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackageAudit {
    pub pkg: String,
    pub findings: Vec<Finding>,
    pub pre_pass: Vec<String>,
}

/// The audit as text: a line per package with its findings, then totals.
pub fn report_lines(audits: &[PackageAudit]) -> Vec<String> {
    let mut out = Vec::new();
    let mut totals: BTreeMap<Kind, usize> = BTreeMap::new();
    let mut clean = 0;
    for a in audits {
        if a.findings.is_empty() && a.pre_pass.is_empty() {
            clean += 1;
            continue;
        }
        out.push(a.pkg.clone());
        for line in &a.pre_pass {
            out.push(format!("  branch: {line}"));
        }
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

    #[test]
    fn the_report_lists_findings_and_totals() {
        let audits = vec![
            PackageAudit {
                pkg: "a".into(),
                findings: vec![Finding {
                    kind: Kind::NoCheck,
                    detail: "the spec has no %check".into(),
                }],
                pre_pass: Vec::new(),
            },
            PackageAudit {
                pkg: "b".into(),
                findings: vec![],
                pre_pass: Vec::new(),
            },
        ];
        let lines = report_lines(&audits);
        assert_eq!(lines[0], "a");
        assert_eq!(lines[1], "  no-check: the spec has no %check");
        assert!(lines.contains(&"2 package(s) checked, 1 without findings".to_string()));
        assert!(lines.contains(&"  no-check: 1".to_string()));
    }

    #[test]
    fn a_package_with_a_pre_pass_finding_is_not_without_findings() {
        let audits = vec![PackageAudit {
            pkg: "c".into(),
            findings: vec![],
            pre_pass: vec!["changes.dependency-bullet: x".into()],
        }];
        let lines = report_lines(&audits);
        assert_eq!(lines[0], "c");
        assert_eq!(lines[1], "  branch: changes.dependency-bullet: x");
        assert!(lines.contains(&"1 package(s) checked, 0 without findings".to_string()));
    }
}
