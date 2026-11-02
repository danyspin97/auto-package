//! Whether a spec gets its vendor archive refreshed, and what the refresh needs
//! from the spec: the zig version that picks the layout, the cache directory
//! the build reads, and the archive's name and compression.
//!
//! A spec is refreshed only when all three hold: it requires zig, it has a
//! URL-less `vendor.tar.{zst,gz,xz}` source, and its build section reads a
//! cache made by `--global-cache-dir` or `--system`. Anything else is not
//! ours to refresh, so [`plan`] answers `None` for it.

use crate::spec::{BuildRequire, Requirement, build_requirements, collect_simple_defines};
use crate::spec::{expand_simple_defines, parse_header, parse_source_line};

use super::vendor_tar::{Compression, Layout};

/// The tags that end a spec's `%build` section.
const SECTIONS: &[&str] = &[
    "%prep",
    "%build",
    "%install",
    "%check",
    "%files",
    "%description",
    "%package",
    "%changelog",
    "%clean",
    "%pre",
    "%post",
    "%preun",
    "%postun",
    "%pretrans",
    "%posttrans",
    "%verifyscript",
    "%sourcelist",
    "%patchlist",
];

/// What a refresh of one spec writes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Plan {
    /// The vendor source's file name in the checkout, e.g. `vendor.tar.zst`.
    pub(super) file: String,
    pub(super) compression: Compression,
    /// The cache directory, relative to the build directory: `vendor`.
    pub(super) root: String,
    pub(super) layout: Layout,
}

/// The plan for `spec`, `None` when the spec is not one this step refreshes.
/// `Err` when it is one (the trigger holds) but its form is not supported, so
/// the package has to stop with the reason.
pub(super) fn plan(spec: &str) -> Result<Option<Plan>, String> {
    let requirements = build_requirements(spec);
    if !requirements.iter().any(|r| zig_name(&r.dep)) {
        return Ok(None);
    }
    let Some((file, compression)) = vendor_source(spec) else {
        return Ok(None);
    };
    let roots = cache_roots(&build_section(spec))?;
    let Some(root) = roots.first().cloned() else {
        return Ok(None);
    };
    if roots.iter().any(|r| *r != root) {
        return Err(format!(
            "the build reads two zig caches ({}); a vendor archive holds one",
            roots.join(", ")
        ));
    }
    let Some(layout) = zig_layout(&requirements)? else {
        return Ok(None);
    };
    Ok(Some(Plan {
        file,
        compression,
        root,
        layout,
    }))
}

/// The file name of the spec's `Source0`, macros expanded. `None` when it has
/// none, or its name still holds a macro that is not defined in the spec.
pub(super) fn source0_file(spec: &str) -> Option<String> {
    let macros = spec_macros(spec);
    let expand = |v: &str| {
        let once = expand_simple_defines(v, &macros);
        expand_simple_defines(&once, &macros)
    };
    spec.lines()
        .filter_map(|l| parse_source_line(l, &["source"], &expand))
        .find(|s| s.index == 0)
        .map(|s| s.file().to_string())
        .filter(|f| !f.is_empty() && !f.contains('%'))
}

/// Whether a requirement names zig: `zig`, or a Factory `zigX.Y`.
fn zig_name(dep: &BuildRequire) -> bool {
    let BuildRequire::Package(name) = dep else {
        return false;
    };
    name == "zig" || zig_version_of_name(name).is_some()
}

/// The version of a Factory name `zigX.Y` (`zig0.16`), `None` for any other.
fn zig_version_of_name(name: &str) -> Option<&str> {
    let v = name.strip_prefix("zig")?;
    (v.contains('.') && v.chars().all(|c| c.is_ascii_digit() || c == '.')).then_some(v)
}

/// The layout that the zig version a spec requires reads. `None` when the
/// spec requires no zig. `Err` when the version cannot be told, or two
/// requirements read different layouts.
fn zig_layout(requirements: &[Requirement]) -> Result<Option<Layout>, String> {
    let mut found: Option<Layout> = None;
    for req in requirements {
        if !zig_name(&req.dep) {
            continue;
        }
        let BuildRequire::Package(name) = &req.dep else {
            continue;
        };
        // `zigX.Y` is one version; a bare `zig` takes its version from its bound.
        let (version, exact) = match (zig_version_of_name(name), &req.bound) {
            (Some(v), _) => (v.to_string(), true),
            (None, Some(b)) if b.op == "=" || b.op == "==" => (b.version.clone(), true),
            (None, Some(b)) if b.op == ">=" => (b.version.clone(), false),
            _ => {
                return Err(
                    "the spec's zig requirement names no version to pick the vendor layout \
                     from; write it as `zig >= X.Y` or `zigX.Y`"
                        .to_string(),
                );
            }
        };
        let layout = Layout::for_zig(&version)?;
        if layout == Layout::Unpacked && !exact {
            return Err(format!(
                "`zig >= {version}` allows zig versions with two vendor layouts; \
                 pin the zig version to pick one"
            ));
        }
        match found {
            Some(prev) if prev != layout => {
                return Err(format!(
                    "the spec requires zig {version}, which reads a different vendor layout \
                     from another zig requirement of the spec"
                ));
            }
            Some(_) => {}
            None => found = Some(layout),
        }
    }
    Ok(found)
}

