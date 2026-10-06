//! Runtime resolution of the `packaging-skills/` Claude Code plugin dir.
//!
//! The 2026-10-06 external review flagged that the previous
//! `concat!(env!("CARGO_MANIFEST_DIR"), "/../packaging-skills")` approach
//! bakes the source checkout's absolute path into the binary, so a built
//! binary carried to another machine or installed under a packaging prefix
//! finds nothing at that path and silently ships without its skills.
//!
//! [`resolve_skills_dir`] takes the three candidate inputs (env override,
//! current exe path, compile-time fallback) and returns the first existing
//! directory, in this priority order:
//!
//! 1. `$AUTO_PACKAGE_SKILLS_DIR`, if set and existing.
//! 2. `<exe-dir>/../share/auto-package/skills` — the installed-prefix layout
//!    (`<prefix>/bin/ap-cli` → `<prefix>/share/auto-package/skills`).
//! 3. `<exe-dir>/../../packaging-skills` — the dev layout from `cargo build`
//!    (`<workspace>/target/<profile>/ap-cli` → `<workspace>/packaging-skills`).
//! 4. The compile-time fallback (historical behavior) so `cargo run` from
//!    inside the source tree keeps working on a developer's machine.
//!
//! On a complete miss the function returns `None` so the caller can emit a
//! `Warning` and continue without the plugin — the agent still runs, just
//! without on-demand skill guidance.

use std::path::{Path, PathBuf};

/// Resolve the packaging-skills plugin directory using the priority order
/// described in this module's docs. Pure — all filesystem probing goes
/// through the paths derived from the three arguments, so tests can drive it
/// against a temp directory without touching the real environment.
pub fn resolve_skills_dir(
    env_override: Option<&str>,
    current_exe: Option<&Path>,
    compile_time_fallback: &Path,
) -> Option<PathBuf> {
    if let Some(p) = env_override.map(PathBuf::from)
        && p.is_dir()
    {
        return Some(p);
    }

    if let Some(exe_dir) = current_exe.and_then(Path::parent) {
        // Installed layout: <prefix>/bin/<exe> → <prefix>/share/auto-package/skills.
        if let Some(prefix) = exe_dir.parent() {
            let installed = prefix.join("share").join("auto-package").join("skills");
            if installed.is_dir() {
                return Some(installed);
            }
        }
        // Dev layout: target/<profile>/<exe> → packaging-skills at workspace root.
        if let Some(workspace) = exe_dir.parent().and_then(Path::parent) {
            let dev = workspace.join("packaging-skills");
            if dev.is_dir() {
                return Some(dev);
            }
        }
    }

    if compile_time_fallback.is_dir() {
        return Some(compile_time_fallback.to_path_buf());
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    /// A temp dir that cleans up on drop. Deliberately in-crate so these
    /// tests don't need a dev-dependency on `tempfile`.
    struct TempDir(PathBuf);
    impl TempDir {
        fn new(tag: &str) -> Self {
            let base = std::env::temp_dir();
            // Nanosecond timestamp + pid is enough entropy for parallel tests.
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0);
            let path = base.join(format!("ap-skills-{tag}-{nanos}-{}", std::process::id()));
            fs::create_dir_all(&path).unwrap();
            Self(path)
        }
        fn path(&self) -> &Path {
            &self.0
        }
    }
    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn env_override_wins_when_dir_exists() {
        let tmp = TempDir::new("env-wins");
        let skills = tmp.path().join("skills-override");
        fs::create_dir_all(&skills).unwrap();
        let exe = tmp.path().join("bin").join("ap-cli");
        let fallback = tmp.path().join("fallback");
        fs::create_dir_all(exe.parent().unwrap()).unwrap();
        fs::create_dir_all(&fallback).unwrap();

        let got = resolve_skills_dir(
            Some(skills.to_str().unwrap()),
            Some(&exe),
            &fallback,
        );
        assert_eq!(got.as_deref(), Some(skills.as_path()));
    }

    #[test]
    fn env_override_ignored_when_dir_missing() {
        let tmp = TempDir::new("env-missing");
        let fallback = tmp.path().join("fallback");
        fs::create_dir_all(&fallback).unwrap();

        let got = resolve_skills_dir(
            Some("/this/path/does/not/exist/xyz"),
            None,
            &fallback,
        );
        assert_eq!(got.as_deref(), Some(fallback.as_path()));
    }

    #[test]
    fn installed_layout_resolves_from_exe_parent() {
        let tmp = TempDir::new("installed");
        let prefix = tmp.path();
        let bin = prefix.join("bin");
        fs::create_dir_all(&bin).unwrap();
        let exe = bin.join("ap-cli");
        fs::write(&exe, b"").unwrap();
        let skills = prefix.join("share").join("auto-package").join("skills");
        fs::create_dir_all(&skills).unwrap();

        let got = resolve_skills_dir(None, Some(&exe), Path::new("/nope"));
        assert_eq!(got.as_deref(), Some(skills.as_path()));
    }

    #[test]
    fn dev_layout_resolves_two_parents_up() {
        let tmp = TempDir::new("dev");
        let workspace = tmp.path();
        let exe_dir = workspace.join("target").join("debug");
        fs::create_dir_all(&exe_dir).unwrap();
        let exe = exe_dir.join("ap-cli");
        fs::write(&exe, b"").unwrap();
        let skills = workspace.join("packaging-skills");
        fs::create_dir_all(&skills).unwrap();

        let got = resolve_skills_dir(None, Some(&exe), Path::new("/nope"));
        assert_eq!(got.as_deref(), Some(skills.as_path()));
    }

    #[test]
    fn falls_back_to_compile_time_path_when_nothing_near_exe() {
        let tmp = TempDir::new("fallback");
        let exe_dir = tmp.path().join("random").join("place");
        fs::create_dir_all(&exe_dir).unwrap();
        let exe = exe_dir.join("ap-cli");
        fs::write(&exe, b"").unwrap();
        let fallback = tmp.path().join("compile-time-skills");
        fs::create_dir_all(&fallback).unwrap();

        let got = resolve_skills_dir(None, Some(&exe), &fallback);
        assert_eq!(got.as_deref(), Some(fallback.as_path()));
    }

    #[test]
    fn returns_none_when_no_candidate_exists() {
        let got = resolve_skills_dir(
            Some("/missing-env"),
            Some(Path::new("/missing/bin/ap-cli")),
            Path::new("/missing-fallback"),
        );
        assert!(got.is_none());
    }

    #[test]
    fn installed_layout_preferred_over_dev_layout() {
        // When BOTH an installed-prefix skills dir and a dev `../../packaging-skills`
        // dir exist around the exe, the installed one must win (priority order).
        let tmp = TempDir::new("both");
        let prefix = tmp.path();
        let bin = prefix.join("bin");
        fs::create_dir_all(&bin).unwrap();
        let exe = bin.join("ap-cli");
        fs::write(&exe, b"").unwrap();
        let installed = prefix.join("share").join("auto-package").join("skills");
        fs::create_dir_all(&installed).unwrap();
        // Simulate a sibling packaging-skills two levels up too.
        let dev = tmp
            .path()
            .parent()
            .map(|p| p.join("packaging-skills"))
            .unwrap_or_else(|| PathBuf::from("/tmp/packaging-skills"));
        let _ = fs::create_dir_all(&dev);

        let got = resolve_skills_dir(None, Some(&exe), Path::new("/nope"));
        assert_eq!(got.as_deref(), Some(installed.as_path()));

        let _ = fs::remove_dir_all(&dev);
    }
}
