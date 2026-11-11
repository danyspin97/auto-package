//! The rpmspec check in its sandbox: the markers that need the real macros, and
//! the sandbox prefix and spec directory.

use super::*;

/// Macro forms that run a program or read a file while `rpmspec` expands them.
/// The spec may come from upstream or from the agent, so these are only
/// expanded inside the sandbox.
const CODE_MACRO_MARKERS: &[&str] = &["%(", "%{lua:", "%{expand:", "%{load:", "%{getenv:"];

/// Whether `text` has a bare `%lua` form: `%lua` not followed by an identifier
/// character, so `%lua:`, `%lua ` and `%lua` at the end count, and `%luajit`
/// does not.
fn bare_lua_marker(text: &str) -> bool {
    text.match_indices("%lua").any(|(i, _)| {
        text[i + "%lua".len()..]
            .chars()
            .next()
            .is_none_or(|c| !(c.is_alphanumeric() || c == '_'))
    })
}

/// The first code-running macro form in `text`, if any.
pub(super) fn code_macro_marker(text: &str) -> Option<&'static str> {
    // `%%` is a literal percent sign, so `%%(` and `%%{lua:` are text. Each
    // pair goes first, left to right: `%%%(` still leaves a real `%(`.
    let text = text.replace("%%", "");
    CODE_MACRO_MARKERS
        .iter()
        .copied()
        .find(|m| text.contains(m))
        .or_else(|| bare_lua_marker(&text).then_some("%lua"))
}

/// Directives that make `rpmspec` read another file from the host while
/// parsing. Matched at the start of a line, after leading whitespace, and only
/// when the keyword ends there (so `%{includedir}` or `%includes_foo` do not match).
const INCLUDE_MARKERS: &[&str] = &["%include", "%{include"];

/// The first file-including directive at the start of a line in `text`, if any.
pub(super) fn include_marker(text: &str) -> Option<&'static str> {
    text.lines().find_map(|line| {
        let line = line.trim_start();
        INCLUDE_MARKERS.iter().copied().find(|m| {
            line.strip_prefix(m).is_some_and(|rest| {
                rest.is_empty()
                    || rest.starts_with(char::is_whitespace)
                    || (m.starts_with("%{") && rest.starts_with('}'))
            })
        })
    })
}

/// The first form in `text` that would make an unsandboxed `rpmspec` run
/// code or read a host file: a code-running macro or a file include.
pub(super) fn unsandboxed_marker(text: &str) -> Option<&'static str> {
    code_macro_marker(text).or_else(|| include_marker(text))
}

/// Whether `rpmspec` can run inside bubblewrap here. Probed once: `bwrap`
/// installed is not enough, unprivileged user namespaces are often disabled.
pub(super) fn rpmspec_sandbox_works() -> bool {
    static WORKS: OnceLock<bool> = OnceLock::new();
    *WORKS.get_or_init(|| {
        let mut args = sandbox_prefix(None);
        args.extend(["rpmspec".to_string(), "--version".to_string()]);
        proc::capture("bwrap", &args, &anywhere(), Emit::quiet()).is_ok_and(|out| out.success())
    })
}

/// bubblewrap options for one `rpmspec` run, up to the program name. The
/// system is read-only, the home directory is not mounted, the environment is
/// cleared, and there is no network. `spec_dir`, when given, is the only user
/// directory mounted, read-only.
fn sandbox_prefix(spec_dir: Option<&str>) -> Vec<String> {
    let mut args = ap_core::sandbox::system_ro_binds();
    args.extend(ap_core::sandbox::etc_ro_binds());
    args.extend(
        [
            // rpm's own configuration, which the shared list does not carry.
            "--ro-bind-try",
            "/etc/rpm",
            "/etc/rpm",
            "--ro-bind-try",
            "/etc/rpmrc",
            "/etc/rpmrc",
            "--cap-drop",
            "ALL",
            "--proc",
            "/proc",
            "--dev",
            "/dev",
            "--tmpfs",
            "/tmp",
            "--unshare-net",
            "--unshare-pid",
            "--unshare-ipc",
            "--die-with-parent",
            "--new-session",
            "--clearenv",
            "--setenv",
            "PATH",
            "/usr/bin:/bin",
            "--setenv",
            "HOME",
            "/tmp",
            "--setenv",
            "LANG",
            "C.UTF-8",
        ]
        .map(String::from),
    );
    let dir = spec_dir.unwrap_or("/tmp");
    if spec_dir.is_some() {
        args.extend(["--ro-bind", dir, dir].map(String::from));
    }
    args.extend(["--chdir", dir].map(String::from));
    args
}

/// The directory bubblewrap binds for a spec: its absolute parent. A relative
/// spec path is taken from `cwd`, so the parent is never the empty path (which
/// `canonicalize` rejects and bwrap refuses as `--ro-bind "" ""`).
pub(super) fn sandbox_spec_dir(spec_path: &Path, cwd: &Path) -> PathBuf {
    let absolute = if spec_path.is_absolute() {
        spec_path.to_path_buf()
    } else {
        cwd.join(spec_path)
    };
    absolute
        .parent()
        .map_or_else(|| PathBuf::from("/"), Path::to_path_buf)
}

/// Run `rpmspec --parse` on a spec file and check for syntax errors.
///
/// Judges on exit status only (warnings in stderr are not treated as failures).
/// Returns the expanded spec on success (stdout from `rpmspec --parse`).
///
/// **Caveat:** `rpmspec` expands
/// macros using the **host** RPM configuration, not the target build repo's
/// configuration. A spec that `rpmspec --parse`s cleanly on this machine can
/// still fail to parse (or expand to different Requires / BuildRequires) on
/// the OBS build worker if the host's macro set diverges from the target
/// distro's — `%python3_sitelib`, `%python_module`, `%suse_version` and the
/// like are all host-local here. The agent still relies on this as a cheap
/// pre-flight syntax check. A durable fix would require containerized
/// validation against the target repo's rpm-config.
///
/// Expanding a spec can run code (`%(...)`, `%{lua:...}`), so it runs inside
/// bubblewrap when that works. Without a sandbox, a spec containing such a
/// macro is refused rather than expanded on the host.
pub fn rpmspec_check(spec_path: &Path, emit: Emit<'_>) -> Result<String, Error> {
    let spec = spec_path.to_string_lossy().into_owned();
    let out = if rpmspec_sandbox_works() {
        let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/"));
        let dir = sandbox_spec_dir(spec_path, &cwd);
        let dir = fs::canonicalize(&dir).unwrap_or(dir);
        let file = dir.join(spec_path.file_name().unwrap_or_default());
        let mut args = sandbox_prefix(Some(&dir.to_string_lossy()));
        let file = file.to_string_lossy().into_owned();
        args.extend(["rpmspec".to_string(), "--parse".to_string(), file]);
        proc::capture("bwrap", &args, &anywhere(), emit)?
    } else {
        let text = fs::read(spec_path).unwrap_or_default();
        if let Some(marker) = unsandboxed_marker(&String::from_utf8_lossy(&text)) {
            return Err(Error::UnsandboxedMacro {
                spec,
                marker: marker.to_string(),
            });
        }
        proc::capture("rpmspec", &["--parse", &spec], &anywhere(), emit)?
    };

    if !out.success() {
        return Err(Error::RpmspecFailed {
            spec: spec_path.display().to_string(),
            code: out.code,
            stderr: out.stderr,
        });
    }

    Ok(out.stdout)
}
