//! RPM spec generation via py2pack, with post-processing.
//!
//! py2pack handles the openSUSE macro conventions that are otherwise painful
//! to get right. We supply what it can't determine from PyPI alone: the
//! build-backend requirements from the sdist, the correct SPDX license
//! expression, and the upstream URL.

use std::process::{Command, Stdio};

use ap_ecosystem::pypi::{License, Release};
use ap_ecosystem::sdist::BuildSystem;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("could not run py2pack: {0}")]
    Launch(std::io::Error),

    #[error("py2pack exited {code}:\n{stderr}")]
    Failed { code: i32, stderr: String },

    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

/// Fix the `%autosetup -n` value in a spec to match the actual tarball directory.
///
/// `actual_top_level` is the top-level directory name from the sdist
/// (e.g. `MarkupSafe-3.0.3`), already extracted by the caller's single-pass
/// [`ap_ecosystem::sdist::inspect`] call. Returns the corrected spec, or `None`
/// if no correction was needed.
pub fn fix_autosetup_dir(spec: &str, actual_top_level: &str) -> Option<String> {
    let actual = actual_top_level;
    let name = tag_value(spec, "Name:")?;
    let version = tag_value(spec, "Version:")?;
    let expanded = autosetup_expected_dir(spec, name, version)?;

    if actual == expanded {
        return None;
    }

    // Replace the version in the actual dir with %{version} so the spec
    // stays correct across version bumps. e.g. markupsafe-3.0.3 → markupsafe-%{version}
    let macro_n = replace_version_component(actual, version);

    // Replace -n <wrong> with -n <macro_n> on the %autosetup line.
    // If there is no -n, append one.
    let fixed = spec
        .lines()
        .map(|line| {
            if !line.trim().starts_with("%autosetup") {
                return line.to_string();
            }
            let parts: Vec<&str> = line.split_whitespace().collect();
            let has_n = parts.windows(2).any(|w| w[0] == "-n");
            if has_n {
                let mut out = String::new();
                let mut iter = parts.iter().peekable();
                while let Some(&part) = iter.next() {
                    if !out.is_empty() {
                        out.push(' ');
                    }
                    if part == "-n" {
                        out.push_str("-n ");
                        out.push_str(&macro_n);
                        iter.next(); // skip old value
                    } else {
                        out.push_str(part);
                    }
                }
                out
            } else {
                format!("{line} -n {macro_n}")
            }
        })
        .collect::<Vec<_>>()
        .join("\n");

    // Preserve trailing newline if original had one.
    Some(if spec.ends_with('\n') {
        fixed + "\n"
    } else {
        fixed
    })
}

/// PyPI project names whose canonical (display) spelling is mixed-case, but
/// whose `[build-system] requires` entry upstream — and so, verbatim, the
/// generator's `%{python_module ...}` output — is routinely written all
/// lowercase. RPM expands `%{python_module <name>}` per-flavor by literal
/// string concatenation (`python313-<name>`), so the lowercase spelling
/// names a package that does not exist (`python313-cython` instead of
/// `python313-Cython`) and OBS reports it "nothing provides" — which looks
/// exactly like a genuinely missing dependency. Keys are lowercase; checked
/// case-insensitively against the name written in the spec.
///
/// This list isn't exhaustive — it's the set observed so far (see convention
/// in `agent.rs`). Extend it as new mismatches are found.
const KNOWN_MIXED_CASE_PYPI_NAMES: &[(&str, &str)] = &[
    ("pillow", "Pillow"),
    ("pyyaml", "PyYAML"),
    ("markupsafe", "MarkupSafe"),
    ("pyjwt", "PyJWT"),
    ("pyside6", "PySide6"),
    ("pyqt5", "PyQt5"),
    ("pymysql", "PyMySQL"),
    ("pynacl", "PyNaCl"),
    ("cython", "Cython"),
    ("pypika", "PyPika"),
];

/// Matches a `%{python_module ...}` token, capturing its inner text (the
/// name plus any version constraint). Shared by every function in this file
/// that needs to find or rewrite these tokens, so the pattern is defined
/// exactly once.
fn python_module_token_re() -> &'static regex::Regex {
    use std::sync::LazyLock;
    static RE: LazyLock<regex::Regex> =
        LazyLock::new(|| regex::Regex::new(r"%\{python_module ([^}]+)\}").expect("static pattern"));
    &RE
}

/// Matches the leading package-name token of a `%{python_module <name>
/// ...}` macro's inner text, stopping before any version-constraint
/// operator or trailing whitespace. Shared with [`rewrite_python_module_name`].
fn leading_name_re() -> &'static regex::Regex {
    use std::sync::LazyLock;
    static NAME: LazyLock<regex::Regex> = LazyLock::new(|| {
        regex::Regex::new(r"^[A-Za-z0-9][A-Za-z0-9._+-]*").expect("static pattern")
    });
    &NAME
}

/// Re-case the leading package name of a `%{python_module <name> ...}`
/// token's inner text to its canonical PyPI spelling, when known. Leaves
/// anything not in [`KNOWN_MIXED_CASE_PYPI_NAMES`] untouched — case
/// mismatches outside this list still rely on the agent catching them (see
/// convention in `agent.rs`).
fn recase_python_module_name(inner: &str) -> String {
    let Some(m) = leading_name_re().find(inner) else {
        return inner.to_string();
    };
    let name = m.as_str();
    match KNOWN_MIXED_CASE_PYPI_NAMES
        .iter()
        .find(|(lower, _)| name.eq_ignore_ascii_case(lower))
    {
        Some((_, canonical)) if *canonical != name => {
            format!("{canonical}{}", &inner[m.end()..])
        }
        _ => inner.to_string(),
    }
}

/// Normalize `%{python_module ...}` tokens.
///
/// Adds spaces around `>=`, `<=`, `>`, `<`, `==`, `!=` that are missing them,
/// e.g. `setuptools>=77` → `setuptools >= 77`, and re-cases the package name
/// to its canonical PyPI spelling when it's a known mixed-case name written
/// lowercase (see [`recase_python_module_name`]).
pub fn normalize_python_module_versions(spec: &str) -> String {
    use std::sync::LazyLock;
    static OP: LazyLock<regex::Regex> =
        LazyLock::new(|| regex::Regex::new(r"\s*(>=|<=|!=|==|>|<)\s*").expect("static pattern"));

    python_module_token_re()
        .replace_all(spec, |caps: &regex::Captures| {
            let inner = &caps[1];
            let normalized = OP.replace_all(inner, " $1 ");
            let recased = recase_python_module_name(&normalized);
            format!("%{{python_module {}}}", recased)
        })
        .into_owned()
}

/// Rewrite the name inside every `%{python_module <name> ...}` token whose
/// name matches `from` under [`crate::osc::fuzzy_key`] (separator- and
/// case-insensitive) to `to`, keeping any version constraint untouched.
///
/// This closes a real stuck loop: OBS reports a dep by its PyPI
/// `requires-dist` spelling verbatim (`huggingface_hub`), the one-off
/// `TranslateUnresolvableDeps` turn correctly identifies the upstream
/// project as `huggingface-hub` (the spelling the dependency actually gets
/// packaged and published under — RPM package names use `-`, not `_`), and
/// that translated name is queued and built successfully. But unless this
/// package's own `%{python_module huggingface_hub}` line is rewritten to
/// match, it keeps asking OBS for `python313-huggingface_hub` forever — a
/// name nothing will ever provide — even though the dependency it actually
/// needs now exists under a different spelling. `already_reviewed_unresolvable`
/// then skips re-asking the agent, so without this rewrite nothing ever
/// revisits the spec and the package is stuck until it hits the retry limit
/// and gets marked permanently failed.
pub fn rewrite_python_module_name(spec: &str, from: &str, to: &str) -> String {
    if from == to {
        return spec.to_string();
    }
    let from_key = crate::osc::fuzzy_key(from);

    python_module_token_re()
        .replace_all(spec, |caps: &regex::Captures| {
            let inner = &caps[1];
            let Some(m) = leading_name_re().find(inner) else {
                return format!("%{{python_module {inner}}}");
            };
            let name = m.as_str();
            if crate::osc::fuzzy_key(name) == from_key {
                format!("%{{python_module {to}{}}}", &inner[m.end()..])
            } else {
                format!("%{{python_module {inner}}}")
            }
        })
        .into_owned()
}

/// Rewrite a dependency name in every dependency line of the spec
/// (`BuildRequires`, `Requires`, `Recommends`, ...) to `to`, wherever its
/// `fuzzy_key` matches `from`: inside `%{python_module ...}`, and as a plain
/// `python-<name>` / `python3-<name>` / `<name>`. The prefix a token carries is
/// kept and a version constraint is untouched.
///
/// [`rewrite_python_module_name`] alone left a runtime `Requires:
/// python-huggingface_hub` behind when it respelled the BuildRequires, so the
/// package built and then installed with a name nothing provides.
pub fn rewrite_dependency_name(spec: &str, from: &str, to: &str) -> String {
    use std::sync::LazyLock;
    static TAG: LazyLock<regex::Regex> = LazyLock::new(|| {
        regex::Regex::new(
            r"(?i)^\s*(?:BuildRequires|Requires|Recommends|Suggests|Supplements|Enhances|Conflicts|BuildConflicts)(?:\([^)]*\))?\s*:",
        )
        .expect("static pattern")
    });
    static TOKEN: LazyLock<regex::Regex> = LazyLock::new(|| {
        regex::Regex::new(r"[A-Za-z0-9][A-Za-z0-9._+-]*").expect("static pattern")
    });
    if from == to {
        return spec.to_string();
    }
    let from_key = crate::osc::fuzzy_key(from);
    spec.split_inclusive('\n')
        .map(|line| {
            if !TAG.is_match(line) {
                return line.to_string();
            }
            let line = rewrite_python_module_name(line, from, to);
            TOKEN
                .replace_all(&line, |caps: &regex::Captures| {
                    let token = &caps[0];
                    let bare = crate::osc::strip_rpm_prefix(token);
                    if bare != to && crate::osc::fuzzy_key(bare) == from_key {
                        format!("{}{to}", &token[..token.len() - bare.len()])
                    } else {
                        token.to_string()
                    }
                })
                .into_owned()
        })
        .collect()
}

/// Fix architecture-related lines when a package has C extensions.
///
/// - Removes `BuildArch: noarch`
/// - Replaces `%{python_sitelib}` with `%{python_sitearch}`
/// - Adds `BuildRequires: %{python_module devel}` (Python headers) if absent
/// - Inserts `export CFLAGS="%{optflags}"` before `%pyproject_wheel`
pub fn fix_c_extension_arch(spec: &str) -> String {
    let has_devel = spec.contains("%{python_module devel}");

    let mut out: Vec<String> = spec
        .lines()
        .filter(|l| !l.trim().starts_with("BuildArch:"))
        .map(|l| l.replace("%{python_sitelib}", "%{python_sitearch}"))
        .collect();

    // Inject devel after the pip BuildRequires line.
    if !has_devel && let Some(pos) = out.iter().position(|l| l.contains("%{python_module pip}")) {
        out.insert(
            pos + 1,
            "BuildRequires:  %{python_module devel}".to_string(),
        );
    }

    // Insert CFLAGS before %pyproject_wheel so the compiler uses RPM's flags.
    if let Some(pos) = out.iter().position(|l| l.trim() == "%pyproject_wheel")
        && !out[..pos].iter().any(|l| l.contains("CFLAGS"))
    {
        out.insert(pos, r#"export CFLAGS="%{optflags}""#.to_string());
    }

    out.join("\n") + if spec.ends_with('\n') { "\n" } else { "" }
}

/// Prefix a bare `%{$python...}` dynamic-macro reference with
/// `%python_expand` when it's missing one.
///
/// `%{$python_sitelib}`/`%{$python_sitearch}` only resolve correctly inside
/// a `%python_expand` block, which binds `$python` per flavor — outside one
/// they silently expand to `%{}` instead of failing loudly, so the bug only
/// surfaces at actual install/build time (observed on python-nvidia-nvtx: a
/// bare `chmod %{$python_sitelib}/...` line). Every real usage in this
/// ecosystem's specs is single-line (`%python_expand <command...>`), so
/// prefixing the whole line is sufficient; comment lines are left alone, and so
/// are continuation lines (after a trailing `\`), which already belong to the
/// command their first line started.
pub fn fix_stray_dollar_macros(spec: &str) -> String {
    let mut continued = false;
    let out: Vec<String> = spec
        .lines()
        .map(|line| {
            let is_continuation =
                std::mem::replace(&mut continued, line.trim_end().ends_with('\\'));
            let trimmed = line.trim_start();
            if is_continuation
                || trimmed.starts_with('#')
                || trimmed.contains("%python_expand")
                || !trimmed.contains("%{$")
            {
                return line.to_string();
            }
            let indent = &line[..line.len() - trimmed.len()];
            format!("{indent}%python_expand {trimmed}")
        })
        .collect();
    out.join("\n") + if spec.ends_with('\n') { "\n" } else { "" }
}

/// Put the parentheses RPM needs around a conditional dependency.
///
/// `Requires:  python-tomli >= 2.0.0 if python-base < 3.11` is not a
/// conditional dependency: without parentheses RPM reads three requirements, and
/// OBS then reports `nothing provides if` (python-fastapi-cli never built). The
/// valid form is `(python-tomli >= 2.0.0 if python-base < 3.11)`. A `%{...}`
/// macro item (`%{python_module x if %python-base < 3.11}`) brings its own and is
/// left alone, as is an item already in parentheses.
pub fn parenthesize_conditional_dependencies(spec: &str) -> String {
    const TAGS: [&str; 6] = [
        "Requires",
        "BuildRequires",
        "Recommends",
        "Suggests",
        "Supplements",
        "Enhances",
    ];
    let out: Vec<String> = spec
        .lines()
        .map(|line| {
            let t = line.trim_start();
            let is_dep_line = TAGS.iter().any(|tag| {
                t.strip_prefix(tag)
                    .is_some_and(|r| r.starts_with(':') || r.starts_with('('))
            });
            let Some((head, value)) = line.split_once(':').filter(|_| is_dep_line) else {
                return line.to_string();
            };
            if !value.contains(" if ") && !value.contains(" unless ") {
                return line.to_string();
            }
            let items: Vec<String> = value
                .split(',')
                .map(|item| {
                    let trimmed = item.trim();
                    let conditional = trimmed.contains(" if ") || trimmed.contains(" unless ");
                    if conditional && !trimmed.starts_with('(') && !trimmed.starts_with("%{") {
                        format!("({trimmed})")
                    } else {
                        trimmed.to_string()
                    }
                })
                .collect();
            let pad = &value[..value.len() - value.trim_start().len()];
            format!("{head}:{pad}{}", items.join(", "))
        })
        .collect();
    out.join("\n") + if spec.ends_with('\n') { "\n" } else { "" }
}

/// Add `Requires(post)`/`Requires(postun): update-alternatives` when the
/// spec registers an alternative but doesn't already declare them.
///
/// `%python_install_alternative`/`%python_uninstall_alternative` expand into
/// `update-alternatives` calls inside `%post`/`%postun` — `update-alternatives`
/// must already be installed for those scriptlets to run, which RPM only
/// guarantees via an explicit `Requires(post)`/`Requires(postun)`. Missing
/// either is silent at generation and review time and only surfaces as
/// `update-alternatives: command not found` once the scriptlets actually run
/// (observed on python-triton). Inserted right before `%python_subpackages`,
/// matching where a correctly-written spec of this shape already puts them.
pub fn fix_missing_alternatives_requires(spec: &str) -> String {
    let needs_it = spec.contains("%python_install_alternative")
        || spec.contains("%python_uninstall_alternative");
    if !needs_it {
        return spec.to_string();
    }
    let has_post = spec.contains("Requires(post):") && spec.contains("update-alternatives");
    let has_postun = spec.contains("Requires(postun):") && spec.contains("update-alternatives");
    if has_post && has_postun {
        return spec.to_string();
    }
    let Some(pos) = spec.lines().position(|l| l.trim() == "%python_subpackages") else {
        return spec.to_string();
    };
    let mut out: Vec<&str> = spec.lines().collect();
    let mut insert = Vec::new();
    if !has_post {
        insert.push("Requires(post):   update-alternatives");
    }
    if !has_postun {
        insert.push("Requires(postun): update-alternatives");
    }
    for (i, line) in insert.into_iter().enumerate() {
        out.insert(pos + i, line);
    }
    out.join("\n") + if spec.ends_with('\n') { "\n" } else { "" }
}

fn tag_value<'a>(spec: &'a str, tag: &str) -> Option<&'a str> {
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
    let name = expand_simple_defines(name_raw, &defines);
    let version = expand_simple_defines(version_raw, &defines);
    let source = tag_value(spec, "Source:")
        .or_else(|| tag_value(spec, "Source0:"))
        .map(str::to_string);
    Some(Header {
        name,
        version,
        source,
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

/// Every `BuildRequires:` entry of a spec, in file order, without duplicates.
/// Conditionals are not evaluated: an entry under `%if` is listed.
pub fn build_requires(spec: &str) -> Vec<BuildRequire> {
    let mut out: Vec<BuildRequire> = Vec::new();
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
        let mut tokens = split_dependency_tokens(list).into_iter();
        while let Some(token) = tokens.next() {
            if matches!(token, ">=" | "<=" | "=" | "<" | ">" | "==") {
                // The bound belongs to the previous name.
                tokens.next();
                continue;
            }
            if token.starts_with(['<', '>', '=']) {
                // `foo >=1.0`: operator and version in one token.
                continue;
            }
            let name = token.split(['<', '>', '=']).next().unwrap_or(token);
            let entry = if !name.is_empty()
                && name
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '+' | '-'))
            {
                BuildRequire::Package(name.to_string())
            } else {
                BuildRequire::Capability(token.to_string())
            };
            if !out.contains(&entry) {
                out.push(entry);
            }
        }
    }
    out
}

/// Collect `%define <name> <value>` and `%global <name> <value>` entries
/// from a spec, in file order so later definitions override earlier ones (as
/// rpmbuild itself does). Only the simple single-token-value shape is
/// understood — parametric macros, conditionals, nested `%()` evaluation,
/// and the lua `%{lua:...}` form are intentionally left untouched; this is
/// a header reader, not an rpm interpreter.
fn collect_simple_defines(spec: &str) -> std::collections::HashMap<String, String> {
    let mut macros = std::collections::HashMap::new();
    // One frame per open `%if`. A define is only recorded when every frame's
    // current branch is known to be taken: recording from an unevaluable
    // branch could silently pick the wrong value.
    let mut stack: Vec<CondFrame> = Vec::new();
    for line in spec.lines() {
        let trimmed = line.trim_start();
        if let Some(cond) = trimmed
            .strip_prefix("%if ")
            .or_else(|| trimmed.strip_prefix("%if\t"))
        {
            let state = eval_condition(cond, &macros);
            stack.push(CondFrame::new(state));
            continue;
        }
        if trimmed.starts_with("%ifarch")
            || trimmed.starts_with("%ifnarch")
            || trimmed.starts_with("%ifos")
            || trimmed.starts_with("%ifnos")
        {
            stack.push(CondFrame::new(None));
            continue;
        }
        if let Some(cond) = trimmed.strip_prefix("%elif ") {
            if let Some(frame) = stack.last_mut() {
                frame.advance();
                frame.state = match frame.prior {
                    Taken::No => eval_condition(cond, &macros),
                    _ => None,
                };
            }
            continue;
        }
        if trimmed == "%else" {
            if let Some(frame) = stack.last_mut() {
                frame.advance();
                frame.state = match frame.prior {
                    Taken::Yes => Some(false),
                    Taken::Maybe => None,
                    Taken::No => Some(true),
                };
            }
            continue;
        }
        if trimmed == "%endif" {
            stack.pop();
            continue;
        }
        if stack.iter().any(|f| f.state != Some(true)) {
            continue;
        }
        let rest = trimmed
            .strip_prefix("%define ")
            .or_else(|| trimmed.strip_prefix("%global "));
        let Some(rest) = rest else { continue };
        let rest = rest.trim();
        let Some((name, value)) = rest.split_once(char::is_whitespace) else {
            continue;
        };
        let name = name.trim();
        let value = value.trim();
        // Skip parametric macros (name(args)) and empty definitions.
        if name.is_empty() || value.is_empty() || name.contains('(') {
            continue;
        }
        // One-pass substitution of already-known defines so chained
        // `%define release_prefix 1.0` / `%define pkgver %{release_prefix}.3`
        // resolves. Deliberately not recursive beyond that — complex nested
        // forms belong to real rpm expansion, not this reader.
        macros.insert(name.to_string(), expand_once(value, &macros));
    }
    macros
}

/// Whether an earlier branch of the current `%if` chain was taken.
#[derive(Clone, Copy, PartialEq)]
enum Taken {
    Yes,
    No,
    Maybe,
}

struct CondFrame {
    /// `Some(true)` taken, `Some(false)` skipped, `None` cannot be evaluated.
    state: Option<bool>,
    prior: Taken,
}

impl CondFrame {
    fn new(state: Option<bool>) -> Self {
        CondFrame {
            state,
            prior: Taken::No,
        }
    }

    /// Fold the branch just left into `prior` before moving to the next one.
    fn advance(&mut self) {
        self.prior = match (self.prior, self.state) {
            (Taken::Yes, _) | (_, Some(true)) => Taken::Yes,
            (Taken::Maybe, _) | (_, None) => Taken::Maybe,
            _ => Taken::No,
        };
    }
}

/// Evaluate the simple `%if` shapes: an integer literal, optionally with a
/// `%{?name}` expansion (`0%{?name}`). A macro the spec itself never defined
/// is unknown, not false — rpm's own config may define it.
fn eval_condition(cond: &str, macros: &std::collections::HashMap<String, String>) -> Option<bool> {
    let cond = cond.trim();
    let mut expanded = String::new();
    let mut rest = cond;
    while let Some(pos) = rest.find("%{?") {
        expanded.push_str(&rest[..pos]);
        let after = &rest[pos + 3..];
        let end = after.find('}')?;
        let name = &after[..end];
        if name.is_empty() || !name.chars().all(|c| c.is_alphanumeric() || c == '_') {
            return None;
        }
        expanded.push_str(macros.get(name)?);
        rest = &after[end + 1..];
    }
    expanded.push_str(rest);
    let n: i64 = expanded.trim().parse().ok()?;
    Some(n != 0)
}

/// Replace every `%{name}` or `%name` reference by its collected `%define`/
/// `%global` value. Unknown macros are left alone so the caller still sees
/// which ones didn't resolve.
pub(crate) fn expand_simple_defines(
    s: &str,
    macros: &std::collections::HashMap<String, String>,
) -> String {
    expand_once(s, macros)
}

fn expand_once(s: &str, macros: &std::collections::HashMap<String, String>) -> String {
    let mut out = s.to_string();
    // Longest name first: replacing `%rel` before `%release` turns `%release`
    // into `1ease`, and HashMap order would make that differ from run to run.
    let mut by_length: Vec<_> = macros.iter().collect();
    by_length.sort_by(|a, b| b.0.len().cmp(&a.0.len()).then_with(|| a.0.cmp(b.0)));
    for (name, value) in by_length {
        out = out
            .replace(&format!("%{{{name}}}"), value)
            .replace(&format!("%{name}"), value);
    }
    out
}

/// Replace `version` in a directory name with `%{version}`, but only where it
/// is not embedded in a longer alphanumeric run. A plain substring replace
/// turned `sqlite3-3` with version `3` into `sqlite%{version}-%{version}`.
///
/// After the version a letter or digit blocks the match (`1.2.33`), while `.`
/// or `-` allow it (`foo-1.2.3.post1`). Before it, a letter is tolerated only
/// for a dotted version (`foo1.2.3`, `foo-v1.2.3`): a bare `3` right after
/// letters is as likely part of a name as a version. A digit never precedes.
fn replace_version_component(dir: &str, version: &str) -> String {
    if version.is_empty() {
        return dir.to_string();
    }
    let bytes = dir.as_bytes();
    let dotted = version.contains('.');
    let mut out = String::with_capacity(dir.len());
    let mut i = 0;
    while i < dir.len() {
        let end = i + version.len();
        let at_start = i == 0
            || !bytes[i - 1].is_ascii_alphanumeric()
            || (dotted && bytes[i - 1].is_ascii_alphabetic());
        let at_end = end >= dir.len() || !bytes[end].is_ascii_alphanumeric();
        if dir[i..].starts_with(version) && at_start && at_end {
            out.push_str("%{version}");
            i = end;
        } else {
            let ch = dir[i..].chars().next().unwrap();
            out.push(ch);
            i += ch.len_utf8();
        }
    }
    out
}

