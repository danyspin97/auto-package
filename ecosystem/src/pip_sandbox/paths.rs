use super::*;

pub(super) fn is_under_any(path: &Path, dirs: &[&str]) -> bool {
    dirs.iter().any(|d| path.starts_with(d))
}

/// Why `path`, a directory the sandbox would get, is too broad or too
/// sensitive to bind, if it is. `home` is the real home directory.
pub(super) fn sensitive_reason(path: &Path, home: &Path) -> Option<&'static str> {
    let real_home = home.canonicalize().unwrap_or_else(|_| home.to_path_buf());
    for h in [home, real_home.as_path()] {
        if h.starts_with(path) {
            return Some("it contains the home directory");
        }
        let Ok(rel) = path.strip_prefix(h) else {
            continue;
        };
        let mut parts = rel.components();
        let Some(first) = parts.next() else { continue };
        let first = first.as_os_str();
        if CREDENTIAL_HOME_DIRS.iter().any(|d| first == *d) {
            return Some("it is inside a credential directory");
        }
        if SENSITIVE_HOME_DIRS.iter().any(|d| first == *d) && parts.next().is_none() {
            return Some("it is a credential or configuration directory");
        }
    }
    None
}

/// Whether `path` is at or under one of `SENSITIVE_HOME_DIRS` in the home
/// directory (`~/.local`, `~/.config`, `~/.cache`, at any depth).
pub(super) fn under_sensitive_home_dir(path: &Path, home: &Path) -> bool {
    let real_home = home.canonicalize().unwrap_or_else(|_| home.to_path_buf());
    [home, real_home.as_path()].into_iter().any(|h| {
        path.strip_prefix(h)
            .ok()
            .and_then(|rel| rel.components().next())
            .is_some_and(|first| SENSITIVE_HOME_DIRS.iter().any(|d| first.as_os_str() == *d))
    })
}

/// Whether `prefix` looks like a Python installation tree: an interpreter in
/// `bin/` and a standard library under `lib/python3*`, as a standalone build
/// (such as one managed by uv) has. An app's config directory with a `bin/`
/// directory but no Python library does not.
pub(super) fn looks_like_interpreter_install(prefix: &Path) -> bool {
    let has_interpreter =
        prefix.join("bin/python3").is_file() || prefix.join("bin/python").is_file();
    let has_stdlib = std::fs::read_dir(prefix.join("lib"))
        .into_iter()
        .flatten()
        .flatten()
        .any(|e| e.file_name().as_bytes().starts_with(b"python3") && e.path().is_dir());
    has_interpreter && has_stdlib
}

/// Whether `name`, an entry at the top level of a prefix, looks like a
/// credential: a netrc, a PyPI or git credential file, an SSH key, a PEM file,
/// a `credentials` file, or one of the credential directories.
pub(super) fn is_credential_entry(name: &OsStr) -> bool {
    let Some(name) = name.to_str() else {
        return false;
    };
    CREDENTIAL_ENTRY_NAMES.contains(&name)
        || CREDENTIAL_HOME_DIRS.contains(&name)
        || name.starts_with(".env.")
        || name.starts_with("id_")
        || name.starts_with("credentials")
        || name.ends_with(".pem")
}

/// Whether the top level of `prefix` holds an entry that `is_credential_entry`
/// accepts. Only the top level is looked at.
pub(super) fn has_credential_entry(prefix: &Path) -> bool {
    std::fs::read_dir(prefix)
        .into_iter()
        .flatten()
        .flatten()
        .any(|e| is_credential_entry(&e.file_name()))
}

pub(super) fn check_prefix(program: &str, prefix: &Path, home: &Path) -> Result<(), SandboxError> {
    let reason = sensitive_reason(prefix, home)
        .or_else(|| {
            // Under the configuration or cache directories only a virtualenv
            // (which has a `pyvenv.cfg`) or an interpreter installation is a
            // prefix of its own; anything else there may be a tool's data, config
            // or keyring.
            (under_sensitive_home_dir(prefix, home)
                && !prefix.join("pyvenv.cfg").is_file()
                && !looks_like_interpreter_install(prefix))
            .then_some("it is inside a configuration or cache directory and is not a virtualenv or Python installation")
        })
        .or_else(|| {
            has_credential_entry(prefix)
                .then_some("it holds a credential file or directory at its top level")
        });
    match reason {
        None => Ok(()),
        Some(why) => Err(SandboxError::UnsafePrefix {
            program: program.to_string(),
            prefix: prefix.to_path_buf(),
            why,
        }),
    }
}

/// Why the file `path` must not be bound into the sandbox, checked as named
/// and as resolved through symlinks.
pub(super) fn sensitive_file(path: &Path, home: &Path) -> Option<&'static str> {
    sensitive_reason(path, home).or_else(|| {
        let real = path.canonicalize().ok()?;
        sensitive_reason(&real, home)
    })
}

/// `path` with its `.` and `..` components resolved without touching the
/// filesystem: `/usr/../home/u` is `/home/u`. `None` when `path` is not
/// absolute or a `..` would climb above `/`.
pub(super) fn normalize_lexically(path: &Path) -> Option<PathBuf> {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::RootDir => out.push("/"),
            Component::CurDir => {}
            Component::ParentDir => {
                if !out.pop() {
                    return None;
                }
            }
            Component::Normal(name) => out.push(name),
            Component::Prefix(_) => return None,
        }
    }
    out.is_absolute().then_some(out)
}