/// The URL-less `vendor.tar.{zst,gz,xz}` source of the spec: its name and its
/// compression. `None` when there is no such source, or more than one.
fn vendor_source(spec: &str) -> Option<(String, Compression)> {
    let macros = spec_macros(spec);
    let expand = |v: &str| {
        let once = expand_simple_defines(v, &macros);
        expand_simple_defines(&once, &macros)
    };
    let mut found: Vec<(String, Compression)> = Vec::new();
    for line in spec.lines() {
        let Some(source) = parse_source_line(line, &["source"], expand) else {
            continue;
        };
        if source.url.contains("://") {
            continue;
        }
        let compression = match source.url.as_str() {
            "vendor.tar.zst" => Compression::Zst,
            "vendor.tar.gz" => Compression::Gz,
            "vendor.tar.xz" => Compression::Xz,
            _ => continue,
        };
        found.push((source.url.clone(), compression));
    }
    match found.len() {
        1 => found.pop(),
        _ => None,
    }
}

/// The macros a spec's own text defines, with `name` and `version`.
fn spec_macros(spec: &str) -> std::collections::HashMap<String, String> {
    let mut macros = collect_simple_defines(spec);
    if let Some(header) = parse_header(spec) {
        macros.insert("name".to_string(), header.name);
        macros.insert("version".to_string(), header.version);
    }
    macros
}

/// The text of the spec's `%build` section, with the bodies of the macros
/// it uses added. A macro that carries the cache flag (`%global zig_opts
/// --global-cache-dir vendor/ …`) is part of the build too.
fn build_section(spec: &str) -> String {
    let mut out = String::new();
    let mut in_build = false;
    for line in spec.lines() {
        let first = line.split_whitespace().next().unwrap_or("");
        if first == "%build" {
            in_build = true;
            continue;
        }
        if in_build && SECTIONS.contains(&first) {
            break;
        }
        if in_build {
            out.push_str(line);
            out.push('\n');
        }
    }
    for line in spec.lines() {
        let mut words = line.split_whitespace();
        let (Some("%global" | "%define"), Some(name)) = (words.next(), words.next()) else {
            continue;
        };
        let body: Vec<&str> = words.collect();
        let body = body.join(" ");
        if out.contains(&format!("%{{{name}}}")) || out.contains(&format!("%{name}")) {
            out.push_str(&body);
            out.push('\n');
        }
    }
    out
}

/// The cache directories the build reads, one per distinct flag: the
/// `--global-cache-dir` argument, or the `--system` one with its `/p` removed.
/// Each is relative, and a `--system` path must sit in the build directory's
/// `%{name}-%{version}/` (what `%autosetup` unpacks into), so the archive's
/// top directory is its first component.
fn cache_roots(build: &str) -> Result<Vec<String>, String> {
    let words: Vec<&str> = build.split_whitespace().collect();
    let mut roots: Vec<String> = Vec::new();
    for (i, word) in words.iter().enumerate() {
        let Some(arg) = words.get(i + 1) else {
            continue;
        };
        let root = match *word {
            "--global-cache-dir" => {
                let dir = arg.trim_end_matches('/');
                if dir.is_empty() {
                    return Err("--global-cache-dir has no directory".to_string());
                }
                dir.to_string()
            }
            "--system" => {
                let rest = arg
                    .strip_prefix("%{_builddir}/%{name}-%{version}/")
                    .unwrap_or("");
                match rest.strip_suffix("/p") {
                    Some(root) if !root.is_empty() => root.to_string(),
                    _ => {
                        return Err(format!(
                            "--system {arg} is not a cache under %{{name}}-%{{version}}/ ending in /p"
                        ));
                    }
                }
            }
            _ => continue,
        };
        if root.starts_with('/')
            || root.contains('%')
            || root
                .split('/')
                .any(|p| p.is_empty() || p == "." || p == "..")
        {
            return Err(format!(
                "the zig cache `{root}` is not a plain relative directory"
            ));
        }
        if !roots.contains(&root) {
            roots.push(root);
        }
    }
    Ok(roots)
}
