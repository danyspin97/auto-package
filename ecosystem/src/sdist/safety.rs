/// Whether `name` is a plain ASCII Python identifier: what a wheel's import
/// name is written as in `python -c "import <name>"`, in a spec's `%check`.
pub fn is_python_identifier(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 255
        && !name.starts_with(|ch: char| ch.is_ascii_digit())
        && name
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
}

/// Whether a wheel member path (or a top-level name taken from one) may be
/// written unescaped into a spec line or a shell command: `/`-separated
/// components, none empty, `.` or `..`, and no whitespace, control character
/// or shell, spec or RPM metacharacter. Stricter than the `%files` listing,
/// which only needs to avoid globs: the paths here are expanded by `sh`.
pub fn is_shell_safe_member(path: &str) -> bool {
    !path.is_empty()
        && path.len() <= 4096
        && !path.chars().any(|c| {
            c.is_whitespace()
                || c.is_control()
                || matches!(
                    c,
                    '%' | '*'
                        | '?'
                        | '['
                        | ']'
                        | '{'
                        | '}'
                        | '$'
                        | '"'
                        | '\''
                        | '\\'
                        | '`'
                        | ';'
                        | '&'
                        | '|'
                        | '('
                        | ')'
                        | '<'
                        | '>'
                        | '!'
                        | '#'
                )
        })
        && path
            .split('/')
            .all(|p| !p.is_empty() && p != "." && p != "..")
}

/// Whether a `/`-separated directory of the archive (a manifest's directory
/// below the top-level one) may be written into a spec. Each component must
/// pass [`is_safe_top_level_dir`]; the archive root (empty) is allowed.
pub fn is_safe_manifest_dir(dir: &str) -> bool {
    dir.is_empty() || dir.split('/').all(is_safe_top_level_dir)
}

/// Whether an archive's top-level directory name can be written verbatim into
/// a spec (`%autosetup -n <name>`) and a shell command. Letters and digits in
/// any script are allowed (e.g. `Pyöpt-1.0`), plus `. _ - + ~`: no whitespace,
/// control character, newline, `%` (RPM macro), `/` or other shell or spec
/// metacharacters. A leading `-` would be read as an option and a leading `~`
/// tilde-expands (`~root`) in the generated `cd`, so neither may start a name.
/// Shared with `ap-build` so both sides accept the same names.
pub fn is_safe_top_level_dir(name: &str) -> bool {
    name.len() <= 255
        && !matches!(name, "" | "." | "..")
        && !name.starts_with(['-', '~'])
        && name
            .chars()
            .all(|c| c.is_alphanumeric() || matches!(c, '.' | '_' | '-' | '+' | '~'))
}

/// Whether an upstream-declared project name is safe to build a package, a
/// directory and a command line around: the characters PyPI and crates.io
/// allow, so no path separator, whitespace, shell or XML metacharacter. The
/// manifest is untrusted, and the name ends up in file names and spec text.
pub(super) fn is_plain_project_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 100
        && name.starts_with(|c: char| c.is_ascii_alphanumeric())
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | '+'))
}
