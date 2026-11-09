//! Fixes to a generated spec: the Python module names and versions, the dependency
//! names, the C extension arch, the stray macros and the missing alternatives.

use crate::spec::expand::*;
use crate::spec::parse::*;

/// Fix the `%autosetup -n` value in a spec to match the actual tarball directory.
///
/// `actual_top_level` is the top-level directory name from the sdist
/// (e.g. `MarkupSafe-3.0.3`), already extracted by the caller's single-pass
/// [`ap_ecosystem::sdist::inspect`] call. Returns the corrected spec, or `None`
/// if no correction was needed.
pub fn fix_autosetup_dir(spec: &str, actual_top_level: &str) -> Option<String> {
    let actual = actual_top_level;
    // The directory is written verbatim into the `%autosetup` line: a name
    // carrying a newline, an RPM macro or whitespace would inject spec lines.
    if !is_safe_autosetup_dir(actual) {
        return None;
    }
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
pub(super) fn leading_name_re() -> &'static regex::Regex {
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
pub(super) fn recase_python_module_name(inner: &str) -> String {
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

/// Whether `name` is a distribution or RPM package name a rewrite may write
/// into a spec: letters and digits first, then `.`, `_`, `+` and `-`. The
/// names come from the review agent, so anything else (a capability with
/// `()` or `:`, whitespace, a macro, a quote, a shell operator) is refused.
pub(super) fn is_plain_dependency_name(name: &str) -> bool {
    name.len() <= 200
        && name.starts_with(|c: char| c.is_ascii_alphanumeric())
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '+' | '-'))
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
    if from == to || !is_plain_dependency_name(from) || !is_plain_dependency_name(to) {
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
    if from == to || !is_plain_dependency_name(from) || !is_plain_dependency_name(to) {
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
/// bare `chmod %{$python_sitelib}/...` line). A reference is bare when no
/// `%{` macro encloses it, so a reference inside a `%{python_expand ...}`
/// block is left alone however many lines the block spans (the opener and
/// the closing `}` may be on other lines). A bare reference gets its whole
/// line prefixed, which is enough for the single-line `%python_expand
/// <command...>` form. Comment lines are left alone, and so are continuation
/// lines (after a trailing `\`), which already belong to the command their
/// first line started.
pub fn fix_stray_dollar_macros(spec: &str) -> String {
    let mut continued = false;
    let mut depth = 0;
    let out: Vec<String> = spec
        .lines()
        .map(|line| {
            let is_continuation =
                std::mem::replace(&mut continued, line.trim_end().ends_with('\\'));
            let trimmed = line.trim_start();
            let inside_block = depth > 0;
            let bare = if trimmed.starts_with('#') {
                false
            } else {
                let (next, bare) = scan_macro_braces(line, depth);
                depth = next;
                bare
            };
            if inside_block
                || is_continuation
                || trimmed.starts_with('#')
                || trimmed.contains("%python_expand")
                || !bare
            {
                return line.to_string();
            }
            let indent = &line[..line.len() - trimmed.len()];
            format!("{indent}%python_expand {trimmed}")
        })
        .collect();
    out.join("\n") + if spec.ends_with('\n') { "\n" } else { "" }
}

/// The macro brace depth after `line`, starting from `depth`, and whether the
/// line holds a `%{$...}` reference that no enclosing `%{` encloses. `%{`
/// opens a macro, `}` closes one; the `%{` of the reference itself does not
/// count as its enclosure.
fn scan_macro_braces(line: &str, mut depth: usize) -> (usize, bool) {
    let bytes = line.as_bytes();
    let mut bare = false;
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i..].starts_with(b"%{$") {
            bare |= depth == 0;
            depth += 1;
            i += 3;
        } else if bytes[i..].starts_with(b"%{") {
            depth += 1;
            i += 2;
        } else {
            if bytes[i] == b'}' {
                depth = depth.saturating_sub(1);
            }
            i += 1;
        }
    }
    (depth, bare)
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
/// `update-alternatives: command not found` once the scriptlets actually run.
/// Inserted right before `%python_subpackages`,
/// matching where a correctly-written spec of this shape already puts them.
pub fn fix_missing_alternatives_requires(spec: &str) -> String {
    let needs_it = spec.contains("%python_install_alternative")
        || spec.contains("%python_uninstall_alternative");
    if !needs_it {
        return spec.to_string();
    }
    let has_post = requires_alternatives_for(spec, "post");
    let has_postun = requires_alternatives_for(spec, "postun");
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

/// Whether a `Requires(<phase>[,..]):` line names `update-alternatives`.
fn requires_alternatives_for(spec: &str, phase: &str) -> bool {
    spec.lines().any(|line| {
        let Some(rest) = line.trim_start().strip_prefix("Requires(") else {
            return false;
        };
        let Some((phases, value)) = rest.split_once("):") else {
            return false;
        };
        phases.split(',').any(|p| p.trim() == phase)
            && value
                .split(|c: char| c.is_whitespace() || c == ',')
                .any(|w| w == "update-alternatives")
    })
}
