//! The patches and the rpmlintrc of the request, judged with the pipeline's
//! own patch rules. Only what the request adds or changes is judged: a header
//! problem the old patch already had is not reported.

use std::collections::HashSet;

use regex::Regex;

use super::gather::Side;
use super::{Finding, Severity, ids};
use crate::pipeline::{
    declared_patches, header_problems, is_patch_comment, patch_delta, source_only_problems,
    spec_route_problems,
};

pub fn patch_findings(package: &str, base: &Side, new: &Side) -> Vec<Finding> {
    let Some(new_spec) = new.spec() else {
        return Vec::new();
    };
    let base_spec = base.spec().unwrap_or("");
    let mk = |id, sev, file: &str, line: Option<usize>, msg: &str| {
        Finding::check(id, sev, package, msg).in_file(file, line)
    };
    let spec_name = new.spec_name.as_deref().unwrap_or("the spec");
    let mut out = Vec::new();
    for problem in spec_route_problems(base_spec, new_spec) {
        out.push(mk(
            ids::PATCH_SPEC_ROUTE,
            Severity::Warning,
            spec_name,
            None,
            &problem,
        ));
    }
    let lines: Vec<&str> = new_spec.lines().collect();
    let declared_before = declared_patches(base_spec);
    let declared = declared_patches(new_spec);
    let was_declared = |raw: &str, name: &str| {
        declared_before
            .iter()
            .any(|(_, r, n)| r == raw || (!name.is_empty() && n == name))
    };
    for (index, raw, name) in &declared {
        let line = Some(index + 1);
        if name.is_empty() || name.contains('%') {
            if !was_declared(raw, name) {
                out.push(mk(
                    ids::PATCH_UNREADABLE,
                    Severity::Note,
                    spec_name,
                    line,
                    &format!("Patch: {raw} names a file that cannot be read from the spec"),
                ));
            }
            continue;
        }
        let new_text = new.texts.get(name);
        if !was_declared(raw, name) {
            if raw.contains("%{version}") || raw.contains("%version") {
                out.push(mk(
                    ids::PATCH_VERSION_IN_NAME,
                    Severity::Warning,
                    spec_name,
                    line,
                    &format!("Patch: {raw} has %{{version}} in its name"),
                ));
            }
            let commented = index
                .checked_sub(1)
                .and_then(|i| lines.get(i))
                .is_some_and(|l| is_patch_comment(l));
            if !commented {
                out.push(mk(
                    ids::PATCH_COMMENT,
                    Severity::Warning,
                    spec_name,
                    line,
                    &format!("Patch: {raw} has no `# PATCH-FIX-...` comment above it"),
                ));
            }
            if let Some(text) = new_text {
                for p in header_problems(text) {
                    out.push(mk(
                        ids::PATCH_HEADER,
                        Severity::Warning,
                        name,
                        None,
                        &format!("{name} {p}"),
                    ));
                }
                for p in source_only_problems(&patch_delta(None, text)) {
                    out.push(mk(
                        ids::PATCH_NOT_SOURCE_ONLY,
                        Severity::Warning,
                        name,
                        None,
                        &format!("{name} {p}"),
                    ));
                }
            }
        } else if let (Some(text), Some(old)) = (new_text, base.texts.get(name))
            && text != old
        {
            let had = header_problems(old);
            for p in header_problems(text)
                .into_iter()
                .filter(|p| !had.contains(p))
            {
                out.push(mk(
                    ids::PATCH_HEADER,
                    Severity::Warning,
                    name,
                    None,
                    &format!("{name} (changed) {p}"),
                ));
            }
            for p in source_only_problems(&patch_delta(Some(old), text)) {
                out.push(mk(
                    ids::PATCH_NOT_SOURCE_ONLY,
                    Severity::Warning,
                    name,
                    None,
                    &format!("{name} (changed) {p}"),
                ));
            }
        }
    }
    for (_, raw, name) in &declared_before {
        if !declared.iter().any(|(_, r, n)| r == raw || n == name) {
            out.push(mk(
                ids::PATCH_DROPPED,
                Severity::Note,
                spec_name,
                None,
                &format!("Patch: {raw} is no longer declared"),
            ));
        }
    }
    out.extend(rpmlintrc_findings(package, base, new));
    out
}

/// The added lines of the new or changed `*rpmlintrc` files.
fn rpmlintrc_findings(package: &str, base: &Side, new: &Side) -> Vec<Finding> {
    let filter = Regex::new(r#"^addFilter\(\s*(r?"[^"]*"|r?'[^']*')\s*\)\s*$"#).expect("fixed");
    let mut out = Vec::new();
    for (name, text) in new.texts.iter().filter(|(n, _)| n.ends_with("rpmlintrc")) {
        let old = base.texts.get(name).map(String::as_str).unwrap_or("");
        let before: HashSet<&str> = old.lines().map(str::trim).collect();
        let lines: Vec<&str> = text.lines().collect();
        for (at, raw) in lines.iter().enumerate() {
            let line = raw.trim();
            if line.is_empty() || line.starts_with('#') || before.contains(line) {
                continue;
            }
            let mk = |id, sev, msg: &str| {
                Finding::check(id, sev, package, msg).in_file(name, Some(at + 1))
            };
            if line.contains("setBadness(") {
                out.push(mk(
                    ids::RPMLINTRC_SET_BADNESS,
                    Severity::Blocker,
                    &format!("setBadness is never used (convention 39): `{line}`"),
                ));
            } else if !filter.is_match(line) {
                out.push(mk(
                    ids::RPMLINTRC_NON_FILTER,
                    Severity::Warning,
                    &format!("the rpmlintrc holds only addFilter lines (convention 39): `{line}`"),
                ));
            } else if !at
                .checked_sub(1)
                .and_then(|i| lines.get(i))
                .is_some_and(|l| l.trim_start().starts_with("# reason:"))
            {
                out.push(mk(
                    ids::RPMLINTRC_NO_REASON,
                    Severity::Warning,
                    &format!("a filter needs a `# reason:` comment line above it: `{line}`"),
                ));
            }
        }
    }
    out
}
