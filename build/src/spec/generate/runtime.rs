//! The runtime dependencies of a generated spec and the normalized module names
//! they use.

use super::*;

/// A runtime dependency read from a release's `Requires-Dist`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeDep {
    pub name: String,
    /// `>= 1.2`, when the release names a lower bound.
    pub bound: Option<String>,
}

/// The dependencies a release needs on every install: its `Requires-Dist`
/// entries without a marker (an extra, a Python version or a platform narrows
/// an entry down, and which narrowing applies is a decision for the review).
///
/// Only a lower bound is kept: an exact pin (`==1.2`) and a compatible release
/// (`~=1.2`) become `>= 1.2`, and `<`, `<=` and `!=` are dropped, because a
/// rolling distro ships one release and an upper bound would make the package
/// unresolvable the day it moves on (convention 59). The review agent was
/// writing exactly this by hand on package after package.
pub fn runtime_dependencies(requires_dist: &[String], own_name: &str) -> Vec<RuntimeDep> {
    let own = crate::osc::fuzzy_key(own_name);
    let mut seen = std::collections::HashSet::new();
    let mut deps = Vec::new();
    for entry in requires_dist {
        let (spec, marker) = match entry.split_once(';') {
            Some((spec, marker)) => (spec, marker.trim()),
            None => (entry.as_str(), ""),
        };
        if !marker.is_empty() && !marker_is_always_true_on_linux(marker) {
            continue;
        }
        let spec = spec.trim();
        let Some(m) = leading_name_re().find(spec) else {
            continue;
        };
        let name = m.as_str().to_string();
        let key = crate::osc::fuzzy_key(&name);
        if key == own || !seen.insert(key) {
            continue;
        }
        // `[extra]` selects extras of the dependency; its own pins follow.
        let rest = spec[m.end()..].trim();
        let (extras, rest): (Vec<&str>, &str) = match (rest.starts_with('['), rest.find(']')) {
            (true, Some(close)) => (
                rest[1..close].split(',').map(str::trim).collect(),
                &rest[close + 1..],
            ),
            _ => (Vec::new(), rest),
        };
        let rest = rest.trim().trim_start_matches('(').trim_end_matches(')');
        // A CUDA component's version is NVIDIA's, not the wheel's, so the
        // wheel's bound does not carry over (`--nvidia-redist`).
        let is_cuda_component = crate::redist::component(&name).is_some();
        let bound = rest.split(',').map(str::trim).find_map(|clause| {
            if is_cuda_component {
                return None;
            }
            let version = ["===", "==", "~=", ">=", ">"]
                .iter()
                .find_map(|op| clause.strip_prefix(op))?
                .trim();
            let usable = !version.is_empty()
                && !version.contains('*')
                && version
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '!' | '+' | '-' | '_'));
            usable.then(|| format!(">= {version}"))
        });
        deps.push(RuntimeDep {
            name: recase_python_module_name(&name),
            bound,
        });
        // A required extra pulls in packages of its own (`psycopg[binary]` needs
        // `psycopg-binary`): the ones the project is known to need.
        for extra in extras {
            for package in extra_requirements(&name, extra) {
                if seen.insert(crate::osc::fuzzy_key(package)) {
                    deps.push(RuntimeDep {
                        name: recase_python_module_name(package),
                        bound: None,
                    });
                }
            }
        }
    }
    deps
}

/// Whether a PEP 508 marker is true on every Linux install: a single clause on
/// the platform that Linux satisfies (`sys_platform == "linux"`,
/// `platform_system != "Windows"`). Anything else (a Python version, an
/// architecture, an `extra`) is for the review to decide.
fn marker_is_always_true_on_linux(marker: &str) -> bool {
    let m: String = marker
        .chars()
        .filter(|c| !c.is_whitespace() && !matches!(c, '"' | '\'' | '(' | ')'))
        .collect();
    matches!(
        m.as_str(),
        "sys_platform==linux"
            | "platform_system==Linux"
            | "os_name==posix"
            | "sys_platform!=win32"
            | "sys_platform!=darwin"
            | "platform_system!=Windows"
            | "platform_system!=Darwin"
            | "os_name!=nt"
    )
}

