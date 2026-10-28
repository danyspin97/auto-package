use super::*;

/// Locate `program` and bind its interpreter prefix. Returns the executable
/// to run and the `bin` directories to put on the sandbox `PATH`.
pub(super) fn bind_interpreter(
    a: &mut Args,
    program: &str,
    host: &Host,
) -> Result<(PathBuf, Vec<PathBuf>), SandboxError> {
    let exe = which(program, &host.path)
        .ok_or_else(|| SandboxError::ProgramNotFound(program.to_string()))?;
    // The program as found, what it resolves to, and the interpreter named by
    // its shebang (as found and resolved): a venv's `python` is a symlink
    // into the system, and both ends matter.
    let mut files = vec![exe.clone()];
    if let Ok(real) = exe.canonicalize() {
        files.push(real);
    }
    if let Some(interp) = shebang_interpreter(&exe, &host.path) {
        if let Ok(real) = interp.canonicalize() {
            files.push(real);
        }
        files.push(interp);
    }

    let mut prefixes: BTreeSet<PathBuf> = BTreeSet::new();
    for file in &files {
        let Some(prefix) = interpreter_prefix(file) else {
            continue;
        };
        check_prefix(program, &prefix, &host.home)?;
        prefixes.insert(prefix);
    }
    // Every prefix is outside the system directories (see `interpreter_prefix`),
    // so only its interpreter parts are bound, never the prefix itself: a
    // prefix such as `~/tools` holds whatever else the user keeps there.
    let mut bin_dirs = Vec::new();
    for prefix in &prefixes {
        for mount in prefix_mounts(prefix) {
            a.ro_bind(&mount, &mount);
        }
        let bin = prefix.join("bin");
        if bin.is_dir() {
            bin_dirs.push(bin);
        }
    }
    Ok((exe, bin_dirs))
}

/// The parts of a prefix outside the system directories that the sandbox gets,
/// each to be bound read-only at its own path: the interpreter's directories
/// (`bin`, `lib`, `lib64`, `include`, `share/python*`) and `pyvenv.cfg`, as
/// far as they exist. Nothing else at the top level of the prefix is bound.
pub(super) fn prefix_mounts(prefix: &Path) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = PREFIX_DIRS
        .iter()
        .map(|d| prefix.join(d))
        .filter(|p| p.is_dir())
        .collect();
    if let Ok(entries) = std::fs::read_dir(prefix.join("share")) {
        for entry in entries.flatten() {
            let name = entry.file_name();
            if name.as_bytes().starts_with(b"python") && entry.path().is_dir() {
                out.push(entry.path());
            }
        }
    }
    let cfg = prefix.join("pyvenv.cfg");
    if cfg.is_file() {
        out.push(cfg);
    }
    out.sort();
    out
}

/// Find `program` the way a shell would: as given when it has a slash,
/// otherwise in each absolute `path` entry.
pub(super) fn which(program: &str, path: &OsStr) -> Option<PathBuf> {
    if program.contains('/') {
        let p = PathBuf::from(program);
        return (p.is_absolute() && p.is_file()).then_some(p);
    }
    std::env::split_paths(path)
        .filter(|d| d.is_absolute())
        .map(|d| d.join(program))
        .find(|p| p.is_file())
}

/// The interpreter named by `file`'s `#!` line, when it is a script.
pub(super) fn shebang_interpreter(file: &Path, path: &OsStr) -> Option<PathBuf> {
    let mut head = [0u8; 256];
    let n = std::fs::File::open(file).ok()?.read(&mut head).ok()?;
    let rest = head[..n].strip_prefix(b"#!")?;
    let line = rest.split(|b| *b == b'\n').next()?;
    let mut words = line
        .split(|b| b.is_ascii_whitespace())
        .filter(|w| !w.is_empty())
        .map(|w| OsStr::from_bytes(w).to_owned());
    let first = PathBuf::from(words.next()?);
    if first.file_name() == Some(OsStr::new("env")) {
        let name = words.find(|w| !w.as_bytes().starts_with(b"-"))?;
        return which(name.to_str()?, path);
    }
    first.is_absolute().then_some(first)
}

/// The directory an installed program belongs to: the parent of its `bin`
/// directory (a venv or install prefix), or its own directory otherwise.
pub(super) fn prefix_of(file: &Path) -> PathBuf {
    let dir = file.parent().unwrap_or(Path::new("/"));
    if dir.file_name() == Some(OsStr::new("bin")) {
        dir.parent().unwrap_or(dir).to_path_buf()
    } else {
        dir.to_path_buf()
    }
}

/// The prefix to bind for `file`, or `None` when nothing needs binding.
///
/// A file under [`SYSTEM_DIRS`] is already in the sandbox through
/// `system_ro_binds`, whatever its prefix would be: `/bin/pip` has the prefix
/// `/`, which must never be bound (it holds the home directory), but `/bin` is
/// bound on its own. Any other file is bound by its prefix, which is checked.
pub(super) fn interpreter_prefix(file: &Path) -> Option<PathBuf> {
    if is_under_any(file, SYSTEM_DIRS) {
        return None;
    }
    Some(prefix_of(file))
}