/// The expanded `%autosetup -n` directory for a spec with the given name and
/// version. Used by `check_autosetup_dir` and exposed for testing.
pub fn autosetup_expected_dir(spec: &str, name: &str, version: &str) -> Option<String> {
    let raw = spec
        .lines()
        .find(|l| l.trim().starts_with("%autosetup"))?
        .split_whitespace()
        .collect::<Vec<_>>()
        .windows(2)
        .find(|w| w[0] == "-n")
        .map(|w| w[1].to_string())
        .unwrap_or_else(|| format!("{name}-%{{version}}"));

    Some(
        raw.replace("%{name}", name)
            .replace("%{version}", version)
            .replace("%{upstream_version}", version),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_is_replaced_in_dir_names_but_not_inside_other_tokens() {
        let r = replace_version_component;
        assert_eq!(r("foo-v1.2.3", "1.2.3"), "foo-v%{version}");
        assert_eq!(r("foo1.2.3", "1.2.3"), "foo%{version}");
        assert_eq!(r("foo.1.2.3", "1.2.3"), "foo.%{version}");
        assert_eq!(r("foo-1.2.3.post1", "1.2.3"), "foo-%{version}.post1");
        assert_eq!(r("sqlite3-3", "3"), "sqlite3-%{version}");
        assert_eq!(r("foo-1.2.33", "1.2.3"), "foo-1.2.33");
    }

    const SPEC_WITH_N: &str = "\
Name:           python-MarkupSafe
Version:        3.0.3
%prep
%autosetup -p1 -n MarkupSafe-%{version}
";

    const SPEC_WITHOUT_N: &str = "\
Name:           python-markupsafe
Version:        3.0.3
%prep
%autosetup -p1
";

    #[test]
    fn parses_header_fields_from_a_pypi_spec() {
        let spec = "\
Name:           python-attrs
Version:        26.1.0
Release:        0
Summary:        Classes Without Boilerplate
License:        MIT
URL:            https://www.attrs.org/
Source:         https://files.pythonhosted.org/packages/source/a/attrs/attrs-%{version}.tar.gz
";
        let h = parse_header(spec).expect("header");
        assert_eq!(h.name, "python-attrs");
        assert_eq!(h.version, "26.1.0");
        assert_eq!(
            h.source.as_deref(),
            Some("https://files.pythonhosted.org/packages/source/a/attrs/attrs-%{version}.tar.gz"),
        );
    }

    #[test]
    fn parse_header_accepts_source0_as_fallback() {
        let spec = "\
Name:           foo
Version:        1.2.3
Source0:        https://example.com/foo-%{version}.tar.gz
";
        let h = parse_header(spec).expect("header");
        assert_eq!(
            h.source.as_deref(),
            Some("https://example.com/foo-%{version}.tar.gz")
        );
    }

    #[test]
    fn parse_header_requires_name_and_version() {
        assert!(parse_header("Version: 1.0\n").is_none());
        assert!(parse_header("Name: foo\n").is_none());
    }

    /// AppCo's versioned-library specs declare the real version as a
    /// `%define` and reference it from `Version:`. Reading the raw tag
    /// value made the comparator see the literal `%{package_version}`
    /// string, which parses as not-PEP-440 and used to be labelled
    /// `UpToDate` (Oct 6 review finding #3). The reader has to resolve the
    /// define up front so the comparator sees `2.4.0` here.
    #[test]
    fn parse_header_expands_a_define_referenced_from_version() {
        let spec = "\
%define package_version 2.4.0
Name:           pytorch
Version:        %{package_version}
Source:         pytorch-%{version}.tar.gz
";
        let h = parse_header(spec).expect("header");
        assert_eq!(h.version, "2.4.0");
    }

    #[test]
    fn parse_header_expands_global_the_same_way_as_define() {
        let spec = "\
%global pkgver 1.5.2
Name:           something
Version:        %{pkgver}
";
        let h = parse_header(spec).expect("header");
        assert_eq!(h.version, "1.5.2");
    }

    #[test]
    fn parse_header_leaves_unknown_macros_alone_rather_than_losing_the_reference() {
        // No matching define — the raw value survives verbatim so callers
        // can tell the macro wasn't resolvable, instead of being handed an
        // empty string or a silent fallback.
        let spec = "\
Name:           foo
Version:        %{something_undefined}
";
        let h = parse_header(spec).expect("header");
        assert_eq!(h.version, "%{something_undefined}");
    }

    #[test]
    fn parse_header_chains_defines_in_file_order() {
        // Real-world case: a release-prefix macro referenced inside a
        // later package-version macro. One pass over defines is enough
        // because each one substitutes against the ones already seen.
        let spec = "\
%define release_prefix 1.0
%define pkgver %{release_prefix}.3
Name:           bar
Version:        %{pkgver}
";
        let h = parse_header(spec).expect("header");
        assert_eq!(h.version, "1.0.3");
    }

    #[test]
    fn parse_header_skips_parametric_macros_without_breaking() {
        // Parametric defines (`%define foo() ...`) are not simple
        // substitutions — don't try to apply one; the Version: here is a
        // literal and should pass through untouched.
        let spec = "\
%define py_sitearch() /usr/lib/python/arch
Name:           ok
Version:        3.2.1
";
        let h = parse_header(spec).expect("header");
        assert_eq!(h.version, "3.2.1");
    }

    #[test]
    fn explicit_n_is_expanded() {
        assert_eq!(
            autosetup_expected_dir(SPEC_WITH_N, "python-MarkupSafe", "3.0.3").as_deref(),
            Some("MarkupSafe-3.0.3")
        );
    }

    #[test]
    fn missing_n_defaults_to_name_version() {
        assert_eq!(
            autosetup_expected_dir(SPEC_WITHOUT_N, "python-markupsafe", "3.0.3").as_deref(),
            Some("python-markupsafe-3.0.3")
        );
    }

    #[test]
    fn mismatch_is_detected() {
        // MarkupSafe-3.0.3 expected, markupsafe-3.0.3 actual — case mismatch.
        let expected = autosetup_expected_dir(SPEC_WITH_N, "python-MarkupSafe", "3.0.3").unwrap();
        let actual = "markupsafe-3.0.3";
        assert_ne!(expected, actual);
    }

    #[test]
    fn fix_replaces_n_argument() {
        // Simulate: spec says -n MarkupSafe-3.0.3, actual dir is markupsafe-3.0.3.
        // fix_autosetup_dir can't run without a real tarball, so test the
        // inner replacement logic directly via autosetup_expected_dir.
        let spec = "%autosetup -p1 -n MarkupSafe-3.0.3\n";
        let parts: Vec<&str> = spec.split_whitespace().collect();
        let idx = parts.windows(2).position(|w| w[0] == "-n").unwrap();
        assert_eq!(parts[idx + 1], "MarkupSafe-3.0.3");
    }

    #[test]
    fn fix_appends_n_when_absent() {
        let spec = "\
Name:           python-markupsafe
Version:        3.0.3
%prep
%autosetup -p1
";
        // Without -n, the default would be python-markupsafe-3.0.3.
        // If the actual dir is markupsafe-3.0.3, fix should append -n.
        // Verify that autosetup_expected_dir sees the default.
        assert_eq!(
            autosetup_expected_dir(spec, "python-markupsafe", "3.0.3").as_deref(),
            Some("python-markupsafe-3.0.3")
        );
    }

    #[test]
    fn python_module_constraints_get_spaces_around_the_operator() {
        // The unspaced form is what pyproject.toml and py2pack produce, and
        // exactly what convention 1 in the agent prompt calls wrong.
        let spec = "BuildRequires:  %{python_module setuptools>=42}\n\
                    BuildRequires:  %{python_module wheel}\n";
        let fixed = normalize_python_module_versions(spec);
        assert!(
            fixed.contains("%{python_module setuptools >= 42}"),
            "{fixed}"
        );
        assert!(
            fixed.contains("%{python_module wheel}"),
            "bare name mangled"
        );
    }

    /// The faster-whisper case: OBS reports `python313-huggingface_hub` as
    /// unresolvable, the translate-deps turn identifies the upstream project
    /// as `huggingface-hub`, and that dependency gets built under that
    /// spelling — but unless this rewrite runs, the spec keeps asking for
    /// the underscore form forever.
    #[test]
    fn translated_dep_name_is_rewritten_in_python_module() {
        let spec = "BuildRequires:  %{python_module huggingface_hub >= 0.21}\n\
                    Requires:       %{python_module huggingface_hub >= 0.21}\n";
        let fixed = rewrite_python_module_name(spec, "huggingface_hub", "huggingface-hub");
        assert!(
            fixed.contains("%{python_module huggingface-hub >= 0.21}"),
            "{fixed}"
        );
        assert_eq!(fixed.matches("huggingface_hub").count(), 0, "{fixed}");
    }

    /// The sentence-transformers case: the BuildRequires was respelled but the
    /// runtime `Requires: python-huggingface_hub` was not.
    #[test]
    fn a_respelled_dependency_is_rewritten_in_every_dependency_line() {
        let spec = "BuildRequires:  %{python_module huggingface_hub >= 0.23.0}\n\
                    Requires:       python-huggingface_hub >= 0.23.0\n\
                    Recommends:     python3-huggingface_hub\n\
                    Requires:       python-numpy\n\
                    # python-huggingface_hub stays in a comment\n";
        let fixed = rewrite_dependency_name(spec, "huggingface_hub", "huggingface-hub");
        assert!(
            fixed.contains("BuildRequires:  %{python_module huggingface-hub >= 0.23.0}"),
            "{fixed}"
        );
        assert!(
            fixed.contains("Requires:       python-huggingface-hub >= 0.23.0"),
            "{fixed}"
        );
        assert!(
            fixed.contains("Recommends:     python3-huggingface-hub\n"),
            "{fixed}"
        );
        assert!(fixed.contains("Requires:       python-numpy\n"), "{fixed}");
        assert!(fixed.contains("# python-huggingface_hub stays"), "{fixed}");
        assert_eq!(
            rewrite_dependency_name(&fixed, "huggingface_hub", "huggingface-hub"),
            fixed
        );
    }

    #[test]
    fn rewrite_leaves_unrelated_python_module_lines_alone() {
        let spec = "BuildRequires:  %{python_module huggingface_hub >= 0.21}\n\
                    BuildRequires:  %{python_module numpy}\n";
        let fixed = rewrite_python_module_name(spec, "huggingface_hub", "huggingface-hub");
        assert!(fixed.contains("%{python_module numpy}"), "{fixed}");
    }

    /// `pyproject.toml`'s `[build-system] requires` is routinely lowercase
    /// upstream (`cython>=3`); copied verbatim into the python_module macro
    /// it RPM-expands to the nonexistent `python313-cython` instead of the
    /// real `python313-Cython`, which made OBS report python-pyarrow as
    /// missing a dependency on every run.
    #[test]
    fn a_known_mixed_case_pypi_name_gets_recased() {
        let spec = "BuildRequires:  %{python_module cython >= 3}\n\
                    BuildRequires:  %{python_module Cython >= 3.1.1}\n\
                    BuildRequires:  %{python_module numpy >= 1.21.6}\n";
        let fixed = normalize_python_module_versions(spec);
        assert!(
            fixed.contains("%{python_module Cython >= 3}"),
            "lowercase name not recased: {fixed}"
        );
        assert!(
            fixed.contains("%{python_module Cython >= 3.1.1}"),
            "already-correct casing changed: {fixed}"
        );
        assert!(
            fixed.contains("%{python_module numpy >= 1.21.6}"),
            "unrelated name touched: {fixed}"
        );
    }

    /// `%{$python_sitelib}` only resolves inside `%python_expand` — a bare
    /// line reproduces the python-nvidia-nvtx bug where it silently expands
    /// to `%{}` instead of failing loudly.
    #[test]
    fn a_continuation_line_is_not_prefixed_again() {
        let spec = "\
%install
%python_expand cp -a foo \\
  %{buildroot}%{$python_sitelib}/foo
chmod +x %{$python_sitelib}/a \\
  %{$python_sitelib}/b
";
        let fixed = fix_stray_dollar_macros(spec);
        assert!(
            fixed.contains("\n  %{buildroot}%{$python_sitelib}/foo\n"),
            "{fixed}"
        );
        assert!(fixed.contains("\n  %{$python_sitelib}/b\n"), "{fixed}");
        assert!(
            fixed.contains("%python_expand chmod +x %{$python_sitelib}/a \\\n"),
            "{fixed}"
        );
        assert_eq!(fixed.matches("%python_expand").count(), 2, "{fixed}");
    }

    #[test]
    fn a_bare_dollar_macro_line_gets_python_expand_prefixed() {
        let spec = "\
%install
%pyproject_install
chmod +x %{buildroot}%{$python_sitelib}/nvidia/cu13/lib/libfoo.so.1
%python_expand %fdupes %{buildroot}%{$python_sitelib}
# %{$python_sitelib} mentioned only in a comment, must not be touched
";
        let fixed = fix_stray_dollar_macros(spec);
        assert!(
            fixed.contains(
                "%python_expand chmod +x %{buildroot}%{$python_sitelib}/nvidia/cu13/lib/libfoo.so.1"
            ),
            "{fixed}"
        );
        assert!(
            fixed.contains("%python_expand %fdupes %{buildroot}%{$python_sitelib}\n"),
            "already-wrapped line got double-wrapped: {fixed}"
        );
        assert!(
            fixed.contains("# %{$python_sitelib} mentioned only in a comment, must not be touched"),
            "comment line was touched: {fixed}"
        );
    }

    /// `%python_install_alternative` registers an update-alternatives slave
    /// in `%post`; without `Requires(post/postun): update-alternatives` the
    /// scriptlet fails at install/removal time with "command not found"
    /// (observed on python-triton).
    #[test]
    fn alternatives_without_requires_post_get_them_added() {
        let spec = "\
BuildRequires:  python-rpm-macros
BuildRequires:  %{python_module pip}
%python_subpackages

%install
%python_install_alternative proton
";
        let fixed = fix_missing_alternatives_requires(spec);
        assert!(
            fixed.contains("Requires(post):   update-alternatives\n"),
            "{fixed}"
        );
        assert!(
            fixed.contains("Requires(postun): update-alternatives\n"),
            "{fixed}"
        );
        // Requires lines must land before %python_subpackages.
        let post_pos = fixed.find("Requires(post)").unwrap();
        let subpkg_pos = fixed.find("%python_subpackages").unwrap();
        assert!(post_pos < subpkg_pos, "{fixed}");

        // Already-correct spec and one with no alternatives at all are left alone.
        assert_eq!(fix_missing_alternatives_requires(&fixed), fixed);
        let no_alternatives = "BuildRequires:  %{python_module pip}\n%python_subpackages\n";
        assert_eq!(
            fix_missing_alternatives_requires(no_alternatives),
            no_alternatives
        );
    }
}

/// Strip all `# ap: fix` comment lines from a spec.
///
/// Model-authored fix comments must not accumulate across attempts: they
/// reference RPM macros (e.g. `# ap: fix 1: Changed %pytest to %pytest_arch`)
/// and RPM expands macros inside comments, causing parse failures. Call this
/// before handing the spec back to the model.
pub fn strip_ap_fix_comments(spec: &str) -> String {
    let filtered: Vec<&str> = spec
        .lines()
        .filter(|l| !l.trim_start().starts_with("# ap: fix"))
        .collect();
    let body = filtered.join("\n");
    if spec.ends_with('\n') {
        body + "\n"
    } else {
        body
    }
}

/// Return the 1-based line number and text of the first comment line that
/// contains a `%` macro reference, or `None` if the spec is clean.
///
/// RPM expands macros inside comment lines, so any `# foo: %something` line
/// will be expanded before the spec is parsed — causing the mysterious
/// `Unknown tag: set +x` class of failure that item 8 exists to prevent.
pub fn first_macro_in_comment(spec: &str) -> Option<(usize, String)> {
    for (i, line) in spec.lines().enumerate() {
        let trimmed = line.trim_start();
        if !trimmed.starts_with('#') {
            continue;
        }
        if let Some(pos) = trimmed.find('%') {
            let after = &trimmed[pos + 1..];
            // `%%` is a literal percent — not a macro.
            if after.starts_with('%') {
                continue;
            }
            if after.starts_with(|c: char| c.is_alphabetic() || c == '{') {
                return Some((i + 1, line.to_string()));
            }
        }
    }
    None
}

/// Insert `cd <dir>` at the top of `%build` and `%install`.
///
/// A repository snapshot of a monorepo has the packaging in a subdirectory
/// (`cuda_python/`, `python/`), so `%autosetup` lands one level above the
/// project and every build macro would run in the wrong place.
pub fn build_in_subdir(spec: &str, dir: &str) -> String {
    let mut out = String::with_capacity(spec.len() + 64);
    for line in spec.lines() {
        out.push_str(line);
        out.push('\n');
        if matches!(line.trim(), "%build" | "%install") {
            out.push_str(&format!("cd {dir}\n"));
        }
    }
    if !spec.ends_with('\n') {
        out.pop();
    }
    out
}

/// Generate an openSUSE RPM spec for `release`.
///
/// Invokes `py2pack generate` and post-processes the result to fix the fields
/// py2pack cannot determine without the sdist: license, URL, and build
/// backend requirements.
pub fn generate(release: &Release, build_system: &BuildSystem) -> Result<String, Error> {
    let dir = tempfile::tempdir().map_err(Error::Io)?;
    let spec_path = dir
        .path()
        .join(format!("python-{}.spec", release.name.to_lowercase()));

    let out = Command::new("py2pack")
        .args([
            "generate",
            "-t",
            "opensuse.spec",
            "-f",
            &spec_path.to_string_lossy(),
            // The name comes from an index; `--` keeps it a positional even
            // when it starts with `-`.
            "--",
            &release.name,
            &release.version,
        ])
        .stdin(Stdio::null())
        .output()
        .map_err(Error::Launch)?;

    if !out.status.success() {
        let code = out.status.code().unwrap_or(-1);
        let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
        return Err(Error::Failed { code, stderr });
    }

    let spec = std::fs::read_to_string(&spec_path).map_err(Error::Io)?;
    Ok(post_process(
        spec,
        release,
        build_system,
        SdistFacts::default(),
    ))
}

/// What the sdist tells us that PyPI metadata cannot.
#[derive(Debug, Clone, Default)]
pub struct SdistFacts {
    pub has_tests: bool,
    pub has_c_extensions: bool,
    /// See `ap_ecosystem::sdist::SdistInfo::top_level_tests_dir`.
    pub top_level_tests_dir: Option<String>,
    /// What the project installs into site-packages, from [`sdist_files_entries`];
    /// `None` when the sdist's layout does not say it unambiguously.
    pub install_entries: Option<Vec<String>>,
    /// Commands the project declares in `[project.scripts]`; see
    /// `ap_ecosystem::sdist::SdistInfo::console_scripts`.
    pub console_scripts: Vec<String>,
}

/// Re-run post-processing once the tarball has been inspected.
pub fn post_process_with_sdist(
    spec: &str,
    release: &Release,
    build_system: &BuildSystem,
    facts: SdistFacts,
) -> String {
    post_process(spec.to_string(), release, build_system, facts)
}

fn post_process(
    spec: String,
    release: &Release,
    build_system: &BuildSystem,
    facts: SdistFacts,
) -> String {
    let has_tests = facts.has_tests;
    let mut out = String::with_capacity(spec.len() + 512);

    let mut in_description = false;
    let mut description_written = false;
    let has_check = spec.contains("%check");

    for line in spec.lines() {
        // Inject %{?sle15_python_module_pythons} before the Name: tag.
        if line.starts_with("Name:") && !spec.contains("sle15_python_module_pythons") {
            out.push_str("%{?sle15_python_module_pythons}\n");
        }

        // Fix License.
        if line.starts_with("License:") {
            let license = license_tag(&release.license);
            out.push_str(&format!("License:        {license}\n"));
            continue;
        }

        // Fix URL.
        if line.starts_with("URL:") {
            let url = release
                .homepage
                .as_deref()
                .unwrap_or("https://pypi.org/project/%{name}");
            out.push_str(&format!("URL:            {url}\n"));
            continue;
        }

        // Inject build-backend BuildRequires (and pytest if tests exist) after the pip line.
        if line.contains("%{python_module pip}") {
            out.push_str(line);
            out.push('\n');
            for req in &build_system.requires {
                // `setuptools>=42` as it appears in pyproject.toml is exactly
                // the form the conventions document as wrong; OBS needs
                // `setuptools >= 42` inside the macro.
                out.push_str(&format!("BuildRequires:  %{{python_module {req}}}\n"));
            }
            if has_tests && !spec.contains("%{python_module pytest}") {
                out.push_str("BuildRequires:  %{python_module pytest}\n");
            }
            continue;
        }

        // Replace the %description body with the one-line summary.
        if line.starts_with("%description") {
            in_description = true;
            description_written = false;
            out.push_str(line);
            out.push('\n');
            continue;
        }

        if in_description {
            if line.starts_with('%') {
                if !description_written {
                    if let Some(s) = &release.summary {
                        out.push_str(s);
                        out.push('\n');
                    }
                    description_written = true;
                }
                in_description = false;
                // rpm wants a blank line between the description body and the
                // next section; without it the agent adds one on every package.
                out.push('\n');
                out.push_str(line);
                out.push('\n');
            }
            continue;
        }

        // Inject %check before %changelog when tests exist. Scoped to the
        // top-level tests/ directory when there is one — otherwise pytest
        // collects from the whole source tree, including anything merely
        // named test_*.py elsewhere (fixture helpers, scripts) as if it
        // were a real test (observed on python-accelerate: 58 spurious
        // failures from src/accelerate/test_utils/scripts/).
        if line.starts_with("%changelog") && has_tests && !has_check {
            match &facts.top_level_tests_dir {
                Some(dir) => out.push_str(&format!("\n%check\n%pytest {dir}/\n\n")),
                None => out.push_str("\n%check\n%pytest\n\n"),
            }
        }

        out.push_str(line);
        out.push('\n');
    }

    // Everything below is a whole-file pass, applied in a fixed order.
    //
    // Each one exists because the review agent was making the identical edit
    // on nearly every package (see the notes.md journals): a deterministic
    // rule here is a turn the model does not have to spend.
    let out = add_runtime_dependencies(&out, &release.requires_dist, &release.name);
    let out = normalize_python_module_versions(&out);
    let out = dedupe_build_requires(&out);
    let out = normalize_files_paths(&out, &release.name);
    let out = match &facts.install_entries {
        Some(entries) => apply_install_entries(&out, entries),
        None => out,
    };
    let out = fix_bare_source_filename(&out, release);
    let out = add_console_script_alternatives(&out, &facts.console_scripts);
    if facts.has_c_extensions {
        fix_c_extension_arch(&out)
    } else {
        out
    }
}

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
            let package = cuda.clone().unwrap_or_else(|| format!("python-{}", dep.name));
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

/// The `%files` entries for what a Python source tree installs into
/// site-packages, read from the sdist's file list, with `@SITE@` standing for
/// the site directory. `None` unless the tree says it without guessing: one
/// import tree under the project root (or its `src/`), no loose top-level module.
///
/// The generator guessed `%{python_sitelib}/<pypi_name>/`, which was wrong for
/// every PEP 420 namespace distribution (`opentelemetry-instrumentation-flask`
/// installs `opentelemetry/instrumentation/flask/`) and wherever the import name
/// differs (`python-docx` -> `docx`, `z3-solver` -> `z3`); the review corrected
/// it on a dozen packages.
pub fn sdist_files_entries(
    files: &[String],
    top_level_dir: &str,
    project_dir: Option<&str>,
) -> Option<Vec<String>> {
    use std::collections::BTreeMap;

    const NOT_INSTALLED: &[&str] = &[
        "tests",
        "test",
        "testing",
        "docs",
        "doc",
        "examples",
        "example",
        "scripts",
        "script",
        "benchmarks",
        "benchmark",
        "tools",
        "build",
        "dist",
        "ci",
        "typings",
        "stubs",
        "tasks",
        "noxfile.py",
        "setup.py",
        "conftest.py",
        "fabfile.py",
        "docs_src",
        "bench",
        "dev",
    ];
    let prefix = match project_dir {
        Some(d) => format!("{top_level_dir}/{d}/"),
        None => format!("{top_level_dir}/"),
    };
    let rel: Vec<&str> = files
        .iter()
        .filter_map(|f| f.strip_prefix(prefix.as_str()))
        .filter(|f| f.ends_with(".py"))
        .collect();
    // A `src/` layout when the Python lives there.
    let root = if rel
        .iter()
        .any(|f| f.starts_with("src/") && f.matches('/').count() >= 2)
    {
        "src/"
    } else {
        ""
    };
    #[derive(Default)]
    struct Dir {
        files: Vec<String>,
        dirs: BTreeMap<String, Dir>,
    }
    let mut tree = Dir::default();
    for f in rel.iter().filter_map(|f| f.strip_prefix(root)) {
        let first = f.split('/').next().unwrap_or("");
        if first.starts_with('.') || NOT_INSTALLED.contains(&first) {
            continue;
        }
        if f.chars()
            .any(|c| c.is_whitespace() || matches!(c, '%' | '$' | '*' | '?' | '['))
        {
            return None;
        }
        let mut node = &mut tree;
        let mut parts = f.split('/').peekable();
        while let Some(part) = parts.next() {
            if parts.peek().is_some() {
                node = node.dirs.entry(part.to_string()).or_default();
            } else {
                node.files.push(part.to_string());
            }
        }
    }
    // A loose module (`six.py`) brings its own `__pycache__` entry: not guessed.
    if !tree.files.is_empty() || tree.dirs.len() != 1 {
        return None;
    }
    fn emit(dir: &Dir, path: &str, out: &mut Vec<String>) {
        let own_files = dir.files.iter().any(|f| f == "__init__.py");
        if own_files || dir.dirs.is_empty() {
            out.push(format!("@SITE@/{path}/"));
            return;
        }
        out.push(format!("%dir @SITE@/{path}"));
        let mut files = dir.files.clone();
        files.sort();
        for f in files {
            out.push(format!("@SITE@/{path}/{f}"));
        }
        for (name, child) in &dir.dirs {
            emit(child, &format!("{path}/{name}"), out);
        }
    }
    let mut out = Vec::new();
    for (name, child) in &tree.dirs {
        emit(child, name, &mut out);
    }
    Some(out)
}

/// Replace the module-directory entry of the generated `%files` with the
/// entries read from the sdist, when the generated one names a directory that
/// is not part of the install tree. The site directory the line used is kept.
fn apply_install_entries(spec: &str, entries: &[String]) -> String {
    let real_tops: std::collections::HashSet<&str> = entries
        .iter()
        .filter_map(|e| {
            let path = e.strip_prefix("%dir ").unwrap_or(e);
            path.strip_prefix("@SITE@/")?.split('/').next()
        })
        .collect();
    let mut out = String::with_capacity(spec.len());
    let mut replaced = false;
    let mut in_files = false;
    for line in spec.lines() {
        const SECTIONS: &[&str] = &[
            "%files",
            "%description",
            "%prep",
            "%build",
            "%install",
            "%check",
            "%changelog",
            "%package",
            "%pre",
            "%post",
            "%preun",
            "%postun",
            "%pretrans",
            "%posttrans",
        ];
        if let Some(section) = SECTIONS.iter().find(|s| {
            line.strip_prefix(**s)
                .is_some_and(|rest| rest.is_empty() || rest.starts_with(char::is_whitespace))
        }) {
            in_files = *section == "%files";
        }
        let trimmed = line.trim().trim_end_matches('/');
        let guessed_site =
            if in_files && !replaced && trimmed.starts_with('%') && !trimmed.contains(".dist-info")
            {
                trimmed
                    .rsplit_once('/')
                    .filter(|(site, last)| {
                        (site.contains("sitelib") || site.contains("sitearch"))
                            && !real_tops.contains(last)
                    })
                    .map(|(site, _)| site)
            } else {
                None
            };
        if let Some(site) = guessed_site {
            for e in entries {
                out.push_str(&e.replace("@SITE@", site));
                out.push('\n');
            }
            replaced = true;
        } else {
            out.push_str(line);
            out.push('\n');
        }
    }
    if !spec.ends_with('\n') {
        out.pop();
    }
    out
}

/// Give the commands a source build installs (`[project.scripts]`) the
/// `update-alternatives` handling convention 18 asks for, when py2pack did not
/// (it only reads `setup.py` metadata): `%python_clone` after
/// `%pyproject_install`, `%post`/`%postun` registration and a
/// `%python_alternative` in `%files`. A script the spec already handles is
/// left alone.
pub fn add_console_script_alternatives(spec: &str, scripts: &[String]) -> String {
    let missing: Vec<&String> = scripts
        .iter()
        .filter(|s| {
            !spec.contains(&format!("%python_clone -a %{{buildroot}}%{{_bindir}}/{s}\n"))
                && !spec.contains(&format!("%python_alternative %{{_bindir}}/{s}\n"))
        })
        .collect();
    let has_install = spec.lines().any(|l| l.trim() == "%pyproject_install");
    let files_pos = spec
        .lines()
        .position(|l| l.starts_with("%files") && l.contains("%{python_files}"));
    if missing.is_empty() || !has_install || files_pos.is_none() {
        return spec.to_string();
    }
    let each = |f: &dyn Fn(&str) -> String| -> Vec<String> {
        missing.iter().map(|s| f(s)).collect()
    };
    let clones = each(&|s| format!("%python_clone -a %{{buildroot}}%{{_bindir}}/{s}"));
    let installs = each(&|s| format!("%python_install_alternative {s}"));
    let uninstalls = each(&|s| format!("%python_uninstall_alternative {s}"));
    let owned = each(&|s| format!("%python_alternative %{{_bindir}}/{s}"));

    let mut lines: Vec<String> = spec.lines().map(str::to_string).collect();
    // Bottom-up, so earlier indices stay valid.
    let files_pos = files_pos.unwrap_or(0);
    lines.splice(files_pos + 1..files_pos + 1, owned);
    for (header, add) in [("%postun", uninstalls), ("%post", installs)] {
        match lines.iter().position(|l| l.trim() == header) {
            Some(pos) => {
                lines.splice(pos + 1..pos + 1, add);
            }
            None => {
                let files_pos = lines
                    .iter()
                    .position(|l| l.starts_with("%files") && l.contains("%{python_files}"))
                    .unwrap_or(0);
                let mut block = vec![header.to_string()];
                block.extend(add);
                block.push(String::new());
                lines.splice(files_pos..files_pos, block);
            }
        }
    }
    if let Some(pos) = lines.iter().position(|l| l.trim() == "%pyproject_install") {
        lines.splice(pos + 1..pos + 1, clones);
    }
    let out = lines.join("\n") + if spec.ends_with('\n') { "\n" } else { "" };
    fix_missing_alternatives_requires(&out)
}

/// Point a bare-filename `Source:` at the archive actually in the checkout.
///
/// py2pack names the tarball after PyPI's own sdist (`Events-%{version}.zip`)
/// even when the checkout holds a different file — a forge release archive
/// (`events-0.5.tar.gz`) that the pipeline downloaded because the project has
/// no sdist. `%prep` then fails on the very first build, and `unzip` is
/// requested for nothing. A URL `Source:` is left alone: it names where the
/// file comes from, not which file is on disk.
fn fix_bare_source_filename(spec: &str, release: &Release) -> String {
    let actual = release.sdist.filename.as_str();
    if actual.is_empty() {
        return spec.to_string();
    }
    let templated = actual.replace(&release.version, "%{version}");
    let mut changed = false;
    let mut out = String::with_capacity(spec.len());
    for line in spec.lines() {
        let rewritten = ["Source:", "Source0:"].iter().find_map(|tag| {
            let value = line.strip_prefix(tag)?.trim();
            (!value.is_empty() && !value.contains("://") && value != templated)
                .then(|| format!("{tag:<16}{templated}"))
        });
        match rewritten {
            Some(l) if !changed => {
                changed = true;
                out.push_str(&l);
            }
            _ => out.push_str(line),
        }
        out.push('\n');
    }
    if !spec.ends_with('\n') {
        out.pop();
    }
    if changed && !actual.ends_with(".zip") {
        out = out
            .lines()
            .filter(|l| l.trim() != "BuildRequires:  unzip")
            .collect::<Vec<_>>()
            .join("\n")
            + if spec.ends_with('\n') { "\n" } else { "" };
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

/// Rewrite `%files` entries that spell the package the way PyPI does.
///
/// The dist-info directory is always `<normalized>-%{version}.dist-info/` —
/// that one is not a guess. The module directory is only rewritten when it
/// currently spells out the PyPI name verbatim; packages whose import name
/// differs entirely (`PyJWT` → `jwt`, `opencv-python-headless` → `cv2`) are
/// left for the agent, which can read `top_level.txt`.
pub fn normalize_files_paths(spec: &str, pypi_name: &str) -> String {
    let normalized = normalized_module_name(pypi_name);
    let mut out = String::with_capacity(spec.len());

    for line in spec.lines() {
        // py2pack writes these entries with a trailing `/`; splitting at the
        // last `/` of the untrimmed line made the "prefix" the whole path
        // and doubled the directory (`x.dist-info/x.dist-info/`) in every
        // generated spec.
        let trimmed = line.trim().trim_end_matches('/');
        let rewritten = if trimmed.contains(".dist-info") {
            // Split at the `/` ending the dist-info component so a trailing
            // file (`.dist-info/LICENSE`) survives the rewrite.
            let idx = trimmed.find(".dist-info").unwrap();
            let dir_start = trimmed[..idx].rfind('/').map(|i| i + 1).unwrap_or(0);
            let after = &trimmed[idx + ".dist-info".len()..];
            let prefix = &trimmed[..dir_start];
            // Directory entries lost their `/` to the trim above.
            let tail = if after.is_empty() { "/" } else { after };
            format!("{prefix}{normalized}-%{{version}}.dist-info{tail}")
        } else if let Some((prefix, last)) = trimmed.rsplit_once('/')
            && trimmed.starts_with('%')
            // Only module directories: `%{_bindir}/black` is a script, not a
            // package directory. Covers `%{python_sitelib}`, `%{$python_sitearch}`
            // and the `%python_expand` spellings.
            && (prefix.contains("sitelib") || prefix.contains("sitearch"))
            && last.trim_end_matches('/').eq_ignore_ascii_case(pypi_name)
        {
            format!("{prefix}/{normalized}/")
        } else {
            line.to_string()
        };
        out.push_str(&rewritten);
        out.push('\n');
    }

    if !spec.ends_with('\n') {
        out.pop();
    }
    out
}

/// [`wheel_binary_skeleton`] for a release whose wheels are each locked to one
/// CPython (convention 49's Triton shape): one `SourceN:` per flavor and one
/// hardcoded `%python<NNN>_pyproject_install` per flavor, since the shared
/// `%pyproject_install` repeats a single argument for every flavor.
///
/// `wheels` is `(flavor, wheel filename)` — e.g. `("python313", "x-cp313-...whl")`,
/// in flavor order.
pub fn wheel_multi_flavor_skeleton(
    pkg_name: &str,
    version: &str,
    wheels: &[(String, String)],
    release: &Release,
    root_is_purelib: Option<bool>,
    layout: Option<&ap_ecosystem::sdist::WheelLayout>,
) -> String {
    wheel_multi_flavor_skeleton_shared(
        pkg_name,
        version,
        wheels,
        release,
        root_is_purelib,
        layout,
        &[],
    )
}

/// [`wheel_multi_flavor_skeleton`] for a project whose other wheels fill the
/// same directories (see [`wheel_binary_skeleton_shared`]).
pub fn wheel_multi_flavor_skeleton_shared(
    pkg_name: &str,
    version: &str,
    wheels: &[(String, String)],
    release: &Release,
    root_is_purelib: Option<bool>,
    layout: Option<&ap_ecosystem::sdist::WheelLayout>,
    siblings: &[SiblingWheel],
) -> String {
    let Some((_, first)) = wheels.first() else {
        return String::new();
    };
    let single = wheel_binary_skeleton_shared(
        pkg_name,
        version,
        first,
        release,
        root_is_purelib,
        layout,
        siblings,
    );
    let sources: String = wheels
        .iter()
        .enumerate()
        .map(|(i, (_, file))| format!("Source{i}:        {file}\n"))
        .collect();
    let installs: String = wheels
        .iter()
        .enumerate()
        .map(|(i, (flavor, _))| format!("%{flavor}_pyproject_install %{{SOURCE{i}}}\n"))
        .collect();
    single
        .replacen(&format!("Source0:        {first}\n"), &sources, 1)
        .replacen("%pyproject_install %{SOURCE0}\n", &installs, 1)
}

/// Generate a minimal skeleton spec for a monorepo package.
///
/// py2pack only understands single-project PyPI packages; for a monorepo with
/// multiple subprojects it would produce a spec for one subproject only,
/// leaving all siblings out. This skeleton carries the correct header fields
/// from the PyPI release and leaves %build/%install/%check empty so the review
/// agent can fill in the full multi-subpackage layout per convention 24.
pub fn monorepo_skeleton(
    pkg_name: &str,
    version: &str,
    subprojects: &[ap_ecosystem::sdist::Subproject],
    release: Option<&ap_ecosystem::pypi::Release>,
) -> String {
    let name = pkg_name.to_lowercase();
    let summary = rpm_text(
        release
            .and_then(|r| r.summary.as_deref())
            .unwrap_or("FIXME: add package summary"),
    );
    let license = release
        .map(|r| license_tag(&r.license))
        .unwrap_or_else(|| "FIXME".to_string());
    let url = rpm_text(
        release
            .and_then(|r| r.homepage.as_deref())
            .unwrap_or("FIXME"),
    );

    let sub_list: String = subprojects
        .iter()
        .map(|s| {
            format!(
                "# TODO: add %package -n python-%{{name}}-{sub} for {dir}/\n",
                sub = s.name.as_deref().unwrap_or(&s.dir),
                dir = s.dir,
            )
        })
        .collect();

    format!(
        "#\n\
         # spec file for package python-{name}\n\
         #\n\
         # Copyright (c) 2026 SUSE LLC\n\
         #\n\
         # All modifications and additions to the file contributed by third parties\n\
         # remain the property of their copyright owners, unless otherwise agreed\n\
         # upon. The license for this file, and modifications and additions to the\n\
         # file, is the same license as for the pristine package itself (unless the\n\
         # license for the pristine package is not an Open Source License, in which\n\
         # case the license is the MIT License). An \"Open Source License\" is a\n\
         # license that conforms to the Open Source Definition (Version 1.9)\n\
         # published by the Open Source Initiative.\n\
         \n\
         # Please submit bugfixes or comments via https://bugs.opensuse.org/\n\
         \n\
         \n\
         %{{?sle15_python_module_pythons}}\n\
         Name:           python-{name}\n\
         Version:        {version}\n\
         Release:        0\n\
         Summary:        {summary}\n\
         License:        {license}\n\
         URL:            {url}\n\
         Source:         {name}-{version}.tar.gz\n\
         BuildRequires:  python-rpm-macros\n\
         BuildRequires:  %{{python_module pip}}\n\
         BuildRequires:  fdupes\n\
         %python_subpackages\n\
         \n\
         # This is a monorepo: all subprojects below must become %%package sections.\n\
         # Convention 24 governs the layout — do not build only one subproject.\n\
         {sub_list}\
         %description\n\
         {summary}\n\
         \n\
         %prep\n\
         %autosetup -p1 -n {name}-{version}\n\
         \n\
         %build\n\
         # TODO: build each subproject in dependency order\n\
         \n\
         %install\n\
         # TODO: install each subproject\n\
         \n\
         %check\n\
         # TODO: check each subproject\n\
         \n\
         %files %{{python_files}}\n\
         %license LICENSE\n\
         \n\
         %changelog\n",
        name = name,
        version = version,
        summary = summary,
        license = license,
        url = url,
        sub_list = sub_list,
    )
}

/// Build a skeleton spec for a release that ships no sdist AND has no
/// discoverable source repository — a closed-source binary redistribution.
/// This is the normal shape of NVIDIA's individual CUDA toolkit component
/// wheels (`nvidia-cuda-nvcc`, `nvidia-cuda-cupti`, `nvidia-cuda-nvrtc`,
/// `nvidia-cuda-runtime`, ...): PyPI is simply how NVIDIA redistributes the
/// proprietary toolkit binaries, and there is nothing to compile.
///
/// `wheel_filename` becomes `Source0`; `%install` unpacks it directly with
/// `%pyproject_install`, which accepts a pre-built wheel path instead of a
/// dist it just built. See convention 24 Exception 3 for when this is the
/// right call versus a workaround to avoid.
/// Whether a repackaged wheel is the shape that reliably trips the same
/// handful of rpmlint false positives on an unmodified vendor binary: an
/// arch-specific wheel (not `-none-any.whl`) whose payload is a shared
/// library rather than importable Python. rpmlint judges these as if they
/// were a from-source build — a detached hash section, an executable bit on
/// the `.so`, `ldconfig` scriptlets, a runtime dependency on `python-base` —
/// none of which apply to a vendor `.so` dropped unmodified under
/// site-packages. See [`prebuilt_wheel_rpmlintrc`].
pub(crate) fn is_prebuilt_binary_wheel(
    wheel_filename: &str,
    layout: Option<&ap_ecosystem::sdist::WheelLayout>,
) -> bool {
    let is_pure = wheel_filename.ends_with("-none-any.whl");
    !is_pure && layout.is_some_and(|l| l.first_shared_lib.is_some())
}

/// The `rpmlintrc` of a prebuilt/vendor-binary wheel (see
/// [`is_prebuilt_binary_wheel`]): only the findings that cannot be fixed in the
/// package, each with the reason it stays. What can be fixed is fixed in the
/// spec instead, by [`wheel_binary_skeleton`]: the executable bit on the
/// libraries, the dependency on `python-base`, the bundled headers.
///
/// - `missing-hash-section`: the libraries are upstream's prebuilt binaries,
///   linked without a build-id/hash section; they cannot be relinked here.
/// - `library-without-ldconfig-postin/postun`: the bundled libraries sit in
///   site-packages and are loaded by the Python modules from their own
///   directory, not from the ldconfig path, so there is no cache to refresh.
/// - `binary-or-shlib-defines-rpath` for `$ORIGIN`: vendor libraries find
///   their siblings that way, and the rule is badness 10000, so one hit aborts
///   rpmlint (`libcusparseLt.so.0 (RUNPATH: $ORIGIN:)`). Only written when a
///   bundled library really has such a RUNPATH
///   ([`ap_ecosystem::sdist::WheelLayout::origin_runpath`]): a filter nothing
///   matches is `unused-rpmlintrc-filter`, an error of its own (convention
///   72). Any other RPATH value still fails the check.
pub fn prebuilt_wheel_rpmlintrc(layout: &ap_ecosystem::sdist::WheelLayout) -> String {
    let mut out = String::from(
        "# The libraries are upstream's prebuilt binaries, linked without a hash\n\
         # section; they cannot be relinked here.\n\
         addFilter(\"missing-hash-section\")\n\
         \n\
         # The bundled libraries live in site-packages and are loaded from their own\n\
         # directory by the Python modules, not from the ldconfig path: there is no\n\
         # ldconfig cache for a scriptlet to refresh.\n\
         addFilter(\"library-without-ldconfig-postin\")\n\
         addFilter(\"library-without-ldconfig-postun\")\n",
    );
    if layout.origin_runpath {
        out.push_str(
            "\n\
             # The bundled libraries find each other through an $ORIGIN RUNPATH, which\n\
             # is how a self-contained vendor wheel has to be laid out.\n\
             addFilter(\".*binary-or-shlib-defines-rpath.*[$]ORIGIN\")\n",
        );
    }
    out
}

/// The `zero-length` filters for the empty files a wheel ships (see
/// [`ap_ecosystem::sdist::WheelLayout::empty_files`]), one per file with the
/// reason. The files are upstream's own placeholders (`custom.css`,
/// `loader.js`): making them non-empty would change the payload, so rpmlint is
/// told instead. A name with a character outside the plain set is left out
/// rather than written into a regex unescaped.
fn wheel_empty_files_filters(layout: &ap_ecosystem::sdist::WheelLayout) -> String {
    let mut out = String::new();
    for name in &layout.empty_files {
        if !name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "._/-@+~".contains(c))
        {
            continue;
        }
        if out.is_empty() {
            out.push_str(
                "# Empty placeholder files that upstream's wheel ships on purpose (a\n\
                 # user-editable stylesheet or script): they must stay as they are.\n",
            );
        }
        let escaped = name.replace('.', "\\.").replace('+', "\\+");
        out.push_str(&format!(
            "addFilter(r\".*zero-length .*/site-packages/{escaped}$\")\n"
        ));
    }
    out
}

/// Whether a repackaged wheel needs a `python-<name>.rpmlintrc`: it is a
/// vendor-binary wheel, or it ships empty files rpmlint calls `zero-length`.
pub(crate) fn wheel_needs_rpmlintrc(
    wheel_filename: &str,
    layout: Option<&ap_ecosystem::sdist::WheelLayout>,
) -> bool {
    is_prebuilt_binary_wheel(wheel_filename, layout)
        || layout.is_some_and(|l| !wheel_empty_files_filters(l).is_empty())
}

/// Write `python-<name>.rpmlintrc` next to the spec for a wheel that needs one
/// (the spec's `Source99:` names it, see [`wheel_needs_rpmlintrc`]); nothing for
/// any other wheel.
pub(crate) fn wheel_rpmlintrc(
    pkg_name: &str,
    wheel_filename: &str,
    layout: Option<&ap_ecosystem::sdist::WheelLayout>,
) -> Option<(String, String)> {
    if !wheel_needs_rpmlintrc(wheel_filename, layout) {
        return None;
    }
    let layout = layout?;
    let mut text = String::new();
    if is_prebuilt_binary_wheel(wheel_filename, Some(layout)) {
        text.push_str(&prebuilt_wheel_rpmlintrc(layout));
    }
    let empties = wheel_empty_files_filters(layout);
    if !empties.is_empty() {
        if !text.is_empty() {
            text.push('\n');
        }
        text.push_str(&empties);
    }
    // A zero-byte rpmlintrc is a `Source99` that does nothing: never written.
    if text.trim().is_empty() {
        return None;
    }
    Some((format!("python-{}.rpmlintrc", pkg_name.to_lowercase()), text))
}

/// The `__requires_exclude` regex for a wheel bundling auditwheel-style
/// libraries (`libavcodec-f34387fd.so.62.11.100`): the generated `Requires`
/// name those hashed sonames, and nothing can provide them. Requires 8 hex
/// digits after a `-` in a `lib*` name, so `libcuda.so.1`, `libz3.so.4.15`
/// and `libfoo-1.2.so` are left alone, nor a date-stamped `libfoo-20240101.so`.
/// Generated from the same rule as `ap_ecosystem::sdist::is_hashed_soname`
/// (the one definition of "hashed"); convention 68 of the agent prompt and the
/// reason text of `depclass.rs` describe it in words.
fn hashed_libs_requires_exclude() -> &'static str {
    ap_ecosystem::sdist::hashed_soname_requires_exclude()
}

/// The regex an earlier generator wrote: it also matched date-stamped names.
/// Specs carrying it are rewritten to the current one.
const OLD_HASHED_LIBS_REQUIRES_EXCLUDE: &str = r"^lib.*-[0-9a-f]{8}.*\.so.*$";

/// The `%global __requires_exclude` line for a wheel that bundles hashed
/// libraries, empty otherwise. rpm honours only ONE such global: the patterns
/// to exclude are folded into a single regex with `|`, which is what a second
/// pattern added to this skeleton has to do.
fn wheel_requires_exclude_line(
    layout: Option<&ap_ecosystem::sdist::WheelLayout>,
    is_pure: bool,
) -> String {
    let mut patterns: Vec<&str> = Vec::new();
    if layout.is_some_and(|l| l.has_hashed_libs) {
        patterns.push(hashed_libs_requires_exclude());
    }
    // A compiled wheel may link the NVIDIA driver (`libcuda.so.1`), which no
    // repository provides: its Requires never name it.
    if !is_pure {
        patterns.push(crate::depclass::driver_requires_exclude());
    }
    let mut line = String::new();
    for p in patterns {
        line = if line.is_empty() {
            format!("%global __requires_exclude {p}\n")
        } else {
            with_requires_exclude_pattern(&format!("{line}%description\n"), p)
                .trim_end_matches("%description\n")
                .to_string()
        };
    }
    line
}

/// `spec` with the hashed-library `__requires_exclude` of a wheel that bundles
/// such libraries (see [`hashed_libs_requires_exclude`]), for a spec written
/// before the generator knew to: python-opencv-python-headless was built, was
/// uninstallable, and every package needing it stayed unresolvable. An existing
/// `%global __requires_exclude` is folded into one regex with `|` (rpm honours
/// only one); the line is otherwise added before `%python_subpackages`, or
/// `%description`. A spec that already carries the pattern is returned as is.
pub fn with_hashed_libs_requires_exclude(spec: &str) -> String {
    let spec = &spec.replace(OLD_HASHED_LIBS_REQUIRES_EXCLUDE, hashed_libs_requires_exclude());
    with_requires_exclude_pattern(spec, hashed_libs_requires_exclude())
}

/// `spec` that leaves the NVIDIA driver's libraries (`libcuda.so.1`, NVML, ...:
/// [`crate::depclass::is_driver_soname`]) out of its Requires: nothing provides
/// them, the host driver does at run time, so a package that links one is
/// unresolvable in OBS (python-flashinfer-python: `nothing provides
/// libcuda.so.1()(64bit)`). An existing `%global __requires_exclude` is
/// extended, never overwritten.
pub fn with_driver_requires_exclude(spec: &str) -> String {
    with_requires_exclude_pattern(spec, crate::depclass::driver_requires_exclude())
}

/// The inside of an anchored `__requires_exclude` regex, without `^`/`$`.
fn requires_exclude_inner(pattern: &str) -> String {
    pattern
        .trim()
        .trim_start_matches('^')
        .trim_end_matches('$')
        .to_string()
}

/// `spec` with `pattern` (an anchored regex) part of its one
/// `%global __requires_exclude`: folded into an existing line with `|` (rpm
/// honours only one such global), or added before `%python_subpackages` /
/// `%description`. A spec already carrying the pattern is returned as is.
fn with_requires_exclude_pattern(spec: &str, pattern: &str) -> String {
    let wanted = requires_exclude_inner(pattern);
    let lines: Vec<&str> = spec.lines().collect();
    let tail = if spec.ends_with('\n') { "\n" } else { "" };
    if let Some(at) = lines
        .iter()
        .position(|l| l.trim_start().starts_with("%global __requires_exclude "))
    {
        let existing = lines[at]
            .trim_start()
            .trim_start_matches("%global __requires_exclude ");
        if existing.contains(&wanted) {
            return spec.to_string();
        }
        let folded = format!(
            "%global __requires_exclude ^({}|{})$",
            requires_exclude_inner(existing),
            wanted
        );
        let mut out: Vec<String> = lines.iter().map(|l| l.to_string()).collect();
        out[at] = folded;
        return out.join("\n") + tail;
    }
    let Some(at) = lines
        .iter()
        .position(|l| l.trim_start().starts_with("%python_subpackages"))
        .or_else(|| lines.iter().position(|l| l.trim_start().starts_with("%description")))
    else {
        return spec.to_string();
    };
    let mut out: Vec<String> = lines.iter().map(|l| l.to_string()).collect();
    out.insert(at, format!("%global __requires_exclude {pattern}"));
    out.join("\n") + tail
}

/// Whether a spec builds an architecture-specific package that may link the
/// driver's library: not `BuildArch: noarch`, and it names CUDA or NVIDIA.
pub fn may_link_driver_library(spec: &str) -> bool {
    let lower = spec.to_ascii_lowercase();
    let noarch = lower.lines().any(|l| {
        let l = l.trim_start();
        l.starts_with("buildarch:") && l.contains("noarch") && !l.contains('%')
    });
    !noarch && (lower.contains("cuda") || lower.contains("nvidia"))
}

/// Spec fragments that give a wheel's console scripts the openSUSE
/// multi-flavor `update-alternatives` handling a source build gets (convention
/// 18): `(requires, clone, scriptlets, files)`, each empty when the wheel
/// declares no script. pip generates `/usr/bin/<script>` per flavor, so it
/// must be cloned to `<script>-<pyver>` and owned through
/// `%python_alternative`, or the build fails with `Installed (but unpackaged)
/// file(s) found` (python-triton's `proton`, python-cuda-tile's
/// `cutile-cache`).
fn wheel_alternatives_fragments(scripts: &[String]) -> (&'static str, String, String, String) {
    if scripts.is_empty() {
        return ("", String::new(), String::new(), String::new());
    }
    let each = |line: &dyn Fn(&str) -> String| -> String { scripts.iter().map(|s| line(s)).collect() };
    (
        "Requires(post):   update-alternatives\nRequires(postun): update-alternatives\n",
        each(&|s| format!("%python_clone -a %{{buildroot}}%{{_bindir}}/{s}\n")),
        format!(
            "%post\n{}\n%postun\n{}\n",
            each(&|s| format!("%python_install_alternative {s}\n")),
            each(&|s| format!("%python_uninstall_alternative {s}\n")),
        ),
        each(&|s| format!("%python_alternative %{{_bindir}}/{s}\n")),
    )
}

/// The Python (for `python -c`) that proves the dotted package `module` is
/// found under `root` alone: `PathFinder.find_spec` on each prefix, searching
/// only the locations the previous prefix found.
fn namespace_find_spec_snippet(module: &str, root: &str) -> String {
    let mut parts = module.split('.');
    let top = parts.next().unwrap_or_default();
    let mut snippet = format!(
        "import importlib.machinery as m, sys; s = m.PathFinder.find_spec('{top}', ['{root}'])"
    );
    let mut prefix = top.to_string();
    for part in parts {
        prefix = format!("{prefix}.{part}");
        snippet.push_str(&format!(
            "; s = s and m.PathFinder.find_spec('{prefix}', s.submodule_search_locations)"
        ));
    }
    snippet.push_str("; sys.exit(0 if s else 1)");
    snippet
}

/// How `%install` clears one bundled header directory of a prebuilt wheel.
#[derive(Debug, Clone, PartialEq, Eq)]
enum HeaderCleanup<'a> {
    /// The directory holds nothing but headers: `rm -rf` it.
    Dir(&'a str),
    /// The directory also holds files the package needs (a Python package's
    /// `__init__.py`, Cython sources, the `.c` a tool reads at run time): only
    /// its `.h`/`.hpp` files go.
    Headers(&'a str),
}

impl HeaderCleanup<'_> {
    /// Whether `%install` removes `path` (relative to site-packages).
    fn removes(&self, path: &str) -> bool {
        match self {
            HeaderCleanup::Dir(d) => path == *d || path.starts_with(&format!("{d}/")),
            HeaderCleanup::Headers(d) => {
                path.starts_with(&format!("{d}/")) && (path.ends_with(".h") || path.ends_with(".hpp"))
            }
        }
    }
}

/// What `%install` removes of a wheel's bundled headers (see
/// [`HeaderCleanup`]); nothing for a wheel whose payload IS headers: when
/// everything outside the header directories is data (no `.py`, no shared
/// library, nothing under `bin/`), the wheel is a header library (NVIDIA's
/// `nvidia-cuda-crt` is only `include/crt/*.h`, `nvidia-cuda-cccl` is CCCL's
/// header tree), and stripping the headers leaves an empty package. Those are
/// split into a `-devel` package by the review (convention 24).
fn wheel_header_cleanups(layout: &ap_ecosystem::sdist::WheelLayout) -> Vec<HeaderCleanup<'_>> {
    let under = |d: &str, f: &str| f.starts_with(&format!("{d}/"));
    let is_payload = |f: &str| {
        let top = f.split('/').next().unwrap_or("");
        !top.ends_with(".dist-info") && !top.ends_with(".data") && top != "__pycache__"
    };
    let is_code = |f: &str| {
        let file = f.rsplit('/').next().unwrap_or("");
        file.ends_with(".py")
            || file.ends_with(".so")
            || file.contains(".so.")
            || f.split('/').rev().skip(1).any(|c| c == "bin")
    };
    let outside = |f: &String| {
        is_payload(f) && !layout.header_dirs.iter().any(|d| under(d, f))
    };
    if layout.header_dirs.is_empty() || !layout.files.iter().filter(|f| outside(f)).any(|f| is_code(f)) {
        return Vec::new();
    }
    let header_like = |f: &str| {
        let file = f.rsplit('/').next().unwrap_or(f);
        match file.rsplit_once('.') {
            None => true,
            Some((_, ext)) => matches!(ext, "h" | "hpp" | "hh" | "hxx" | "cuh" | "inl" | "ipp"),
        }
    };
    layout
        .header_dirs
        .iter()
        .map(|d| {
            if layout.files.iter().filter(|f| under(d, f)).all(|f| header_like(f)) {
                HeaderCleanup::Dir(d.as_str())
            } else {
                HeaderCleanup::Headers(d.as_str())
            }
        })
        .collect()
}

