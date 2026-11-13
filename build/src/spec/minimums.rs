//! The minimum versions upstream declares for a dependency, written on the
//! lines of an update that name the dependency without one (convention 75).
//!
//! The declarations are the source's `[build-system] requires` (a `BuildRequires`
//! takes these first) and its `[project] dependencies` (a `Requires`, and a
//! `BuildRequires` the build-system list does not name). Only the lines an update
//! adds or changes are written: a line the origin's spec has unchanged belongs to
//! the origin and is reported, not edited. A name the spec already bounds is left
//! alone, and so is a line that names more than one package.

use super::generate::runtime_dependencies;
use crate::osc::fuzzy_key;
use std::collections::HashSet;
use std::path::Path;
use std::sync::LazyLock;

/// The minimums one source declares, as `(name, ">= X")`, one list per side.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct UpstreamMinimums {
    /// From `[build-system] requires`.
    pub build: Vec<(String, String)>,
    /// From `[project] dependencies`.
    pub runtime: Vec<(String, String)>,
}

impl UpstreamMinimums {
    /// The minimums of `dir/pyproject.toml`; none when it is missing or does not parse.
    pub fn read(dir: &Path) -> Self {
        std::fs::read_to_string(dir.join("pyproject.toml"))
            .map(|text| Self::parse(&text))
            .unwrap_or_default()
    }

    /// The minimums declared in a pyproject.toml text.
    pub fn parse(text: &str) -> Self {
        let Ok(doc) = toml::from_str::<toml::Table>(text) else {
            return Self::default();
        };
        let list = |table: &str, key: &str| -> Vec<String> {
            doc.get(table)
                .and_then(|t| t.get(key))
                .and_then(toml::Value::as_array)
                .map(|items| {
                    items
                        .iter()
                        .filter_map(|v| v.as_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_default()
        };
        Self {
            build: minimums(&list("build-system", "requires")),
            runtime: minimums(&list("project", "dependencies")),
        }
    }
}

/// `(name, ">= X")` for each requirement that declares a minimum and applies on
/// Linux (the same reading the generator gives a release's `Requires-Dist`).
fn minimums(requirements: &[String]) -> Vec<(String, String)> {
    runtime_dependencies(requirements, "")
        .into_iter()
        .filter_map(|dep| dep.bound.map(|bound| (dep.name, bound)))
        .collect()
}

/// Which list a line's package is looked up in.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Tag {
    Build,
    Runtime,
}

/// The `Tag` and the value of a `BuildRequires:` or `Requires:` line.
fn tag_and_value(line: &str) -> Option<(Tag, &str)> {
    let line = line.trim();
    if let Some(value) = line.strip_prefix("BuildRequires:") {
        return Some((Tag::Build, value.trim()));
    }
    line.strip_prefix("Requires:")
        .map(|value| (Tag::Runtime, value.trim()))
}

/// The minimum upstream declares for `name` on the side `tag` names.
fn minimum_for<'a>(tag: Tag, up: &'a UpstreamMinimums, name: &str) -> Option<&'a str> {
    let find = |list: &'a [(String, String)]| {
        let key = fuzzy_key(name);
        list.iter()
            .find(|(n, _)| fuzzy_key(n) == key)
            .map(|(_, bound)| bound.as_str())
    };
    match tag {
        Tag::Build => find(&up.build).or_else(|| find(&up.runtime)),
        Tag::Runtime => find(&up.runtime),
    }
}

static MACRO_NAME: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(r"%\{python_module\s+([A-Za-z0-9][A-Za-z0-9._+-]*)([^}]*)\}")
        .expect("static pattern")
});
static PLAIN_NAME: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(r"(?:^|\s)python(?:\d+)?-([A-Za-z0-9][A-Za-z0-9._+-]*)(\s*[<>=!~]+\s*\S+)?")
        .expect("static pattern")
});
static ONLY_MACRO: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(r"^%\{python_module ([A-Za-z0-9][A-Za-z0-9._+-]*)\}$")
        .expect("static pattern")
});
static ONLY_PLAIN: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(r"^python(?:\d+)?-([A-Za-z0-9][A-Za-z0-9._+-]*)$").expect("static pattern")
});

