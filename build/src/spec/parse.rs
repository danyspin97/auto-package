//! Reading a spec: its header tags and the build requirements in it.

use crate::spec::expand::*;

pub(super) fn tag_value<'a>(spec: &'a str, tag: &str) -> Option<&'a str> {
    spec.lines()
        .find(|l| l.starts_with(tag))
        .map(|l| l[tag.len()..].trim())
}

/// Minimal spec header: just the three fields the update sweep needs.
///
/// `source` is the first `Source:` or `Source0:` line verbatim, including any
/// `%{version}` or `%{name}` macros — callers detect the ecosystem from the
/// host, not from the expanded URL.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Header {
    pub name: String,
    pub version: String,
    pub source: Option<String>,
    /// The `Url:` tag, with the simple macros expanded.
    pub url: Option<String>,
    /// `source` with `%{name}`, `%{version}`, `%{url}` and the spec's simple
    /// defines expanded, and the `#/<file>` rename OBS reads off the end cut
    /// away: the address a download would use.
    pub expanded_source: Option<String>,
}

/// Parse `Name:`, `Version:`, and the first `Source:` / `Source0:` line out of
/// a spec file's text. Returns `None` when `Name:` or `Version:` is missing —
/// a spec without either is not something this tool should try to update.
///
/// `Version:` is run through [`expand_simple_defines`] so that AppCo-style
/// `Version: %{package_version}` resolves to the actual version string
/// rather than being passed to the update sweep's PEP 440 comparator as a
/// literal `%{package_version}` — which previously parsed as unparseable,
/// returned "not newer", and silently labelled the package `UpToDate` even
/// when upstream had moved on (Oct 6 review finding #3).
pub fn parse_header(spec: &str) -> Option<Header> {
    let name_raw = tag_value(spec, "Name:")?;
    let version_raw = tag_value(spec, "Version:")?;
    let defines = collect_simple_defines(spec);
    let name = expand_lookup_text(name_raw, &defines);
    let version = expand_simple_defines(version_raw, &defines);
    let source = tag_value(spec, "Source:")
        .or_else(|| tag_value(spec, "Source0:"))
        .map(str::to_string);
    let mut macros = defines;
    macros.insert("name".to_string(), name.clone());
    macros.insert("version".to_string(), version.clone());
    let url = tag_value(spec, "Url:")
        .or_else(|| tag_value(spec, "URL:"))
        .map(|u| expand_lookup_text(u, &macros));
    if let Some(u) = &url {
        macros.insert("url".to_string(), u.clone());
    }
    // Twice: a define may be spelled with another.
    let expanded_source = source.as_deref().map(|s| {
        let once = expand_lookup_text(s, &macros);
        let twice = expand_lookup_text(&once, &macros);
        twice.split('#').next().unwrap_or(&twice).trim().to_string()
    });
    Some(Header {
        name,
        version,
        source,
        url,
        expanded_source,
    })
}

/// What one `BuildRequires:` entry asks for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BuildRequire {
    /// A plain package name (`cmake`, `libfoo-devel`), version bound dropped.
    Package(String),
    /// Anything that is not a package name — `pkgconfig(foo)`, `/usr/bin/x`,
    /// `%{python_module foo}`: what provides it cannot be told from the name.
    Capability(String),
}

/// Split a dependency list at whitespace and commas outside `%{...}` and
/// `(...)`, so `%{python_module foo >= 1}` and `pkgconfig(a b)` stay whole.
fn split_dependency_tokens(list: &str) -> Vec<&str> {
    let mut tokens = Vec::new();
    let (mut depth, mut start) = (0usize, None);
    for (i, c) in list.char_indices() {
        match c {
            '{' | '(' => depth += 1,
            '}' | ')' => depth = depth.saturating_sub(1),
            c if depth == 0 && (c.is_whitespace() || c == ',') => {
                if let Some(s) = start.take() {
                    tokens.push(&list[s..i]);
                }
                continue;
            }
            _ => {}
        }
        start.get_or_insert(i);
    }
    if let Some(s) = start {
        tokens.push(&list[s..]);
    }
    tokens
}

