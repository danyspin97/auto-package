//! The spelling a BuildRequires takes for one `[build-system] requires` entry.
//!
//! The entry is written as `%{python_module <name> <bound>}`, which OBS resolves
//! to `pythonNNN-<name>`. A PEP 508 extra (`setuptools-scm[toml]`) is no
//! capability of that package: OBS answers "nothing provides
//! python313-setuptools_scm[toml]", so the extra is dropped here and the
//! version bound is kept. The extras a RUNTIME Requires needs are a separate
//! matter and are not touched by this.

/// Names the distro publishes with an underscore where PyPI spells a hyphen
/// (`python313-setuptools_scm`; there is no `python313-setuptools-scm`). Checked
/// in openSUSE:Factory for python313 (noarch). Matched by `fuzzy_key`, so the
/// spelling in the spec does not matter.
const UNDERSCORE_DISTRO_NAMES: &[&str] = &[
    "setuptools_scm",
    "typing_extensions",
    "mypy_extensions",
    "prompt_toolkit",
    "async_timeout",
];

/// The distro's spelling of `name` when it is one of [`UNDERSCORE_DISTRO_NAMES`],
/// else `name` itself.
fn distro_spelling(name: &str) -> &str {
    let key = crate::osc::fuzzy_key(name);
    UNDERSCORE_DISTRO_NAMES
        .iter()
        .copied()
        .find(|known| crate::osc::fuzzy_key(known) == key)
        .unwrap_or(name)
}

/// The entry as a BuildRequires spells it: its extras removed, the version
/// bound (if any) kept, and the name in the distro's spelling.
/// `setuptools-scm[toml] >= 6.2` is `setuptools_scm >= 6.2`.
pub fn build_requires_spelling(req: &str) -> String {
    let req = req.trim();
    let name_end = req
        .find(|c: char| !(c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-')))
        .unwrap_or(req.len());
    let (name, tail) = req.split_at(name_end);
    let name = distro_spelling(name);
    let Some(after_extras) = tail
        .trim_start()
        .strip_prefix('[')
        .and_then(|inner| inner.split_once(']'))
        .map(|(_, after)| after.trim())
    else {
        return format!("{name}{tail}");
    };
    if after_extras.is_empty() {
        name.to_string()
    } else {
        format!("{name} {after_extras}")
    }
}

#[cfg(test)]
mod tests {
    use super::build_requires_spelling;

    #[test]
    fn extras_are_dropped_and_the_bound_is_kept() {
        for (req, want) in [
            ("setuptools-scm[toml]", "setuptools_scm"),
            ("setuptools_scm[toml]>=3.4.1", "setuptools_scm >=3.4.1"),
            ("pydantic-core[x] >= 6.2, < 9", "pydantic-core >= 6.2, < 9"),
            ("pkg[a,b]", "pkg"),
            ("setuptools >= 42", "setuptools >= 42"),
            ("numpy", "numpy"),
        ] {
            assert_eq!(build_requires_spelling(req), want, "{req}");
        }
    }

    #[test]
    fn a_name_the_distro_spells_with_an_underscore_is_written_so() {
        for (req, want) in [
            ("setuptools-scm", "setuptools_scm"),
            ("typing-extensions>=4", "typing_extensions>=4"),
            ("Mypy_Extensions", "mypy_extensions"),
            ("pydantic-core", "pydantic-core"),
            ("backports.zstd", "backports.zstd"),
        ] {
            assert_eq!(build_requires_spelling(req), want, "{req}");
        }
    }
}
