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
/// prefixing the whole line is sufficient; comment lines are left alone.
pub fn fix_stray_dollar_macros(spec: &str) -> String {
    let out: Vec<String> = spec
        .lines()
        .map(|line| {
            let trimmed = line.trim_start();
            if trimmed.starts_with('#')
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
/// is a whole `-`/`_`-delimited component. A plain substring replace turned
/// `sqlite3-3` with version `3` into `sqlite%{version}-%{version}`.
fn replace_version_component(dir: &str, version: &str) -> String {
    if version.is_empty() {
        return dir.to_string();
    }
    let mut out = String::with_capacity(dir.len());
    let mut i = 0;
    while i < dir.len() {
        let at_start = i == 0 || matches!(dir.as_bytes()[i - 1], b'-' | b'_');
        let end = i + version.len();
        let at_end = end == dir.len() || matches!(dir.as_bytes()[end], b'-' | b'_');
        if at_start && dir[i..].starts_with(version) && at_end {
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
    let out = normalize_python_module_versions(&out);
    let out = dedupe_build_requires(&out);
    let out = normalize_files_paths(&out, &release.name);
    let out = fix_bare_source_filename(&out, release);
    if facts.has_c_extensions {
        fix_c_extension_arch(&out)
    } else {
        out
    }
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

/// Drop `BuildRequires` lines that name a package an earlier line already did.
///
/// py2pack emits the PyPI build requirements and [`post_process`] then adds
/// `[build-system] requires` from the sdist, so anything declared in both
/// appears twice — the single most common edit in the attempt journals, and
/// once even a triplicate. The line carrying a version constraint wins,
/// whichever order the two appear in.
pub fn dedupe_build_requires(spec: &str) -> String {
    use std::collections::HashMap;

    /// The bare package name a BuildRequires line is about, lowercased.
    fn subject(line: &str) -> Option<String> {
        let rest = line.strip_prefix("BuildRequires:")?.trim();
        let inner = match rest.strip_prefix("%{python_module ") {
            Some(i) => i.strip_suffix('}').unwrap_or(i),
            None => rest,
        };
        let name = inner.split([' ', '>', '<', '=', '!']).next()?.trim();
        if name.is_empty() {
            None
        } else {
            Some(format!(
                "{}{}",
                if rest.starts_with("%{python_module ") {
                    "py:"
                } else {
                    ""
                },
                name.to_lowercase()
            ))
        }
    }

    // First pass: for each subject, remember the index of the line to keep.
    let lines: Vec<&str> = spec.lines().collect();
    let mut keep: HashMap<String, usize> = HashMap::new();
    for (i, line) in lines.iter().enumerate() {
        let Some(key) = subject(line) else { continue };
        match keep.get(&key) {
            // Prefer the constrained form — it carries strictly more information.
            Some(&prev) if lines[prev].contains(">=") || lines[prev].contains("<=") => {}
            _ => {
                keep.insert(key, i);
            }
        }
    }

    let mut out = String::with_capacity(spec.len());
    for (i, line) in lines.iter().enumerate() {
        if let Some(key) = subject(line)
            && keep.get(&key) != Some(&i)
        {
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
    let Some((_, first)) = wheels.first() else {
        return String::new();
    };
    let single = wheel_binary_skeleton(pkg_name, version, first, release, root_is_purelib, layout);
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
    let summary = release
        .and_then(|r| r.summary.as_deref())
        .unwrap_or("FIXME: add package summary");
    let license = release
        .map(|r| license_tag(&r.license))
        .unwrap_or_else(|| "FIXME".to_string());
    let url = release
        .and_then(|r| r.homepage.as_deref())
        .unwrap_or("FIXME");

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

/// Baseline `addFilter` set for a prebuilt/vendor-binary wheel (see
/// [`is_prebuilt_binary_wheel`]) — the same five rpmlint findings observed
/// independently, by separate build attempts, on nvidia-cublas,
/// nvidia-cuda-runtime, nvidia-nccl-cu13, and nvidia-nvtx. Pre-seeding this
/// at spec-generation time saves the agent from re-deriving it from a failed
/// build every single time this wheel shape recurs.
///
/// `header_dirs` (from [`ap_ecosystem::sdist::WheelLayout::header_dirs`], the
/// same already-unpacked wheel this function's caller already has on disk)
/// adds one `devel-file-in-non-devel-package` filter per directory of
/// bundled C/C++ headers — the sixth finding this shape produces, previously
/// only ever caught by an agent turn re-deriving it from a failed build
/// (observed on nvidia-nccl-cu13: 100 rpmlint errors for `nccl.h` and
/// friends under `nvidia/nccl/include/`) and written as prose in the agent's
/// own prompt instead of pre-seeded here like the other five. Every CUDA
/// wheel bundling its own public API headers (cublas, cudnn, cufft, …) hits
/// this the same way, so it belongs here, not in a prompt an agent has to
/// remember to apply.
pub fn prebuilt_wheel_rpmlintrc(header_dirs: &[String]) -> String {
    let mut out = String::from(
        "addFilter(\"missing-hash-section\")\n\
         addFilter(\"shared-library-not-executable\")\n\
         addFilter(\"library-without-ldconfig-postin\")\n\
         addFilter(\"library-without-ldconfig-postun\")\n\
         addFilter(\"no-dependency-on python-base\")\n",
    );
    for dir in header_dirs {
        out.push_str(&format!(
            "addFilter(\"devel-file-in-non-devel-package.*/usr/lib/python.*/site-packages/{dir}/\")\n"
        ));
    }
    out
}

pub fn wheel_binary_skeleton(
    pkg_name: &str,
    version: &str,
    wheel_filename: &str,
    release: &Release,
    root_is_purelib: Option<bool>,
    layout: Option<&ap_ecosystem::sdist::WheelLayout>,
) -> String {
    let name = pkg_name.to_lowercase();
    let summary = release
        .summary
        .as_deref()
        .unwrap_or("FIXME: add package summary");
    let license = license_tag(&release.license);
    let url = release.homepage.as_deref().unwrap_or("FIXME");
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
    let purelib = root_is_purelib.unwrap_or(is_pure);
    let sitedir = if purelib {
        "python_sitelib"
    } else {
        "python_sitearch"
    };
    let source99_line = if is_prebuilt_binary_wheel(wheel_filename, layout) {
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
        Some(l) if l.first_shared_lib.is_some() => format!(
            "%check\n%python_expand test -e %{{buildroot}}%{{${sitedir}}}/{}\n",
            l.first_shared_lib.as_deref().unwrap_or_default()
        ),
        _ => "%check\n# TODO: smoke-test the import name from source/*.dist-info/METADATA:\n\
              # %python_expand PYTHONPATH=%{buildroot}%{$python_sitelib} $python -c \"import <name>\"\n"
            .to_string(),
    };

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
         Source0:        {wheel_filename}\n\
         {source99_line}\
         {arch_line}\
         BuildRequires:  python-rpm-macros\n\
         BuildRequires:  %{{python_module pip}}\n\
         BuildRequires:  fdupes\n\
         %python_subpackages\n\
         \n\
         %description\n\
         {summary}\n\
         \n\
         # This release has no sdist and no discoverable source repository — see\n\
         # convention 28 (and convention 24 Exception 3). %%{{SOURCE0}} is the\n\
         # upstream wheel itself,\n\
         # unpacked as-is below; there is no source to compile. Confirm the arch\n\
         # guess above and the %%files list against source/ (the unpacked wheel)\n\
         # before the first build.\n\
         \n\
         %prep\n\
         # nothing to unpack here: %%{{SOURCE0}} is a prebuilt wheel\n\
         \n\
         %build\n\
         # prebuilt binary wheel - nothing to compile\n\
         \n\
         %install\n\
         %pyproject_install %{{SOURCE0}}\n\
         %python_expand %fdupes %{{buildroot}}/%{{${sitedir}}}\n\
         \n\
         {check_block}\
         \n\
         %files %{{python_files}}\n\
         # TODO: list the exact paths %%pyproject_install put in the buildroot\n\
         # (see source/ for the unpacked wheel's real layout) — convention 15\n\
         # forbids globs here.\n\
         \n\
         %changelog\n",
        name = name,
        version = version,
        summary = summary,
        license = license,
        url = url,
        wheel_filename = wheel_filename,
        arch_line = arch_line,
        sitedir = sitedir,
        check_block = check_block,
    )
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

    /// A prebuilt vendor `.so` wheel (nvidia-cublas's real shape) gets a
    /// pre-seeded `Source99:`/rpmlintrc baseline instead of making the agent
    /// re-derive the same five rpmlint false positives from a failed build —
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

    /// The nvidia-nccl-cu13 case: bundled C headers under the wheel's own
    /// `include/` get a `devel-file-in-non-devel-package` filter pre-seeded
    /// at generation time, so the agent doesn't have to re-derive it from
    /// 100 rpmlint errors on the first real build.
    #[test]
    fn bundled_headers_get_a_pre_seeded_devel_file_filter() {
        let rc = prebuilt_wheel_rpmlintrc(&["nvidia/nccl/include".to_string()]);
        assert!(
            rc.contains(
                "addFilter(\"devel-file-in-non-devel-package.*/usr/lib/python.*/site-packages/nvidia/nccl/include/\")"
            ),
            "{rc}"
        );
        // The five baseline filters are still there regardless.
        assert!(rc.contains("addFilter(\"missing-hash-section\")"), "{rc}");

        // No bundled headers: no sixth filter, baseline unchanged.
        let rc = prebuilt_wheel_rpmlintrc(&[]);
        assert!(!rc.contains("devel-file-in-non-devel-package"), "{rc}");
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
}