/// The one package a line names, and whether it is written as the
/// `%{python_module NAME}` macro (else `python-NAME`). A line with anything else
/// on it, a bound included, names no single package and is `None`.
fn single_package(tag: Tag, value: &str) -> Option<(&str, bool)> {
    if let Some(caps) = ONLY_MACRO.captures(value) {
        return caps.get(1).map(|m| (m.as_str(), true));
    }
    if tag == Tag::Runtime {
        if let Some(caps) = ONLY_PLAIN.captures(value) {
            return caps.get(1).map(|m| (m.as_str(), false));
        }
    }
    None
}

/// The fuzzy keys of the names the spec bounds on a dependency line: a
/// `%{python_module NAME >= X}`, or a `Requires: python-NAME >= X`.
fn bounded_names(spec: &str) -> HashSet<String> {
    let mut out = HashSet::new();
    for line in spec.lines() {
        let Some((tag, value)) = tag_and_value(line) else {
            continue;
        };
        for caps in MACRO_NAME.captures_iter(value) {
            if !caps[2].trim().is_empty() {
                out.insert(fuzzy_key(&caps[1]));
            }
        }
        if tag == Tag::Runtime {
            for caps in PLAIN_NAME.captures_iter(value) {
                if caps.get(2).is_some() {
                    out.insert(fuzzy_key(&caps[1]));
                }
            }
        }
    }
    out
}

/// The lines of `origin`, trimmed, for a line of the spec to be matched against.
fn inherited_lines(origin: &str) -> HashSet<&str> {
    origin.lines().map(str::trim).collect()
}

/// `line` with `>= bound` added to its one package, keeping the line's tag and
/// spacing.
fn with_bound(line: &str, value: &str, name: &str, macro_form: bool, bound: &str) -> String {
    let colon = line.find(':').unwrap_or(0);
    let after = &line[colon + 1..];
    let head = &line[..line.len() - after.trim_start().len()];
    let written = if macro_form {
        format!("%{{python_module {name} {bound}}}")
    } else {
        format!("{}{name} {bound}", &value[..value.len() - name.len()])
    };
    format!("{head}{written}")
}

/// `spec` with the minimum upstream declares added to each dependency line that
/// the update added or changed and that has no bound yet. A line the origin has
/// unchanged is left as it is, as is a name the spec bounds anywhere.
pub fn add_upstream_minimums(spec: &str, origin: &str, up: &UpstreamMinimums) -> String {
    let inherited = inherited_lines(origin);
    let bounded = bounded_names(spec);
    let lines: Vec<String> = spec
        .lines()
        .map(|line| {
            if inherited.contains(line.trim()) {
                return line.to_string();
            }
            let Some((tag, value)) = tag_and_value(line) else {
                return line.to_string();
            };
            let Some((name, macro_form)) = single_package(tag, value) else {
                return line.to_string();
            };
            if bounded.contains(&fuzzy_key(name)) {
                return line.to_string();
            }
            match minimum_for(tag, up, name) {
                Some(bound) => with_bound(line, value, name, macro_form, bound),
                None => line.to_string(),
            }
        })
        .collect();
    lines.join("\n") + if spec.ends_with('\n') { "\n" } else { "" }
}

/// The unchanged dependency lines of `spec` that name a package upstream
/// declares a minimum for, while the line has no bound (`hatchling >= 1.27`).
/// They are not written: an origin's line is the origin's to change.
pub fn inherited_minimum_gaps(spec: &str, origin: &str, up: &UpstreamMinimums) -> Vec<String> {
    let inherited = inherited_lines(origin);
    let bounded = bounded_names(spec);
    spec.lines()
        .filter(|line| inherited.contains(line.trim()))
        .filter_map(|line| {
            let (tag, value) = tag_and_value(line)?;
            let (name, _) = single_package(tag, value)?;
            if bounded.contains(&fuzzy_key(name)) {
                return None;
            }
            let bound = minimum_for(tag, up, name)?;
            Some(format!("{name} {bound}"))
        })
        .collect()
}