/// The path to assert in `%check` for a wheel with nothing to import and no
/// shared library (NVIDIA's `nvcc`/`nvdisasm`: executables under `bin/`): the
/// shortest file `%install` leaves in site-packages, outside the dist-info.
fn wheel_payload_probe(layout: &ap_ecosystem::sdist::WheelLayout) -> Option<String> {
    let cleanups = wheel_header_cleanups(layout);
    layout
        .files
        .iter()
        .filter(|f| {
            let top = f.split('/').next().unwrap_or("");
            !top.ends_with(".dist-info")
                && !top.ends_with(".data")
                && top != "__pycache__"
                && !f.ends_with(".pyc")
                && !cleanups.iter().any(|c| c.removes(f))
                && !f.chars().any(|c| {
                    c.is_whitespace()
                        || matches!(c, '%' | '*' | '?' | '[' | ']' | '$' | '"' | '\'' | '\\')
                })
        })
        .min_by_key(|f| (f.len(), f.as_str()))
        .cloned()
}

/// The `%files` entries for a prebuilt wheel, from its own file list, or `None`
/// when the layout is not one this can list without guessing (a `.data`
/// directory, a top-level module, an odd file name): the review then fills the
/// section in. Convention 15 forbids globs, so every entry is an exact path or
/// a package directory.
///
///  * a directory with an `__init__.py` is a package: the directory is listed;
///  * a directory without one is a namespace shared with other wheels
///    (`nvidia/`, `cuda/`): it is `%dir`, and its children are listed;
///  * a directory that holds only files is listed as a whole (`nvidia/cu13/lib/`);
///  * what `%install` removes (bundled headers) is left out;
///  * the dist-info directory is listed with its licence files split off as
///    `%exclude` + `%license`, so each is owned once.
fn wheel_files_entries(
    layout: &ap_ecosystem::sdist::WheelLayout,
    sitedir: &str,
    foreign: &[String],
) -> Option<String> {
    use std::collections::BTreeMap;

    let dist_info = layout.dist_info.as_deref()?;
    let cleanups = wheel_header_cleanups(layout);
    let removed = |path: &str| cleanups.iter().any(|c| c.removes(path));
    let odd = |path: &str| {
        path.chars().any(|c| {
            c.is_whitespace() || matches!(c, '%' | '*' | '?' | '[' | ']' | '$' | '"' | '\'' | '\\')
        })
    };
    let mut payload: Vec<&str> = Vec::new();
    let mut licenses: Vec<&str> = Vec::new();
    for file in &layout.files {
        let file = file.as_str();
        if odd(file) {
            return None;
        }
        let top = file.split('/').next().unwrap_or("");
        if top == dist_info {
            let rel = &file[dist_info.len() + 1..];
            let leaf = rel.rsplit('/').next().unwrap_or(rel).to_ascii_uppercase();
            let is_license = rel.starts_with("licenses/")
                || ["LICENSE", "LICENCE", "COPYING", "NOTICE"]
                    .iter()
                    .any(|p| leaf.starts_with(p));
            if is_license {
                licenses.push(file);
            }
        } else if file.ends_with(".pth") && !file.contains('/') {
            // A top-level `.pth` is data for the interpreter, with no bytecode.
            payload.push(file);
        } else if top.ends_with(".data") || top == "__pycache__" || !file.contains('/') {
            // Scripts and data land outside site-packages; a top-level module
            // brings its own `__pycache__`: neither is listed blind.
            return None;
        } else if !removed(file) && !file.ends_with(".pyc") {
            payload.push(file);
        }
    }
    // A wheel with nothing but its dist-info (a metapackage: `cuda-toolkit`)
    // owns just that directory.

    // Directory tree of the payload: dir -> (direct files, child dirs).
    #[derive(Default)]
    struct Dir {
        files: Vec<String>,
        dirs: BTreeMap<String, Dir>,
    }
    let mut root = Dir::default();
    for path in &payload {
        let mut node = &mut root;
        let mut parts = path.split('/').peekable();
        while let Some(part) = parts.next() {
            if parts.peek().is_some() {
                node = node.dirs.entry(part.to_string()).or_default();
            } else {
                node.files.push(part.to_string());
            }
        }
    }
    fn emit(dir: &Dir, path: &str, sitedir: &str, foreign: &[String], out: &mut Vec<String>) {
        // A directory another package of the project also fills (convention
        // 83): co-owned with `%dir`, only this wheel's own files listed, never
        // the directory as a whole.
        let prefix = format!("{path}/");
        let shared = foreign.iter().any(|f| f.starts_with(&prefix));
        if !shared && (dir.files.iter().any(|f| f == "__init__.py") || dir.dirs.is_empty()) {
            out.push(format!("%{{{sitedir}}}/{path}/"));
            return;
        }
        out.push(format!("%dir %{{{sitedir}}}/{path}"));
        let mut files = dir.files.clone();
        files.sort();
        for f in &files {
            out.push(format!("%{{{sitedir}}}/{path}/{f}"));
        }
        // The build byte-compiles every module it installs: the `__pycache__`
        // beside modules listed one by one is installed too, and unowned unless
        // listed (a package directory above owns its own). In a shared
        // directory the sibling's bytecode lives there too, so list this
        // wheel's own modules' only.
        let own_py: Vec<&String> = files.iter().filter(|f| f.ends_with(".py")).collect();
        if !own_py.is_empty() {
            if shared {
                out.push(format!("%dir %{{{sitedir}}}/{path}/__pycache__"));
                for f in own_py {
                    let stem = f.trim_end_matches(".py");
                    out.push(format!("%{{{sitedir}}}/{path}/__pycache__/{stem}.*.pyc"));
                }
            } else {
                out.push(format!("%{{{sitedir}}}/{path}/__pycache__/"));
            }
        }
        for (name, child) in &dir.dirs {
            emit(child, &format!("{path}/{name}"), sitedir, foreign, out);
        }
    }
    let mut lines = Vec::new();
    root.files.sort();
    for f in &root.files {
        lines.push(format!("%{{{sitedir}}}/{f}"));
    }
    for (name, child) in &root.dirs {
        emit(child, name, sitedir, foreign, &mut lines);
    }
    licenses.sort();
    // Several files under `licenses/` are one entry: the directory.
    let licenses_dir = format!("{dist_info}/licenses/");
    if licenses.len() >= 2 && licenses.iter().all(|l| l.starts_with(&licenses_dir)) {
        lines.push(format!("%exclude %{{{sitedir}}}/{dist_info}/licenses"));
        lines.push(format!("%license %{{{sitedir}}}/{dist_info}/licenses"));
    } else {
        for l in &licenses {
            lines.push(format!("%exclude %{{{sitedir}}}/{l}"));
        }
        for l in &licenses {
            lines.push(format!("%license %{{{sitedir}}}/{l}"));
        }
    }
    lines.push(format!("%{{{sitedir}}}/{dist_info}/"));
    Some(lines.join("\n") + "\n")
}

pub fn wheel_binary_skeleton(
    pkg_name: &str,
    version: &str,
    wheel_filename: &str,
    release: &Release,
    root_is_purelib: Option<bool>,
    layout: Option<&ap_ecosystem::sdist::WheelLayout>,
) -> String {
    wheel_binary_skeleton_shared(
        pkg_name,
        version,
        wheel_filename,
        release,
        root_is_purelib,
        layout,
        &[],
    )
}

/// [`wheel_binary_skeleton`] for a wheel whose project has other wheels filling
/// the same directories (convention 83). `siblings` are the other wheels of the
/// project (see [`sibling_wheels`]; build order does not matter): directories
/// they also hold are co-owned with `%dir` and this wheel's own files listed one
/// by one, and a sibling that owns a package (`__init__.py`) this wheel adds
/// files to becomes a `Requires:`, so the import chain is complete. Nothing in a
/// sibling's `RECORD` or payload is touched.
///
/// The install directory follows the wheel's own `Root-Is-Purelib`, with one
/// exception: a wheel that says `false` but shares a top-level directory with a
/// sibling that says `true` moves its tree into `%{python_sitelib}` in
/// `%install`, because a package tree split between `lib` and `lib64` cannot be
/// imported (a regular package in one never sees the other) and the pure
/// sibling, being noarch, cannot go anywhere else.
pub fn wheel_binary_skeleton_shared(
    pkg_name: &str,
    version: &str,
    wheel_filename: &str,
    release: &Release,
    root_is_purelib: Option<bool>,
    layout: Option<&ap_ecosystem::sdist::WheelLayout>,
    siblings: &[SiblingWheel],
) -> String {
    let foreign: Vec<String> = siblings.iter().flat_map(|s| s.files.iter().cloned()).collect();
    let name = pkg_name.to_lowercase();
    let summary = release
        .summary
        .as_deref()
        .unwrap_or("FIXME: add package summary");
    // rpm expands macros in `Summary:` and `URL:`: a `%` of upstream's text is
    // doubled (`wheel_description` does the same for the description).
    let summary_tag = rpm_text(summary);
    // The release's own licence, or, when PyPI has none the tool can use, what the
    // wheel's METADATA declares (a `License: FIXME` is a turn the review spends
    // reading the wheel for what the generator already read).
    let license = wheel_declared_license(release, layout);
    let license = match layout {
        Some(l) => with_license_terms(license, &wheel_vendor_license_terms(l)),
        None => license,
    };
    let description = wheel_description(layout.and_then(|l| l.description.as_deref()), summary);
    let url = rpm_text(release.homepage.as_deref().unwrap_or("FIXME"));
    // Two independent facts about a wheel:
    //  * is the PACKAGE arch-specific? Only the platform tag says so:
    //    `-none-any.whl` is noarch; anything else (`manylinux_*_x86_64`) is not,
    //    and gets `ExclusiveArch` for the architecture it was built for.
    //  * which DIRECTORY does it install into? That is `Root-Is-Purelib` in the
    //    wheel's WHEEL file (`true` -> %{python_sitelib}, `false` ->
    //    %{python_sitearch}), whatever the tag. NVIDIA's CUDA wheels are
    //    manylinux-tagged but purelib, so a spec that assumes sitearch for them
    //    packages nothing and `%check` cannot import the module.
    let is_pure = wheel_filename.ends_with("-none-any.whl");
    let arch_line = if is_pure {
        "BuildArch:      noarch\n".to_string()
    } else if wheel_filename.contains("x86_64") {
        "ExclusiveArch:  x86_64\n".to_string()
    } else if wheel_filename.contains("aarch64") {
        "ExclusiveArch:  aarch64\n".to_string()
    } else {
        String::new()
    };
    // No `WHEEL` verdict: the platform tag decides, and any tag but `-none-any`
    // (compiled code, convention 83 b) means `%{python_sitearch}`.
    let declared_purelib = root_is_purelib.unwrap_or(is_pure);
    let purelib = declared_purelib
        || layout.is_some_and(|l| sibling_forces_purelib(l, siblings));
    let sitedir = if purelib {
        "python_sitelib"
    } else {
        "python_sitearch"
    };
    let source99_line = if wheel_needs_rpmlintrc(wheel_filename, layout) {
        format!("Source99:       python-{name}.rpmlintrc\n")
    } else {
        String::new()
    };
    // A `%check` that works the first time. There is no source tree to test, so
    // either import the top-level package, or — for namespace-only/binary-only
    // wheels (NVIDIA's `nvidia/` holding just `lib/*.so`) — assert that the
    // payload landed where `%files` will look. Importing needs the buildroot on
    // PYTHONPATH (the build directory is not on `sys.path`), and everything runs
    // inside `%python_expand` so each flavor gets its own interpreter and path.
    let check_block = match layout {
        Some(l) if l.importable.is_some() => format!(
            "%check\n%python_expand PYTHONPATH=%{{buildroot}}%{{${sitedir}}} $python -c \"import {}\"\n",
            l.importable.as_deref().unwrap_or_default()
        ),
        // A PEP 420 namespace wheel (`cuda/bindings/`): nothing to `import` at
        // the top, and the regular package below it may need a device library
        // at import. The spec is located with `PathFinder`, one dotted prefix at
        // a time and in the buildroot only: no package code runs, and a regular
        // package of the same name that a sibling wheel installed (nccl-
        // extensions' `nccl/ep/` beside nccl4py's `nccl/__init__.py`) cannot hide
        // this wheel's files, which `importlib.util.find_spec` lets it do.
        Some(l) if l.namespace_module.is_some() => format!(
            "%check\n%python_expand PYTHONPATH=%{{buildroot}}%{{${sitedir}}} $python -c \"{}\"\n",
            namespace_find_spec_snippet(
                l.namespace_module.as_deref().unwrap_or_default(),
                &format!("%{{buildroot}}%{{${sitedir}}}")
            )
        ),
        Some(l) if l.first_shared_lib.is_some() => format!(
            "%check\n%python_expand test -e %{{buildroot}}%{{${sitedir}}}/{}\n",
            l.first_shared_lib.as_deref().unwrap_or_default()
        ),
        // Executables only (`bin/nvdisasm`), a header library, a `.pth`: the
        // payload is the shortest file `%install` leaves.
        Some(l) if wheel_payload_probe(l).is_some() => format!(
            "%check\n%python_expand test -e %{{buildroot}}%{{${sitedir}}}/{}\n",
            wheel_payload_probe(l).unwrap_or_default()
        ),
        // A metapackage wheel (`cuda-toolkit`) installs only its dist-info.
        Some(l) if l.dist_info.is_some() && !l.files.is_empty() => format!(
            "%check\n%python_expand test -d %{{buildroot}}%{{${sitedir}}}/{}\n",
            l.dist_info.as_deref().unwrap_or_default()
        ),
        _ => "%check\n# TODO: smoke-test the import name from source/*.dist-info/METADATA:\n\
              # %python_expand PYTHONPATH=%{buildroot}%{$python_sitelib} $python -c \"import <name>\"\n"
            .to_string(),
    };
    // What rpmlint would otherwise be told to ignore is fixed here: the wheel's
    // libraries lose their missing executable bit, and the bundled C headers
    // (not needed at run time) are removed.
    let mut install_extra = String::new();
    if let (true, false, Some(l)) = (purelib, declared_purelib, layout) {
        install_extra.push_str(&sitelib_relocation(l));
    }
    if layout.is_some_and(|l| l.first_shared_lib.is_some()) {
        install_extra.push_str(&format!(
            "# the wheel ships its libraries without the executable bit\n\
             %python_expand find %{{buildroot}}%{{${sitedir}}} -name '*.so*' -type f -exec chmod 0755 {{}} +\n"
        ));
    }
    // Upstream wheels carry the executable bit on files that are not programs
    // (a Python module, a stylesheet): rpmlint's `script-without-shebang`, one
    // error per file. Only an ELF file or a script with a `#!` line keeps it.
    let mode_fix = format!(
        "# files without a shebang or ELF header do not stay executable\n\
         %python_expand find %{{buildroot}}%{{${sitedir}}} -type f -perm /111 -exec sh -c 'for f; do head -c4 \"$f\" | grep -aqE \"^#!|ELF\" || chmod 0644 \"$f\"; done' sh {{}} +\n"
    );
    if layout.is_some_and(|l| !l.header_dirs.is_empty() && wheel_header_cleanups(l).is_empty()) {
        install_extra.push_str(
            "# the headers are this wheel's payload, so they are kept; they belong in a\n\
             # -devel package rather than being removed\n",
        );
    }
    let cleanups = layout.map(wheel_header_cleanups).unwrap_or_default();
    if !cleanups.is_empty() {
        install_extra.push_str("# bundled headers are not needed at run time\n");
    }
    for cleanup in cleanups {
        install_extra.push_str(&match cleanup {
            HeaderCleanup::Dir(dir) => format!(
                "%python_expand rm -rf %{{buildroot}}%{{${sitedir}}}/{dir}\n"
            ),
            HeaderCleanup::Headers(dir) => format!(
                "%python_expand find %{{buildroot}}%{{${sitedir}}}/{dir} -type f \\( -name '*.h' -o -name '*.hpp' \\) -delete\n"
            ),
        });
    }

    let (alt_requires, alt_clone, alt_scriptlets, alt_files) =
        wheel_alternatives_fragments(layout.map_or(&[][..], |l| l.console_scripts.as_slice()));
    let requires_exclude = wheel_requires_exclude_line(layout, is_pure);

    let files_block = layout
        .and_then(|l| wheel_files_entries(l, sitedir, &foreign))
        .unwrap_or_else(|| {
            "# TODO: list the exact paths pyproject_install put in the buildroot\n\
             # (see source/ for the unpacked wheel's real layout); globs are not allowed\n\
             # here.\n"
                .to_string()
        });

    let spec = format!(
        "#\n\
         # spec file for package python-{name}\n\
         #\n\
         # Copyright (c) 2026 SUSE LLC\n\
         #\n\
         # All modifications and additions to the file contributed by third parties\n\
         # remain the property of their copyright owners, unless otherwise agreed\n\
         # upon. The license for this file, and modifications and additions to the\n\
         # file, is the same license as for the pristine package itself (unless the\n\
         # license for the pristine package is not an Open Source License, in which\n\
         # case the license is the MIT License). An \"Open Source License\" is a\n\
         # license that conforms to the Open Source Definition (Version 1.9)\n\
         # published by the Open Source Initiative.\n\
         \n\
         # Please submit bugfixes or comments via https://bugs.opensuse.org/\n\
         \n\
         \n\
         %{{?sle15_python_module_pythons}}\n\
         Name:           python-{name}\n\
         Version:        {version}\n\
         Release:        0\n\
         Summary:        {summary_tag}\n\
         License:        {license}\n\
         URL:            {url}\n\
         Source0:        {wheel_filename}\n\
         {source99_line}\
         {arch_line}\
         BuildRequires:  python-rpm-macros\n\
         BuildRequires:  %{{python_module pip}}\n\
         BuildRequires:  fdupes\n\
         Requires:       python-base\n\
         {alt_requires}\
         {requires_exclude}\
         %python_subpackages\n\
         \n\
         %description\n\
         {description}\n\
         \n\
         %prep\n\
         # nothing to unpack here: the source is a prebuilt wheel\n\
         \n\
         %build\n\
         # prebuilt binary wheel - nothing to compile\n\
         \n\
         %install\n\
         %pyproject_install %{{SOURCE0}}\n\
         {install_extra}\
         {alt_clone}\
         {mode_fix}\
         %python_expand %fdupes %{{buildroot}}/%{{${sitedir}}}\n\
         \n\
         {check_block}\
         \n\
         {alt_scriptlets}\
         %files %{{python_files}}\n\
         {files_block}\
         {alt_files}\
         \n\
         %changelog\n",
        name = name,
        version = version,
        summary_tag = summary_tag,
        description = description,
        license = license,
        url = url,
        wheel_filename = wheel_filename,
        arch_line = arch_line,
        sitedir = sitedir,
        check_block = check_block,
        files_block = files_block,
    );
    // The siblings that own a package this wheel adds files to.
    let mut spec = spec;
    if let Some(l) = layout {
        let mut lines = String::new();
        for owner in shared_package_owners(l, siblings) {
            if !spec.lines().any(|x| x.trim_end() == format!("Requires:       {owner}")) {
                lines.push_str(&format!("Requires:       {owner}\n"));
            }
        }
        if !lines.is_empty() {
            spec = spec.replacen(
                "Requires:       python-base\n",
                &format!("Requires:       python-base\n{lines}"),
                1,
            );
        }
    }
    // What the wheel's own metadata requires at run time.
    match layout {
        Some(l) => add_runtime_dependencies(&spec, &l.requires_dist, pkg_name),
        None => spec,
    }
}

/// The names of the `siblings` that own (hold the `__init__.py` of) a package
/// directory `layout` adds files to: this wheel completes their package, so it
/// needs them installed (convention 83 a). A directory this wheel owns itself is
/// not listed. Sorted, without duplicates.
fn shared_package_owners(
    layout: &ap_ecosystem::sdist::WheelLayout,
    siblings: &[SiblingWheel],
) -> Vec<String> {
    let mut owners: Vec<String> = Vec::new();
    for SiblingWheel { name, files, .. } in siblings {
        let owns = files.iter().any(|f| {
            let Some(dir) = f.strip_suffix("/__init__.py") else {
                return false;
            };
            let prefix = format!("{dir}/");
            let own_init = layout.files.iter().any(|m| m == f);
            !own_init && layout.files.iter().any(|m| m.starts_with(&prefix))
        });
        if owns && !owners.contains(name) {
            owners.push(name.clone());
        }
    }
    owners.sort();
    owners
}

/// Another wheel of the same project (convention 83): `name` is the
/// flavor-neutral RPM name, `files` what it installs relative to site-packages
/// (its dist-info left out), `purelib` its `Root-Is-Purelib` when known.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SiblingWheel {
    pub name: String,
    pub files: Vec<String>,
    pub purelib: Option<bool>,
}

/// The top-level directories of `layout`'s payload (dist-info, `.data` and
/// bytecode directories left out).
fn payload_tops(layout: &ap_ecosystem::sdist::WheelLayout) -> Vec<&str> {
    let mut tops: Vec<&str> = layout
        .files
        .iter()
        .filter(|f| f.contains('/'))
        .filter_map(|f| f.split('/').next())
        .filter(|t| !t.ends_with(".dist-info") && !t.ends_with(".data") && *t != "__pycache__")
        .collect();
    tops.sort_unstable();
    tops.dedup();
    tops
}

/// Whether a sibling that declares `Root-Is-Purelib: true` fills a top-level
/// directory of `layout`: the whole tree then lives in `%{python_sitelib}`.
fn sibling_forces_purelib(
    layout: &ap_ecosystem::sdist::WheelLayout,
    siblings: &[SiblingWheel],
) -> bool {
    let tops = payload_tops(layout);
    siblings.iter().any(|s| {
        s.purelib == Some(true)
            && s.files
                .iter()
                .any(|f| f.split_once('/').is_some_and(|(t, _)| tops.contains(&t)))
    })
}

/// The `%install` lines that move a wheel installed into `%{python_sitearch}`
/// (it says `Root-Is-Purelib: false`) into `%{python_sitelib}`, beside the
/// sibling wheels that fill the same tree.
fn sitelib_relocation(layout: &ap_ecosystem::sdist::WheelLayout) -> String {
    let mut text = String::from(
        "# the wheel says Root-Is-Purelib: false, but a sibling wheel of this project (a\n\
         # noarch package) fills the same tree in sitelib, and a package tree split\n\
         # between lib and lib64 cannot be imported: install it beside them (convention 83)\n\
         %python_expand mkdir -p %{buildroot}%{$python_sitelib}\n",
    );
    let mut tops: Vec<&str> = payload_tops(layout);
    if let Some(d) = layout.dist_info.as_deref() {
        tops.push(d);
    }
    for top in tops {
        text.push_str(&format!(
            "%python_expand mv %{{buildroot}}%{{$python_sitearch}}/{top} %{{buildroot}}%{{$python_sitelib}}/\n"
        ));
    }
    text
}

/// `spec`, generated for a wheel before a sibling wheel that fills the same
/// tree in `%{python_sitelib}` was known (convention 83), moved onto that one
/// tree: python-tokenspeed-mla failed with `No module named 'cutlass.cute'`
/// because `-base` and `-cu12` had been generated into sitearch while `-core`
/// is noarch. Every `sitearch` of the spec becomes `sitelib` and the wheel's
/// tree is moved there right after the install ([`sitelib_relocation`]), as the
/// generator does when it knows the siblings. A spec that does not use
/// sitearch, or already relocates, is returned as is.
pub fn with_sibling_tree_in_sitelib(
    spec: &str,
    layout: &ap_ecosystem::sdist::WheelLayout,
    siblings: &[SiblingWheel],
) -> String {
    if !sibling_forces_purelib(layout, siblings)
        || !spec.contains("python_sitearch")
        || spec.contains("mv %{buildroot}%{$python_sitearch}/")
    {
        return spec.to_string();
    }
    let tail = if spec.ends_with('\n') { "\n" } else { "" };
    let mut out: Vec<String> = Vec::new();
    for line in spec.lines() {
        out.push(
            line.replace("%{$python_sitearch}", "%{$python_sitelib}")
                .replace("%{python_sitearch}", "%{python_sitelib}"),
        );
    }
    // After the last install line.
    let at = out.iter().rposition(|l| {
        let t = l.trim_start();
        t.starts_with("%pyproject_install")
            || (t.starts_with("%python") && t.contains("_pyproject_install"))
    });
    if let Some(at) = at {
        let reloc: Vec<String> = sitelib_relocation(layout).lines().map(str::to_string).collect();
        for (k, l) in reloc.into_iter().enumerate() {
            out.insert(at + 1 + k, l);
        }
    } else {
        return spec.to_string();
    }
    out.join("\n") + tail
}

