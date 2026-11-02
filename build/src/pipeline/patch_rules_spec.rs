//! The spec lines an edit adds that the patch rules refuse: a new `Source:` that
//! names a patch, a patch applied from `%prep` by `patch` or `git apply`, and a
//! `sed -i` in `%prep` in an edit that adds a patch. A `sed -i` that edits the spec
//! itself, or that substitutes a path or a `#!` line in a script, is a fix of the
//! spec or the build and is not refused. Also the macro expansion of a patch name.

use crate::pipeline::patch_check::*;
use crate::pipeline::patch_rules::*;

/// The spec sections; a line that opens one of them ends `%prep`.
const SPEC_SECTIONS: &[&str] = &[
    "prep",
    "conf",
    "generate_buildrequires",
    "build",
    "install",
    "check",
    "files",
    "changelog",
    "description",
    "package",
    "pre",
    "post",
    "preun",
    "postun",
    "pretrans",
    "posttrans",
    "verifyscript",
    "clean",
    "trigger",
    "triggerin",
    "triggerun",
    "filetriggerin",
];

/// Path prefixes and the `#!` of a script: a `sed -i` that substitutes one fixes the
/// build (a `/usr/bin/python` the script calls), it does not edit the sources.
const SED_PATH_MARKERS: &[&str] = &["/usr/", "/bin/", "/sbin/", "/etc/", "/opt/", "/lib", "#!"];

/// Whether a `sed -i` line of `%prep` may stay in an edit that adds a patch: it edits
/// the spec file itself, or it substitutes a path or a `#!` line.
fn sed_edit_is_allowed(line: &str) -> bool {
    line.contains(".spec") || SED_PATH_MARKERS.iter().any(|m| line.contains(m))
}

/// The problems the spec lines of an edit have: a new `Source:` that names a patch, a
/// patch applied from `%prep` by a shell command, and (in an edit that adds a patch) a
/// `sed -i` in `%prep` that does not fix a path or the spec itself.
pub(super) fn spec_route_problems(before: &str, after: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut old: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();
    for line in before.lines() {
        *old.entry(line.trim()).or_default() += 1;
    }
    let declared_before: std::collections::HashSet<String> = declared_patches(before)
        .iter()
        .map(|(_, raw, name)| patch_key(raw, name))
        .collect();
    let adds_patch = declared_patches(after)
        .iter()
        .any(|(_, raw, name)| !declared_before.contains(&patch_key(raw, name)));
    let mut in_prep = false;
    for line in after.lines() {
        let t = line.trim();
        if let Some(section) = line.strip_prefix('%') {
            let word = section.split_whitespace().next().unwrap_or("");
            if word == "prep" {
                in_prep = true;
            } else if SPEC_SECTIONS.contains(&word) {
                in_prep = false;
            }
        }
        if let Some(n) = old.get_mut(t)
            && *n > 0
        {
            *n -= 1;
            continue;
        }
        if t.starts_with('#') {
            continue;
        }
        if let Some(src) = crate::spec::parse_source_line(line, &["source"], str::to_string)
            && (src.url.ends_with(".patch") || src.url.ends_with(".diff"))
        {
            out.push(format!(
                "adds `Source: {}`, a patch: put it in a `PatchN:` line, which the patch rules check",
                src.url
            ));
        }
        if in_prep {
            let shell_patch = t
                .split(['|', ';', '&', '(', ')'])
                .map(str::trim_start)
                .any(|seg| seg == "patch" || seg.starts_with("patch "))
                || t.contains("%{__patch}")
                || t.contains("%__patch");
            if shell_patch {
                out.push(
                    "adds a `patch` command to %prep: put the patch in a `PatchN:` line, which \
                     the patch rules check"
                        .to_string(),
                );
            }
            let sed = t.contains("sed -i") || t.contains("sed --in-place");
            if sed && adds_patch && !sed_edit_is_allowed(t) {
                out.push(
                    "adds `sed -i` to %prep in an edit that adds a patch: put the change in the \
                     patch with a `PatchN:` line"
                        .to_string(),
                );
            }
            if t.contains("git apply") {
                out.push(
                    "adds `git apply` to %prep: put the patch in a `PatchN:` line, which the \
                     patch rules check"
                        .to_string(),
                );
            }
        }
    }
    out
}

/// A `PatchN:` name with the spec's macros expanded: `%{name}` and `%{version}` as
/// [`expand_patch_name`] does, then the `%global` and `%define` values of the spec
/// (`%global pkgver 1.2` expands `%{pkgver}`), a few rounds for a value that names
/// another macro. A macro with no value here stays, and the name is judged unresolved.
pub(super) fn expand_patch_macros(raw: &str, spec: &str) -> String {
    let mut name = expand_patch_name(raw, spec);
    let values: Vec<(String, String)> = spec
        .lines()
        .filter_map(|line| {
            let line = line.trim_start();
            let rest = line
                .strip_prefix("%global ")
                .or_else(|| line.strip_prefix("%define "))?;
            let (macro_name, value) = rest.trim().split_once(char::is_whitespace)?;
            let value = value.trim();
            (!value.contains("%(") && !value.contains('`'))
                .then(|| (macro_name.to_string(), value.to_string()))
        })
        .collect();
    for _ in 0..3 {
        for (macro_name, value) in &values {
            name = name.replace(&format!("%{{{macro_name}}}"), value);
        }
    }
    name
}