/// The packages a well-known project's extra requires (`httpx[brotli]` needs
/// `brotli`). Only extras whose packages are known; an unknown extra adds
/// nothing, as it did before. Project and extra names compare normalised.
fn extra_requirements(project: &str, extra: &str) -> &'static [&'static str] {
    let norm = |s: &str| s.to_ascii_lowercase().replace(['_', '.'], "-");
    match (norm(project).as_str(), norm(extra).as_str()) {
        ("psycopg", "binary") => &["psycopg-binary"],
        ("psycopg", "c") => &["psycopg-c"],
        ("psycopg", "pool") => &["psycopg-pool"],
        ("httpx", "brotli") => &["brotli"],
        ("httpx", "zstd") => &["zstandard"],
        ("httpx", "socks") => &["socksio"],
        ("httpx", "http2") => &["h2"],
        ("httpx", "cli") => &["click", "pygments", "rich"],
        ("requests", "socks") => &["pysocks"],
        ("pydantic", "email") => &["email-validator"],
        ("pydantic", "timezone") => &["tzdata"],
        ("sqlalchemy", "asyncio") => &["greenlet"],
        ("redis", "hiredis") => &["hiredis"],
        _ => &[],
    }
}

/// Add the release's runtime dependencies as `Requires` (before
/// `%python_subpackages`, as plain `python-<name>` so a flavor requires its own
/// build) and as `BuildRequires` (the tests and `%check` import them). A
/// dependency the spec already names in either place is left as it is.
pub fn add_runtime_dependencies(spec: &str, requires_dist: &[String], own_name: &str) -> String {
    let deps = runtime_dependencies(requires_dist, own_name);
    if deps.is_empty() {
        return spec.to_string();
    }
    let names = |tag: &str| -> std::collections::HashSet<String> {
        spec.lines()
            .filter_map(|l| l.trim_start().strip_prefix(tag))
            .flat_map(|v| {
                v.replace(['{', '}'], " ")
                    .split_whitespace()
                    .map(|t| crate::osc::fuzzy_key(crate::osc::strip_rpm_prefix(t)))
                    .collect::<Vec<_>>()
            })
            .collect()
    };
    let (have_requires, have_build) = (names("Requires:"), names("BuildRequires:"));
    let mut out = spec.to_string();
    let mut requires_lines = String::new();
    for dep in &deps {
        // `--nvidia-redist`: a CUDA wheel is the package built from NVIDIA's
        // archive, which is not a Python package and has no flavors.
        let cuda = crate::redist::package_name(&dep.name);
        let key = crate::osc::fuzzy_key(cuda.as_deref().unwrap_or(&dep.name));
        let bound = dep
            .bound
            .as_deref()
            .map(|b| format!(" {b}"))
            .unwrap_or_default();
        if !have_requires.contains(&key) {
            let package = cuda
                .clone()
                .unwrap_or_else(|| format!("python-{}", dep.name));
            requires_lines.push_str(&format!("Requires:       {package}{bound}\n"));
        }
        if !have_build.contains(&key) {
            let package = cuda
                .clone()
                .unwrap_or_else(|| format!("%{{python_module {}{bound}}}", dep.name));
            if let Some(next) = add_build_requires(&out, &package) {
                out = next;
            }
        }
    }
    if !requires_lines.is_empty()
        && let Some(at) = out.find("%python_subpackages")
    {
        out.insert_str(at, &requires_lines);
    }
    out
}

/// The pip-installed form of a PyPI name: lowercase, `-`/`.` → `_`.
///
/// This is what ends up on disk: `MarkupSafe` installs as `markupsafe`,
/// `rich-toolkit` as `rich_toolkit`, and the dist-info directory always uses
/// this spelling.
pub fn normalized_module_name(pypi_name: &str) -> String {
    pypi_name.to_lowercase().replace(['-', '.'], "_")
}