/// The other wheels of `pkg_name`'s project, from every source that knows what
/// they install, in a fixed order so the result does not depend on which was
/// built first:
///  1. the wheel file in the sibling's own checkout (`<checkouts>/python-<name>/
///     *.whl`, the first by name), listed from the zip: present as soon as that
///     package's spec was generated, built or not;
///  2. otherwise the RPM already built in `rpms_dir`, listed with `rpm -qlp`.
///
/// A sibling is a package whose name shares all but the last two dash-separated
/// words (at least two) with `pkg_name`: `nvidia-cutlass-dsl-libs-base`/`-core`/
/// `-cu13`. Without any source the result is empty.
pub fn sibling_wheels(
    rpms_dir: &std::path::Path,
    checkouts: Option<&std::path::Path>,
    pkg_name: &str,
) -> Vec<SiblingWheel> {
    let mut out: Vec<SiblingWheel> = checkouts
        .map(|c| sibling_wheels_in_checkouts(c, pkg_name))
        .unwrap_or_default();
    for (name, files, purelib) in sibling_rpm_files(rpms_dir, pkg_name) {
        if !out.iter().any(|s| s.name == name) {
            out.push(SiblingWheel { name, files, purelib });
        }
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

/// The siblings among the `python-*` package checkouts in `checkouts` that hold
/// a wheel (see [`sibling_wheels`]).
fn sibling_wheels_in_checkouts(checkouts: &std::path::Path, pkg_name: &str) -> Vec<SiblingWheel> {
    let Ok(entries) = std::fs::read_dir(checkouts) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for e in entries.flatten() {
        let dir = e.file_name().to_string_lossy().into_owned();
        let Some(name) = dir.strip_prefix("python-") else {
            continue;
        };
        let name = name.to_lowercase();
        if !is_sibling_project(&name, pkg_name) {
            continue;
        }
        let Ok(inner) = std::fs::read_dir(e.path()) else {
            continue;
        };
        let mut wheels: Vec<std::path::PathBuf> = inner
            .flatten()
            .map(|f| f.path())
            .filter(|p| p.extension().is_some_and(|x| x == "whl") && p.is_file())
            .collect();
        wheels.sort();
        let Some(wheel) = wheels.first() else {
            continue;
        };
        // Names only: reading a full layout would open the libraries of an
        // NVIDIA wheel, and this runs for every sibling.
        let Some(names) = ap_ecosystem::sdist::wheel_file_names(wheel) else {
            continue;
        };
        let files: Vec<String> = names
            .into_iter()
            .filter(|f| {
                f.split_once('/')
                    .is_some_and(|(top, _)| !top.ends_with(".dist-info"))
            })
            .collect();
        if files.is_empty() {
            continue;
        }
        out.push(SiblingWheel {
            name: format!("python-{name}"),
            files,
            purelib: ap_ecosystem::sdist::wheel_root_is_purelib(wheel),
        });
    }
    out
}

/// The RPMs in `rpms_dir` built for the other wheels of `pkg_name`'s project,
/// as `(flavor-neutral name, files relative to site-packages, purelib)`, one
/// flavor per package. Asking `rpm` for the file lists is the only way to know
/// what a sibling really installs once built; without `rpm` the result is empty.
fn sibling_rpm_files(
    rpms_dir: &std::path::Path,
    pkg_name: &str,
) -> Vec<(String, Vec<String>, Option<bool>)> {
    let Ok(entries) = std::fs::read_dir(rpms_dir) else {
        return Vec::new();
    };
    let mut paths: Vec<(String, std::path::PathBuf)> = entries
        .flatten()
        .filter_map(|e| {
            let file = e.file_name().to_string_lossy().into_owned();
            sibling_rpm_name(&file, pkg_name).map(|_| (file, e.path()))
        })
        .collect();
    paths.sort();
    let mut out: Vec<(String, Vec<String>, Option<bool>)> = Vec::new();
    for (file, path) in paths {
        let Some(name) = sibling_rpm_name(&file, pkg_name) else {
            continue;
        };
        if out.iter().any(|(n, _, _)| *n == name) {
            continue;
        }
        let Ok(listing) = std::process::Command::new("rpm")
            .args(["-qlp", "--nosignature"])
            .arg(&path)
            .output()
        else {
            return Vec::new();
        };
        if !listing.status.success() {
            continue;
        }
        let text = String::from_utf8_lossy(&listing.stdout);
        let files: Vec<String> = site_package_files(&text)
            .into_iter()
            .filter(|f| {
                f.split('/').next().is_some_and(|t| !t.ends_with(".dist-info")) && f.contains('/')
            })
            .collect();
        if !files.is_empty() {
            let purelib = if text.contains("/usr/lib64/") {
                Some(false)
            } else if text.contains("/usr/lib/python") {
                Some(true)
            } else {
                None
            };
            out.push((name, files, purelib));
        }
    }
    out
}

/// Whether the (lowercase, dash-separated) package `name` is another wheel of
/// the project `pkg_name` belongs to: not `pkg_name` itself, and sharing all
/// but the last two words with it, at least two.
fn is_sibling_project(name: &str, pkg_name: &str) -> bool {
    let own = pkg_name.to_lowercase().replace('_', "-");
    if name == own || name.ends_with("-debuginfo") || name.ends_with("-debugsource") {
        return false;
    }
    let a: Vec<&str> = name.split('-').collect();
    let b: Vec<&str> = own.split('-').collect();
    let common = a.iter().zip(&b).take_while(|(x, y)| x == y).count();
    let need = a.len().min(b.len()).saturating_sub(2).max(2);
    common >= need
}

/// The flavor-neutral package name of the RPM file `file` when it belongs to a
/// sibling wheel project of `pkg_name` (see [`sibling_wheels`]); `None`
/// for another project, for `pkg_name` itself, and for a source or debug RPM.
fn sibling_rpm_name(file: &str, pkg_name: &str) -> Option<String> {
    let stem = file.strip_suffix(".rpm")?;
    if stem.ends_with(".src") || stem.ends_with(".nosrc") {
        return None;
    }
    let rest = stem.strip_prefix("python3")?;
    let digits = rest.chars().take_while(|c| c.is_ascii_digit()).count();
    if digits == 0 {
        return None;
    }
    let base = rest[digits..].strip_prefix('-')?;
    // `<name>-<version>-<release>.<arch>`: cut the last two dashes.
    let mut parts = base.rsplitn(3, '-');
    let (_release, _version, name) = (parts.next()?, parts.next()?, parts.next()?);
    let name = name.to_lowercase();
    is_sibling_project(&name, pkg_name).then(|| format!("python-{name}"))
}

/// The paths below `site-packages` in an `rpm -ql` listing, for files only
/// (directory lines end the same way, so a path that is a prefix of another is a
/// directory and dropped).
fn site_package_files(listing: &str) -> Vec<String> {
    let mut paths: Vec<String> = listing
        .lines()
        .filter_map(|l| l.split_once("/site-packages/").map(|(_, p)| p.trim().to_string()))
        .filter(|p| !p.is_empty())
        .collect();
    paths.sort();
    paths.dedup();
    let all = paths.clone();
    paths.retain(|p| !all.iter().any(|q| q.starts_with(&format!("{p}/"))));
    paths
}

#[cfg(test)]
mod shared_wheel_tests {
    use super::*;
    use ap_ecosystem::sdist::WheelLayout;

    fn layout(files: &[&str]) -> WheelLayout {
        WheelLayout {
            files: files.iter().map(|f| f.to_string()).collect(),
            dist_info: files
                .iter()
                .find_map(|f| f.split('/').next().filter(|t| t.ends_with(".dist-info")))
                .map(str::to_string),
            ..Default::default()
        }
    }

    fn rel() -> Release {
        Release {
            name: "x".into(),
            version: "1".into(),
            summary: Some("S".into()),
            homepage: Some("https://example.org".into()),
            license: License::Spdx("MIT".into()),
            requires_python: None,
            requires_dist: Vec::new(),
            yanked: false,
            sdist: ap_ecosystem::pypi::Sdist {
                filename: String::new(),
                url: String::new(),
                sha256: String::new(),
                size: 0,
            },
        }
    }

    fn files_of(spec: &str) -> Vec<String> {
        let f = spec.split("%files %{python_files}\n").nth(1).unwrap();
        f.split("%changelog").next().unwrap().lines().map(str::to_string).collect()
    }

    #[test]
    fn a_directory_a_sibling_fills_is_co_owned_not_listed_whole() {
        // -base owns the regular package `pk/_mlir`; -cu13 adds the arch half.
        let base = vec![SiblingWheel {
            name: "python-pk-base".to_string(),
            files: vec!["pk/_mlir/__init__.py".to_string(), "pk/_mlir/a.py".to_string()],
            purelib: Some(false),
        }];
        let l = layout(&[
            "pk/_mlir/_ods.py",
            "pk/_mlir/ir.cpython-313-x86_64-linux-gnu.so",
            "pk/lib/libx.so",
            "pk_cu13-1.dist-info/METADATA",
        ]);
        let spec = wheel_binary_skeleton_shared(
            "pk-cu13",
            "1",
            "pk_cu13-1-cp313-cp313-manylinux_2_28_x86_64.whl",
            &rel(),
            Some(false),
            Some(&l),
            &base,
        );
        let files = files_of(&spec);
        assert_eq!(
            files[..7].to_vec(),
            vec![
                "%dir %{python_sitearch}/pk",
                "%dir %{python_sitearch}/pk/_mlir",
                "%{python_sitearch}/pk/_mlir/_ods.py",
                "%{python_sitearch}/pk/_mlir/ir.cpython-313-x86_64-linux-gnu.so",
                "%dir %{python_sitearch}/pk/_mlir/__pycache__",
                "%{python_sitearch}/pk/_mlir/__pycache__/_ods.*.pyc",
                "%{python_sitearch}/pk/lib/",
            ]
        );
        assert!(spec.contains("Requires:       python-base\nRequires:       python-pk-base\n"), "{spec}");
        // Alone, the same wheel lists the package directory whole.
        let alone = wheel_binary_skeleton(
            "pk-cu13",
            "1",
            "pk_cu13-1-cp313-cp313-manylinux_2_28_x86_64.whl",
            &rel(),
            Some(false),
            Some(&l),
        );
        assert!(files_of(&alone).contains(&"%{python_sitearch}/pk/_mlir/".to_string()), "{alone}");
        assert!(!alone.contains("python-pk-base"));
    }

    #[test]
    fn the_owner_of_the_init_needs_no_requires_on_itself() {
        let l = layout(&["pk/__init__.py", "pk/b.py", "pk-1.dist-info/METADATA"]);
        let sw = |files: &[&str]| {
            vec![SiblingWheel {
                name: "python-pk-core".to_string(),
                files: files.iter().map(|f| f.to_string()).collect(),
                purelib: None,
            }]
        };
        let sib = sw(&["pk/c.py"]);
        assert!(shared_package_owners(&l, &sib).is_empty());
        let sib = sw(&["pk/__init__.py"]);
        assert!(shared_package_owners(&l, &sib).is_empty());
        let l2 = layout(&["pk/b.py"]);
        assert_eq!(shared_package_owners(&l2, &sib), vec!["python-pk-core".to_string()]);
    }

    #[test]
    fn siblings_are_found_by_name_and_listings_parse() {
        let n = |f| sibling_rpm_name(f, "nvidia-cutlass-dsl-libs-cu13");
        assert_eq!(
            n("python313-nvidia-cutlass-dsl-libs-base-4.5.0-1.1.x86_64.rpm").as_deref(),
            Some("python-nvidia-cutlass-dsl-libs-base")
        );
        assert_eq!(n("python313-nvidia-cutlass-dsl-libs-cu13-4.5.0-1.1.x86_64.rpm"), None);
        assert_eq!(n("python313-numpy-2.0-1.1.x86_64.rpm"), None);
        assert_eq!(n("python313-nvidia-cutlass-dsl-libs-base-4.5.0-1.1.src.rpm"), None);
        assert_eq!(n("python-nvidia-cutlass-dsl-libs-base-4.5.0-1.1.noarch.rpm"), None);
        let listing = "/usr/lib/python3.13/site-packages/pk\n/usr/lib/python3.13/site-packages/pk/a.py\n\
                       /usr/share/doc/x\n/usr/lib/python3.13/site-packages/pk/sub\n\
                       /usr/lib/python3.13/site-packages/pk/sub/b.so\n";
        assert_eq!(site_package_files(listing), vec!["pk/a.py", "pk/sub/b.so"]);
        assert!(sibling_wheels(std::path::Path::new("/nonexistent-r2b"), None, "x").is_empty());
    }
}

/// The four real wheels of NVIDIA's `nvidia-cutlass-dsl-libs-*` 4.7.1 (convention
/// 83): one namespace tree `nvidia_cutlass_dsl/` filled by `-base` (compiled MLIR
/// half, `Root-Is-Purelib: false`), `-core` (pure Python, `true`) and the
/// `-cu12`/`-cu13` flavors (a `.so` each, `false`). The file lists are the
/// wheels' own, copied from the zips.
#[cfg(test)]
mod cutlass_split_wheels_tests {
    use super::*;
    use std::collections::{BTreeMap, BTreeSet};
    use std::io::Write;

    /// `(short name, file list, Root-Is-Purelib, platform tag)`.
    const WHEELS: [(&str, &str, bool, &str); 4] = [
        (
            "base",
            include_str!("testdata/cutlass_dsl_libs_base.txt"),
            false,
            "cp313-cp313-manylinux_2_28_x86_64",
        ),
        (
            "core",
            include_str!("testdata/cutlass_dsl_libs_core.txt"),
            true,
            "py3-none-any",
        ),
        (
            "cu12",
            include_str!("testdata/cutlass_dsl_libs_cu12.txt"),
            false,
            "cp313-cp313-manylinux_2_28_x86_64",
        ),
        (
            "cu13",
            include_str!("testdata/cutlass_dsl_libs_cu13.txt"),
            false,
            "cp313-cp313-manylinux_2_28_x86_64",
        ),
    ];

    fn write_wheel(dir: &std::path::Path, short: &str, names: &str, purelib: bool, tag: &str) {
        std::fs::create_dir_all(dir).unwrap();
        let path = dir.join(format!("nvidia_cutlass_dsl_libs_{short}-4.7.1-{tag}.whl"));
        let mut zw = zip::ZipWriter::new(std::fs::File::create(path).unwrap());
        let opts = zip::write::SimpleFileOptions::default();
        for n in names.lines().filter(|l| !l.is_empty()) {
            zw.start_file(n, opts).unwrap();
            let body: String = if n.ends_with(".dist-info/WHEEL") {
                format!("Wheel-Version: 1.0\nRoot-Is-Purelib: {purelib}\nTag: {tag}\n")
            } else if n.ends_with(".dist-info/METADATA") {
                format!("Metadata-Version: 2.4\nName: nvidia-cutlass-dsl-libs-{short}\nVersion: 4.7.1\n")
            } else {
                "x".into()
            };
            zw.write_all(body.as_bytes()).unwrap();
        }
        zw.finish().unwrap();
    }

    /// A checkouts directory with the four packages' wheels, created in `order`.
    fn checkouts(order: &[usize]) -> tempfile::TempDir {
        let tmp = tempfile::tempdir().unwrap();
        for &i in order {
            let (short, names, purelib, tag) = WHEELS[i];
            write_wheel(
                &tmp.path().join(format!("python-nvidia-cutlass-dsl-libs-{short}")),
                short,
                names,
                purelib,
                tag,
            );
        }
        // Unrelated checkouts are no siblings.
        write_wheel(&tmp.path().join("python-nvidia-nccl-cu13"), "base", "x/y.py\n", true, "py3-none-any");
        tmp
    }

    fn rel() -> Release {
        Release {
            name: "x".into(),
            version: "4.7.1".into(),
            summary: Some("S".into()),
            homepage: Some("https://example.org".into()),
            license: License::Spdx("MIT".into()),
            requires_python: None,
            requires_dist: Vec::new(),
            yanked: false,
            sdist: ap_ecosystem::pypi::Sdist {
                filename: String::new(),
                url: String::new(),
                sha256: String::new(),
                size: 0,
            },
        }
    }

    /// The skeleton of wheel `i`, its siblings read from `checkouts` like the
    /// pipeline does.
    fn skeleton(i: usize, checkouts: &std::path::Path) -> String {
        let (short, _, _, tag) = WHEELS[i];
        let own = format!("nvidia-cutlass-dsl-libs-{short}");
        let wheel_dir = checkouts.join(format!("python-{own}"));
        let file = format!("nvidia_cutlass_dsl_libs_{short}-4.7.1-{tag}.whl");
        let wheel = wheel_dir.join(&file);
        let layout = ap_ecosystem::sdist::wheel_layout(&wheel).unwrap();
        let purelib = ap_ecosystem::sdist::wheel_root_is_purelib(&wheel);
        let siblings = sibling_wheels(
            std::path::Path::new("/nonexistent-r3a"),
            wheel_dir.parent(),
            &own,
        );
        wheel_binary_skeleton_shared(&own, "4.7.1", &file, &rel(), purelib, Some(&layout), &siblings)
    }

    fn files_section(spec: &str) -> Vec<String> {
        let f = spec.split("%files %{python_files}\n").nth(1).unwrap();
        f.split("%changelog")
            .next()
            .unwrap()
            .lines()
            .map(str::to_string)
            .filter(|l| !l.is_empty())
            .collect()
    }

    fn site_path(entry: &str) -> &str {
        entry
            .strip_prefix("%{python_sitelib}/")
            .or_else(|| entry.strip_prefix("%{python_sitearch}/"))
            .unwrap_or_else(|| panic!("not a site path: {entry}"))
    }

    #[test]
    fn siblings_are_found_from_their_wheels_whatever_order_they_appeared_in() {
        let a = checkouts(&[0, 1, 2, 3]);
        let b = checkouts(&[3, 2, 1, 0]);
        let none = std::path::Path::new("/nonexistent-r3a");
        for (i, (short, ..)) in WHEELS.iter().enumerate() {
            let own = format!("nvidia-cutlass-dsl-libs-{short}");
            let sa = sibling_wheels(none, Some(a.path()), &own);
            let sb = sibling_wheels(none, Some(b.path()), &own);
            assert_eq!(sa, sb, "{short}");
            let names: Vec<&str> = sa.iter().map(|s| s.name.as_str()).collect();
            let mut want: Vec<String> = WHEELS
                .iter()
                .filter(|(n, ..)| n != short)
                .map(|(n, ..)| format!("python-nvidia-cutlass-dsl-libs-{n}"))
                .collect();
            want.sort();
            assert_eq!(names, want, "{short}: nccl and itself are no siblings");
            assert_eq!(skeleton(i, a.path()), skeleton(i, b.path()), "{short}");
            // dist-info is never part of what a sibling owns.
            assert!(sa.iter().all(|s| s.files.iter().all(|f| !f.contains(".dist-info/"))));
        }
        let core = sibling_wheels(none, Some(a.path()), "nvidia-cutlass-dsl-libs-cu13")
            .into_iter()
            .find(|s| s.name.ends_with("-core"))
            .unwrap();
        assert_eq!(core.purelib, Some(true));
    }

    #[test]
    fn the_first_wheel_built_already_sees_the_siblings_that_have_not_been_built() {
        // Only the sibling's wheel exists: no RPM anywhere.
        let only_core = checkouts(&[1, 3]);
        let spec = skeleton(3, only_core.path());
        assert!(spec.contains("Requires:       python-nvidia-cutlass-dsl-libs-core\n"), "{spec}");
        // -cu13 shares the tree with the pure -core: both land in sitelib.
        assert!(spec.contains("%{python_sitelib}/nvidia_cutlass_dsl/cu13/lib/libcute_dsl_runtime.so"), "{spec}");
        // And alone (no sibling wheel at all) it follows its own WHEEL file.
        let alone = checkouts(&[3]);
        let spec = skeleton(3, alone.path());
        assert!(spec.contains("%{python_sitearch}/nvidia_cutlass_dsl/"), "{spec}");
        assert!(!spec.contains("python_sitelib"), "{spec}");
    }

    /// python-tokenspeed-mla's `%check`: `No module named 'cutlass.cute'`.
    /// `nvidia-cutlass-dsl`'s `.pth` puts `nvidia_cutlass_dsl.__path__[0] +
    /// /dsl_packages` on `sys.path`, and `nvidia_cutlass_dsl` is a namespace
    /// package (no `__init__.py` anywhere), so `__path__[0]` is the FIRST site
    /// directory that holds one: with the compiled wheels in sitearch (searched
    /// first on openSUSE) and `-core` in sitelib, `cutlass/__init__.py` was
    /// found and `cutlass/cute` was not. Merge what the four generated specs
    /// install, as rpm does, and resolve the imports on both interpreters.
    /// The specs of `-base` and `-cu12` were generated while `-core` was not
    /// known, so they sit in sitearch; the spec fix moves them to the tree.
    #[test]
    fn a_sibling_spec_generated_alone_is_moved_into_the_shared_tree() {
        let tmp = checkouts(&[0, 1, 2, 3]);
        let alone = checkouts(&[2]);
        let old = skeleton(2, alone.path());
        assert!(old.contains("%{python_sitearch}/nvidia_cutlass_dsl"), "{old}");
        let own = "nvidia-cutlass-dsl-libs-cu12";
        let wheel_dir = tmp.path().join(format!("python-{own}"));
        let wheel = wheel_dir.join("nvidia_cutlass_dsl_libs_cu12-4.7.1-cp313-cp313-manylinux_2_28_x86_64.whl");
        let layout = ap_ecosystem::sdist::wheel_layout(&wheel).unwrap();
        let siblings = sibling_wheels(std::path::Path::new("/nonexistent-r4b"), Some(tmp.path()), own);
        let fixed = with_sibling_tree_in_sitelib(&old, &layout, &siblings);
        assert_ne!(fixed, old);
        for l in files_section(&fixed) {
            assert!(!l.contains("python_sitearch"), "{l}\n{fixed}");
        }
        let install = fixed.find("%pyproject_install").unwrap();
        let mv = fixed
            .find("%python_expand mv %{buildroot}%{$python_sitearch}/nvidia_cutlass_dsl ")
            .expect(&fixed);
        let cleanup = fixed.find("rm -rf %{buildroot}%{$python_sitelib}/nvidia_cutlass_dsl/cu12/include").expect(&fixed);
        assert!(install < mv && mv < cleanup, "{fixed}");
        assert_eq!(with_sibling_tree_in_sitelib(&fixed, &layout, &siblings), fixed);
        // Without a pure sibling nothing changes.
        assert_eq!(with_sibling_tree_in_sitelib(&old, &layout, &[]), old);
    }

    #[test]
    fn the_merged_install_of_the_four_wheels_resolves_cutlass_cute_in_one_tree() {
        let tmp = checkouts(&[0, 1, 2, 3]);
        // site directory -> files, per interpreter flavor (the macros are the
        // same for both; each flavor expands them against its own directories).
        for flavor in ["python313", "python314"] {
            let mut trees: BTreeMap<&str, BTreeSet<String>> = BTreeMap::new();
            for i in 0..4 {
                let spec = skeleton(i, tmp.path());
                let (short, names, purelib, _) = WHEELS[i];
                // Where `%pyproject_install` puts the wheel: its WHEEL file decides.
                let mut placed: Vec<(&str, String)> = names
                    .lines()
                    .filter(|f| !f.is_empty() && !f.contains(".dist-info/"))
                    .map(|f| (if purelib { "sitelib" } else { "sitearch" }, f.to_string()))
                    .collect();
                // The relocation lines of the spec, each for every flavor.
                for line in spec.lines().filter(|l| l.contains("mv %{buildroot}")) {
                    assert!(line.starts_with("%python_expand "), "{short}: {line}");
                    let top = line
                        .split("%{$python_sitearch}/")
                        .nth(1)
                        .and_then(|r| r.split(' ').next())
                        .unwrap();
                    for (site, f) in placed.iter_mut() {
                        if f.split('/').next() == Some(top) {
                            *site = "sitelib";
                        }
                    }
                }
                // `import cutlass` runs the CUDA IR extension setup, which needs a
                // device (`cannot import name 'ir' from _mlir`): %check never runs it.
                let check = spec.split("%check\n").nth(1).unwrap().split("%files").next().unwrap();
                assert!(!check.contains("import cutlass"), "{short}: {check}");
                assert!(!check.contains("import nvidia_cutlass_dsl"), "{short}: {check}");
                // %files names the directory the file really is in.
                for l in files_section(&spec) {
                    assert!(!l.contains("python_sitearch"), "{flavor} {short}: {l}");
                }
                for (site, f) in placed {
                    trees.entry(site).or_default().insert(f);
                }
            }
            // One tree: nothing of the namespace is left in sitearch.
            assert_eq!(trees.keys().copied().collect::<Vec<_>>(), ["sitelib"], "{flavor}");
            let lib = &trees["sitelib"];
            // `sys.path` of an openSUSE interpreter: sitearch before sitelib;
            // `__path__[0]` is the first directory with `nvidia_cutlass_dsl/`.
            let first = ["sitearch", "sitelib"]
                .into_iter()
                .find(|s| {
                    trees.get(s).is_some_and(|t| {
                        t.iter().any(|f| f.starts_with("nvidia_cutlass_dsl/"))
                    })
                })
                .unwrap();
            assert_eq!(first, "sitelib", "{flavor}");
            // Every module of the import chain is in that one tree.
            for need in [
                "nvidia_cutlass_dsl/dsl_packages/cutlass/__init__.py",
                "nvidia_cutlass_dsl/dsl_packages/cutlass/cute/__init__.py",
                "nvidia_cutlass_dsl/dsl_packages/cutlass/_mlir/_mlir_libs/__init__.py",
                "nvidia_cutlass_dsl/dsl_packages/cutlass/_mlir/ir.py",
                "nvidia_cutlass_dsl/cu13/lib/libcute_dsl_runtime.so",
                "nvidia_cutlass_dsl/cu12/lib/libcute_dsl_runtime.so",
            ] {
                assert!(lib.contains(need), "{flavor}: {need} is not in sitelib");
            }
            // And every top-level package directory resolves in the one tree.
            let tops: BTreeSet<&str> =
                lib.iter().filter_map(|f| f.split('/').next()).collect();
            assert_eq!(tops.iter().copied().collect::<Vec<_>>(), ["nvidia_cutlass_dsl"], "{flavor}");
        }
    }

    #[test]
    fn the_four_skeletons_share_one_tree_without_listing_a_file_twice() {
        let tmp = checkouts(&[0, 1, 2, 3]);
        let specs: Vec<String> = (0..4).map(|i| skeleton(i, tmp.path())).collect();
        let payload = |i: usize| -> Vec<&'static str> {
            WHEELS[i]
                .1
                .lines()
                .filter(|f| !f.contains(".dist-info/") && !f.is_empty())
                .collect()
        };

        for (i, spec) in specs.iter().enumerate() {
            let (short, _, purelib, _) = WHEELS[i];
            // One site directory for the whole tree: sitelib, where the noarch
            // -core must be; the arch wheels carry ExclusiveArch for their .so.
            assert!(!spec.contains("python_sitearch}/nvidia_cutlass_dsl/"), "{short}: {spec}");
            if purelib {
                assert!(spec.contains("BuildArch:      noarch\n"), "{short}");
                assert!(!spec.contains("ExclusiveArch"), "{short}");
                assert!(!spec.contains("%python_expand mv "), "{short}: {spec}");
            } else {
                assert!(spec.contains("ExclusiveArch:  x86_64\n"), "{short}");
                // The wheel's tree (and its dist-info) is moved next to -core's.
                assert!(
                    spec.contains(
                        "%python_expand mv %{buildroot}%{$python_sitearch}/nvidia_cutlass_dsl %{buildroot}%{$python_sitelib}/\n"
                    ),
                    "{short}: {spec}"
                );
                assert!(spec.contains(&format!(
                    "%python_expand mv %{{buildroot}}%{{$python_sitearch}}/nvidia_cutlass_dsl_libs_{short}-4.7.1.dist-info "
                )), "{short}: {spec}");
            }
            // Nothing writes into another package's dist-info.
            assert!(!spec.contains("RECORD"), "{short}");
        }

        // What each package owns: exact files, plus whole directories (`.../`).
        let mut dirs_listed: Vec<BTreeSet<String>> = Vec::new();
        let mut explicit: Vec<BTreeSet<String>> = Vec::new();
        let mut whole: Vec<BTreeSet<String>> = Vec::new();
        for (i, spec) in specs.iter().enumerate() {
            let mut d = BTreeSet::new();
            let mut e = BTreeSet::new();
            let mut w = BTreeSet::new();
            for line in files_section(spec) {
                if let Some(dir) = line.strip_prefix("%dir ") {
                    assert!(d.insert(site_path(dir).to_string()), "{}: %dir twice: {line}", WHEELS[i].0);
                } else if line.starts_with("%exclude ") || line.starts_with("%license ") {
                    continue;
                } else if line.contains("dist-info") || line.contains("/__pycache__/") {
                    continue;
                } else if let Some(dir) = line.strip_suffix('/') {
                    w.insert(site_path(dir).to_string());
                } else {
                    assert!(e.insert(site_path(&line).to_string()), "{}: listed twice: {line}", WHEELS[i].0);
                }
            }
            dirs_listed.push(d);
            explicit.push(e);
            whole.push(w);
        }

        // Which packages put a file below each directory.
        let mut fillers: BTreeMap<String, BTreeSet<usize>> = BTreeMap::new();
        for i in 0..4 {
            for f in payload(i) {
                let mut dir = f;
                while let Some((parent, _)) = dir.rsplit_once('/') {
                    fillers.entry(parent.to_string()).or_default().insert(i);
                    dir = parent;
                }
            }
        }
        // No two packages own one file: an explicit entry is one package's, a
        // whole directory contains no other package's file.
        for i in 0..4 {
            for w in &whole[i] {
                for j in (0..4).filter(|j| *j != i) {
                    assert!(
                        payload(j).iter().all(|f| !f.starts_with(&format!("{w}/"))),
                        "{} lists {w}/ whole but {} has files in it",
                        WHEELS[i].0,
                        WHEELS[j].0
                    );
                }
            }
            for j in (i + 1)..4 {
                assert!(
                    explicit[i].is_disjoint(&explicit[j]),
                    "{} and {} list the same file: {:?}",
                    WHEELS[i].0,
                    WHEELS[j].0,
                    explicit[i].intersection(&explicit[j]).collect::<Vec<_>>()
                );
                assert!(dirs_listed[i].is_disjoint(&whole[j]), "a co-owned directory is whole in another package");
            }
        }
        // A directory two packages fill is `%dir` in each of them, listed whole
        // by none.
        for (dir, who) in &fillers {
            if who.len() < 2 {
                continue;
            }
            for &i in who {
                assert!(
                    dirs_listed[i].contains(dir),
                    "{} fills the shared directory {dir} without owning it with %dir",
                    WHEELS[i].0
                );
                assert!(!whole[i].contains(dir), "{} lists {dir}/ whole", WHEELS[i].0);
            }
        }
        // Every payload file of a package is owned by it: listed, or below a
        // whole directory (bundled headers, stubs and the static archive are
        // removed in %install, bytecode comes with its module).
        for i in 0..4 {
            for f in payload(i) {
                if f.contains("/include/") || f.contains("/lib/stubs/") || f.ends_with(".a") {
                    continue;
                }
                let owned = explicit[i].contains(f)
                    || whole[i].iter().any(|w| f.starts_with(&format!("{w}/")));
                assert!(owned, "{} does not own {f}\n{}", WHEELS[i].0, specs[i]);
            }
        }
        // The arch-specific .so stay where the wheel puts them relative to the
        // rest of the tree: cu12/cu13's `_cutlass_ir.cuNN...so` is in `_mlir_libs`
        // beside -base's `__init__.py`, as the wheels were built to be.
        let cu13_so = "nvidia_cutlass_dsl/dsl_packages/cutlass/_mlir/_mlir_libs/_cutlass_ir.cu13.cpython-313-x86_64-linux-gnu.so";
        assert!(explicit[3].contains(cu13_so));
        assert!(WHEELS[0].1.contains("_mlir_libs/__init__.py"));

        // The import chain: -cu13 and -base need -core (`cutlass/__init__.py`);
        // -cu13 needs -base (`_mlir_libs/__init__.py`); -core needs neither.
        assert!(specs[0].contains("Requires:       python-nvidia-cutlass-dsl-libs-core\n"));
        assert!(specs[3].contains("Requires:       python-nvidia-cutlass-dsl-libs-base\n"));
        assert!(specs[3].contains("Requires:       python-nvidia-cutlass-dsl-libs-core\n"));
        assert!(!specs[1].contains("Requires:       python-nvidia-cutlass-dsl-libs-"));
    }
}

/// Copy into `checkout` the files of `missing` (names a spec's `Source`/`Patch`
/// lines give) that the unpacked source `tree` already holds, at its top or in
/// its single top-level directory: a lock file (`package-lock.json`) a
/// `node_modules` service reads, a licence, a config the spec ships as its own
/// source. The agent cannot copy them itself (open-webui: `Source10:
/// package-lock.json` stayed missing because the `cp` was refused), and nothing
/// else is needed to fix it. Names with a directory part, symlinks and files
/// over 64 MiB are left alone; an existing file is never overwritten. Returns
/// the names copied.
pub fn adopt_sources_from_tree(
    missing: &[String],
    tree: &std::path::Path,
    checkout: &std::path::Path,
) -> Vec<String> {
    let mut roots = vec![tree.to_path_buf()];
    if let Ok(entries) = std::fs::read_dir(tree) {
        let dirs: Vec<_> = entries
            .flatten()
            .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
            .map(|e| e.path())
            .collect();
        if dirs.len() == 1 {
            roots.insert(0, dirs[0].clone());
        }
    }
    let mut copied = Vec::new();
    for name in missing {
        if name.is_empty() || name.contains('/') || name.starts_with('.') {
            continue;
        }
        let dest = checkout.join(name);
        if dest.exists() {
            continue;
        }
        let found = roots.iter().map(|r| r.join(name)).find(|c| {
            std::fs::symlink_metadata(c)
                .is_ok_and(|m| m.is_file() && m.len() <= 64 * 1024 * 1024)
        });
        if let Some(src) = found
            && std::fs::copy(&src, &dest).is_ok()
        {
            copied.push(name.clone());
        }
    }
    copied
}

#[cfg(test)]
mod adopt_sources_tests {
    use super::adopt_sources_from_tree;

    /// python-open-webui: Source10 is the sdist's own package-lock.json.
    #[test]
    fn a_source_the_unpacked_tree_holds_is_copied_into_the_checkout() {
        let tmp = tempfile::tempdir().unwrap();
        let tree = tmp.path().join("sdist");
        let top = tree.join("open_webui-0.12.2");
        std::fs::create_dir_all(top.join("deep")).unwrap();
        std::fs::write(top.join("package-lock.json"), "{}").unwrap();
        std::fs::write(top.join("deep/other.json"), "{}").unwrap();
        let checkout = tmp.path().join("co");
        std::fs::create_dir_all(&checkout).unwrap();
        std::fs::write(checkout.join("keep.txt"), "mine").unwrap();
        std::fs::write(top.join("keep.txt"), "theirs").unwrap();
        let missing: Vec<String> = ["package-lock.json", "other.json", "keep.txt", "a/b", "nope"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let copied = adopt_sources_from_tree(&missing, &tree, &checkout);
        assert_eq!(copied, ["package-lock.json"]);
        assert_eq!(std::fs::read_to_string(checkout.join("package-lock.json")).unwrap(), "{}");
        assert_eq!(std::fs::read_to_string(checkout.join("keep.txt")).unwrap(), "mine");
        assert!(!checkout.join("other.json").exists());
    }
}

/// `text` for a spec tag or description: one line, every `%` doubled so rpm does
/// not expand a macro out of upstream's wording.
fn rpm_text(text: &str) -> String {
    text.replace('%', "%%").split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The `%description` body of a wheel spec: the METADATA's first paragraph,
/// wrapped, with every `%` doubled (rpm expands macros in a description), or the
/// summary when the wheel has none.
fn wheel_description(paragraph: Option<&str>, summary: &str) -> String {
    let text = paragraph.map(str::trim).filter(|t| !t.is_empty()).unwrap_or(summary);
    let mut lines: Vec<String> = Vec::new();
    let mut line = String::new();
    for word in text.replace('%', "%%").split_whitespace() {
        if !line.is_empty() && line.len() + 1 + word.len() > 76 {
            lines.push(std::mem::take(&mut line));
        }
        if !line.is_empty() {
            line.push(' ');
        }
        line.push_str(word);
    }
    lines.push(line);
    lines.join("\n")
}

/// Whether `file` (a path inside a wheel) is a licence text: a licence-named
/// leaf anywhere, or anything under `<dist-info>/licenses/`.
fn is_license_path(file: &str) -> bool {
    let leaf = file.rsplit('/').next().unwrap_or(file).to_ascii_uppercase();
    file.contains(".dist-info/licenses/")
        || ["LICENSE", "LICENCE", "COPYING"]
            .iter()
            .any(|p| leaf.starts_with(p))
}

/// Whether the wheel ships a licence text of its own.
pub fn wheel_has_license_file(layout: &ap_ecosystem::sdist::WheelLayout) -> bool {
    layout.files.iter().any(|f| is_license_path(f))
}

/// The leaf name and bytes of the licence file at the top of a `.tar.gz` sdist
/// (`LICENSE`, `LICENSE.txt`, `COPYING`; the shortest name wins), at most 1 MiB.
pub fn sdist_top_level_license(tarball: &std::path::Path) -> Option<(String, Vec<u8>)> {
    use std::io::Read;
    let file = std::fs::File::open(tarball).ok()?;
    let mut archive = tar::Archive::new(flate2::read::GzDecoder::new(file));
    let mut best: Option<(String, Vec<u8>)> = None;
    for entry in archive.entries().ok()? {
        let Ok(entry) = entry else { continue };
        if !entry.header().entry_type().is_file() || entry.size() > (1 << 20) {
            continue;
        }
        let Ok(path) = entry.path() else { continue };
        let parts: Vec<String> = path
            .components()
            .map(|c| c.as_os_str().to_string_lossy().into_owned())
            .collect();
        let [_top, leaf] = parts.as_slice() else { continue };
        let upper = leaf.to_ascii_uppercase();
        let plain = leaf
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'));
        let named = ["LICENSE", "LICENCE", "COPYING"]
            .iter()
            .any(|p| upper.starts_with(p));
        if !plain || !named || upper.ends_with(".PY") {
            continue;
        }
        if best.as_ref().is_some_and(|(b, _)| b.len() <= leaf.len()) {
            continue;
        }
        let leaf = leaf.clone();
        let mut bytes = Vec::new();
        if entry.take(1 << 20).read_to_end(&mut bytes).is_ok() {
            best = Some((leaf, bytes));
        }
    }
    best
}

/// A wheel with no licence text of its own, whose sdist has one: copy it from
/// the sdist in `dir` (the checkout) next to the wheel and return its file name,
/// for [`with_license_source`]. `None` when the wheel has one, there is no sdist
/// there, or the sdist has none.
pub fn copy_sdist_license(
    dir: &std::path::Path,
    version: &str,
    layout: &ap_ecosystem::sdist::WheelLayout,
) -> Option<String> {
    if wheel_has_license_file(layout) {
        return None;
    }
    let mut tarballs: Vec<std::path::PathBuf> = std::fs::read_dir(dir)
        .ok()?
        .filter_map(|e| Some(e.ok()?.path()))
        .filter(|p| {
            p.file_name()
                .map(|n| n.to_string_lossy())
                .is_some_and(|n| n.ends_with(".tar.gz") && n.contains(version))
        })
        .collect();
    tarballs.sort();
    let (name, bytes) = tarballs.iter().find_map(|t| sdist_top_level_license(t))?;
    std::fs::write(dir.join(&name), bytes).ok()?;
    Some(name)
}

/// `spec` of a prebuilt wheel with the licence `file` (copied from the sdist by
/// [`copy_sdist_license`]) as `Source1`, copied to the build directory in
/// `%prep` and owned as `%license`, for a wheel that carries no licence text.
/// Left alone when the spec has a `%license` or a `Source1` already.
pub fn with_license_source(spec: &str, file: &str) -> String {
    if spec.contains("%license") || spec.contains("Source1:") {
        return spec.to_string();
    }
    let mut out = String::with_capacity(spec.len() + 160);
    for line in spec.split_inclusive('\n') {
        out.push_str(line);
        let t = line.trim_end();
        if t.starts_with("Source0:") {
            out.push_str(&format!(
                "# the wheel has no licence text; this one is the sdist's\nSource1:        {file}\n"
            ));
        } else if t == "%prep" {
            out.push_str(&format!("cp -p %{{SOURCE1}} {file}\n"));
        } else if t.starts_with("%files") && t.contains("%{python_files}") {
            out.push_str(&format!("%license {file}\n"));
        }
    }
    out
}

#[cfg(test)]
mod sdist_license_tests {
    use super::*;

    fn tarball(dir: &std::path::Path, files: &[(&str, &[u8])]) -> std::path::PathBuf {
        let path = dir.join("x-1.0.tar.gz");
        let gz = flate2::write::GzEncoder::new(
            std::fs::File::create(&path).unwrap(),
            flate2::Compression::fast(),
        );
        let mut b = tar::Builder::new(gz);
        for (name, data) in files {
            let mut h = tar::Header::new_gnu();
            h.set_size(data.len() as u64);
            h.set_mode(0o644);
            h.set_cksum();
            b.append_data(&mut h, name, *data).unwrap();
        }
        b.into_inner().unwrap().finish().unwrap();
        path
    }

    #[test]
    fn the_top_level_licence_is_found_and_deeper_ones_are_not() {
        let dir = tempfile::tempdir().unwrap();
        let t = tarball(
            dir.path(),
            &[
                ("x-1.0/src/LICENSE", b"deep".as_slice()),
                ("x-1.0/LICENSE.txt", b"mit".as_slice()),
                ("x-1.0/README.md", b"r".as_slice()),
            ],
        );
        let (name, bytes) = sdist_top_level_license(&t).unwrap();
        assert_eq!((name.as_str(), bytes.as_slice()), ("LICENSE.txt", b"mit".as_slice()));
    }

    #[test]
    fn the_spec_gets_source1_prep_copy_and_license_entry() {
        let spec = "Source0:        x.whl
Source99:       r
%prep
# n

%files %{python_files}
%{python_sitelib}/x/
";
        let out = with_license_source(spec, "LICENSE");
        assert!(out.contains("Source0:        x.whl
# the wheel has no licence text; this one is the sdist's
Source1:        LICENSE
"), "{out}");
        assert!(out.contains("%prep
cp -p %{SOURCE1} LICENSE
"), "{out}");
        assert!(out.contains("%files %{python_files}
%license LICENSE
"), "{out}");
        assert_eq!(with_license_source(&out, "LICENSE"), out);
    }
}

/// `spec` with a note under `%prep` that the release has no sdist and no source
/// repository, so the unpacked wheel is all there is. Only for a true no-sdist
/// release; a package switched to its wheel after a failed source build has a
/// sdist and gets no such note.
pub fn with_no_sdist_note(spec: &str) -> String {
    const NOTE: &str = "# this release has no sdist and no source repository: the upstream wheel is\n\
                        # installed as it is, so check the architecture and the file list against it\n";
    let mut out = String::with_capacity(spec.len() + NOTE.len());
    let mut done = false;
    for line in spec.split_inclusive('\n') {
        out.push_str(line);
        if !done && line.trim_end() == "%prep" {
            out.push_str(NOTE);
            done = true;
        }
    }
    out
}

/// The `(major, minor)` of a Python flavor name: `python313` -> (3, 13),
/// `python39` -> (3, 9), `python314t` -> (3, 14).
fn flavor_version(flavor: &str) -> Option<(u32, u32)> {
    let rest = flavor.strip_prefix("python")?.trim_end_matches('t');
    let (major, minor) = rest.split_at_checked(1)?;
    Some((major.parse().ok()?, minor.parse().ok()?))
}

/// Whether a `Requires-Python` specifier excludes a flavor the project builds
/// through an upper bound (`<3.13`, `<=3.12`, `==3.12.*`): pip then refuses to
/// install the wheel for that flavor unless told otherwise.
pub fn requires_python_caps_below(requires_python: &str, flavors: &[String]) -> bool {
    let versions: Vec<(u32, u32)> = flavors.iter().filter_map(|f| flavor_version(f)).collect();
    let parse = |v: &str| -> Option<(u32, u32)> {
        let mut it = v.trim().trim_end_matches(".*").split('.');
        let major = it.next()?.parse().ok()?;
        let minor = it.next().map_or(Some(0), |m| m.parse().ok())?;
        Some((major, minor))
    };
    requires_python.split(',').map(str::trim).any(|clause| {
        if let Some(v) = clause.strip_prefix("<=") {
            // `<=3.12` still allows 3.12.x; `<=3.12.5` allows all of 3.12.
            parse(v).is_some_and(|cap| versions.iter().any(|f| *f > cap))
        } else if let Some(v) = clause.strip_prefix('<') {
            let patch = v.trim().split('.').count() > 2;
            parse(v).is_some_and(|cap| {
                versions
                    .iter()
                    .any(|f| if patch { *f > cap } else { *f >= cap })
            })
        } else if let Some(v) = clause.strip_prefix("==") {
            v.trim().ends_with(".*") && parse(v).is_some_and(|only| versions.iter().any(|f| *f != only))
        } else {
            false
        }
    })
}

/// `spec` with `export PIP_IGNORE_REQUIRES_PYTHON=1` as the first line of `%build`
/// and `%install` (each is its own shell and each may run pip), when the wheel's
/// `Requires-Python` has an upper bound below a flavor that is built. A spec that
/// already sets the variable is returned as it is.
pub fn with_pip_ignore_requires_python(
    spec: &str,
    requires_python: Option<&str>,
    flavors: &[String],
) -> String {
    if spec.contains("PIP_IGNORE_REQUIRES_PYTHON")
        || !requires_python.is_some_and(|r| requires_python_caps_below(r, flavors))
    {
        return spec.to_string();
    }
    let mut out = String::with_capacity(spec.len() + 160);
    for line in spec.split_inclusive('\n') {
        out.push_str(line);
        if matches!(line.trim_end(), "%build" | "%install") {
            out.push_str(
                "# the wheel caps Requires-Python below a Python we build for\n\
                 export PIP_IGNORE_REQUIRES_PYTHON=1\n",
            );
        }
    }
    out
}

/// Which native build system a plain C/C++ project uses. Decided by
/// `ap_ecosystem::sdist::SdistInfo::cmake_manifest`/`autotools_manifest`.
pub enum CBuildSystem {
    Cmake,
    Autotools,
}

/// Build a skeleton spec for a plain C/C++ project: no Python or Cargo
/// packaging anywhere in the archive, just a native build system. This is a
/// third ecosystem for the generator (Python and Rust are the other two), and
/// a much less uniform one — whether the project ships a shared library
/// needing a `lib<name><soversion>`/`-devel` split, and which `-devel`/
/// `pkgconfig()` BuildRequires its `find_package()`/`pkg_check_modules()`
/// calls need, both require reading `CMakeLists.txt`/`configure.ac` and
/// `source/` directly. The skeleton stays deliberately bare; see convention 29.
pub fn c_project_skeleton(
    pkg_name: &str,
    version: &str,
    url: &str,
    top_level_dir: &str,
    build: CBuildSystem,
) -> String {
    let name = pkg_name.to_lowercase();
    let (build_kind, build_requires, build_section, install_section): (&str, &str, &str, &str) =
        match build {
            CBuildSystem::Cmake => (
                "CMake",
                "BuildRequires:  cmake\n\
             BuildRequires:  gcc-c++\n\
             BuildRequires:  pkg-config\n",
                "%cmake\n\
             %cmake_build\n",
                "%cmake_install\n",
            ),
            CBuildSystem::Autotools => (
                "autotools",
                "BuildRequires:  autoconf\n\
             BuildRequires:  automake\n\
             BuildRequires:  libtool\n\
             BuildRequires:  gcc-c++\n\
             BuildRequires:  pkg-config\n",
                // A release archive is a repository snapshot, not a `make dist`
                // tarball — it has configure.ac/Makefile.am but no generated
                // `configure` script yet (see libuv.spec's `./autogen.sh`, the
                // real-world precedent this mirrors).
                "autoreconf -fiv\n\
             %configure\n\
             %make_build\n",
                "%make_install\n\
             find %{buildroot} -type f -name \"*.la\" -delete -print\n",
            ),
        };

    format!(
        "#\n\
         # spec file for package {name}\n\
         #\n\
         # Copyright (c) 2026 SUSE LLC\n\
         #\n\
         # All modifications and additions to the file contributed by third parties\n\
         # remain the property of their copyright owners, unless otherwise agreed\n\
         # upon. The license for this file, and modifications and additions to the\n\
         # file, is the same license as for the pristine package itself (unless the\n\
         # license for the pristine package is not an Open Source License, in which\n\
         # case the license is the MIT License). An \"Open Source License\" is a\n\
         # license that conforms to the Open Source Definition (Version 1.9)\n\
         # published by the Open Source Initiative.\n\
         \n\
         # Please submit bugfixes or comments via https://bugs.opensuse.org/\n\
         \n\
         \n\
         Name:           {name}\n\
         Version:        {version}\n\
         Release:        0\n\
         Summary:        TODO: fill in summary\n\
         License:        TODO: fill in SPDX license - read LICENSE/COPYING in source/\n\
         URL:            {url}\n\
         Source0:        %{{name}}-%{{version}}.tar.gz\n\
         {build_requires}\
         \n\
         %description\n\
         TODO: fill in description\n\
         \n\
         # No Python or Cargo packaging found anywhere in this archive - a plain\n\
         # C/C++ project built with {build_kind}. See convention 29: decide whether\n\
         # this ships a shared library needing a lib%%{{name}}<soversion>/%%{{name}}-devel\n\
         # split, find the -devel/pkgconfig() BuildRequires the build actually needs,\n\
         # and add %%ldconfig_scriptlets to any shared-library subpackage.\n\
         \n\
         %prep\n\
         %autosetup -n {top_level_dir}\n\
         \n\
         %build\n\
         {build_section}\
         \n\
         %install\n\
         {install_section}\
         \n\
         %check\n\
         # TODO: run the test suite if there is one\n\
         \n\
         %files\n\
         # TODO: list the files %%install put in the buildroot - convention 15's\n\
         # \"no globs\" rule applies here too.\n\
         %license TODO\n\
         \n\
         %changelog\n",
        name = name,
        version = version,
        url = url,
        build_requires = build_requires,
        build_kind = build_kind,
        top_level_dir = top_level_dir,
        build_section = build_section,
        install_section = install_section,
    )
}

/// SPDX id for an OSI classifier name, if it is one we know for certain.
fn classifier_spdx(name: &str) -> Option<&'static str> {
    Some(match name {
        "MIT License" => "MIT",
        "Apache Software License" => "Apache-2.0",
        "BSD License" => return None, // 2- vs 3-clause is not recoverable
        "ISC License (ISCL)" => "ISC",
        "Mozilla Public License 2.0 (MPL 2.0)" => "MPL-2.0",
        "GNU General Public License v2 (GPLv2)" => "GPL-2.0-only",
        "GNU General Public License v2 or later (GPLv2+)" => "GPL-2.0-or-later",
        "GNU General Public License v3 (GPLv3)" => "GPL-3.0-only",
        "GNU General Public License v3 or later (GPLv3+)" => "GPL-3.0-or-later",
        "GNU Lesser General Public License v3 (LGPLv3)" => "LGPL-3.0-only",
        "GNU Lesser General Public License v3 or later (LGPLv3+)" => "LGPL-3.0-or-later",
        "GNU Affero General Public License v3" => "AGPL-3.0-only",
        "Python Software Foundation License" => "PSF-2.0",
        "The Unlicense (Unlicense)" => "Unlicense",
        _ => return None,
    })
}

/// The `License:` value a wheel's own METADATA supports, or `None`: the SPDX
/// `License-Expression`, else the `License ::` classifiers that map to one SPDX
/// id, else a free-text `License:` that is a known SPDX id or spelling of one
/// (`MIT`, `Apache 2.0`, `3-clause BSD`) or NVIDIA's proprietary redistribution
/// text. Anything else stays `FIXME` for a person: a guessed licence is worse
/// than a flagged one.
fn wheel_license(layout: &ap_ecosystem::sdist::WheelLayout) -> Option<String> {
    if let Some(expr) = &layout.license_expression {
        return Some(spdx_expression_for_spec(expr));
    }
    if !layout.license_classifiers.is_empty() {
        let tag = license_tag(&License::Classifiers(layout.license_classifiers.clone()));
        if tag != "FIXME" {
            return Some(tag);
        }
    }
    spdx_from_free_text(layout.license_text.as_deref()?)
}

/// An SPDX expression for `License:`: a vendor's own licence reference
/// (`LicenseRef-NVIDIA-Proprietary`) is not a SPDX id and becomes the non-free
/// tag; anything else is kept as declared.
fn spdx_expression_for_spec(expr: &str) -> String {
    // Only the term that names the vendor licence is replaced: the rest of an
    // expression (`MIT AND LicenseRef-NVIDIA-Proprietary`) stays.
    let mut out = String::new();
    let mut word = String::new();
    let flush = |word: &mut String, out: &mut String| {
        if word.is_empty() {
            return;
        }
        let low = word.to_ascii_lowercase();
        if low.contains("nvidia") || low.contains("proprietary") {
            out.push_str(NVIDIA_PROPRIETARY);
        } else {
            out.push_str(word);
        }
        word.clear();
    };
    for c in expr.chars() {
        if c.is_whitespace() || c == '(' || c == ')' {
            flush(&mut word, &mut out);
            out.push(c);
        } else {
            word.push(c);
        }
    }
    flush(&mut word, &mut out);
    // Two vendor terms are one. Dropping the second would leave its operator
    // dangling (`MIT AND SUSE-NonFree AND`), so the repeat is merged with the
    // operator that joins it instead.
    for op in ["AND", "OR"] {
        let twice = format!("{NVIDIA_PROPRIETARY} {op} {NVIDIA_PROPRIETARY}");
        while out.contains(&twice) {
            out = out.replace(&twice, NVIDIA_PROPRIETARY);
        }
    }
    out
}

/// The SPDX id a free-text licence name spells (`MIT`, `Apache 2.0`,
/// `3-clause BSD`), or the non-free tag for NVIDIA's proprietary redistribution
/// text; `None` for anything else.
fn spdx_from_free_text(text: &str) -> Option<String> {
    let key: String = text
        .to_ascii_lowercase()
        .replace(['_', ',', '.'], " ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    let spdx = match key.as_str() {
        "mit" | "mit license" => "MIT",
        "mit-0" => "MIT-0",
        "apache 2 0" | "apache-2 0" | "apache license 2 0" | "apache license version 2 0"
        | "apache software license" | "apache software license 2 0" | "apache 2" => "Apache-2.0",
        "bsd-3-clause" | "bsd 3-clause" | "bsd 3 clause" | "3-clause bsd" | "3 clause bsd"
        | "new bsd" | "modified bsd" | "bsd-3" => "BSD-3-Clause",
        "bsd-2-clause" | "bsd 2-clause" | "bsd 2 clause" | "2-clause bsd" | "simplified bsd"
        | "freebsd" => "BSD-2-Clause",
        "isc" | "isc license" => "ISC",
        "mpl-2 0" | "mpl 2 0" | "mozilla public license 2 0" => "MPL-2.0",
        "psf" | "psf-2 0" | "python software foundation license" => "PSF-2.0",
        "unlicense" | "the unlicense" => "Unlicense",
        "zlib" => "Zlib",
        "0bsd" => "0BSD",
        _ => {
            let nvidia = key.contains("nvidia")
                && ["proprietary", "license agreement", "software license"]
                    .iter()
                    .any(|w| key.contains(w));
            return nvidia.then(|| NVIDIA_PROPRIETARY.to_string());
        }
    };
    Some(spdx.to_string())
}

/// The `License:` of a repackaged wheel: the release's own licence when it is a
/// usable tag, else what the wheel's METADATA supports (see [`wheel_license`]).
/// A free-text licence that is a known spelling is mapped to its SPDX id; one
/// that is prose or pasted licence text (multi-line, long) is never written into
/// the tag. A vendor's proprietary licence is the non-free tag.
fn wheel_declared_license(
    release: &Release,
    layout: Option<&ap_ecosystem::sdist::WheelLayout>,
) -> String {
    let own = match &release.license {
        License::FreeText(text) => {
            let text = text.trim();
            match spdx_from_free_text(text) {
                Some(id) => id,
                None if text.is_empty() || text.contains('\n') || text.len() > 100 => {
                    "FIXME".to_string()
                }
                None => text.to_string(),
            }
        }
        License::Spdx(expr) => spdx_expression_for_spec(expr),
        other => license_tag(other),
    };
    match own.as_str() {
        "FIXME" => layout.and_then(wheel_license).unwrap_or(own),
        _ => own,
    }
}

/// The licence of a GCC runtime library bundled in a wheel (torch's `libgomp`).
const GCC_RUNTIME_LICENSE: &str = "GPL-3.0-or-later WITH GCC-exception-3.1";

/// Licence terms a wheel's own `License:` does not carry because the wheel
/// bundles a third party's binaries: NVIDIA's toolchain programs and the CUPTI
/// libraries (triton declares MIT but ships `ptxas`) are non-free, and GCC's
/// `libgomp` is GPL with the runtime exception. In a fixed order, no
/// duplicates; found from the file names only.
fn wheel_vendor_license_terms(layout: &ap_ecosystem::sdist::WheelLayout) -> Vec<&'static str> {
    let mut nonfree = false;
    let mut gcc = false;
    for file in &layout.files {
        let top = file.split('/').next().unwrap_or("");
        if top.ends_with(".dist-info") || top.ends_with(".data") {
            continue;
        }
        let name = file.rsplit('/').next().unwrap_or(file);
        if matches!(
            name,
            "ptxas" | "ptxas-blackwell" | "cuobjdump" | "nvdisasm" | "nvlink" | "fatbinary" | "nvprune"
        ) || (name.starts_with("libcupti") && name.contains(".so"))
        {
            nonfree = true;
        }
        if name.starts_with("libgomp") && name.contains(".so") {
            gcc = true;
        }
    }
    let mut terms = Vec::new();
    if nonfree {
        terms.push(NVIDIA_PROPRIETARY);
    }
    if gcc {
        terms.push(GCC_RUNTIME_LICENSE);
    }
    terms
}

/// `license` with each of `terms` it does not already name, joined by `AND`
/// (an `OR` expression is parenthesised first). A licence still `FIXME` is left
/// for a person: terms beside an unknown are noise.
fn with_license_terms(license: String, terms: &[&str]) -> String {
    if license == "FIXME" {
        return license;
    }
    let mut out = license;
    for term in terms {
        if out.contains(term) {
            continue;
        }
        // `AND` binds tighter than `OR`: an expression with an `OR` outside any
        // parentheses has to be grouped before a term is added to all of it.
        if has_top_level_or(&out) {
            out = format!("({out})");
        }
        out = format!("{out} AND {term}");
    }
    out
}

/// Whether an SPDX expression has an `OR` outside every pair of parentheses
/// (`MIT OR Apache-2.0`, `(A OR B) AND C OR D`; not `(A OR B)`).
fn has_top_level_or(expr: &str) -> bool {
    let mut depth = 0i32;
    let mut word = String::new();
    let mut found = false;
    for c in expr.chars().chain(std::iter::once(' ')) {
        match c {
            '(' | ')' => {
                if depth == 0 && word == "OR" {
                    found = true;
                }
                word.clear();
                depth += if c == '(' { 1 } else { -1 };
            }
            c if c.is_whitespace() => {
                if depth == 0 && word == "OR" {
                    found = true;
                }
                word.clear();
            }
            c => word.push(c),
        }
    }
    found
}

fn license_tag(license: &License) -> String {
    match license {
        License::Spdx(expr) => expr.clone(),
        License::FreeText(text) => text.clone(),
        License::Classifiers(classifiers) => {
            // Only classifiers with a known SPDX id are emitted: OSI display
            // names ("MIT License") are not valid in `License:`. When nothing
            // maps, `FIXME` is what the spec validator and agent already flag,
            // rather than an empty tag that looks like a finished spec.
            let ids: Vec<&str> = classifiers
                .iter()
                .filter_map(|c| c.strip_prefix("License :: OSI Approved :: "))
                .filter_map(classifier_spdx)
                .collect();
            if ids.is_empty() {
                "FIXME".to_string()
            } else {
                ids.join(" AND ")
            }
        }
        License::Unknown => "FIXME".to_string(),
    }
}

/// Whether a wheel's declared licence is a vendor's proprietary one rather than
/// an open source licence: free text or an SPDX expression naming NVIDIA or a
/// proprietary / end-user agreement, or the "Proprietary" trove classifier.
pub fn license_is_proprietary(license: &License) -> bool {
    let text = match license {
        License::Spdx(t) | License::FreeText(t) => t.to_lowercase(),
        License::Classifiers(c) => c.join(" ").to_lowercase(),
        License::Unknown => return false,
    };
    [
        "proprietary",
        "nvidia",
        "license agreement",
        "eula",
        "nonfree",
    ]
    .iter()
    .any(|w| text.contains(w))
}

/// `spec` with a sentence added to the `%description`, saying the package is a
/// vendor's prebuilt library unpacked from its wheel and not built from source.
pub fn note_prebuilt_library(spec: &str, proprietary: bool) -> String {
    let note = if proprietary {
        "This package redistributes the vendor's prebuilt binary library, unpacked from \
         its wheel; it is not built from source and its licence is not an open source one."
    } else {
        "This package redistributes the upstream prebuilt binary library, unpacked from \
         its wheel; it is not built from source."
    };
    let mut out = Vec::new();
    let mut after_description = false;
    let mut done = false;
    for line in spec.lines() {
        out.push(line.to_string());
        if !done && after_description {
            out.push(String::new());
            out.push(note.to_string());
            done = true;
        }
        if line.trim_end() == "%description" {
            after_description = true;
        }
    }
    out.join("\n") + if spec.ends_with('\n') { "\n" } else { "" }
}

#[cfg(test)]
mod prebuilt_library_tests {
    use super::*;

    #[test]
    fn the_description_gets_the_note_once() {
        let spec = "%description\nNCCL runtime\n\n%prep\n";
        let out = note_prebuilt_library(spec, true);
        assert!(
            out.starts_with(
                "%description\nNCCL runtime\n\nThis package redistributes the vendor's"
            )
        );
        assert_eq!(out.matches("redistributes").count(), 1);
        assert!(out.contains("%prep"));
    }

    #[test]
    fn nvidia_licence_text_is_proprietary() {
        assert!(license_is_proprietary(&License::FreeText(
            "LicenseRef-NVIDIA-Proprietary".into()
        )));
        assert!(license_is_proprietary(&License::Spdx(
            NVIDIA_PROPRIETARY.into()
        )));
        assert!(!license_is_proprietary(&License::Spdx("MIT".into())));
        assert!(!license_is_proprietary(&License::Unknown));
    }
}

/// Runtime dependency tags that must come before `%python_subpackages`: after
/// it they belong to no subpackage (or end up in the description).
const DEPENDENCY_TAGS: &[&str] = &[
    "Requires",
    "Recommends",
    "Suggests",
    "Supplements",
    "Conflicts",
    "Obsoletes",
    "Provides",
];

fn is_dependency_tag_line(line: &str) -> bool {
    let t = line.trim_start();
    DEPENDENCY_TAGS.iter().any(|tag| {
        t.strip_prefix(tag).is_some_and(|rest| {
            let rest = rest
                .strip_prefix('(')
                .and_then(|r| r.split_once(')'))
                .map(|(_, after)| after)
                .unwrap_or(rest);
            rest.starts_with(':')
        })
    })
}

/// A runtime `Requires:`/`Recommends:` written with `%{python_module X}` names
/// every flavor's package, so the python314 RPM requires python313-X as well.
/// The runtime form is flavor-neutral, `python-X`, which `%python_subpackages`
/// rewrites per flavor; `%{python_module}` is for `BuildRequires`. Rewrites the
/// simple forms (`%{python_module name}` with an optional version bound) and
/// leaves conditional ones alone. Idempotent.
pub fn normalize_runtime_python_module(spec: &str) -> String {
    let mut out: Vec<String> = Vec::new();
    for line in spec.lines() {
        let t = line.trim_start();
        let redist_line;
        let line = if crate::redist::enabled() {
            redist_line = redist_runtime_requires(line);
            redist_line.as_str()
        } else {
            line
        };
        let runtime = ["Requires", "Recommends", "Suggests", "Supplements"]
            .iter()
            .any(|tag| {
                t.strip_prefix(tag)
                    .is_some_and(|r| r.starts_with(':') || r.starts_with('('))
            });
        if runtime && line.contains("%{python_module ") && !line.contains(" if ") {
            let mut rewritten = line.to_string();
            while let Some(start) = rewritten.find("%{python_module ") {
                let Some(end) = rewritten[start..].find('}') else {
                    break;
                };
                let inner = rewritten[start + "%{python_module ".len()..start + end].to_string();
                if inner.contains('%') || inner.contains('{') {
                    break;
                }
                rewritten.replace_range(start..start + end + 1, &format!("python-{inner}"));
            }
            out.push(rewritten);
        } else {
            out.push(line.to_string());
        }
    }
    out.join("\n") + if spec.ends_with('\n') { "\n" } else { "" }
}

/// With `--nvidia-redist`, a runtime `Requires:` of a CUDA wheel, written as
/// `%{python_module nvidia-cublas >= 13.1}` or `python-nvidia-cublas`, names the
/// package built from NVIDIA's archive instead: `cuda-cublas`, with no version
/// (the wheel's scheme is not the component's). Other lines are returned as they
/// are.
fn redist_runtime_requires(line: &str) -> String {
    static MODULE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    static PLAIN: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let t = line.trim_start();
    let runtime = ["Requires", "Recommends", "Suggests", "Supplements"]
        .iter()
        .any(|tag| {
            t.strip_prefix(tag)
                .is_some_and(|r| r.starts_with(':') || r.starts_with('('))
        });
    if !runtime || line.contains(" if ") {
        return line.to_string();
    }
    let module = MODULE.get_or_init(|| {
        regex::Regex::new(r"%\{python_module\s+([^\s}]+)(?:\s*(?:[<>=!~]+)\s*[^\s}]+)?\}").unwrap()
    });
    let plain = PLAIN.get_or_init(|| {
        regex::Regex::new(r"(^|[\s:])python-([^\s,]+)(?:\s*(?:[<>=!~]+)\s*[^\s,]+)?").unwrap()
    });
    let out = module.replace_all(line, |c: &regex::Captures| {
        crate::redist::package_name(&c[1]).unwrap_or_else(|| c[0].to_string())
    });
    plain
        .replace_all(&out, |c: &regex::Captures| match crate::redist::package_name(&c[2]) {
            Some(cuda) => format!("{}{cuda}", &c[1]),
            None => c[0].to_string(),
        })
        .into_owned()
}

/// Dependency tags written after `%python_subpackages` (up to the next section)
/// moved above it.
pub fn hoist_dependency_tags(spec: &str) -> String {
    let lines: Vec<&str> = spec.lines().collect();
    let Some(sub) = lines.iter().position(|l| l.trim() == "%python_subpackages") else {
        return spec.to_string();
    };
    let mut moved: Vec<&str> = Vec::new();
    let mut kept_after: Vec<&str> = Vec::new();
    let mut in_preamble = true;
    for line in &lines[sub + 1..] {
        if in_preamble
            && line.trim_start().starts_with('%')
            && !line.trim_start().starts_with("%global")
            && !line.trim_start().starts_with("%define")
        {
            in_preamble = false;
        }
        if in_preamble && is_dependency_tag_line(line) {
            moved.push(line);
        } else {
            kept_after.push(line);
        }
    }
    if moved.is_empty() {
        return spec.to_string();
    }
    let mut out: Vec<&str> = lines[..sub].to_vec();
    out.extend(moved);
    out.push(lines[sub]);
    out.extend(kept_after);
    out.join("\n") + if spec.ends_with('\n') { "\n" } else { "" }
}

/// The licence tag for NVIDIA's redistributable proprietary binaries: openSUSE
/// uses `SUSE-NonFree`, and rpmlint reports every `LicenseRef-...` spelling that
/// crept in (`LicenseRef-NVIDIA-Proprietary`, `LicenseRef-NvidiaProprietary`,
/// `NVIDIA Proprietary Software`) as `invalid-license`.
pub const NVIDIA_PROPRIETARY: &str = "SUSE-NonFree";

pub fn normalize_nvidia_license(spec: &str) -> String {
    let mut out: Vec<String> = Vec::new();
    for line in spec.lines() {
        if let Some(rest) = line.strip_prefix("License:") {
            let mut value = rest.trim().to_string();
            for variant in [
                "LicenseRef-NVIDIA-Proprietary",
                "LicenseRef-NvidiaProprietary",
                "LicenseRef-Nvidia-Proprietary",
                "LicenseRef-NVIDIA-proprietary",
                "NVIDIA Proprietary Software",
                "LicenseRef-NVIDIAProprietary",
            ] {
                value = value.replace(variant, NVIDIA_PROPRIETARY);
            }
            out.push(format!("License:        {value}"));
        } else {
            out.push(line.to_string());
        }
    }
    out.join("\n") + if spec.ends_with('\n') { "\n" } else { "" }
}

/// A second `%changelog` line (the file ends `%changelog` twice) removed.
pub fn dedupe_changelog_header(spec: &str) -> String {
    let mut seen = false;
    let mut out: Vec<&str> = Vec::new();
    for line in spec.lines() {
        if line.trim_end() == "%changelog" {
            if seen {
                continue;
            }
            seen = true;
        }
        out.push(line);
    }
    out.join("\n") + if spec.ends_with('\n') { "\n" } else { "" }
}

/// Style problems a reviewer would send back: an emoji, or a `%description`
/// that only repeats the `Summary:`. One line each, empty when the spec is fine.
pub fn description_problems(spec: &str) -> Vec<String> {
    let mut problems = Vec::new();
    let body = spec.split("\n%changelog").next().unwrap_or(spec);
    let is_emoji =
        |c: char| matches!(c as u32, 0x1F300..=0x1FAFF | 0x2600..=0x27BF | 0x1F000..=0x1F2FF);
    if body.chars().any(is_emoji) {
        problems.push("the spec contains an emoji (Summary, description or a comment)".to_string());
    }
    let summary = tag_value(body, "Summary:").map(|s| s.trim().to_string());
    let mut description = String::new();
    let mut in_desc = false;
    for line in body.lines() {
        let t = line.trim_end();
        if t.starts_with("%description") && (t == "%description" || t.starts_with("%description "))
        {
            if in_desc {
                break;
            }
            in_desc = true;
            continue;
        }
        if in_desc {
            if t.starts_with('%') {
                break;
            }
            if !t.is_empty() {
                if !description.is_empty() {
                    description.push(' ');
                }
                description.push_str(t.trim());
            }
        }
    }
    if let Some(summary) = summary
        && !summary.is_empty()
        && description.trim_end_matches('.') == summary.trim_end_matches('.')
    {
        problems.push(
            "the %description only repeats the Summary: say what the package does and \
             what it is for, in a sentence or two"
                .to_string(),
        );
    }
    problems
}

/// Whether the spec's `%files` names a `%license` (or `%doc` of a licence
/// file). Specs with no `%files` count as having one.
pub fn has_license_file_entry(spec: &str) -> bool {
    let body = spec.split("\n%changelog").next().unwrap_or(spec);
    if !body.contains("%files") {
        return true;
    }
    body.lines().any(|l| l.trim_start().starts_with("%license"))
}

#[cfg(test)]
mod generator_tests {
    use super::*;

    /// "removed duplicate BuildRequires" appears in 20 of the 25 attempt
    /// journals — py2pack emits the PyPI build requirements and we then add
    /// `[build-system] requires` from the sdist on top.
    #[test]
    fn duplicate_build_requires_collapse_to_the_constrained_line() {
        let spec = "\
BuildRequires:  %{python_module pip}
BuildRequires:  %{python_module hatchling}
BuildRequires:  %{python_module setuptools >= 42}
BuildRequires:  %{python_module hatchling >= 1.0}
BuildRequires:  %{python_module setuptools}
BuildRequires:  fdupes
";
        let out = dedupe_build_requires(spec);
        assert_eq!(out.matches("hatchling").count(), 1, "{out}");
        assert!(out.contains("%{python_module hatchling >= 1.0}"), "{out}");
        assert_eq!(out.matches("setuptools").count(), 1, "{out}");
        assert!(out.contains("%{python_module setuptools >= 42}"), "{out}");
        assert!(out.contains("fdupes"), "non-python BR dropped");
        assert!(out.contains("pip"), "pip dropped");
    }

    #[test]
    fn dedupe_respects_conditionals_multi_package_lines_and_distinct_constraints() {
        let spec = "\
BuildRequires:  gcc make
BuildRequires:  make
BuildRequires:  foo >= 1
BuildRequires:  foo < 2
BuildRequires:  foo >= 1
%if 0%{?suse_version} > 1500
BuildRequires:  bar
%else
BuildRequires:  bar
%endif
BuildRequires:  bar
BuildRequires:  baz
%if 1
BuildRequires:  baz
BuildRequires:  baz
%endif
";
        let out = dedupe_build_requires(spec);
        assert!(out.contains("BuildRequires:  gcc make\n"), "{out}");
        assert_eq!(out.matches("make").count(), 2, "{out}");
        assert_eq!(out.matches("foo >= 1").count(), 1, "{out}");
        assert_eq!(out.matches("foo < 2").count(), 1, "{out}");
        // both branches and the top level keep their own `bar`
        assert_eq!(out.matches("bar").count(), 3, "{out}");
        // top-level `baz` plus one inside the %if
        assert_eq!(out.matches("baz").count(), 2, "{out}");
    }

    #[test]
    fn a_non_python_buildrequires_is_not_confused_with_the_macro_form() {
        // `cmake` the RPM and `%{python_module cmake}` are different packages.
        let spec = "\
BuildRequires:  cmake
BuildRequires:  %{python_module cmake}
";
        assert_eq!(dedupe_build_requires(spec), spec);
    }

    /// A top-level `tests/` directory scopes the default `%pytest`
    /// invocation; without one, the old unscoped `%pytest` is kept.
    #[test]
    fn default_check_scopes_pytest_to_a_top_level_tests_dir() {
        let release = dummy_release("https://example.org", License::Spdx("MIT".to_string()));
        let build_system = BuildSystem {
            requires: vec![],
            backend: None,
        };
        let spec = "\
BuildRequires:  %{python_module pip}
%changelog
";
        let scoped = post_process_with_sdist(
            spec,
            &release,
            &build_system,
            SdistFacts {
                has_tests: true,
                has_c_extensions: false,
                top_level_tests_dir: Some("tests".to_string()),
                install_entries: None,
                console_scripts: Vec::new(),
            },
        );
        assert!(scoped.contains("%check\n%pytest tests/\n"), "{scoped}");

        let unscoped = post_process_with_sdist(
            spec,
            &release,
            &build_system,
            SdistFacts {
                has_tests: true,
                has_c_extensions: false,
                top_level_tests_dir: None,
                install_entries: None,
                console_scripts: Vec::new(),
            },
        );
        assert!(unscoped.contains("%check\n%pytest\n"), "{unscoped}");
    }

    fn dummy_release(homepage: &str, license: License) -> Release {
        Release {
            name: "pkg".to_string(),
            version: "1.0".to_string(),
            summary: Some("A package".to_string()),
            homepage: Some(homepage.to_string()),
            license,
            requires_python: None,
            requires_dist: Vec::new(),
            yanked: false,
            sdist: ap_ecosystem::pypi::Sdist {
                filename: String::new(),
                url: String::new(),
                sha256: String::new(),
                size: 0,
            },
        }
    }

    /// The running example: NVIDIA's CUDA toolkit component wheels ship
    /// compiled binaries under a manylinux platform tag — sitearch, not noarch.
    #[test]
    fn wheel_binary_skeleton_marks_platform_wheels_as_arch() {
        let release = dummy_release(
            "https://developer.nvidia.com/cuda-toolkit",
            License::FreeText("NVIDIA Proprietary".to_string()),
        );
        let wheel = "nvidia_cuda_nvcc_cu13-13.0.88-py3-none-manylinux_2_17_x86_64.whl";
        let spec = wheel_binary_skeleton(
            "nvidia-cuda-nvcc-cu13",
            "13.0.88",
            wheel,
            &release,
            Some(false),
            None,
        );

        assert!(spec.contains(&format!("Source0:        {wheel}")), "{spec}");
        assert!(
            !spec.contains("BuildArch:      noarch"),
            "platform wheel must not be noarch:\n{spec}"
        );
        assert!(spec.contains("%{buildroot}/%{$python_sitearch}"), "{spec}");
        assert!(spec.contains("%pyproject_install %{SOURCE0}"), "{spec}");
    }

    /// NVIDIA's real shape: arch-specific tag, but `Root-Is-Purelib: true`.
    /// Not noarch (it ships `.so`), exclusive to the wheel's arch, and
    /// installed under sitelib — where pip actually puts it.
    #[test]
    fn a_manylinux_purelib_wheel_is_arch_specific_but_installs_to_sitelib() {
        let release = dummy_release("https://example.org", License::Spdx("MIT".to_string()));
        let wheel = "nvidia_curand-10.4.0.35-py3-none-manylinux_2_27_x86_64.whl";
        let spec = wheel_binary_skeleton(
            "nvidia-curand",
            "10.4.0.35",
            wheel,
            &release,
            Some(true),
            None,
        );
        assert!(!spec.contains("BuildArch:      noarch"), "{spec}");
        assert!(spec.contains("ExclusiveArch:  x86_64"), "{spec}");
        assert!(spec.contains("%{buildroot}/%{$python_sitelib}"), "{spec}");
        assert!(!spec.contains("python_sitearch"), "{spec}");
    }

    #[test]
    fn a_multi_flavor_skeleton_installs_each_flavors_own_wheel() {
        let release = dummy_release("https://example.org", License::Spdx("MIT".to_string()));
        let wheels = vec![
            (
                "python313".to_string(),
                "triton-3.8.0-cp313-cp313-manylinux_2_27_x86_64.whl".to_string(),
            ),
            (
                "python314".to_string(),
                "triton-3.8.0-cp314-cp314-manylinux_2_27_x86_64.whl".to_string(),
            ),
        ];
        let spec =
            wheel_multi_flavor_skeleton("triton", "3.8.0", &wheels, &release, Some(false), None);
        assert!(
            spec.contains("Source0:        triton-3.8.0-cp313-cp313"),
            "{spec}"
        );
        assert!(
            spec.contains("Source1:        triton-3.8.0-cp314-cp314"),
            "{spec}"
        );
        assert!(
            spec.contains("%python313_pyproject_install %{SOURCE0}"),
            "{spec}"
        );
        assert!(
            spec.contains("%python314_pyproject_install %{SOURCE1}"),
            "{spec}"
        );
        assert!(!spec.contains("%pyproject_install %{SOURCE0}"), "{spec}");
        assert!(spec.contains("ExclusiveArch:  x86_64"), "{spec}");
        assert!(spec.contains("%{buildroot}/%{$python_sitearch}"), "{spec}");
    }

    /// cuda-bindings / nccl4py: a namespace wheel with modules is not given the
    /// `test -e` probe the validator rejects, and its licence comes from the
    /// wheel when PyPI has none.
    #[test]
    fn a_namespace_wheel_gets_a_find_spec_check_and_the_wheels_licence() {
        use ap_ecosystem::sdist::WheelLayout;
        let release = dummy_release("https://example.org", License::Unknown);
        let layout = WheelLayout {
            namespace_module: Some("cuda.bindings".into()),
            first_shared_lib: Some("cuda/bindings/_internal/x.so".into()),
            license_expression: Some("LicenseRef-NVIDIA-SOFTWARE-LICENSE".into()),
            ..Default::default()
        };
        let spec = wheel_binary_skeleton(
            "cuda-bindings",
            "1.0",
            "cuda_bindings-1.0-cp313-abi3-manylinux_2_27_x86_64.whl",
            &release,
            Some(false),
            Some(&layout),
        );
        assert!(
            spec.contains(
                "$python -c \"import importlib.machinery as m, sys; s = m.PathFinder.find_spec('cuda', ['%{buildroot}%{$python_sitearch}']); s = s and m.PathFinder.find_spec('cuda.bindings', s.submodule_search_locations); sys.exit(0 if s else 1)\""
            ),
            "{spec}"
        );
        assert!(!spec.contains("test -e"), "{spec}");
        assert!(!spec.contains("smoke-test the import name"), "{spec}");
        assert!(
            spec.contains("License:        SUSE-NonFree"),
            "{spec}"
        );
        assert!(!check_is_file_probe_only(&spec));
        // Older wheels: classifiers, then free text; an unknown text is not guessed.
        let from = |classifiers: &[&str], text: Option<&str>| {
            let l = WheelLayout {
                license_classifiers: classifiers.iter().map(|c| c.to_string()).collect(),
                license_text: text.map(str::to_string),
                ..Default::default()
            };
            wheel_license(&l)
        };
        assert_eq!(
            from(&["License :: OSI Approved :: MIT License"], None).as_deref(),
            Some("MIT")
        );
        // `BSD License` does not say 2 or 3 clauses: the free text may.
        assert_eq!(
            from(&["License :: OSI Approved :: BSD License"], Some("3-Clause BSD")).as_deref(),
            Some("BSD-3-Clause")
        );
        assert_eq!(from(&[], Some("Apache License, Version 2.0")).as_deref(), Some("Apache-2.0"));
        assert_eq!(from(&[], Some("NVIDIA Proprietary Software")).as_deref(), Some("SUSE-NonFree"));
        assert_eq!(from(&[], Some("Custom terms, see LICENSE")), None);
        assert_eq!(from(&[], None), None);
        // The release's own licence wins over the wheel's.
        let own = dummy_release("https://example.org", License::Spdx("MIT".to_string()));
        let spec = wheel_binary_skeleton("p", "1", "p-1-py3-none-any.whl", &own, Some(true), Some(&layout));
        assert!(spec.contains("License:        MIT"), "{spec}");
    }

    /// The generated `%check` must be one that works first time: importable
    /// wheels import with the buildroot on PYTHONPATH inside %python_expand;
    /// namespace/binary-only wheels assert their payload file instead.
    #[test]
    fn the_generated_check_imports_or_asserts_the_payload() {
        use ap_ecosystem::sdist::WheelLayout;
        let release = dummy_release("https://example.org", License::Spdx("MIT".to_string()));
        let wheel = "pkg-1.0-py3-none-any.whl";

        let importable = WheelLayout {
            importable: Some("pkg".into()),
            first_shared_lib: None,
            ..Default::default()
        };
        let spec =
            wheel_binary_skeleton("pkg", "1.0", wheel, &release, Some(true), Some(&importable));
        assert!(
            spec.contains(
                "%check\n%python_expand PYTHONPATH=%{buildroot}%{$python_sitelib} $python -c \"import pkg\"\n"
            ),
            "{spec}"
        );
        assert!(!spec.contains("# TODO: smoke-test"), "{spec}");

        let binary_only = WheelLayout {
            importable: None,
            first_shared_lib: Some("nvidia/cu13/lib/libnvJitLink.so.13".into()),
            ..Default::default()
        };
        let spec = wheel_binary_skeleton(
            "nvidia-nvjitlink",
            "1.0",
            "nvidia_nvjitlink-1.0-py3-none-manylinux_2_27_x86_64.whl",
            &release,
            Some(true),
            Some(&binary_only),
        );
        assert!(
            spec.contains(
                "%check\n%python_expand test -e %{buildroot}%{$python_sitelib}/nvidia/cu13/lib/libnvJitLink.so.13\n"
            ),
            "{spec}"
        );

        // Unknown layout keeps the TODO rather than guessing.
        let spec = wheel_binary_skeleton("pkg", "1.0", wheel, &release, Some(true), None);
        assert!(spec.contains("# TODO: smoke-test"), "{spec}");
    }

    #[test]
    fn wheel_binary_skeleton_marks_pure_wheels_as_noarch() {
        let release = dummy_release("https://example.org", License::Spdx("MIT".to_string()));
        let wheel = "somepkg-1.0-py3-none-any.whl";
        let spec = wheel_binary_skeleton("somepkg", "1.0", wheel, &release, Some(true), None);

        assert!(spec.contains("BuildArch:      noarch"), "{spec}");
        assert!(spec.contains("%{buildroot}/%{$python_sitelib}"), "{spec}");
    }

    /// open-webui's wheel repack tripped 238 `script-without-shebang` and 5
    /// `zero-length` errors: every wheel repack normalises the modes in
    /// `%install`, and a wheel with empty files gets exactly their filters.
    #[test]
    fn a_pure_wheel_repack_normalises_modes_and_filters_empty_files() {
        use ap_ecosystem::sdist::WheelLayout;
        let release = dummy_release("https://example.org", License::Spdx("MIT".to_string()));
        let wheel = "open_webui-1.0-py3-none-any.whl";
        let plain = wheel_binary_skeleton("open-webui", "1.0", wheel, &release, Some(true), None);
        assert!(plain.contains("-type f -perm /111 -exec sh -c"), "{plain}");
        assert!(
            plain.contains("%python_expand find %{buildroot}%{$python_sitelib} "),
            "{plain}"
        );
        assert!(plain.contains("ELF"), "{plain}");
        assert!(!plain.contains("Source99"), "{plain}");
        assert!(wheel_rpmlintrc("open-webui", wheel, None).is_none());

        let layout = WheelLayout {
            empty_files: vec![
                "open_webui/static/custom.css".into(),
                "open_webui/static/loader.js".into(),
                "weird name/x".into(),
            ],
            ..Default::default()
        };
        let spec =
            wheel_binary_skeleton("open-webui", "1.0", wheel, &release, Some(true), Some(&layout));
        assert!(spec.contains("Source99:       python-open-webui.rpmlintrc
"), "{spec}");
        let (file, text) = wheel_rpmlintrc("open-webui", wheel, Some(&layout)).unwrap();
        assert_eq!(file, "python-open-webui.rpmlintrc");
        assert!(
            text.contains(r#"addFilter(r".*zero-length .*/site-packages/open_webui/static/custom\.css$")"#),
            "{text}"
        );
        assert!(text.contains(r"loader\.js$"), "{text}");
        assert!(!text.contains("weird"), "{text}");
        assert!(!text.contains("missing-hash-section"), "{text}");
    }

    /// A prebuilt vendor `.so` wheel (nvidia-cublas's real shape) gets a
    /// pre-seeded `Source99:`/rpmlintrc baseline instead of making the agent
    /// re-derive the same five rpmlint false positives from a failed build —
    fn layout_of(files: &[&str], requires: &[&str]) -> ap_ecosystem::sdist::WheelLayout {
        ap_ecosystem::sdist::WheelLayout {
            files: files.iter().map(|f| f.to_string()).collect(),
            dist_info: files
                .iter()
                .find_map(|f| f.split('/').next().filter(|t| t.ends_with(".dist-info")))
                .map(str::to_string),
            requires_dist: requires.iter().map(|r| r.to_string()).collect(),
            importable: Some("x".into()),
            ..Default::default()
        }
    }

    /// NVIDIA's shape (cublas, cufft, nvjitlink ...): a namespace shared with
    /// other wheels, headers removed in %install, one licence in the dist-info.
    /// Every one of those specs had its %files written by hand.
    #[test]
    fn a_namespace_wheel_lists_its_files_without_a_todo() {
        let release = dummy_release("https://example.org", License::Spdx("MIT".to_string()));
        let mut layout = layout_of(
            &[
                "nvidia/cu13/lib/libnvJitLink.so.13",
                "nvidia/cu13/include/nvJitLink.h",
                "nvidia_nvjitlink-1.0.dist-info/METADATA",
                "nvidia_nvjitlink-1.0.dist-info/RECORD",
                "nvidia_nvjitlink-1.0.dist-info/licenses/License.txt",
            ],
            &["nvidia-cuda-nvrtc", "nvidia-foo (>=2.0); extra == \"x\""],
        );
        layout.header_dirs = vec!["nvidia/cu13/include".into()];
        let spec = wheel_binary_skeleton(
            "nvidia-nvjitlink",
            "1.0",
            "nvidia_nvjitlink-1.0-py3-none-manylinux_2_27_x86_64.whl",
            &release,
            Some(true),
            Some(&layout),
        );
        let files = spec.split("%files %{python_files}\n").nth(1).unwrap();
        let files = files.split("%changelog").next().unwrap();
        assert_eq!(
            files.trim(),
            "%dir %{python_sitelib}/nvidia\n\
             %dir %{python_sitelib}/nvidia/cu13\n\
             %{python_sitelib}/nvidia/cu13/lib/\n\
             %exclude %{python_sitelib}/nvidia_nvjitlink-1.0.dist-info/licenses/License.txt\n\
             %license %{python_sitelib}/nvidia_nvjitlink-1.0.dist-info/licenses/License.txt\n\
             %{python_sitelib}/nvidia_nvjitlink-1.0.dist-info/"
        );
        assert!(!spec.contains("TODO"), "{spec}");
        // The wheel's own unconditional dependency, and not its extra.
        assert!(
            spec.contains("Requires:       python-nvidia-cuda-nvrtc\n"),
            "{spec}"
        );
        assert!(!spec.contains("nvidia-foo"), "{spec}");
    }

    /// nccl-extensions installs `nccl/ep/` and requires nccl4py, which owns the
    /// regular package `nccl`: with nccl4py in the build environment
    /// `find_spec('nccl.ep')` answered `None` and `%check` failed. The generated
    /// snippet looks in the buildroot only; run it against both trees.
    #[test]
    fn the_namespace_check_is_not_hidden_by_a_sibling_regular_package() {
        let snippet = namespace_find_spec_snippet("nccl.ep", "/tmp/root");
        assert_eq!(
            snippet,
            "import importlib.machinery as m, sys; s = m.PathFinder.find_spec('nccl', ['/tmp/root']); \
             s = s and m.PathFinder.find_spec('nccl.ep', s.submodule_search_locations); \
             sys.exit(0 if s else 1)"
        );
        let Ok(python) = std::process::Command::new("python3").arg("--version").output() else {
            return;
        };
        assert!(python.status.success());
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("root");
        let system = dir.path().join("system");
        std::fs::create_dir_all(root.join("nccl/ep")).unwrap();
        std::fs::write(root.join("nccl/ep/__init__.py"), "").unwrap();
        std::fs::create_dir_all(system.join("nccl")).unwrap();
        std::fs::write(system.join("nccl/__init__.py"), "").unwrap();
        let run = |module: &str| {
            std::process::Command::new("python3")
                .current_dir(dir.path())
                .env(
                    "PYTHONPATH",
                    format!("{}:{}", root.display(), system.display()),
                )
                .args(["-c", &namespace_find_spec_snippet(module, &root.display().to_string())])
                .status()
                .unwrap()
                .success()
        };
        assert!(run("nccl.ep"));
        assert!(!run("nccl.missing"));
        assert!(!run("other.ep"));
        // What it replaces fails on the same tree.
        let old = std::process::Command::new("python3")
            .current_dir(dir.path())
            .env("PYTHONPATH", format!("{}:{}", root.display(), system.display()))
            .args(["-c", "import importlib.util as u, sys; sys.exit(0 if u.find_spec('nccl.ep') else 1)"])
            .status()
            .unwrap();
        assert!(!old.success());
    }

    /// The `%install` and `%files` of a wheel, from the skeleton of `layout`.
    fn wheel_install_and_files(
        layout: &ap_ecosystem::sdist::WheelLayout,
        purelib: bool,
    ) -> (String, String, String) {
        let release = dummy_release("https://example.org", License::Spdx("MIT".to_string()));
        let spec = wheel_binary_skeleton(
            "pkg",
            "1.0",
            "pkg-1.0-py3-none-manylinux_2_27_x86_64.whl",
            &release,
            Some(purelib),
            Some(layout),
        );
        let part = |from: &str, to: &str| {
            let tail = spec.split(from).nth(1).unwrap_or("");
            tail.split(to).next().unwrap_or("").to_string()
        };
        (
            part("%install\n", "%check\n"),
            part("%check\n", "%files"),
            part("%files %{python_files}\n", "%changelog"),
        )
    }

    /// cuda-bindings: `cuda/bindings/_lib/` holds one header AND the regular
    /// package `cuda.bindings._lib` (an `__init__.py`, Cython sources). Removing
    /// the directory broke the import; only the header goes, and `%files` does
    /// not list it.
    #[test]
    fn a_header_directory_that_holds_python_loses_only_its_headers() {
        let mut layout = layout_of(
            &[
                "cuda/bindings/__init__.py",
                "cuda/bindings/_lib/__init__.py",
                "cuda/bindings/_lib/utils.pxd",
                "cuda/bindings/_lib/utils.h",
                "cuda/bindings/_internal/nvml.so",
                "pkg-1.0.dist-info/METADATA",
            ],
            &[],
        );
        layout.importable = None;
        layout.first_shared_lib = Some("cuda/bindings/_internal/nvml.so".into());
        layout.header_dirs = vec!["cuda/bindings/_lib".into()];
        let (install, _, files) = wheel_install_and_files(&layout, false);
        assert!(
            install.contains(
                "find %{buildroot}%{$python_sitearch}/cuda/bindings/_lib -type f \\( -name '*.h' -o -name '*.hpp' \\) -delete\n"
            ),
            "{install}"
        );
        assert!(!install.contains("rm -rf"), "{install}");
        assert!(files.contains("%{python_sitearch}/cuda/bindings/\n"), "{files}");

        // A directory of headers and extension-less headers only is still removed whole.
        let mut headers = layout_of(
            &[
                "p/lib/libx.so.1",
                "p/include/a.h",
                "p/include/b.cuh",
                "p/include/cuda/atomic",
                "pkg-1.0.dist-info/METADATA",
            ],
            &[],
        );
        headers.header_dirs = vec!["p/include".into()];
        let (install, _, _) = wheel_install_and_files(&headers, true);
        assert!(
            install.contains("rm -rf %{buildroot}%{$python_sitelib}/p/include\n"),
            "{install}"
        );

        // In a namespace directory the remaining files are listed one by one,
        // without the removed headers.
        let mut mixed = layout_of(
            &[
                "ns/data/a.h",
                "ns/data/b.txt",
                "ns/data/sub/c.txt",
                "ns/lib/libx.so.1",
                "pkg-1.0.dist-info/METADATA",
            ],
            &[],
        );
        mixed.header_dirs = vec!["ns/data".into()];
        let (_, _, files) = wheel_install_and_files(&mixed, true);
        assert!(files.contains("%{python_sitelib}/ns/data/b.txt\n"), "{files}");
        assert!(files.contains("%{python_sitelib}/ns/data/sub/"), "{files}");
        assert!(!files.contains("a.h"), "{files}");
    }

    /// nvidia-cuda-crt is `include/crt/*.h` and nothing else: removing the
    /// headers left an empty package and a `%files` TODO. The headers are the
    /// payload there, and stay (the review splits them into a `-devel`).
    #[test]
    fn a_header_library_wheel_keeps_its_headers() {
        let mut layout = layout_of(
            &[
                "nvidia/cu13/include/crt/mma.h",
                "nvidia/cu13/include/crt/mma.hpp",
                "pkg-1.0.dist-info/METADATA",
                "pkg-1.0.dist-info/licenses/License.txt",
            ],
            &[],
        );
        layout.importable = None;
        layout.header_dirs = vec!["nvidia/cu13/include/crt".into()];
        let (install, check, files) = wheel_install_and_files(&layout, true);
        assert!(!install.contains("rm -rf"), "{install}");
        assert!(!install.contains("-delete"), "{install}");
        assert!(install.contains("belong in a\n# -devel package"), "{install}");
        assert_eq!(
            check,
            "%python_expand test -e %{buildroot}%{$python_sitelib}/nvidia/cu13/include/crt/mma.h\n\n"
        );
        assert!(!files.contains("TODO"), "{files}");
        assert!(
            files.contains("%{python_sitelib}/nvidia/cu13/include/crt/\n"),
            "{files}"
        );
        // Executables next to the headers make it a toolchain, not a header library.
        let mut toolchain = layout.clone();
        toolchain.files.push("nvidia/cu13/bin/nvcc".into());
        let (install, _, _) = wheel_install_and_files(&toolchain, true);
        assert!(
            install.contains("rm -rf %{buildroot}%{$python_sitelib}/nvidia/cu13/include/crt\n"),
            "{install}"
        );
    }

    /// nvidia-cuda-nvdisasm (an executable under `bin/`), cuda-toolkit (only a
    /// dist-info) and nvidia-cutlass-dsl (a top-level `.pth`): each had a `%check`
    /// and `%files` TODO although the file list says what they install.
    #[test]
    fn a_wheel_without_modules_or_libraries_gets_a_check_and_files() {
        let mut bin = layout_of(
            &[
                "nvidia/cu13/bin/nvdisasm",
                "pkg-1.0.dist-info/METADATA",
            ],
            &[],
        );
        bin.importable = None;
        let (_, check, files) = wheel_install_and_files(&bin, true);
        assert_eq!(
            check,
            "%python_expand test -e %{buildroot}%{$python_sitelib}/nvidia/cu13/bin/nvdisasm\n\n"
        );
        assert!(files.contains("%{python_sitelib}/nvidia/cu13/bin/\n"), "{files}");
        assert!(!files.contains("TODO"), "{files}");

        let mut meta = layout_of(
            &[
                "pkg-1.0.dist-info/METADATA",
                "pkg-1.0.dist-info/WHEEL",
                "pkg-1.0.dist-info/RECORD",
            ],
            &[],
        );
        meta.importable = None;
        let (_, check, files) = wheel_install_and_files(&meta, true);
        assert_eq!(
            check,
            "%python_expand test -d %{buildroot}%{$python_sitelib}/pkg-1.0.dist-info\n\n"
        );
        assert_eq!(files.trim(), "%{python_sitelib}/pkg-1.0.dist-info/");

        let mut pth = layout_of(
            &[
                "pkg_packages.pth",
                "pkg-1.0.dist-info/METADATA",
                "pkg-1.0.dist-info/licenses/LICENSE",
            ],
            &[],
        );
        pth.importable = None;
        let (_, check, files) = wheel_install_and_files(&pth, true);
        assert!(check.contains("test -e %{buildroot}%{$python_sitelib}/pkg_packages.pth"), "{check}");
        assert!(
            files.starts_with("%{python_sitelib}/pkg_packages.pth\n%exclude"),
            "{files}"
        );
        assert!(!files.contains("TODO"), "{files}");
    }

    /// nvidia-cutlass-dsl-libs-base: `_mlir/` has modules but no `__init__.py`
    /// here (the sibling wheel owns the package), so its files are listed one by
    /// one, and the `__pycache__` the build writes beside them has to be listed
    /// too or the build ends in "Installed (but unpackaged) file(s)".
    #[test]
    fn modules_listed_one_by_one_bring_their_pycache() {
        let mut layout = layout_of(
            &[
                "ns/pkg/_mlir/ir.py",
                "ns/pkg/_mlir/_mlir_libs/_x.so",
                "ns/pkg/_mlir/dialects/_a_gen.py",
                "ns/pkg/_mlir/dialects/_b_gen.py",
                "pkg-1.0.dist-info/METADATA",
            ],
            &[],
        );
        layout.importable = None;
        let (_, _, files) = wheel_install_and_files(&layout, false);
        assert_eq!(
            files.trim(),
            "%dir %{python_sitearch}/ns\n\
             %dir %{python_sitearch}/ns/pkg\n\
             %dir %{python_sitearch}/ns/pkg/_mlir\n\
             %{python_sitearch}/ns/pkg/_mlir/ir.py\n\
             %{python_sitearch}/ns/pkg/_mlir/__pycache__/\n\
             %{python_sitearch}/ns/pkg/_mlir/_mlir_libs/\n\
             %{python_sitearch}/ns/pkg/_mlir/dialects/\n\
             %{python_sitearch}/pkg-1.0.dist-info/"
        );
    }

    #[test]
    fn a_package_directory_is_listed_whole_and_an_odd_layout_keeps_the_todo() {
        let release = dummy_release("https://example.org", License::Spdx("MIT".to_string()));
        let pkg = layout_of(
            &[
                "pkg/__init__.py",
                "pkg/sub/x.py",
                "pkg-1.0.dist-info/METADATA",
            ],
            &[],
        );
        let spec = wheel_binary_skeleton(
            "pkg",
            "1.0",
            "pkg-1.0-py3-none-any.whl",
            &release,
            Some(true),
            Some(&pkg),
        );
        assert!(
            spec.contains("%files %{python_files}\n%{python_sitelib}/pkg/\n%{python_sitelib}/pkg-1.0.dist-info/\n"),
            "{spec}"
        );
        // A top-level module (its own __pycache__) or a .data tree is not listed blind.
        for odd in [
            &["mod.py", "mod-1.0.dist-info/METADATA"][..],
            &[
                "pkg/__init__.py",
                "pkg-1.0.data/scripts/tool",
                "pkg-1.0.dist-info/METADATA",
            ][..],
        ] {
            let layout = layout_of(odd, &[]);
            let spec = wheel_binary_skeleton(
                "pkg",
                "1.0",
                "pkg-1.0-py3-none-any.whl",
                &release,
                Some(true),
                Some(&layout),
            );
            assert!(spec.contains("# TODO: list the exact paths"), "{spec}");
        }
    }

    /// see `is_prebuilt_binary_wheel`/`prebuilt_wheel_rpmlintrc`.
    #[test]
    fn a_binary_wheel_with_a_shared_lib_gets_a_pre_seeded_rpmlintrc() {
        use ap_ecosystem::sdist::WheelLayout;
        let release = dummy_release("https://example.org", License::Spdx("MIT".to_string()));
        let binary_only = WheelLayout {
            importable: None,
            first_shared_lib: Some("nvidia/cu13/lib/libfoo.so.1".into()),
            ..Default::default()
        };
        let spec = wheel_binary_skeleton(
            "nvidia-foo",
            "1.0",
            "nvidia_foo-1.0-py3-none-manylinux_2_27_x86_64.whl",
            &release,
            Some(true),
            Some(&binary_only),
        );
        assert!(
            spec.contains("Source99:       python-nvidia-foo.rpmlintrc\n"),
            "{spec}"
        );

        // A pure wheel, or one with no shared library, gets neither.
        let importable = WheelLayout {
            importable: Some("pkg".into()),
            first_shared_lib: None,
            ..Default::default()
        };
        let spec = wheel_binary_skeleton(
            "pkg",
            "1.0",
            "pkg-1.0-py3-none-any.whl",
            &release,
            Some(true),
            Some(&importable),
        );
        assert!(!spec.contains("Source99"), "{spec}");
    }

    /// The rpmlintrc keeps only what the package cannot fix, with the reason;
    /// the executable bit, `python-base` and bundled headers are fixed in the spec.
    #[test]
    fn the_wheel_rpmlintrc_has_only_unfixable_filters_with_reasons() {
        use ap_ecosystem::sdist::WheelLayout;
        let rc = prebuilt_wheel_rpmlintrc(&WheelLayout::default());
        assert!(rc.contains("addFilter(\"missing-hash-section\")"), "{rc}");
        assert!(rc.contains("library-without-ldconfig-postin"), "{rc}");
        assert!(!rc.contains("shared-library-not-executable"), "{rc}");
        assert!(!rc.contains("no-dependency-on python-base"), "{rc}");
        assert!(!rc.contains("devel-file-in-non-devel-package"), "{rc}");
        assert!(!rc.contains("no-binary"), "{rc}");
        assert!(rc.matches('#').count() >= 2, "{rc}");
    }

    /// `libcusparseLt.so.0 (RUNPATH: $ORIGIN:)` is `binary-or-shlib-defines-rpath`
    /// at badness 10000 and aborts rpmlint; but an `unused-rpmlintrc-filter` is an
    /// error too, so the filter is written only for a wheel that has such a RUNPATH.
    #[test]
    fn the_origin_runpath_filter_is_written_only_when_a_library_has_one() {
        use ap_ecosystem::sdist::WheelLayout;
        let with = prebuilt_wheel_rpmlintrc(&WheelLayout {
            first_shared_lib: Some("nvidia/cusparselt/lib/libcusparseLt.so.0".into()),
            origin_runpath: true,
            ..Default::default()
        });
        assert!(
            with.contains("addFilter(\".*binary-or-shlib-defines-rpath.*[$]ORIGIN\")\n"),
            "{with}"
        );
        // It matches the `$ORIGIN` finding and no other RPATH value.
        let re = regex::Regex::new("binary-or-shlib-defines-rpath.*[$]ORIGIN").unwrap();
        assert!(re.is_match(
            "binary-or-shlib-defines-rpath /usr/lib/python3.13/site-packages/nvidia/libcusparseLt.so.0 $ORIGIN:"
        ));
        assert!(!re.is_match("binary-or-shlib-defines-rpath /usr/bin/x /opt/evil"));

        let without = prebuilt_wheel_rpmlintrc(&WheelLayout {
            first_shared_lib: Some("nvidia/cusparselt/lib/libcusparseLt.so.0".into()),
            ..Default::default()
        });
        assert!(!without.contains("rpath"), "{without}");
    }

    /// The pipeline writes the file only for a prebuilt binary wheel, and it
    /// carries the layout's filters.
    #[test]
    fn the_rpmlintrc_file_follows_the_wheel_shape() {
        use ap_ecosystem::sdist::WheelLayout;
        let layout = WheelLayout {
            first_shared_lib: Some("p/libx.so.1".into()),
            origin_runpath: true,
            ..Default::default()
        };
        let (file, text) = wheel_rpmlintrc(
            "Nvidia-Foo",
            "nvidia_foo-1.0-py3-none-manylinux_2_27_x86_64.whl",
            Some(&layout),
        )
        .unwrap();
        assert_eq!(file, "python-nvidia-foo.rpmlintrc");
        assert!(text.contains("[$]ORIGIN"), "{text}");
        assert!(wheel_rpmlintrc("p", "p-1.0-py3-none-any.whl", Some(&layout)).is_none());
        assert!(
            wheel_rpmlintrc(
                "p",
                "p-1-py3-none-manylinux_2_27_x86_64.whl",
                Some(&WheelLayout::default())
            )
            .is_none()
        );
        assert!(wheel_rpmlintrc("p", "p-1-py3-none-manylinux_2_27_x86_64.whl", None).is_none());
    }

    /// python-triton's `proton`/`proton-viewer` and python-cuda-tile's
    /// `cutile-cache`: the skeleton carries all of convention 18's pieces for
    /// each wheel console script from the start, not after a failed build.
    #[test]
    fn wheel_console_scripts_get_the_alternatives_handling() {
        use ap_ecosystem::sdist::WheelLayout;
        let release = dummy_release("https://example.org", License::Spdx("MIT".to_string()));
        let layout = WheelLayout {
            importable: Some("triton".into()),
            console_scripts: vec!["proton".into(), "proton-viewer".into()],
            ..Default::default()
        };
        let spec = wheel_binary_skeleton(
            "triton",
            "3.0",
            "triton-3.0-py3-none-any.whl",
            &release,
            Some(true),
            Some(&layout),
        );
        for want in [
            "Requires:       python-base\nRequires(post):   update-alternatives\n\
             Requires(postun): update-alternatives\n%python_subpackages\n",
            "%pyproject_install %{SOURCE0}\n%python_clone -a %{buildroot}%{_bindir}/proton\n\
             %python_clone -a %{buildroot}%{_bindir}/proton-viewer\n# files without a shebang",
            "%post\n%python_install_alternative proton\n%python_install_alternative proton-viewer\n\n\
             %postun\n%python_uninstall_alternative proton\n%python_uninstall_alternative proton-viewer\n\n\
             %files %{python_files}\n",
            "%python_alternative %{_bindir}/proton\n%python_alternative %{_bindir}/proton-viewer\n\n%changelog",
        ] {
            assert!(spec.contains(want), "missing {want:?} in\n{spec}");
        }

        // Multi-flavor: the clone lines follow the per-flavor installs.
        let wheels = vec![
            (
                "python313".to_string(),
                "t-3.0-cp313-cp313-manylinux_2_17_x86_64.whl".to_string(),
            ),
            (
                "python314".to_string(),
                "t-3.0-cp314-cp314-manylinux_2_17_x86_64.whl".to_string(),
            ),
        ];
        let multi =
            wheel_multi_flavor_skeleton("t", "3.0", &wheels, &release, Some(false), Some(&layout));
        for want in [
            "%python313_pyproject_install %{SOURCE0}\n%python314_pyproject_install %{SOURCE1}\n\
             %python_clone -a %{buildroot}%{_bindir}/proton\n",
            "Requires(post):   update-alternatives\n",
            "%python_install_alternative proton-viewer\n",
            "%python_alternative %{_bindir}/proton\n",
        ] {
            assert!(multi.contains(want), "missing {want:?} in\n{multi}");
        }

        // No scripts: none of it appears, in either skeleton.
        let plain = WheelLayout::default();
        for spec in [
            wheel_binary_skeleton(
                "plain",
                "1.0",
                "plain-1.0-py3-none-any.whl",
                &release,
                Some(true),
                Some(&plain),
            ),
            wheel_binary_skeleton("plain", "1.0", "p-1.0-py3-none-any.whl", &release, None, None),
            wheel_multi_flavor_skeleton("t", "3.0", &wheels, &release, Some(false), Some(&plain)),
        ] {
            for absent in [
                "update-alternatives",
                "python_clone",
                "%post",
                "python_alternative",
            ] {
                assert!(!spec.contains(absent), "{absent} in\n{spec}");
            }
        }
    }

    /// opencv-python-headless: auditwheel's hashed libraries are vendored under
    /// `<name>.libs/`, so the generated Requires for their sonames must be
    /// excluded or the package is uninstallable.
    #[test]
    fn a_wheel_with_hashed_libs_excludes_their_requires() {
        use ap_ecosystem::sdist::WheelLayout;
        let release = dummy_release("https://example.org", License::Spdx("MIT".to_string()));
        let layout = WheelLayout {
            importable: Some("cv2".into()),
            first_shared_lib: Some(
                "opencv_python_headless.libs/libavcodec-f34387fd.so.62.11.100".into(),
            ),
            has_hashed_libs: true,
            ..Default::default()
        };
        let wheel = "opencv_python_headless-4.12-cp37-abi3-manylinux_2_28_x86_64.whl";
        let spec = wheel_binary_skeleton(
            "opencv-python-headless",
            "4.12",
            wheel,
            &release,
            Some(false),
            Some(&layout),
        );
        // The hashed libraries and the driver's share the ONE regex.
        let line = format!(
            "%global __requires_exclude ^({}|{})$\n%python_subpackages\n",
            requires_exclude_inner(hashed_libs_requires_exclude()),
            requires_exclude_inner(crate::depclass::driver_requires_exclude())
        );
        assert!(spec.contains(&line), "{spec}");
        assert_eq!(spec.matches("__requires_exclude").count(), 1, "{spec}");
        let multi = wheel_multi_flavor_skeleton(
            "opencv-python-headless",
            "4.12",
            &[("python313".to_string(), wheel.to_string())],
            &release,
            Some(false),
            Some(&layout),
        );
        assert!(multi.contains(&line), "{multi}");

        // The regex matches hashed sonames only.
        let re = regex::Regex::new(hashed_libs_requires_exclude()).unwrap();
        for hashed in [
            "libavcodec-f34387fd.so.62.11.100()(64bit)",
            "libopenblasp-r0-37b5f859.3.3.so",
            "libaom-969ba5f6.so.3.13.1",
        ] {
            assert!(re.is_match(hashed), "{hashed}");
        }
        for ordinary in [
            "libcuda.so.1()(64bit)",
            "libz3.so.4.15",
            "libfoo-1.2.so",
            "libc.so.6",
        ] {
            assert!(!re.is_match(ordinary), "{ordinary}");
        }

        // No hashed libs: no exclude.
        let spec = wheel_binary_skeleton(
            "p",
            "1.0",
            "p-1.0-py3-none-any.whl",
            &release,
            Some(true),
            Some(&WheelLayout::default()),
        );
        assert!(!spec.contains("__requires_exclude"), "{spec}");
    }

    /// The regex and `is_hashed_soname` are one rule: both accept auditwheel
    /// names and refuse date stamps, and agree on every name in between.
    #[test]
    fn the_hashed_libs_regex_agrees_with_is_hashed_soname() {
        let re = regex::Regex::new(hashed_libs_requires_exclude()).unwrap();
        let names = [
            "libfoo-1a2b3c4d.so.1",
            "libfoo-1a2b3c4d.so",
            "libavcodec-f34387fd.so.62.11.100",
            "libopenblasp-r0-37b5f859.3.3.so",
            "libfoo-deadbeef.so.1",
            "libfoo-0000000a.so.1",
            "libfoo-a0000000.so.1",
            "libfoo-20240101.so",
            "libfoo-20240101.so.1",
            "libfoo-00000000.so.1",
            "libfoo-1a2b3c4.so.1",
            "libfoo-1a2b3c4d5.so.1",
            "libfoo-1a2b3c4dx.so.1",
            "libfoo-1a2b3c4d.txt",
            "libfoo-1a2b3c4d",
            "libfoo-1a2b3c4dso",
            "libfoo-1.2.so",
            "libz3.so.4.15",
            "libcuda.so.1",
            "foo-1a2b3c4d.so.1",
            "libfoo-20240101-1a2b3c4d.so.2",
            "libfoo-1a2b3c4d.so.1.2-20240101",
            "libfoo-1A2B3C4D.so.1",
        ];
        for n in names {
            let sym = ap_ecosystem::sdist::is_hashed_soname(n);
            assert_eq!(re.is_match(n), sym, "{n}: regex vs predicate");
            // As rpm names the Requires of it.
            let req = format!("{n}()(64bit)");
            let req_ok = sym && (n.ends_with(".so") || n.contains(".so."));
            assert_eq!(re.is_match(&req), req_ok, "{req}");
        }
        assert!(ap_ecosystem::sdist::is_hashed_soname("libfoo-1a2b3c4d.so.1"));
        assert!(!ap_ecosystem::sdist::is_hashed_soname("libfoo-20240101.so"));
    }

    /// python-flashinfer-python: the wheel links libcuda.so.1, nothing
    /// provides it, OBS reported it unresolvable.
    #[test]
    fn a_compiled_wheel_never_requires_the_nvidia_driver() {
        use ap_ecosystem::sdist::WheelLayout;
        let release = dummy_release("https://example.org", License::Spdx("MIT".to_string()));
        let spec = wheel_binary_skeleton(
            "flashinfer-python",
            "0.6",
            "flashinfer_python-0.6-cp39-abi3-manylinux_2_28_x86_64.whl",
            &release,
            Some(false),
            Some(&WheelLayout::default()),
        );
        let line = format!(
            "%global __requires_exclude {}\n%python_subpackages\n",
            crate::depclass::driver_requires_exclude()
        );
        assert!(spec.contains(&line), "{spec}");
        let re = regex::Regex::new(crate::depclass::driver_requires_exclude()).unwrap();
        for r in ["libcuda.so.1()(64bit)", "libnvidia-ml.so.1()(64bit)"] {
            assert!(re.is_match(r), "{r}");
        }
    }

    #[test]
    fn the_driver_exclude_extends_an_existing_requires_exclude_once() {
        let spec = "Name: x\n%global __requires_exclude ^lib(mpi|ucp)\\.so.*$\n%python_subpackages\n";
        let out = with_driver_requires_exclude(spec);
        assert_eq!(out.matches("__requires_exclude").count(), 1, "{out}");
        let line = out.lines().find(|l| l.contains("__requires_exclude")).unwrap();
        let re = regex::Regex::new(line.trim_start_matches("%global __requires_exclude ")).unwrap();
        assert!(re.is_match("libmpi.so.40()(64bit)"), "{line}");
        assert!(re.is_match("libcuda.so.1()(64bit)"), "{line}");
        assert!(!re.is_match("libcudart.so.12()(64bit)"), "{line}");
        assert_eq!(with_driver_requires_exclude(&out), out);
        // Without any, it is added before %python_subpackages.
        let bare = with_driver_requires_exclude("Name: x\n%python_subpackages\n");
        assert!(bare.contains("%global __requires_exclude ^(libcuda"), "{bare}");
        assert_eq!(with_driver_requires_exclude(&bare), bare);
    }

    #[test]
    fn only_an_arch_package_naming_cuda_may_link_the_driver() {
        assert!(may_link_driver_library("Name: x\nBuildRequires: cuda-nvcc\n"));
        assert!(!may_link_driver_library("Name: x\nBuildArch: noarch\nRequires: cuda\n"));
        assert!(!may_link_driver_library("Name: x\n"));
    }

    #[test]
    fn a_spec_with_the_looser_old_regex_is_rewritten() {
        let spec = "%global __requires_exclude ^lib.*-[0-9a-f]{8}.*\\.so.*$\n%python_subpackages\n";
        let out = with_hashed_libs_requires_exclude(spec);
        assert!(out.contains(hashed_libs_requires_exclude()), "{out}");
        assert!(!out.contains(OLD_HASHED_LIBS_REQUIRES_EXCLUDE), "{out}");
        assert_eq!(with_hashed_libs_requires_exclude(&out), out);
    }

    /// The prompt and the dependency reason text must not quote a regex of
    /// their own: it would drift from the generated one.
    #[test]
    fn nothing_else_quotes_the_old_hashed_libs_regex() {
        for (name, text) in [
            ("depclass.rs", include_str!("depclass.rs")),
            ("agent.rs", include_str!("agent.rs")),
        ] {
            assert!(!text.contains("[0-9a-f]{8}.*"), "{name} quotes a regex");
        }
    }

    #[test]
    fn c_project_skeleton_uses_cmake_macros() {
        let spec = c_project_skeleton(
            "libthing",
            "1.0",
            "https://example.org",
            "libthing-1.0",
            CBuildSystem::Cmake,
        );
        assert!(spec.contains("Name:           libthing"), "{spec}");
        assert!(spec.contains("%autosetup -n libthing-1.0"), "{spec}");
        assert!(spec.contains("BuildRequires:  cmake"), "{spec}");
        assert!(spec.contains("%cmake\n"), "{spec}");
        assert!(spec.contains("%cmake_build"), "{spec}");
        assert!(spec.contains("%cmake_install"), "{spec}");
        assert!(
            !spec.contains("autoreconf"),
            "cmake skeleton must not run autoreconf:\n{spec}"
        );
    }

    #[test]
    fn c_project_skeleton_uses_autotools_macros() {
        let spec = c_project_skeleton(
            "libthing",
            "1.0",
            "https://example.org",
            "libthing-1.0",
            CBuildSystem::Autotools,
        );
        assert!(spec.contains("autoreconf -fiv"), "{spec}");
        assert!(spec.contains("%configure"), "{spec}");
        assert!(spec.contains("%make_build"), "{spec}");
        assert!(spec.contains("%make_install"), "{spec}");
        assert!(spec.contains("BuildRequires:  autoconf"), "{spec}");
        assert!(
            !spec.contains("%cmake"),
            "autotools skeleton must not reference cmake:\n{spec}"
        );
    }

    #[test]
    fn a_monorepo_project_is_built_from_its_subdirectory() {
        let spec = "%build\n%pyproject_wheel\n\n%install\n%pyproject_install\n";
        let out = build_in_subdir(spec, "cuda_python");
        assert_eq!(
            out,
            "%build\ncd cuda_python\n%pyproject_wheel\n\n%install\ncd cuda_python\n%pyproject_install\n"
        );
    }

    #[test]
    fn dist_info_always_uses_the_normalized_name() {
        let spec = "\
%files %{python_files}
%{python_sitelib}/rich-toolkit
%{python_sitelib}/rich-toolkit-%{version}.dist-info
";
        let out = normalize_files_paths(spec, "rich-toolkit");
        assert!(out.contains("%{python_sitelib}/rich_toolkit/"), "{out}");
        assert!(
            out.contains("%{python_sitelib}/rich_toolkit-%{version}.dist-info/"),
            "{out}"
        );
    }

    #[test]
    fn a_bare_source_filename_follows_the_real_archive() {
        let mut release = ap_ecosystem::pypi::Release {
            name: "Events".into(),
            version: "0.5".into(),
            summary: None,
            homepage: None,
            license: ap_ecosystem::pypi::License::Unknown,
            requires_python: None,
            requires_dist: Vec::new(),
            yanked: false,
            sdist: ap_ecosystem::pypi::Sdist {
                filename: "events-0.5.tar.gz".into(),
                url: String::new(),
                sha256: String::new(),
                size: 0,
            },
        };
        let spec = "Source:         Events-%{version}.zip\nBuildRequires:  unzip\nBuildRequires:  fdupes\n";
        assert_eq!(
            fix_bare_source_filename(spec, &release),
            "Source:         events-%{version}.tar.gz\nBuildRequires:  fdupes\n"
        );
        // A URL names where the file comes from; leave it.
        let url = "Source:         https://files.pythonhosted.org/x/Events-%{version}.zip\n";
        assert_eq!(fix_bare_source_filename(url, &release), url);
        // Nothing to say without a known archive.
        release.sdist.filename.clear();
        assert_eq!(fix_bare_source_filename(spec, &release), spec);
    }

    #[test]
    fn trailing_slashes_do_not_double_the_dist_info_directory() {
        let spec = "\
%{python_sitelib}/accelerate/
%{python_sitelib}/accelerate-%{version}.dist-info/
";
        let out = normalize_files_paths(spec, "accelerate");
        assert_eq!(
            out,
            "%{python_sitelib}/accelerate/\n%{python_sitelib}/accelerate-%{version}.dist-info/\n"
        );
        // Idempotent: a second pass changes nothing.
        assert_eq!(normalize_files_paths(&out, "accelerate"), out);
    }

    #[test]
    fn a_module_name_that_is_not_the_package_name_is_left_alone() {
        // PyJWT installs as `jwt`; only the agent can know that from
        // top_level.txt, so the module line must survive untouched.
        let spec = "%files %{python_files}\n%{python_sitelib}/jwt/\n";
        assert_eq!(normalize_files_paths(spec, "PyJWT"), spec);
    }

    #[test]
    fn a_script_named_like_the_package_is_not_made_a_directory() {
        let spec = "%files %{python_files}\n%{_bindir}/black\n\
                    %python_alternative %{_bindir}/black\n\
                    %{_bindir}/black/\n%{python_sitelib}/Black\n\
                    %{$python_sitelib}/Black/\n";
        let out = normalize_files_paths(spec, "Black");
        assert!(out.contains("\n%{_bindir}/black\n"), "{out}");
        assert!(
            out.contains("\n%python_alternative %{_bindir}/black\n"),
            "{out}"
        );
        assert!(out.contains("\n%{python_sitelib}/black/\n"), "{out}");
        assert!(out.contains("\n%{$python_sitelib}/black/\n"), "{out}");
    }

    #[test]
    fn display_name_in_files_is_normalized() {
        let spec = "%files %{python_files}\n%{python_sitearch}/MarkupSafe\n";
        let out = normalize_files_paths(spec, "MarkupSafe");
        assert!(out.contains("%{python_sitearch}/markupsafe/"), "{out}");
    }
}

#[cfg(test)]
mod expand_order_tests {
    use super::expand_once;
    use std::collections::HashMap;

    #[test]
    fn longer_macro_names_win_over_their_prefixes() {
        let macros: HashMap<String, String> = [("rel", "1"), ("release", "2")]
            .into_iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        assert_eq!(expand_once("%release", &macros), "2");
        assert_eq!(expand_once("%rel", &macros), "1");
    }
}

#[cfg(test)]
mod review_fix_tests {
    use super::*;

    fn pkgver(spec: &str) -> Option<String> {
        collect_simple_defines(spec).get("pkgver").cloned()
    }

    #[test]
    fn define_follows_the_evaluable_branch() {
        let tmpl = |flag: &str| {
            format!(
                "%define flag {flag}\n%if 0%{{?flag}}\n%define pkgver 2.0\n%else\n%define pkgver 1.0\n%endif\n"
            )
        };
        assert_eq!(pkgver(&tmpl("1")).as_deref(), Some("2.0"));
        assert_eq!(pkgver(&tmpl("0")).as_deref(), Some("1.0"));
    }

    #[test]
    fn define_in_unevaluable_branch_is_not_recorded() {
        let spec = "%if 0%{?suse_version}\n%define pkgver 2.0\n%else\n%define pkgver 1.0\n%endif\n";
        assert_eq!(pkgver(spec), None);
        let spec = "%ifarch x86_64\n%define pkgver 2.0\n%endif\n";
        assert_eq!(pkgver(spec), None);
        assert_eq!(
            pkgver("%if 1\n%define pkgver 3.0\n%endif\n").as_deref(),
            Some("3.0")
        );
    }

    #[test]
    fn autosetup_fix_only_replaces_whole_version_component() {
        let spec = "Name: sqlite3\nVersion: 3\n%autosetup -n wrong\n";
        let out = fix_autosetup_dir(spec, "sqlite3-3").unwrap();
        assert!(out.contains("-n sqlite3-%{version}"), "{out}");
        let spec = "Name: markupsafe\nVersion: 3.0.3\n%autosetup -n wrong\n";
        let out = fix_autosetup_dir(spec, "MarkupSafe-3.0.3").unwrap();
        assert!(out.contains("-n MarkupSafe-%{version}"), "{out}");
    }

    #[test]
    fn dist_info_rewrite_keeps_the_trailing_file() {
        let out = normalize_files_paths(
            "%license %{python_sitelib}/foo-%{version}.dist-info/LICENSE\n",
            "foo",
        );
        assert_eq!(
            out,
            "%license %{python_sitelib}/foo-%{version}.dist-info/LICENSE\n"
        );
    }

    #[test]
    fn non_spdx_classifiers_do_not_produce_an_empty_or_osi_name() {
        let l = |c: &str| license_tag(&License::Classifiers(vec![c.to_string()]));
        assert_eq!(l("License :: Other/Proprietary License"), "FIXME");
        assert_eq!(l("License :: OSI Approved :: MIT License"), "MIT");
    }

    #[test]
    fn build_requires_lists_names_without_bounds_or_duplicates() {
        use BuildRequire::*;
        let spec = "Name: foo\n\
                    BuildRequires:  cmake >= 3.10\n\
                    BuildRequires:  gcc-c++, libfoo-devel<2 libbar-devel\n\
                    BuildRequires:  cmake\n\
                    BuildRequires(pre):  fdupes\n\
                    Requires:       notthis\n\
                    BuildRequires:  libx >=1.0\n";
        assert_eq!(
            build_requires(spec),
            [
                Package("cmake".into()),
                Package("gcc-c++".into()),
                Package("libfoo-devel".into()),
                Package("libbar-devel".into()),
                Package("fdupes".into()),
                Package("libx".into()),
            ]
        );
    }

    #[test]
    fn build_requires_keeps_capabilities_whole() {
        use BuildRequire::*;
        let spec =
            "BuildRequires:  pkgconfig(glib-2.0) >= 2.5 %{python_module foo >= 1} /usr/bin/x\n";
        assert_eq!(
            build_requires(spec),
            [
                Capability("pkgconfig(glib-2.0)".into()),
                Capability("%{python_module foo >= 1}".into()),
                Capability("/usr/bin/x".into()),
            ]
        );
    }
}

/// Lines of `spec` that still carry a `TODO` or `FIXME` placeholder, as `(line number,
/// text)`. The skeleton generators leave these for the agent to fill in, and a
/// spec that keeps one builds an empty or half-described package. The
/// `%changelog` is history, not a placeholder, and is not searched.
pub fn todo_placeholders(spec: &str) -> Vec<(usize, String)> {
    let mut found = Vec::new();
    for (i, line) in spec.lines().enumerate() {
        if line.trim_end() == "%changelog" {
            break;
        }
        if line.contains("TODO") || line.contains("FIXME") {
            found.push((i + 1, line.trim().to_string()));
        }
    }
    found
}

/// Lines of the spec that set a variable whose only purpose is to tell an
/// upstream build "I accept the licence terms" or "this is not a licence
/// violation" (`I_CONFIRM_THIS_IS_NOT_A_LICENSE_VIOLATION=1`,
/// `ACCEPT_EULA=Y`). A guard like that exists because a person has to decide;
/// the review turn set torchcodec's to get its build past a wheel-state check
/// that protects against shipping FFmpeg builds with a different licence. Comments
/// and `%changelog` are not searched.
pub fn license_guard_overrides(spec: &str) -> Vec<(usize, String)> {
    use std::sync::LazyLock;
    static GUARD: LazyLock<regex::Regex> = LazyLock::new(|| {
        regex::Regex::new(
            r"(?i)\b[A-Z0-9_]*(?:I_CONFIRM|NOT_A_LICENSE_VIOLATION|LICEN[SC]E_VIOLATION|ACCEPT_[A-Z0-9_]*(?:LICEN[SC]E|EULA|TERMS)|AGREE_[A-Z0-9_]*(?:LICEN[SC]E|EULA|TERMS)|ACCEPT_EULA)[A-Z0-9_]*\s*=",
        )
        .expect("static pattern")
    });
    let mut found = Vec::new();
    for (i, line) in spec.lines().enumerate() {
        if line.trim_end() == "%changelog" {
            break;
        }
        if line.trim_start().starts_with('#') {
            continue;
        }
        if GUARD.is_match(line) {
            found.push((i + 1, line.trim().to_string()));
        }
    }
    found
}

/// The comment a spec carries when a licence-confirmation variable was set
/// after checking what the guard asks about (see
/// [`unverified_license_guards`]).
pub const LICENSE_GUARD_EVIDENCE_TAG: &str = "licence-guard-verified:";

/// The guards of [`license_guard_overrides`] that are not backed by a check.
///
/// A guard that asks the builder to CONFIRM A FACT (`I_CONFIRM_THIS_IS_NOT_A_LICENSE_VIOLATION`:
/// the FFmpeg linked is not GPL-licensed) is satisfied honestly by checking the
/// fact: the spec then carries a `# licence-guard-verified: <what was checked,
/// where>` comment of some substance, and the build goes ahead (a package that
/// cannot build is the worst result; a confirmation nobody checked is the other
/// one). A guard that asks to ACCEPT TERMS (`ACCEPT_EULA`, `AGREE_*_TERMS`) is a
/// person's decision, evidence or not, and is always returned.
pub fn unverified_license_guards(spec: &str) -> Vec<(usize, String)> {
    let evidence = spec
        .lines()
        .take_while(|l| l.trim_end() != "%changelog")
        .filter_map(|l| l.trim_start().strip_prefix('#'))
        .filter_map(|c| {
            let c = c.trim();
            let at = c.to_ascii_lowercase().find(LICENSE_GUARD_EVIDENCE_TAG)?;
            Some(c[at + LICENSE_GUARD_EVIDENCE_TAG.len()..].trim().len())
        })
        .any(|len| len >= 30);
    license_guard_overrides(spec)
        .into_iter()
        .filter(|(_, line)| {
            let upper = line.to_ascii_uppercase();
            let terms = ["EULA", "ACCEPT_", "AGREE_", "TERMS"]
                .iter()
                .any(|t| upper.contains(t));
            terms || !evidence
        })
        .collect()
}

#[cfg(test)]
mod sdist_files_tests {
    use super::*;

    fn files(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn a_namespace_distribution_lists_its_real_directory() {
        let f = files(&[
            "otel_flask-0.66b1/src/opentelemetry/instrumentation/flask/__init__.py",
            "otel_flask-0.66b1/src/opentelemetry/instrumentation/flask/package.py",
            "otel_flask-0.66b1/tests/test_flask.py",
            "otel_flask-0.66b1/pyproject.toml",
        ]);
        let entries = sdist_files_entries(&f, "otel_flask-0.66b1", None).unwrap();
        assert_eq!(
            entries,
            [
                "%dir @SITE@/opentelemetry",
                "%dir @SITE@/opentelemetry/instrumentation",
                "@SITE@/opentelemetry/instrumentation/flask/"
            ]
        );
        let spec = "%files %{python_files}\n%license LICENSE\n%{python_sitelib}/opentelemetry_instrumentation_flask/\n%{python_sitelib}/opentelemetry_instrumentation_flask-%{version}.dist-info/\n\n%changelog\n";
        let out = apply_install_entries(spec, &entries);
        assert_eq!(
            out,
            "%files %{python_files}\n%license LICENSE\n%dir %{python_sitelib}/opentelemetry\n%dir %{python_sitelib}/opentelemetry/instrumentation\n%{python_sitelib}/opentelemetry/instrumentation/flask/\n%{python_sitelib}/opentelemetry_instrumentation_flask-%{version}.dist-info/\n\n%changelog\n"
        );
    }

    #[test]
    fn a_different_import_name_replaces_the_pypi_guess_and_a_right_guess_is_kept() {
        let f = files(&[
            "python-docx-1.2.0/src/docx/__init__.py",
            "python-docx-1.2.0/src/docx/x.py",
            "python-docx-1.2.0/tests/t.py",
        ]);
        let entries = sdist_files_entries(&f, "python-docx-1.2.0", None).unwrap();
        assert_eq!(entries, ["@SITE@/docx/"]);
        let wrong = "%files %{python_files}\n%{python_sitelib}/python_docx/\n";
        assert_eq!(
            apply_install_entries(wrong, &entries),
            "%files %{python_files}\n%{python_sitelib}/docx/\n"
        );
        let right = "%files %{python_files}\n%{python_sitearch}/docx/\n";
        assert_eq!(apply_install_entries(right, &entries), right);
    }

    #[test]
    fn an_ambiguous_layout_is_left_to_the_review() {
        // a loose module, or two import trees
        let module = files(&["x-1/six.py", "x-1/tests/t.py"]);
        assert_eq!(sdist_files_entries(&module, "x-1", None), None);
        let two = files(&["x-1/a/__init__.py", "x-1/b/__init__.py"]);
        assert_eq!(sdist_files_entries(&two, "x-1", None), None);
    }
}

#[cfg(test)]
mod license_guard_tests {
    use super::{license_guard_overrides, unverified_license_guards};

    /// torchcodec: the guard asks whether the FFmpeg linked is licence-compatible;
    /// a spec that says what was checked builds, one that does not, or one that
    /// accepts terms, stops.
    #[test]
    fn a_confirmation_backed_by_a_check_is_allowed_but_terms_never_are() {
        let guard = "export I_CONFIRM_THIS_IS_NOT_A_LICENSE_VIOLATION=1\n";
        assert_eq!(unverified_license_guards(guard).len(), 1);
        let ok = format!(
            "# licence-guard-verified: ffmpeg-8 spec configures without --enable-gpl/--enable-nonfree (LGPL-2.1+)\n{guard}"
        );
        assert!(unverified_license_guards(&ok).is_empty());
        // Too little to be a check.
        let thin = format!("# licence-guard-verified: ok\n{guard}");
        assert_eq!(unverified_license_guards(&thin).len(), 1);
        // Accepting terms is a person's decision, whatever the comment says.
        let eula = format!("{ok}ACCEPT_EULA=Y make\n");
        let left = unverified_license_guards(&eula);
        assert_eq!(left.len(), 1);
        assert!(left[0].1.contains("ACCEPT_EULA"));
    }

    #[test]
    fn a_variable_that_confirms_a_licence_is_found_outside_comments() {
        let spec = "%build
# I_CONFIRM_THIS_IS_NOT_A_LICENSE_VIOLATION=1 would bypass it
                    export I_CONFIRM_THIS_IS_NOT_A_LICENSE_VIOLATION=1
                    ACCEPT_EULA=Y make
                    export CMAKE_ARGS=\"-DLICENSE_FILE=x\"
%changelog
- ACCEPT_EULA=Y\n";
        let found = license_guard_overrides(spec);
        assert_eq!(found.len(), 2, "{found:?}");
        assert_eq!(found[0].0, 3);
        assert_eq!(found[1].1, "ACCEPT_EULA=Y make");
    }
}

#[cfg(test)]
mod todo_placeholder_tests {
    use super::todo_placeholders;

    #[test]
    fn finds_markers_outside_the_changelog_only() {
        let spec = "Summary: TODO: fill in summary\n%install\n# TODO: install\n%changelog\n- TODO in history\n";
        let found = todo_placeholders(spec);
        assert_eq!(found.len(), 2);
        assert_eq!(found[0].0, 1);
        assert_eq!(found[1].1, "# TODO: install");
    }

    #[test]
    fn a_finished_spec_has_none() {
        assert!(todo_placeholders("Summary: A tool\n%build\n%make_build\n").is_empty());
    }
}

/// `spec` with its `Version:` set to `version`, keeping everything else: the
/// recipe a version bump carries forward. A `Version:` that is a single macro
/// (`%{package_version}`) is followed to its plain `%define`/`%global` and that
/// is set instead. `None` when the version cannot be found or is computed.
pub fn set_version(spec: &str, version: &str) -> Option<String> {
    let raw = tag_value(spec, "Version:")?;
    let macro_name = raw
        .strip_prefix("%{")
        .and_then(|r| r.strip_suffix('}'))
        .or_else(|| raw.strip_prefix('%'));
    let defines = collect_simple_defines(spec);
    let (prefixes, old_value): (Vec<String>, String) = match macro_name {
        Some(name) if !name.is_empty() && name.chars().all(|c| c.is_alphanumeric() || c == '_') => {
            (
                vec![format!("%define {name}"), format!("%global {name}")],
                defines.get(name)?.clone(),
            )
        }
        Some(_) => return None,
        None if raw.contains('%') => return None,
        None => (vec!["Version:".to_string()], raw.to_string()),
    };
    let mut replaced = false;
    let lines: Vec<String> = spec
        .lines()
        .map(|line| {
            let t = line.trim_start();
            let hit = !replaced
                && prefixes.iter().any(|p| {
                    t.strip_prefix(p.as_str())
                        .is_some_and(|rest| rest.starts_with([' ', '\t']) || rest.is_empty())
                })
                && line.contains(old_value.as_str());
            if hit {
                replaced = true;
                line.replacen(old_value.as_str(), version, 1)
            } else {
                line.to_string()
            }
        })
        .collect();
    replaced.then(|| lines.join("\n") + if spec.ends_with('\n') { "\n" } else { "" })
}

#[cfg(test)]
mod set_version_tests {
    use super::set_version;

    #[test]
    fn plain_version_is_replaced_and_the_rest_kept() {
        let spec = "Name:           python-foo\nVersion:        0.1.0\nRelease:        0\n%build\n";
        let out = set_version(spec, "0.2.0").unwrap();
        assert_eq!(
            out,
            "Name:           python-foo\nVersion:        0.2.0\nRelease:        0\n%build\n"
        );
    }

    #[test]
    fn a_version_macro_sets_its_define() {
        let spec = "%define package_version 1.0\nName: x\nVersion: %{package_version}\n";
        let out = set_version(spec, "2.0").unwrap();
        assert!(out.contains("%define package_version 2.0"), "{out}");
        assert!(out.contains("Version: %{package_version}"), "{out}");
    }

    #[test]
    fn a_computed_version_is_refused() {
        assert!(set_version("Name: x\nVersion: %(echo 1)\n", "2").is_none());
    }
}

#[cfg(test)]
mod normalizer_tests {
    use super::*;

    #[test]
    fn runtime_requires_become_flavor_neutral_and_build_requires_stay() {
        let spec = "BuildRequires:  %{python_module pip}\nRequires:       %{python_module click >= 8.1}\nRequires:       %{python_module python-dotenv}\nRecommends:     %{python_module torch}\nRequires:       %{python_module x if %python-base < 3.11}\n";
        let out = normalize_runtime_python_module(spec);
        assert!(out.contains("BuildRequires:  %{python_module pip}"));
        assert!(out.contains("Requires:       python-click >= 8.1"), "{out}");
        assert!(
            out.contains("Requires:       python-python-dotenv"),
            "{out}"
        );
        assert!(out.contains("Recommends:     python-torch"), "{out}");
        assert!(out.contains("%{python_module x if"), "{out}");
        assert_eq!(normalize_runtime_python_module(&out), out);
    }

    #[test]
    fn dependency_tags_after_the_subpackages_macro_move_above_it() {
        let spec = "Name: x\n%python_subpackages\nRequires:  python-foo\nRecommends: python-bar\n\n%description\nText\n";
        let out = hoist_dependency_tags(spec);
        let sub = out.find("%python_subpackages").unwrap();
        assert!(out.find("Requires:  python-foo").unwrap() < sub, "{out}");
        assert!(out.find("Recommends: python-bar").unwrap() < sub, "{out}");
        assert!(out.contains("%description\nText"));
        assert_eq!(hoist_dependency_tags(&out), out);
    }

    #[test]
    fn the_nvidia_licence_spelling_is_unified() {
        let out =
            normalize_nvidia_license("Name: x\nLicense:        LicenseRef-NvidiaProprietary\n");
        assert!(out.contains("License:        SUSE-NonFree"));
        let out = normalize_nvidia_license("License: NVIDIA Proprietary Software\n");
        assert!(out.contains(NVIDIA_PROPRIETARY));
        assert_eq!(
            normalize_nvidia_license("License: MIT\n"),
            "License:        MIT\n"
        );
    }

    #[test]
    fn a_second_changelog_line_is_dropped() {
        assert_eq!(
            dedupe_changelog_header("a\n%changelog\n\n%changelog\n"),
            "a\n%changelog\n\n"
        );
    }

    #[test]
    fn description_problems_catch_a_repeated_summary_and_an_emoji() {
        let spec = "Summary: A tool\n\n%description\nA tool\n\n%prep\n";
        assert_eq!(description_problems(spec).len(), 1);
        let spec = "Summary: A tool\n\n%description\nFast CLI \u{1F680}\n";
        assert_eq!(description_problems(spec).len(), 1);
        let spec = "Summary: A tool\n\n%description\nA tool that does things well.\n";
        assert!(description_problems(spec).is_empty());
    }

    #[test]
    fn the_licence_entry_check_reads_files() {
        assert!(has_license_file_entry(
            "%files\n%license LICENSE\n%doc README\n"
        ));
        assert!(!has_license_file_entry("%files\n%doc README\n"));
        assert!(has_license_file_entry("Name: x\n"));
    }
}

/// Collapse the per-file licence lines under one `<name>.dist-info/licenses`
/// directory in `%files` (torch lists ~86 `licenses/third_party/...` files) into
/// the directory itself: `%exclude <dir>` beside `%license <dir>`, so the
/// dist-info entry that also matches them does not list them twice. Applies to a
/// directory with at least three such `%license` lines; the `%exclude` lines for
/// the same files are dropped with them.
pub fn collapse_dist_info_license_lines(spec: &str) -> String {
    const MIN_LINES: usize = 3;
    let dir_of = |line: &str| -> Option<String> {
        let rest = line
            .trim()
            .strip_prefix("%license")
            .or_else(|| line.trim().strip_prefix("%exclude"))?;
        if !rest.starts_with(char::is_whitespace) {
            return None;
        }
        let path = rest.trim();
        let (dir, _file) = path.split_once(".dist-info/licenses/")?;
        (!path.contains(char::is_whitespace)).then(|| format!("{dir}.dist-info/licenses"))
    };
    let mut counts: std::collections::HashMap<String, usize> = Default::default();
    for l in spec.lines().filter(|l| l.trim().starts_with("%license")) {
        if let Some(d) = dir_of(l) {
            *counts.entry(d).or_default() += 1;
        }
    }
    counts.retain(|_, n| *n >= MIN_LINES);
    if counts.is_empty() {
        return spec.to_string();
    }
    let mut done: Vec<String> = Vec::new();
    let mut out: Vec<String> = Vec::new();
    for line in spec.lines() {
        match dir_of(line) {
            Some(d) if counts.contains_key(&d) => {
                if !done.contains(&d) {
                    out.push(format!("%exclude {d}"));
                    out.push(format!("%license {d}"));
                    done.push(d);
                }
            }
            _ => out.push(line.to_string()),
        }
    }
    out.join("\n") + if spec.ends_with('\n') { "\n" } else { "" }
}

#[cfg(test)]
mod license_collapse_tests {
    use super::collapse_dist_info_license_lines;

    #[test]
    fn many_license_files_become_the_directory() {
        let mut spec = String::from("%files %{python_files}\n%{python_sitearch}/torch/\n");
        for i in 0..5 {
            spec.push_str(&format!(
                "%license %{{python_sitearch}}/torch-%{{version}}.dist-info/licenses/third_party/l{i}/LICENSE\n"
            ));
        }
        spec.push_str("%{python_sitearch}/torch-%{version}.dist-info/\n\n%changelog\n");
        let out = collapse_dist_info_license_lines(&spec);
        let d = "%{python_sitearch}/torch-%{version}.dist-info/licenses";
        assert_eq!(
            out,
            format!(
                "%files %{{python_files}}\n%{{python_sitearch}}/torch/\n%exclude {d}\n%license {d}\n\
                 %{{python_sitearch}}/torch-%{{version}}.dist-info/\n\n%changelog\n"
            )
        );
        assert_eq!(collapse_dist_info_license_lines(&out), out);
        let two = "%license a.dist-info/licenses/x\n%license a.dist-info/licenses/y\n";
        assert_eq!(collapse_dist_info_license_lines(two), two);
    }
}

/// The lines of the `%check` section of `spec`, empty when there is none.
fn check_section(spec: &str) -> Vec<&str> {
    let body = spec.split("\n%changelog").next().unwrap_or(spec);
    let mut in_check = false;
    let mut out = Vec::new();
    for line in body.lines() {
        let t = line.trim();
        if t == "%check" {
            in_check = true;
            continue;
        }
        if in_check
            && t.starts_with('%')
            && [
                "%files", "%install", "%prep", "%build", "%package", "%description", "%post",
                "%pre", "%preun", "%postun", "%conf", "%generate_buildrequires",
            ]
            .iter()
            .any(|h| t == *h || t.starts_with(&format!("{h} ")))
        {
            break;
        }
        if in_check {
            out.push(line);
        }
    }
    out
}

/// Most `--ignore` options one `%check` may carry before it is a sign that the
/// suite is being cut down for a single root cause (a missing device, a missing
/// fixture directory) instead of replaced by an import smoke test.
pub const MAX_CHECK_IGNORES: usize = 6;

/// `%check` problems a reviewer sends back (convention 67): more than
/// [`MAX_CHECK_IGNORES`] `--ignore` options. Empty when fine.
pub fn check_quality_problems(spec: &str) -> Vec<String> {
    let ignores: usize = check_section(spec)
        .iter()
        .filter(|l| !l.trim_start().starts_with('#'))
        .map(|l| {
            l.split_whitespace()
                .filter(|t| {
                    let t = t.trim_matches(|c| matches!(c, '\'' | '"'));
                    t == "--ignore"
                        || t == "--ignore-glob"
                        || t.starts_with("--ignore=")
                        || t.starts_with("--ignore-glob=")
                })
                .count()
        })
        .sum();
    if ignores > MAX_CHECK_IGNORES {
        return vec![format!(
            "its %check carries {ignores} `--ignore` options: a suite cut down this far has \
             one root cause (a device, a missing fixture directory, a network); replace it \
             with an import smoke test of the package, plus only the tests that can pass \
             (convention 67)"
        )];
    }
    Vec::new()
}

/// Test paths (`tests/test_configs`, `test/data`) the `%check` runs or reads
/// that `exists` says the source does not have. A sdist often leaves the test
/// fixtures out (accelerate's `tests/test_configs`, brotlicffi's `testdata`):
/// the fix is the project's GitHub tag as a second source with the directory
/// extracted in `%prep` (convention 82 b). A path given to `--ignore` or
/// `--deselect` is not read, and one with a `%` or `$` is not a path.
pub fn missing_check_paths(spec: &str, exists: &dyn Fn(&str) -> bool) -> Vec<String> {
    let mut missing: Vec<String> = Vec::new();
    for line in check_section(spec) {
        let t = line.trim();
        if t.starts_with('#') {
            continue;
        }
        let mut skip_next = false;
        for tok in t.split_whitespace() {
            let tok = tok.trim_matches(|c| matches!(c, '\'' | '"'));
            // `--ignore tests/x` (the value in the next word) is not read either.
            if std::mem::take(&mut skip_next) {
                continue;
            }
            if matches!(tok, "--ignore" | "--ignore-glob" | "--deselect") {
                skip_next = true;
                continue;
            }
            let tok = tok.strip_prefix("./").unwrap_or(tok);
            let path = tok.split("::").next().unwrap_or(tok).trim_end_matches('/');
            let under_tests = ["tests/", "test/", "testdata/", "test_data/"]
                .iter()
                .any(|d| path.starts_with(d))
                && path.contains('/');
            let plain = path
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-' | b'/'));
            if under_tests && plain && !path.contains("..") && !exists(path) {
                let path = path.to_string();
                if !missing.contains(&path) {
                    missing.push(path);
                }
            }
        }
    }
    missing
}

/// The review problem for [`missing_check_paths`], when there are any.
pub fn missing_check_path_problems(spec: &str, exists: &dyn Fn(&str) -> bool) -> Vec<String> {
    let missing = missing_check_paths(spec, exists);
    // %prep already extracts a directory from a second source.
    if missing.is_empty() || spec.contains("--wildcards") {
        return Vec::new();
    }
    vec![format!(
        "its %check uses {} which the source archive does not ship: add the project's \
         GitHub tag archive as `Source1` (in a `_service` with `download_files`) and extract \
         the directory in %prep after the autosetup with `tar -xzf %{{SOURCE1}} \
         --strip-components=1 --wildcards '*/<dir>/*'` (convention 82 b); or drop the test \
         that needs it",
        missing.join(", ")
    )]
}

#[cfg(test)]
mod missing_check_path_tests {
    use super::*;

    #[test]
    fn only_absent_test_paths_that_are_run_are_reported() {
        let spec = "%check\n%pytest tests/test_configs tests/a.py::t --ignore=tests/gone %{x}/tests/y\n\
                    # tests/commented\npython -m x test/data/\n\n%files\n";
        let have = |p: &str| p == "tests/a.py";
        assert_eq!(
            missing_check_paths(spec, &have),
            vec!["tests/test_configs".to_string(), "test/data".to_string()]
        );
        assert_eq!(missing_check_path_problems(spec, &|_| true).len(), 0);
        assert_eq!(missing_check_path_problems(spec, &have).len(), 1);
    }
}

/// Add the project's tag archive as a further source and extract `dirs` from it
/// in `%prep` (convention 82 b): a `SourceN: <url>#/<name>` line after the last
/// `Source` line, and `tar -xzf %{SOURCEN} --strip-components=1 --wildcards
/// '*/<dir>/*'` after the `%autosetup`/`%setup` of `%prep`. The `download_files`
/// service that fetches it is written by the pipeline when the file is missing.
/// `version` inside `url` becomes `%{version}`. `None` when the spec has no
/// `Source` line or no setup line in `%prep`, already extracts with
/// `--wildcards`, or a directory is not a plain relative path.
pub fn add_fixture_source(spec: &str, url: &str, version: &str, dirs: &[String]) -> Option<String> {
    let plain = |d: &str| {
        !d.is_empty()
            && !d.starts_with('/')
            && !d.contains("..")
            && d.bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-' | b'/'))
    };
    if dirs.is_empty()
        || !dirs.iter().all(|d| plain(d))
        || spec.contains("--wildcards")
        || url.contains(|c: char| c.is_whitespace() || c == '%' || c == '#')
    {
        return None;
    }
    let lines: Vec<&str> = spec.lines().collect();
    let source_num = |l: &str| -> Option<u32> {
        let rest = l.strip_prefix("Source")?;
        let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
        rest[digits.len()..].trim_start().starts_with(':').then(|| digits.parse().unwrap_or(0))
    };
    let mut last_source = None;
    let mut next = 0;
    for (i, l) in lines.iter().enumerate() {
        if l.starts_with("%description") || l.starts_with("%package") {
            break;
        }
        if let Some(n) = source_num(l) {
            last_source = Some(i);
            next = next.max(n + 1);
        }
    }
    let last_source = last_source?;
    let prep = lines.iter().position(|l| l.trim() == "%prep")?;
    let mut setup = None;
    for (i, l) in lines.iter().enumerate().skip(prep + 1) {
        let t = l.trim();
        if t.starts_with('%')
            && !t.starts_with("%autosetup")
            && !t.starts_with("%setup")
            && !t.starts_with("%{")
            && !t.starts_with("%if")
            && !t.starts_with("%endif")
            && !t.starts_with("%else")
        {
            break;
        }
        if t.starts_with("%autosetup") || t.starts_with("%setup") {
            // The directive may continue on the next lines.
            let mut end = i;
            while lines[end].trim_end().ends_with('\\') && end + 1 < lines.len() {
                end += 1;
            }
            setup = Some(end);
            break;
        }
    }
    let setup = setup?;
    let url = if version.is_empty() { url.to_string() } else { url.replace(version, "%{version}") };
    let file = url.rsplit('/').next().unwrap_or("fixtures.tar.gz");
    let repo = url
        .split('/')
        .nth(4)
        .filter(|r| !r.is_empty())
        .unwrap_or("fixtures");
    let rename = format!("{repo}-fixtures-{file}");
    let wild: Vec<String> = dirs.iter().map(|d| format!("'*/{}/*'", d.trim_end_matches('/'))).collect();
    let tar = format!(
        "tar -xzf %{{SOURCE{next}}} --strip-components=1 --wildcards {}",
        wild.join(" ")
    );
    let mut out: Vec<String> = Vec::with_capacity(lines.len() + 3);
    for (i, l) in lines.iter().enumerate() {
        out.push((*l).to_string());
        if i == last_source {
            out.push(format!("Source{next}:        {url}#/{rename}"));
        }
        if i == setup {
            out.push(format!("# The sdist leaves out {}", dirs.join(", ")));
            out.push(tar.clone());
        }
    }
    let mut text = out.join("\n");
    if spec.ends_with('\n') {
        text.push('\n');
    }
    Some(text)
}

#[cfg(test)]
mod add_fixture_source_tests {
    use super::*;

    const SPEC: &str = "Name: python-x\nVersion: 1.2\nSource0: https://files/x-%{version}.tar.gz\n\
Source1: x.rpmlintrc\nBuildRequires: a\n\n%description\nd\n\n%prep\n%autosetup -p1 \\\n  -n x-%{version}\n\
%build\n%check\n%pytest tests/test_configs\n";

    #[test]
    fn adds_the_next_source_and_the_extract_line() {
        let dirs = vec!["tests/test_configs".to_string()];
        let out = add_fixture_source(
            SPEC,
            "https://github.com/o/x/archive/refs/tags/v1.2.tar.gz",
            "1.2",
            &dirs,
        )
        .unwrap();
        assert!(out.contains(
            "Source1: x.rpmlintrc\nSource2:        https://github.com/o/x/archive/refs/tags/v%{version}.tar.gz#/x-fixtures-v%{version}.tar.gz\nBuildRequires"
        ), "{out}");
        assert!(out.contains(
            "  -n x-%{version}\n# The sdist leaves out tests/test_configs\ntar -xzf %{SOURCE2} --strip-components=1 --wildcards '*/tests/test_configs/*'\n%build"
        ), "{out}");
        // Applied once, no matter how often the check runs.
        assert_eq!(add_fixture_source(&out, "https://github.com/o/x/a.tar.gz", "1.2", &dirs), None);
        assert!(missing_check_path_problems(&out, &|_| false).is_empty());
    }

    #[test]
    fn refuses_what_it_cannot_place() {
        let d = vec!["tests/a".to_string()];
        let u = "https://github.com/o/x/archive/v1.tar.gz";
        assert_eq!(add_fixture_source("Name: x\n%prep\n%autosetup\n", u, "1", &d), None);
        assert_eq!(add_fixture_source("Source0: a\n%description\n%prep\nmake\n", u, "1", &d), None);
        assert_eq!(add_fixture_source(SPEC, u, "1", &["../x".to_string()]), None);
        assert_eq!(add_fixture_source(SPEC, u, "1", &[]), None);
        assert_eq!(add_fixture_source(SPEC, "https://h/a b", "1", &d), None);
    }
}

/// [`add_fixture_source`] for the paths [`missing_check_paths`] reports: the
/// repository comes from the sdist metadata (`pkg_info`, `pyproject`), the tag
/// archive URL from `resolve(repo, version)`. `None` when there is nothing
/// missing, no repository, or no tag, so the reviewer prompt stays the fallback.
pub fn auto_fixture_source(
    spec: &str,
    exists: &dyn Fn(&str) -> bool,
    pkg_info: &str,
    pyproject: &str,
    version: &str,
    resolve: &dyn Fn(&str, &str) -> Option<String>,
) -> Option<String> {
    let missing = missing_check_paths(spec, exists);
    if missing.is_empty() || spec.contains("--wildcards") {
        return None;
    }
    // One directory at the top of tests is enough when the missing path is a
    // file inside a fixtures directory already listed.
    let mut dirs: Vec<String> = Vec::new();
    for m in missing {
        if !dirs.iter().any(|d| m == *d || m.starts_with(&format!("{d}/"))) {
            dirs.retain(|d| !d.starts_with(&format!("{m}/")));
            dirs.push(m);
        }
    }
    let repo = ap_ecosystem::sdist::source_repo_from_metadata(pkg_info, pyproject)?;
    let url = resolve(&repo, version)?;
    add_fixture_source(spec, &url, version, &dirs)
}

#[cfg(test)]
mod auto_fixture_source_tests {
    use super::*;

    const SPEC: &str = "Name: python-x\nVersion: 1.2\nSource0: https://files/x-%{version}.tar.gz\n\
%description\nd\n\n%prep\n%autosetup\n%check\n%pytest tests/test_configs tests/test_configs/a.yml\n";
    const PI: &str = "Project-URL: Source, https://github.com/o/x\n";

    #[test]
    fn resolves_the_repo_and_tag_or_leaves_the_prompt() {
        let found = |r: &str, v: &str| Some(format!("{r}/archive/refs/tags/v{v}.tar.gz"));
        let out = auto_fixture_source(SPEC, &|_| false, PI, "", "1.2", &found).unwrap();
        assert!(out.contains("Source1:        https://github.com/o/x/archive/refs/tags/v%{version}.tar.gz#/"));
        // The file inside the directory does not add a second wildcard.
        assert!(out.contains("--wildcards '*/tests/test_configs/*'\n"), "{out}");
        assert_eq!(out.matches("--wildcards").count(), 1);
        assert_eq!(auto_fixture_source(SPEC, &|_| false, PI, "", "1.2", &|_, _| None), None);
        assert_eq!(auto_fixture_source(SPEC, &|_| false, "", "", "1.2", &found), None);
        assert_eq!(auto_fixture_source(SPEC, &|_| true, PI, "", "1.2", &found), None);
    }
}

#[cfg(test)]
mod sharp_edge_tests {
    use super::*;

    #[test]
    fn a_licence_term_is_added_to_the_whole_expression() {
        let t = |l: &str| with_license_terms(l.to_string(), &["SUSE-NonFree"]);
        assert_eq!(t("MIT"), "MIT AND SUSE-NonFree");
        assert_eq!(t("MIT OR Apache-2.0"), "(MIT OR Apache-2.0) AND SUSE-NonFree");
        // Parenthesised at the start is not parenthesised as a whole.
        assert_eq!(
            t("(A OR B) AND C OR D"),
            "((A OR B) AND C OR D) AND SUSE-NonFree"
        );
        assert_eq!(t("(A OR B)"), "(A OR B) AND SUSE-NonFree");
        assert_eq!(t("A AND (B OR C)"), "A AND (B OR C) AND SUSE-NonFree");
        assert_eq!(t("SUSE-NonFree OR MIT"), "SUSE-NonFree OR MIT");
        assert_eq!(t("FIXME"), "FIXME");
        assert!(!has_top_level_or("ORACLE-1.0"));
        assert!(has_top_level_or("A OR(B)"));
    }

    #[test]
    fn only_the_vendor_term_of_a_licence_expression_is_replaced() {
        assert_eq!(spdx_expression_for_spec("LicenseRef-NVIDIA-Proprietary"), "SUSE-NonFree");
        assert_eq!(
            spdx_expression_for_spec("MIT AND LicenseRef-NVIDIA-Proprietary"),
            "MIT AND SUSE-NonFree"
        );
        assert_eq!(
            spdx_expression_for_spec("(Apache-2.0 OR MIT) AND LicenseRef-Proprietary"),
            "(Apache-2.0 OR MIT) AND SUSE-NonFree"
        );
        assert_eq!(spdx_expression_for_spec("MIT"), "MIT");
        assert_eq!(spdx_expression_for_spec("MIT OR Apache-2.0"), "MIT OR Apache-2.0");
        // Two vendor terms are one, with no operator left dangling.
        assert_eq!(
            spdx_expression_for_spec("MIT AND LicenseRef-NVIDIA-A AND LicenseRef-NVIDIA-B"),
            "MIT AND SUSE-NonFree"
        );
        assert_eq!(
            spdx_expression_for_spec("LicenseRef-NVIDIA-A AND LicenseRef-NVIDIA-B AND MIT"),
            "SUSE-NonFree AND MIT"
        );
        assert_eq!(
            spdx_expression_for_spec("(LicenseRef-NVIDIA-A OR LicenseRef-Proprietary)"),
            "(SUSE-NonFree)"
        );
    }

    #[test]
    fn a_percent_in_upstream_text_does_not_become_a_macro() {
        let mut r = Release {
            name: "x".into(),
            version: "1".into(),
            summary: Some("100% pure %{python_sitelib} tool".into()),
            homepage: Some("https://example.org/%7Euser".into()),
            license: License::Spdx("MIT".into()),
            requires_python: None,
            requires_dist: Vec::new(),
            yanked: false,
            sdist: ap_ecosystem::pypi::Sdist {
                filename: String::new(),
                url: String::new(),
                sha256: String::new(),
                size: 0,
            },
        };
        let spec = wheel_binary_skeleton("x", "1", "x-1-py3-none-any.whl", &r, Some(true), None);
        assert!(spec.contains("Summary:        100%% pure %%{python_sitelib} tool\n"), "{spec}");
        assert!(spec.contains("URL:            https://example.org/%%7Euser\n"), "{spec}");
        r.summary = Some("two\nlines".into());
        let spec = wheel_binary_skeleton("x", "1", "x-1-py3-none-any.whl", &r, Some(true), None);
        assert!(spec.contains("Summary:        two lines\n"), "{spec}");
        let mono = monorepo_skeleton("x", "1", &[], Some(&r));
        assert!(mono.contains("Summary:        two lines\n"), "{mono}");
    }

    #[test]
    fn ignore_options_are_counted_as_options() {
        let spec = format!(
            "%check\npip install --ignore-installed x\n%pytest {}\n",
            (0..6).map(|i| format!("--ignore=t/{i}.py")).collect::<Vec<_>>().join(" ")
        );
        assert!(check_quality_problems(&spec).is_empty());
        let seven = format!("%check\n%pytest {} --ignore t/x\n", "--ignore-glob=a ".repeat(6));
        assert_eq!(check_quality_problems(&seven).len(), 1);
    }

    #[test]
    fn a_path_given_to_ignore_in_the_next_word_is_not_read() {
        let spec = "%check\n%pytest --ignore tests/gone --deselect tests/x.py::t tests/real\n";
        assert_eq!(missing_check_paths(spec, &|_| false), vec!["tests/real".to_string()]);
    }
}

/// Drop the `python-pytest` BuildRequires of a spec whose `%check` never runs
/// pytest (an import smoke test): the dependency only slows the build and
/// pulls plugins in. A spec with no `%check` section, or one that mentions
/// pytest, is left alone.
pub fn drop_unused_pytest_build_requires(spec: &str) -> String {
    let check = check_section(spec);
    if check.is_empty()
        || check
            .iter()
            .any(|l| !l.trim_start().starts_with('#') && l.contains("pytest"))
    {
        return spec.to_string();
    }
    let mut out: Vec<&str> = Vec::new();
    for line in spec.lines() {
        let t = line.trim();
        let is_pytest = t.starts_with("BuildRequires:")
            && t.trim_start_matches("BuildRequires:").trim().split_whitespace().eq([
                "%{python_module",
                "pytest}",
            ]);
        if !is_pytest {
            out.push(line);
        }
    }
    out.join("\n") + if spec.ends_with('\n') { "\n" } else { "" }
}

#[cfg(test)]
mod check_quality_tests {
    use super::*;

    #[test]
    fn many_ignores_are_flagged_and_few_are_not() {
        let many = format!(
            "%check\n%pytest {}\n\n%files\n",
            (0..7).map(|i| format!("--ignore=t/{i}.py")).collect::<Vec<_>>().join(" ")
        );
        assert_eq!(check_quality_problems(&many).len(), 1);
        let few = "%check\n%pytest --ignore=a --ignore=b\n";
        assert!(check_quality_problems(few).is_empty());
        // An ignore in another section does not count.
        let other = format!("%install\n{}\n%check\nx\n", "--ignore ".repeat(9));
        assert!(check_quality_problems(&other).is_empty());
    }

    #[test]
    fn pytest_is_dropped_only_when_check_does_not_run_it() {
        let import = "BuildRequires:  %{python_module pytest}\nBuildRequires:  %{python_module pytest-xdist}\n%check\n%python_expand $python -c 'import x'\n\n%files\n";
        let out = drop_unused_pytest_build_requires(import);
        assert!(!out.contains("module pytest}"), "{out}");
        assert!(out.contains("pytest-xdist"));
        let runs = "BuildRequires:  %{python_module pytest}\n%check\n%pytest\n";
        assert_eq!(drop_unused_pytest_build_requires(runs), runs);
        let none = "BuildRequires:  %{python_module pytest}\n%files\n";
        assert_eq!(drop_unused_pytest_build_requires(none), none);
    }
}

/// Whether the spec's `%check` does nothing but test that a file exists
/// (`test -e`, `test -x`, `ls`, also behind `%python_expand`): it has at least
/// one such command and no other. A package that ships Python modules is
/// imported instead (convention 67).
pub fn check_is_file_probe_only(spec: &str) -> bool {
    let body = spec.split("\n%changelog").next().unwrap_or(spec);
    let mut in_check = false;
    let mut probes = 0;
    for line in body.lines() {
        let t = line.trim();
        if t == "%check" {
            in_check = true;
            continue;
        }
        const SECTIONS: &[&str] = &[
            "%files",
            "%changelog",
            "%install",
            "%prep",
            "%build",
            "%package",
            "%description",
            "%post",
            "%pre",
            "%preun",
            "%postun",
            "%conf",
            "%generate_buildrequires",
        ];
        if in_check
            && SECTIONS
                .iter()
                .any(|h| t == *h || t.starts_with(&format!("{h} ")))
        {
            break;
        }
        if !in_check || t.is_empty() || t.starts_with('#') {
            continue;
        }
        let command = t.strip_prefix("%python_expand").map(str::trim).unwrap_or(t);
        if command.starts_with("test -e")
            || command.starts_with("test -x")
            || command.starts_with("test -f")
            || command.starts_with("ls ")
        {
            probes += 1;
        } else if t.starts_with("%if") || t.starts_with("%endif") || t.starts_with("%else") {
            continue;
        } else {
            return false;
        }
    }
    probes > 0
}

#[cfg(test)]
mod console_script_tests {
    use super::add_console_script_alternatives;

    const SPEC: &str = "Name: python-x\n%python_subpackages\n\n%install\n%pyproject_install\n\
                        %python_expand %fdupes x\n\n%files %{python_files}\n%license LICENSE\n\
                        %{python_sitelib}/x\n\n%changelog\n";

    #[test]
    fn scripts_get_clone_scriptlets_and_files_entries() {
        let out = add_console_script_alternatives(SPEC, &["xcli".to_string()]);
        assert!(out.contains("%pyproject_install\n%python_clone -a %{buildroot}%{_bindir}/xcli\n"));
        assert!(out.contains("%post\n%python_install_alternative xcli\n"), "{out}");
        assert!(out.contains("%postun\n%python_uninstall_alternative xcli\n"), "{out}");
        assert!(out.contains("%files %{python_files}\n%python_alternative %{_bindir}/xcli\n"));
        assert!(out.contains("Requires(post):   update-alternatives"));
        // Idempotent, and a no-script project is untouched.
        assert_eq!(add_console_script_alternatives(&out, &["xcli".to_string()]), out);
        assert_eq!(add_console_script_alternatives(SPEC, &[]), SPEC);
    }
}

#[cfg(test)]
mod check_probe_tests {
    use super::check_is_file_probe_only;

    #[test]
    fn only_file_probes_are_weak() {
        let weak =
            "%check\n%python_expand test -e %{buildroot}%{$python_sitearch}/x/y.so\n\n%files\n";
        assert!(check_is_file_probe_only(weak));
        let real = "%check\n%python_expand test -e a\n%pytest_arch tests/\n";
        assert!(!check_is_file_probe_only(real));
        let import = "%check\n%python_expand $python -c 'import x'\n";
        assert!(!check_is_file_probe_only(import));
        assert!(!check_is_file_probe_only("%install\nx\n"));
    }
}

/// Parse `op version` (`>= 0.1.10`, `< 0.1.12`, `= 2.0`) of one RPM dependency
/// item and say how `shipped` stands against it: `Some(None)` when it meets it,
/// `Some(Some(true))` when it is too new, `Some(Some(false))` when too old.
/// `None` when it is not a constraint this reads.
pub(crate) fn rpm_constraint_miss(
    constraint: &str,
    shipped: &pep440_rs::Version,
) -> Option<Option<bool>> {
    use std::str::FromStr;
    let c = constraint.trim();
    let (op, rest) = ["<=", ">=", "==", "<", ">", "="]
        .iter()
        .find_map(|op| c.strip_prefix(op).map(|r| (*op, r.trim())))?;
    // An RPM version may carry an epoch or a release: only the upstream part.
    let ver = rest.split(['-', ' ']).next().unwrap_or(rest);
    let ver = ver.rsplit_once(':').map(|(_, v)| v).unwrap_or(ver);
    let wanted = pep440_rs::Version::from_str(ver).ok()?;
    let met = match op {
        "<=" => shipped <= &wanted,
        ">=" => shipped >= &wanted,
        "<" => shipped < &wanted,
        ">" => shipped > &wanted,
        _ => shipped == &wanted,
    };
    if met {
        return Some(None);
    }
    Some(Some(match op {
        "<=" | "<" => true,
        ">=" | ">" => false,
        _ => shipped > &wanted,
    }))
}

/// The dependency lines of `spec` for `dependency` with every version
/// constraint the version `shipped` misses removed, the name kept: a package the
/// distro ships is used as it is, and the bound that excludes it (an upstream
/// `<= 0.1.11`, an exact `==` pin) is dropped instead of the dependency
/// (convention 59). Constraints it meets stay. Duplicate lines this leaves are
/// merged.
pub fn relax_dependency_bounds(spec: &str, dependency: &str, shipped: &str) -> String {
    use std::str::FromStr;
    let Ok(shipped_v) = pep440_rs::Version::from_str(shipped.trim()) else {
        return spec.to_string();
    };
    let wanted = crate::osc::fuzzy_key(dependency);
    let mut out: Vec<String> = Vec::new();
    let mut seen: std::collections::HashSet<String> = Default::default();
    for line in spec.lines() {
        let t = line.trim_start();
        let is_dep_line = ["BuildRequires", "Requires", "Recommends", "Suggests"]
            .iter()
            .any(|tag| {
                t.strip_prefix(tag)
                    .is_some_and(|r| r.starts_with(':') || r.starts_with('('))
            });
        if !is_dep_line {
            out.push(line.to_string());
            continue;
        }
        let (head, value) = line.split_once(':').unwrap_or((line, ""));
        let items: Vec<String> = value
            .split(',')
            .map(|item| {
                let item = item.trim();
                // `%{python_module name >= v}` or `python-name >= v`.
                let (open, inner, close) = match item
                    .strip_prefix("%{python_module ")
                    .and_then(|r| r.strip_suffix('}'))
                {
                    Some(inner) => ("%{python_module ", inner, "}"),
                    None => ("", item, ""),
                };
                let name_end = inner
                    .find(|c: char| c.is_whitespace() || matches!(c, '<' | '>' | '='))
                    .unwrap_or(inner.len());
                let name = &inner[..name_end];
                let key = crate::osc::fuzzy_key(
                    name.strip_prefix("python-")
                        .or_else(|| name.strip_prefix("python3-"))
                        .unwrap_or(name),
                );
                let key_plain = crate::osc::fuzzy_key(name);
                if key != wanted && key_plain != wanted {
                    return item.to_string();
                }
                let constraint = inner[name_end..].trim();
                if constraint.is_empty() || constraint.contains(" if ") {
                    return item.to_string();
                }
                // `%{python_module trio >= 0.25.1 with %python-trio < 0.34}`:
                // each clause is judged on its own, the ones met stay.
                if !open.is_empty() && constraint.contains(" with ") {
                    let clauses: Vec<&str> = constraint.split(" with ").collect();
                    let kept: Vec<&str> = clauses
                        .iter()
                        .map(|c| c.trim_start_matches(|ch: char| !matches!(ch, '<' | '>' | '=')))
                        .map(str::trim)
                        .filter(|c| !matches!(rpm_constraint_miss(c, &shipped_v), Some(Some(_))))
                        .collect();
                    return if kept.len() == clauses.len() {
                        item.to_string()
                    } else if kept.is_empty() {
                        format!("{open}{name}{close}")
                    } else {
                        format!(
                            "{open}{name} {}{close}",
                            kept.join(&format!(" with %python-{name} "))
                        )
                    };
                }
                match rpm_constraint_miss(constraint, &shipped_v) {
                    Some(Some(_)) => format!("{open}{name}{close}"),
                    _ => item.to_string(),
                }
            })
            .collect();
        let rebuilt = format!(
            "{head}:{}{}",
            if value.starts_with(' ') || value.starts_with('\t') {
                &value[..value.len() - value.trim_start().len()]
            } else {
                " "
            },
            items.join(", ")
        );
        // A line that reads the same as one already kept is not repeated.
        if rebuilt != line && !seen.insert(rebuilt.trim().to_string()) {
            continue;
        }
        seen.insert(rebuilt.trim().to_string());
        out.push(rebuilt);
    }
    out.join("\n") + if spec.ends_with('\n') { "\n" } else { "" }
}

/// [`relax_dependency_bounds`] for each `(dependency, shipped version)` a
/// package's state records (`PkgState::relaxed_dependencies`): applied on every
/// validation, root package included, before anything is built.
pub fn relax_dependencies(spec: &str, relaxed: &[(String, String)]) -> String {
    relaxed.iter().fold(spec.to_string(), |spec, (dep, shipped)| {
        relax_dependency_bounds(&spec, dep, shipped)
    })
}

#[cfg(test)]
mod relax_bound_tests {
    use super::{relax_dependencies, relax_dependency_bounds};

    #[test]
    fn a_bound_the_shipped_version_misses_is_dropped_the_name_kept() {
        let spec = "BuildRequires:  %{python_module apache-tvm-ffi >= 0.1.10}\nBuildRequires:  %{python_module apache-tvm-ffi < 0.1.12}\nRequires:       python-apache-tvm-ffi >= 0.1.10, python-apache-tvm-ffi <= 0.1.11\nRequires:       python-numpy < 1\n";
        let out = relax_dependency_bounds(spec, "apache-tvm-ffi", "0.1.14.post1");
        assert!(
            out.contains("BuildRequires:  %{python_module apache-tvm-ffi >= 0.1.10}"),
            "{out}"
        );
        assert!(
            out.contains("BuildRequires:  %{python_module apache-tvm-ffi}"),
            "{out}"
        );
        assert!(!out.contains("< 0.1.12"), "{out}");
        assert!(
            out.contains("python-apache-tvm-ffi >= 0.1.10, python-apache-tvm-ffi"),
            "{out}"
        );
        assert!(!out.contains("<= 0.1.11"), "{out}");
        assert!(out.contains("python-numpy < 1"), "{out}");
    }

    /// python-pycrdt: a rich `with` dependency whose upper clause the distro's
    /// trio 0.34.0 misses; only the clause it misses goes.
    #[test]
    fn a_missed_clause_of_a_with_dependency_is_dropped_the_met_one_stays() {
        let spec = "BuildRequires:  %{python_module trio >= 0.25.1 with %python-trio < 0.34}\n";
        assert_eq!(
            relax_dependency_bounds(spec, "trio", "0.34.0"),
            "BuildRequires:  %{python_module trio >= 0.25.1}\n"
        );
        assert_eq!(
            relax_dependency_bounds(
                "BuildRequires:  %{python_module trio >= 1 with %python-trio < 2}\n",
                "trio",
                "3.0"
            ),
            "BuildRequires:  %{python_module trio >= 1}\n"
        );
        assert_eq!(
            relax_dependency_bounds(
                "BuildRequires:  %{python_module trio >= 5 with %python-trio < 6}\n",
                "trio",
                "3.0"
            ),
            "BuildRequires:  %{python_module trio < 6}\n"
        );
        // Both met: untouched.
        assert_eq!(
            relax_dependency_bounds(spec, "trio", "0.30.0"),
            spec
        );
    }

    /// open-webui declares `==` pins on packages the distro ships newer:
    /// BuildRequires, Requires and a rich dependency all lose the pin and keep
    /// the name, whichever package's spec it is (the root included), and a pin
    /// the shipped version meets stays.
    #[test]
    fn every_recorded_pin_is_relaxed_in_every_dependency_line() {
        let spec = "Name:           python-open-webui\n\
                    BuildRequires:  %{python_module pydantic == 2.13.4}\n\
                    BuildRequires:  %{python_module redis == 8.0.1}\n\
                    BuildRequires:  %{python_module mcp == 1.27.2}\n\
                    Requires:       python-pydantic == 2.13.4\n\
                    Requires:       python-redis = 8.0.1\n\
                    Requires:       python-mcp == 1.27.2\n\
                    Recommends:     python-redis == 8.0.1\n\
                    %python_subpackages\n";
        let relaxed = vec![
            ("pydantic".to_string(), "2.13.5".to_string()),
            ("redis".to_string(), "8.1.0".to_string()),
        ];
        let out = relax_dependencies(spec, &relaxed);
        assert_eq!(
            out,
            "Name:           python-open-webui\n\
             BuildRequires:  %{python_module pydantic}\n\
             BuildRequires:  %{python_module redis}\n\
             BuildRequires:  %{python_module mcp == 1.27.2}\n\
             Requires:       python-pydantic\n\
             Requires:       python-redis\n\
             Requires:       python-mcp == 1.27.2\n\
             Recommends:     python-redis\n\
             %python_subpackages\n"
        );
        assert_eq!(relax_dependencies(spec, &[]), spec);
    }

    #[test]
    fn an_exact_pin_is_relaxed_and_a_met_bound_stays() {
        let out = relax_dependency_bounds(
            "Requires: python-x = 1.0\nRequires: python-y >= 1\n",
            "x",
            "2.0",
        );
        assert!(out.contains("Requires: python-x\n"), "{out}");
        assert!(out.contains("python-y >= 1"));
        let same = "Requires: python-x >= 1\n";
        assert_eq!(relax_dependency_bounds(same, "x", "2.0"), same);
    }
}

/// `spec` with `BuildRequires:  <requirement>` added after the last
/// `BuildRequires:` line, or `None` when it is already there. Used to settle a
/// provider the solver cannot choose between.
pub fn add_build_requires(spec: &str, requirement: &str) -> Option<String> {
    let wanted = requirement.trim();
    let has = spec.lines().any(|l| {
        l.trim_start()
            .strip_prefix("BuildRequires:")
            .is_some_and(|v| v.trim() == wanted)
    });
    if has {
        return None;
    }
    let lines: Vec<&str> = spec.lines().collect();
    let at = lines
        .iter()
        .rposition(|l| l.trim_start().starts_with("BuildRequires:"))?;
    let mut out: Vec<String> = lines.iter().map(|l| l.to_string()).collect();
    out.insert(at + 1, format!("BuildRequires:  {wanted}"));
    Some(out.join("\n") + if spec.ends_with('\n') { "\n" } else { "" })
}

#[cfg(test)]
mod runtime_dependency_tests {
    use super::*;

    fn dist(lines: &[&str]) -> Vec<String> {
        lines.iter().map(|s| s.to_string()).collect()
    }

    /// fastapi-cli: PyPI lists its dependencies, and the generated spec had
    /// none of them (the review added them by hand).
    #[test]
    fn only_unconditional_dependencies_with_a_lower_bound_are_kept() {
        let deps = runtime_dependencies(
            &dist(&[
                "typer>=0.16.0",
                "uvicorn[standard]>=0.15.0",
                "tomli>=2.0.0; python_version < \"3.11\"",
                "fastapi-cloud-cli>=0.1.1; extra == \"standard\"",
                "rich-toolkit==0.14.8",
                "mcp<3.0.0,>=2.0.0",
                "pydantic (>=2,<3)",
                "idna!=3.0",
                "fastapi-cli",
                "Fastapi_CLI>=1",
            ]),
            "fastapi-cli",
        );
        let shown: Vec<String> = deps
            .iter()
            .map(|d| {
                format!(
                    "{}{}",
                    d.name,
                    d.bound
                        .as_deref()
                        .map(|b| format!(" {b}"))
                        .unwrap_or_default()
                )
            })
            .collect();
        assert_eq!(
            shown,
            [
                "typer >= 0.16.0",
                "uvicorn >= 0.15.0",
                "rich-toolkit >= 0.14.8",
                "mcp >= 2.0.0",
                "pydantic >= 2",
                "idna"
            ]
        );
    }

    #[test]
    fn requires_go_before_the_subpackages_and_are_mirrored_into_build_requires() {
        let spec = "Name: python-x\nBuildRequires:  python-rpm-macros\nBuildRequires:  %{python_module pip}\nRequires:       python-click\n%python_subpackages\n\n%description\nx\n";
        let out = add_runtime_dependencies(spec, &dist(&["click>=8", "rich>=13.0"]), "x");
        assert!(
            out.contains("Requires:       python-click\nRequires:       python-rich >= 13.0\n%python_subpackages"),
            "{out}"
        );
        assert!(
            out.contains("BuildRequires:  %{python_module rich >= 13.0}"),
            "{out}"
        );
        assert!(
            out.contains("BuildRequires:  %{python_module click >= 8}"),
            "{out}"
        );
        // Nothing to add twice.
        assert_eq!(
            add_runtime_dependencies(&out, &dist(&["click>=8", "rich>=13.0"]), "x"),
            out
        );
    }
}

#[cfg(test)]
mod add_build_requires_tests {
    use super::add_build_requires;

    #[test]
    fn the_line_goes_after_the_last_build_requires_once() {
        let spec = "Name: x\nBuildRequires:  a\nBuildRequires:  b\nRequires: c\n";
        let out = add_build_requires(spec, "%{python_module nvidia-nccl-cu13}").unwrap();
        assert_eq!(
            out,
            "Name: x\nBuildRequires:  a\nBuildRequires:  b\nBuildRequires:  %{python_module nvidia-nccl-cu13}\nRequires: c\n"
        );
        assert!(add_build_requires(&out, "%{python_module nvidia-nccl-cu13}").is_none());
        assert!(add_build_requires("Name: x\n", "a").is_none());
    }
}

#[cfg(test)]
mod conditional_dependency_tests {
    use super::parenthesize_conditional_dependencies;

    #[test]
    fn a_bare_conditional_dependency_gets_its_parentheses() {
        let spec = "Requires:       python-tomli >= 2.0.0 if python-base < 3.11\nRequires:       python-a, python-b if python-base < 3.12\nBuildRequires:  %{python_module tomli >= 2.0.0 if %python-base < 3.11}\nRequires:       (python-x if python-base < 3.11)\nRequires:       python-plain >= 1\n# Requires: a if b\n";
        let out = parenthesize_conditional_dependencies(spec);
        assert_eq!(
            out,
            "Requires:       (python-tomli >= 2.0.0 if python-base < 3.11)\nRequires:       python-a, (python-b if python-base < 3.12)\nBuildRequires:  %{python_module tomli >= 2.0.0 if %python-base < 3.11}\nRequires:       (python-x if python-base < 3.11)\nRequires:       python-plain >= 1\n# Requires: a if b\n"
        );
        assert_eq!(parenthesize_conditional_dependencies(&out), out);
    }
}

#[cfg(test)]
mod hashed_libs_exclude_tests {
    use super::*;

    #[test]
    fn the_exclude_is_added_before_the_subpackages_once() {
        let spec = "Name: python-x\n%global __provides_exclude_from ^x$\n%python_subpackages\n\n%description\nd\n";
        let out = with_hashed_libs_requires_exclude(spec);
        assert!(out.contains(&format!(
            "%global __requires_exclude {}\n%python_subpackages",
            hashed_libs_requires_exclude()
        )));
        assert_eq!(with_hashed_libs_requires_exclude(&out), out);
    }

    #[test]
    fn an_existing_exclude_is_folded_into_one_regex() {
        let spec = "%global __requires_exclude ^lib(mpi|ucp)\\.so.*$\n%python_subpackages\n";
        let out = with_hashed_libs_requires_exclude(spec);
        assert_eq!(out.matches("__requires_exclude").count(), 1, "{out}");
        let want = format!(
            r"^(lib(mpi|ucp)\.so.*|{})$",
            hashed_libs_requires_exclude().trim_start_matches('^').trim_end_matches('$')
        );
        assert!(out.contains(&want), "{out}");
    }
}

#[cfg(test)]
mod redist_requires_tests {
    use super::*;
    use crate::redist::TestMode;

    fn dist(lines: &[&str]) -> Vec<String> {
        lines.iter().map(|s| s.to_string()).collect()
    }

    const SPEC: &str = "Name: python-x\nBuildRequires:  python-rpm-macros\nBuildRequires:  %{python_module pip}\n%python_subpackages\n\n%description\nx\n";

    #[test]
    fn a_cuda_wheel_requires_the_cuda_package_with_the_mode_on() {
        let reqs = dist(&["nvidia-cublas>=13.1", "numpy>=2"]);
        {
            let _off = TestMode::set(false);
            let out = add_runtime_dependencies(SPEC, &reqs, "x");
            assert!(out.contains("Requires:       python-nvidia-cublas >= 13.1\n"), "{out}");
        }
        let _on = TestMode::set(true);
        let deps = runtime_dependencies(&reqs, "x");
        assert_eq!(deps[0].bound, None);
        assert_eq!(deps[1].bound.as_deref(), Some(">= 2"));
        let out = add_runtime_dependencies(SPEC, &reqs, "x");
        assert!(out.contains("Requires:       cuda-cublas\n"), "{out}");
        assert!(out.contains("BuildRequires:  cuda-cublas\n"), "{out}");
        assert!(out.contains("Requires:       python-numpy >= 2\n"), "{out}");
        assert!(!out.contains("python-nvidia"), "{out}");
        // Already there: left alone.
        assert_eq!(add_runtime_dependencies(&out, &reqs, "x"), out);
    }

    #[test]
    fn the_review_agents_runtime_requires_of_a_cuda_wheel_is_rewritten() {
        let spec = "Requires:       %{python_module nvidia-cublas >= 13.1}\n\
                    Requires:       python-nvidia-cuda-runtime >= 13.0\n\
                    Requires:       python-numpy >= 2\n\
                    BuildRequires:  %{python_module nvidia-cublas}\n\
                    Requires:       python-nvidia-cudnn-cu13\n";
        {
            let _off = TestMode::set(false);
            let out = normalize_runtime_python_module(spec);
            assert!(out.contains("Requires:       python-nvidia-cublas >= 13.1\n"), "{out}");
        }
        let _on = TestMode::set(true);
        let out = normalize_runtime_python_module(spec);
        assert_eq!(
            out,
            "Requires:       cuda-cublas\n\
             Requires:       cuda-cudart\n\
             Requires:       python-numpy >= 2\n\
             BuildRequires:  %{python_module nvidia-cublas}\n\
             Requires:       cuda-cudnn\n"
        );
        assert_eq!(normalize_runtime_python_module(&out), out);
    }
}

#[cfg(test)]
mod wheel_generator_tests {
    use super::*;
    use ap_ecosystem::sdist::WheelLayout;

    fn release(license: License) -> Release {
        Release {
            name: "pkg".to_string(),
            version: "1.0".to_string(),
            summary: Some("A package".to_string()),
            homepage: Some("https://example.org".to_string()),
            license,
            requires_python: None,
            requires_dist: Vec::new(),
            yanked: false,
            sdist: ap_ecosystem::pypi::Sdist {
                filename: String::new(),
                url: String::new(),
                sha256: String::new(),
                size: 0,
            },
        }
    }

    fn skeleton(layout: &WheelLayout, license: License) -> String {
        wheel_binary_skeleton(
            "pkg",
            "1.0",
            "pkg-1.0-py3-none-any.whl",
            &release(license),
            Some(true),
            Some(layout),
        )
    }

    #[test]
    fn the_skeleton_never_mentions_conventions_or_a_missing_sdist() {
        let spec = skeleton(&WheelLayout::default(), License::Spdx("MIT".into()));
        assert!(!spec.contains("convention"), "{spec}");
        assert!(!spec.contains("no sdist"), "{spec}");
        assert!(!spec.contains("Confirm the arch"), "{spec}");
        let desc = spec.split("%description\n").nth(1).unwrap();
        let desc = desc.split("%prep").next().unwrap();
        assert!(!desc.contains('#') && !desc.contains("SOURCE0"), "{desc}");
    }

    #[test]
    fn the_no_sdist_note_goes_under_prep_once() {
        let spec = skeleton(&WheelLayout::default(), License::Spdx("MIT".into()));
        let noted = with_no_sdist_note(&spec);
        let (before, after) = noted.split_once("%prep\n").unwrap();
        assert!(!before.contains("no sdist"));
        assert!(after.starts_with("# this release has no sdist and no source repository"));
        assert_eq!(noted.matches("no sdist").count(), 1);
        assert!(!noted.contains("convention"));
    }

    #[test]
    fn header_cleanups_share_one_comment() {
        let layout = WheelLayout {
            importable: Some("pkg".into()),
            files: vec![
                "pkg/__init__.py".into(),
                "pkg/inc/a.h".into(),
                "pkg/other/b.h".into(),
                "pkg/other/b.py".into(),
            ],
            header_dirs: vec!["pkg/inc".into(), "pkg/other".into()],
            dist_info: Some("pkg-1.0.dist-info".into()),
            ..Default::default()
        };
        let spec = skeleton(&layout, License::Spdx("MIT".into()));
        assert_eq!(spec.matches("bundled headers are not needed").count(), 1, "{spec}");
        assert!(spec.contains("rm -rf"), "{spec}");
        assert!(spec.contains("-name '*.h'"), "{spec}");
    }

    #[test]
    fn the_description_comes_from_the_metadata_paragraph() {
        let layout = WheelLayout {
            description: Some(
                "Pkg does 100% of the work for you and it is a very long sentence that \
                 has to be wrapped over more than one line to stay readable."
                    .into(),
            ),
            ..Default::default()
        };
        let spec = skeleton(&layout, License::Spdx("MIT".into()));
        let desc = spec.split("%description\n").nth(1).unwrap();
        let desc = desc.split("\n\n").next().unwrap();
        assert!(desc.contains("100%% of the work"), "{desc}");
        assert!(desc.lines().count() >= 2 && desc.lines().all(|l| l.len() <= 80), "{desc}");
        assert!(description_problems(&spec).is_empty(), "{spec}");
        // Without a paragraph the summary stays.
        let plain = skeleton(&WheelLayout::default(), License::Spdx("MIT".into()));
        assert!(plain.contains("%description\nA package\n"), "{plain}");
    }

    #[test]
    fn the_licence_is_mapped_and_never_pasted_text() {
        let layout = WheelLayout {
            license_expression: Some("LicenseRef-NVIDIA-Proprietary".into()),
            ..Default::default()
        };
        let spec = skeleton(&layout, License::Unknown);
        assert!(spec.contains("License:        SUSE-NonFree\n"), "{spec}");
        // PyPI's `license` field holding the whole text of the licence.
        let pasted = License::FreeText("Permission is hereby granted,\nfree of charge...".into());
        let layout = WheelLayout {
            license_expression: Some("MIT".into()),
            ..Default::default()
        };
        let spec = skeleton(&layout, pasted.clone());
        assert!(spec.contains("License:        MIT\n"), "{spec}");
        let spec = skeleton(&WheelLayout::default(), pasted);
        assert!(spec.contains("License:        FIXME\n"), "{spec}");
        let spec = skeleton(&WheelLayout::default(), License::FreeText("Apache 2.0".into()));
        assert!(spec.contains("License:        Apache-2.0\n"), "{spec}");
    }

    #[test]
    fn several_licence_files_are_one_licence_entry() {
        let layout = WheelLayout {
            importable: Some("pkg".into()),
            files: vec![
                "pkg/__init__.py".into(),
                "pkg-1.0.dist-info/METADATA".into(),
                "pkg-1.0.dist-info/licenses/LICENSE".into(),
                "pkg-1.0.dist-info/licenses/NOTICE".into(),
            ],
            dist_info: Some("pkg-1.0.dist-info".into()),
            ..Default::default()
        };
        let spec = skeleton(&layout, License::Spdx("MIT".into()));
        assert!(spec.contains("%license %{python_sitelib}/pkg-1.0.dist-info/licenses\n"), "{spec}");
        assert!(spec.contains("%exclude %{python_sitelib}/pkg-1.0.dist-info/licenses\n"), "{spec}");
        assert_eq!(spec.matches("%license").count(), 1, "{spec}");
        // One file keeps its own entry.
        let one = WheelLayout {
            files: layout.files[..3].to_vec(),
            ..layout
        };
        let spec = skeleton(&one, License::Spdx("MIT".into()));
        assert!(spec.contains("%license %{python_sitelib}/pkg-1.0.dist-info/licenses/LICENSE\n"), "{spec}");
    }

    #[test]
    fn bundled_vendor_binaries_add_their_licence_terms() {
        let layout = |files: &[&str]| WheelLayout {
            files: files.iter().map(|f| f.to_string()).collect(),
            ..Default::default()
        };
        let triton = layout(&["triton/backends/nvidia/bin/ptxas", "triton/__init__.py"]);
        let spec = skeleton(&triton, License::Spdx("MIT".into()));
        assert!(spec.contains("License:        MIT AND SUSE-NonFree\n"), "{spec}");
        let torch = layout(&["torch/lib/libgomp-a34b3233.so.1", "torch/lib/libcupti.so.13"]);
        let spec = skeleton(&torch, License::Spdx("BSD-3-Clause OR MIT".into()));
        assert!(
            spec.contains(
                "License:        (BSD-3-Clause OR MIT) AND SUSE-NonFree AND GPL-3.0-or-later WITH GCC-exception-3.1\n"
            ),
            "{spec}"
        );
        // Already non-free: not repeated. Unknown licence: left for a person.
        let spec = skeleton(&triton, License::Spdx("SUSE-NonFree".into()));
        assert!(spec.contains("License:        SUSE-NonFree\n"), "{spec}");
        let spec = skeleton(&triton, License::Unknown);
        assert!(spec.contains("License:        FIXME\n"), "{spec}");
        // A dist-info file named like a vendor binary does not count.
        let plain = layout(&["pkg-1.0.dist-info/ptxas"]);
        assert!(wheel_vendor_license_terms(&plain).is_empty());
    }

    #[test]
    fn a_required_extra_adds_the_packages_it_needs() {
        let deps = runtime_dependencies(
            &[
                "psycopg[binary]>=3.2".to_string(),
                "httpx[brotli,socks]".to_string(),
                "foo[unknown]".to_string(),
                "ext; extra == 'x'".to_string(),
                "uvloop; sys_platform != \"win32\"".to_string(),
                "pywin; sys_platform == \"win32\"".to_string(),
            ],
            "own",
        );
        let names: Vec<&str> = deps.iter().map(|d| d.name.as_str()).collect();
        assert_eq!(
            names,
            ["psycopg", "psycopg-binary", "httpx", "brotli", "socksio", "foo", "uvloop"],
            "{names:?}"
        );
        assert_eq!(deps[0].bound.as_deref(), Some(">= 3.2"));
        let spec = add_runtime_dependencies(
            "Name: x\n%python_subpackages\n",
            &["psycopg[binary]".to_string()],
            "own",
        );
        assert!(spec.contains("Requires:       python-psycopg-binary\n"), "{spec}");
    }

    #[test]
    fn the_rpmlintrc_path_filters_start_with_a_wildcard_and_it_is_never_empty() {
        let layout = WheelLayout {
            first_shared_lib: Some("p/lib.so".into()),
            origin_runpath: true,
            empty_files: vec!["p/custom.css".into()],
            ..Default::default()
        };
        let (_, text) = wheel_rpmlintrc("p", "p-1-cp313-cp313-manylinux_2_28_x86_64.whl", Some(&layout)).unwrap();
        for line in text.lines().filter(|l| l.contains("zero-length") || l.contains("rpath")) {
            assert!(line.starts_with("addFilter(r\".*") || line.starts_with("addFilter(\".*"), "{line}");
        }
        // Nothing to filter: no file at all, never an empty one.
        let none = WheelLayout::default();
        assert!(wheel_rpmlintrc("p", "p-1-py3-none-any.whl", Some(&none)).is_none());
        assert!(wheel_rpmlintrc("p", "p-1-py3-none-any.whl", None).is_none());
    }

    #[test]
    fn pip_ignores_requires_python_when_a_flavor_is_above_the_cap() {
        let flavors: Vec<String> = ["python311", "python313", "python314t"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert!(requires_python_caps_below(">=3.9,<3.13", &flavors));
        assert!(requires_python_caps_below("<=3.12", &flavors));
        assert!(requires_python_caps_below("==3.11.*", &flavors));
        assert!(!requires_python_caps_below(">=3.9", &flavors));
        assert!(!requires_python_caps_below(">=3.9,<4", &flavors));
        assert!(!requires_python_caps_below("<3.15", &flavors));
        assert!(!requires_python_caps_below("<3.12", &flavors[..1]));
        let spec = skeleton(&WheelLayout::default(), License::Spdx("MIT".into()));
        let out = with_pip_ignore_requires_python(&spec, Some("<3.12"), &flavors);
        assert_eq!(out.matches("export PIP_IGNORE_REQUIRES_PYTHON=1").count(), 2, "{out}");
        assert!(out.contains("%build\n# the wheel caps Requires-Python"), "{out}");
        assert!(out.contains("%install\n# the wheel caps"), "{out}");
        // Idempotent, and absent without a cap.
        assert_eq!(with_pip_ignore_requires_python(&out, Some("<3.12"), &flavors), out);
        assert_eq!(with_pip_ignore_requires_python(&spec, Some(">=3.9"), &flavors), spec);
        assert_eq!(with_pip_ignore_requires_python(&spec, None, &flavors), spec);
    }

    #[test]
    fn the_skeleton_keeps_requires_python_base() {
        // `%python_subpackages` does not add it: rpmlint's no-dependency-on python-base.
        let spec = skeleton(&WheelLayout::default(), License::Spdx("MIT".into()));
        assert!(spec.contains("\nRequires:       python-base\n"), "{spec}");
    }
}
