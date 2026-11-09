//! The spelling a BuildRequires takes for one `[build-system] requires` entry.
//!
//! The entry is written as `%{python_module <name> <bound>}`, which OBS resolves
//! to `pythonNNN-<name>`. A PEP 508 extra (`setuptools-scm[toml]`) is no
//! capability of that package: OBS answers "nothing provides
//! python313-setuptools_scm[toml]", so the extra is dropped here and the
//! version bound is kept. The extras a RUNTIME Requires needs are a separate
//! matter and are not touched by this.

/// The entry as a BuildRequires spells it: its extras removed, the version
/// bound (if any) kept. `setuptools-scm[toml] >= 6.2` is
/// `setuptools-scm >= 6.2`; a name without extras is returned as it is.
pub fn build_requires_spelling(req: &str) -> String {
    let req = req.trim();
    let name_end = req
        .find(|c: char| !(c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-')))
        .unwrap_or(req.len());
    let (name, tail) = req.split_at(name_end);
    let Some(after_extras) = tail
        .trim_start()
        .strip_prefix('[')
        .and_then(|inner| inner.split_once(']'))
        .map(|(_, after)| after.trim())
    else {
        return req.to_string();
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
            ("setuptools-scm[toml]", "setuptools-scm"),
            ("setuptools_scm[toml]>=3.4.1", "setuptools_scm >=3.4.1"),
            (
                "setuptools-scm[toml] >= 6.2, < 9",
                "setuptools-scm >= 6.2, < 9",
            ),
            ("pkg[a,b]", "pkg"),
            ("setuptools >= 42", "setuptools >= 42"),
            ("numpy", "numpy"),
        ] {
            assert_eq!(build_requires_spelling(req), want, "{req}");
        }
    }
}