/// A version bound a `BuildRequires:` entry states: `>= 5.5`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Bound {
    /// One of `>=`, `<=`, `=`, `==`, `<`, `>`.
    pub op: String,
    pub version: String,
}

/// One `BuildRequires:` entry and the bound it states, if any.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Requirement {
    pub dep: BuildRequire,
    pub bound: Option<Bound>,
}

const BOUND_OPS: [&str; 6] = [">=", "<=", "=", "==", "<", ">"];

/// Split `pkgconfig(x)>=1` into the name and its bound. An operator inside
/// `(...)` or `%{...}` is part of the name.
fn split_inline_bound(token: &str) -> (&str, Option<Bound>) {
    let mut depth = 0usize;
    for (i, c) in token.char_indices() {
        match c {
            '{' | '(' => depth += 1,
            '}' | ')' => depth = depth.saturating_sub(1),
            '<' | '>' | '=' if depth == 0 => {
                let rest = &token[i..];
                let op_len = rest
                    .chars()
                    .take_while(|c| matches!(c, '<' | '>' | '='))
                    .count();
                let version = &rest[op_len..];
                let bound = (!version.is_empty()).then(|| Bound {
                    op: rest[..op_len].to_string(),
                    version: version.to_string(),
                });
                return (&token[..i], bound);
            }
            _ => {}
        }
    }
    (token, None)
}

/// Every `BuildRequires:` entry of a spec, in file order, with the bound it
/// states, without duplicates. Conditionals are not evaluated: an entry under
/// `%if` is listed.
pub fn build_requirements(spec: &str) -> Vec<Requirement> {
    let mut out: Vec<Requirement> = Vec::new();
    for line in spec.lines() {
        let Some(rest) = line.trim_start().strip_prefix("BuildRequires") else {
            continue;
        };
        // `BuildRequires(pre):` and friends carry a qualifier before the colon.
        let rest = rest.trim_start();
        let rest = match rest.strip_prefix('(') {
            Some(q) => q.split_once(')').map(|(_, r)| r.trim_start()),
            None => Some(rest),
        };
        let Some(list) = rest.and_then(|r| r.strip_prefix(':')) else {
            continue;
        };
        let mut tokens = split_dependency_tokens(list).into_iter().peekable();
        while let Some(token) = tokens.next() {
            if BOUND_OPS.contains(&token) {
                // A stray operator: its version is skipped with it.
                tokens.next();
                continue;
            }
            if token.starts_with(['<', '>', '=']) {
                continue;
            }
            let (name, inline) = split_inline_bound(token);
            // A spaced bound (`foo >= 1.0`) follows the name as two tokens.
            let bound = match tokens.peek() {
                Some(op) if BOUND_OPS.contains(op) => {
                    let op = tokens.next().unwrap_or_default().to_string();
                    tokens.next().map(|version| Bound {
                        op,
                        version: version.to_string(),
                    })
                }
                _ => inline,
            };
            let dep = if !name.is_empty()
                && name
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '+' | '-'))
            {
                BuildRequire::Package(name.to_string())
            } else {
                BuildRequire::Capability(name.to_string())
            };
            let entry = Requirement { dep, bound };
            if !out.contains(&entry) {
                out.push(entry);
            }
        }
    }
    out
}

/// Every `BuildRequires:` entry of a spec, in file order, without duplicates.
/// Conditionals are not evaluated: an entry under `%if` is listed.
pub fn build_requires(spec: &str) -> Vec<BuildRequire> {
    let mut out: Vec<BuildRequire> = Vec::new();
    for r in build_requirements(spec) {
        if !out.contains(&r.dep) {
            out.push(r.dep);
        }
    }
    out
}