/// Drop `BuildRequires` lines that repeat a package an earlier line already
/// required.
///
/// py2pack emits the PyPI build requirements and [`post_process`] then adds
/// `[build-system] requires` from the sdist, so anything declared in both
/// appears twice — the single most common edit in the attempt journals, and
/// once even a triplicate. A duplicate is the same package with the same
/// version constraint, or a bare line next to a constrained one (the
/// constrained line wins, whichever order the two appear in). Lines in
/// different `%if`/`%else` branches never duplicate each other, and a line
/// naming several packages (`BuildRequires: gcc make`) is left untouched
/// rather than risk dropping one of them.
pub fn dedupe_build_requires(spec: &str) -> String {
    use std::collections::HashMap;

    /// Conditional scope plus package name, the unit duplicates are found in.
    type Group = (Vec<u32>, String);

    /// Split `BuildRequires` text into tokens, keeping `%{...}` whole.
    fn tokens(text: &str) -> Vec<&str> {
        let mut out = Vec::new();
        let (mut start, mut depth) = (None, 0usize);
        for (i, c) in text.char_indices() {
            match c {
                '{' => depth += 1,
                '}' => depth = depth.saturating_sub(1),
                _ => {}
            }
            let sep = depth == 0 && (c.is_whitespace() || c == ',');
            match (sep, start) {
                (true, Some(s)) => {
                    out.push(&text[s..i]);
                    start = None;
                }
                (false, None) if !sep => start = Some(i),
                _ => {}
            }
        }
        if let Some(s) = start {
            out.push(&text[s..]);
        }
        out
    }

    /// `(name, constraint)` when `text` names exactly one package.
    fn single(text: &str) -> Option<(String, String)> {
        let toks = tokens(text);
        let is_op = |t: &str| !t.is_empty() && t.chars().all(|c| "<>=!".contains(c));
        let (name, constraint) = match toks.as_slice() {
            [one] => match one.find(['<', '>', '=', '!']) {
                Some(i) if i > 0 => (&one[..i], one[i..].to_string()),
                Some(_) => return None,
                None => (*one, String::new()),
            },
            [name, op, ver] if is_op(op) => (*name, format!("{op}{ver}")),
            _ => return None,
        };
        // Any other macro or a rich dependency is not something to compare.
        (!name.starts_with('%') && !name.starts_with('('))
            .then(|| (name.to_lowercase(), constraint))
    }

    /// The package a BuildRequires line is about and its constraint, keyed so
    /// `cmake` and `%{python_module cmake}` stay different packages.
    fn subject(line: &str) -> Option<(String, String)> {
        let rest = line.strip_prefix("BuildRequires:")?.trim();
        match rest.strip_prefix("%{python_module ") {
            Some(inner) => {
                let (name, constraint) = single(inner.strip_suffix('}')?)?;
                Some((format!("py:{name}"), constraint))
            }
            None => single(rest),
        }
    }

    // Each line's conditional scope: the path of branch ids it sits in.
    let lines: Vec<&str> = spec.lines().collect();
    let mut stack: Vec<u32> = Vec::new();
    let mut next_branch = 0u32;
    let mut groups: HashMap<Group, Vec<(usize, String)>> = HashMap::new();
    for (i, line) in lines.iter().enumerate() {
        let word = line.split_whitespace().next().unwrap_or("");
        match word {
            "%if" | "%ifarch" | "%ifnarch" | "%ifos" | "%ifnos" => {
                next_branch += 1;
                stack.push(next_branch);
            }
            "%else" | "%elif" | "%elifarch" | "%elifos" => {
                next_branch += 1;
                if let Some(top) = stack.last_mut() {
                    *top = next_branch;
                }
            }
            "%endif" => {
                stack.pop();
            }
            _ => {}
        }
        if let Some((name, constraint)) = subject(line) {
            groups
                .entry((stack.clone(), name))
                .or_default()
                .push((i, constraint));
        }
    }

    let mut drop = vec![false; lines.len()];
    for entries in groups.values() {
        let any_constrained = entries.iter().any(|(_, c)| !c.is_empty());
        let mut seen: Vec<&str> = Vec::new();
        for (i, constraint) in entries {
            if (constraint.is_empty() && any_constrained) || seen.contains(&constraint.as_str()) {
                drop[*i] = true;
            } else {
                seen.push(constraint);
            }
        }
    }

    let mut out = String::with_capacity(spec.len());
    for (i, line) in lines.iter().enumerate() {
        if drop[i] {
            continue;
        }
        out.push_str(line);
        out.push('\n');
    }
    if !spec.ends_with('\n') {
        out.pop();
    }
    out
}
