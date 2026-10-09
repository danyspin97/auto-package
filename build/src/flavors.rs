//! Which Python flavors (`python313`, `python314`, ...) this work directory
//! builds for — read once from `<work-dir>/flavors.toml`, not passed on the
//! command line.
//!
//! The actual flavor set a spec builds for comes from the distro-wide
//! `%{?sle15_python_module_pythons}` macro (convention 8) and is not
//! something a single invocation decides; it also does not change between
//! runs against the same work directory, since that always targets the same
//! `--distro`. A command-line flag would have to be remembered and repeated
//! on every invocation with no way to tell a forgotten one from "no
//! restriction" — a file next to `sources.toml` is read the same way on
//! every run with nothing to remember.
//!
//! Today this has exactly one consumer: [`ap_ecosystem::pypi::pick_wheel`]'s
//! decision about whether a CPython-ABI-locked wheel
//! (`wheel_is_flavor_agnostic`'s `false` case) is still safe to repackage
//! automatically. It is safe only when this file says the project builds for
//! exactly one flavor and that flavor's own ABI tag is the one locked wheel
//! on offer — seeing more than one configured flavor (or none at all, the
//! default) must not relax anything, since a single wheel file fundamentally
//! cannot satisfy two different CPython ABIs (see convention 49 for the
//! manual, per-flavor alternative).

use std::path::{Path, PathBuf};

#[derive(Debug, Default, Clone, serde::Serialize, serde::Deserialize)]
pub struct PythonFlavors {
    #[serde(default)]
    flavors: Vec<String>,
}

impl PythonFlavors {
    pub fn path(work_dir: &Path) -> PathBuf {
        work_dir.join("flavors.toml")
    }

    /// Read `<work-dir>/flavors.toml`, or an empty (unrestricted) set if it
    /// is absent — unrestricted means "don't know", which must keep today's
    /// conservative behavior (refuse every CPython-locked wheel) rather than
    /// being read as "builds for zero flavors".
    ///
    /// A malformed file is an error the user needs to see — same reasoning
    /// as [`crate::sources::SourceMap::load`]: a setting they wrote that
    /// silently fails to apply is worse than a loud one.
    pub fn load(work_dir: &Path) -> Result<Self, String> {
        let path = Self::path(work_dir);
        match std::fs::read_to_string(&path) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(format!("reading {}: {e}", path.display())),
            Ok(text) => {
                toml::from_str(&text).map_err(|e| format!("parsing {}: {e}", path.display()))
            }
        }
    }

    /// The flavors to build wheels for: the configured set, else the host's
    /// `%{pythons}` (what the target distro builds for; the review agent reads
    /// it the same way). Empty when neither is known.
    pub fn effective(&self) -> Vec<String> {
        if !self.flavors.is_empty() {
            return self.flavors.clone();
        }
        std::process::Command::new("rpmspec")
            .args(["-E", "%{pythons}"])
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| {
                String::from_utf8_lossy(&o.stdout)
                    .split_whitespace()
                    .filter(|f| abi_tag_for_flavor(f).is_some())
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default()
    }

    /// The single flavor this project is configured to build for, when it is
    /// configured for exactly one. `None` both when the file is absent
    /// (default, "don't know") and when it names several — only the
    /// single-flavor case ever changes wheel-picking behavior (see the
    /// module doc comment for why).
    pub fn single_flavor(&self) -> Option<&str> {
        match self.flavors.as_slice() {
            [only] => Some(only),
            _ => None,
        }
    }
}

/// The CPython ABI tag a Python flavor name corresponds to in a wheel
/// filename, e.g. `python313` -> `cp313`, `python313t` -> `cp313t` (the
/// free-threaded build). `None` for anything not shaped like `python<rest>`.
pub fn abi_tag_for_flavor(flavor: &str) -> Option<String> {
    let rest = flavor.strip_prefix("python")?;
    if rest.is_empty() {
        return None;
    }
    Some(format!("cp{rest}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_missing_file_has_no_single_flavor() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(
            PythonFlavors::load(dir.path()).unwrap().single_flavor(),
            None
        );
    }

    #[test]
    fn a_broken_file_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(PythonFlavors::path(dir.path()), "this is not toml {{{").unwrap();
        assert!(PythonFlavors::load(dir.path()).is_err());
    }

    #[test]
    fn exactly_one_configured_flavor_is_the_single_flavor() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            PythonFlavors::path(dir.path()),
            "flavors = [\"python313\"]\n",
        )
        .unwrap();
        assert_eq!(
            PythonFlavors::load(dir.path()).unwrap().single_flavor(),
            Some("python313")
        );
    }

    #[test]
    fn multiple_configured_flavors_have_no_single_flavor() {
        // Two-or-more is deliberately treated the same as "unknown": a single
        // wheel file cannot satisfy two different CPython ABIs, so nothing
        // about the automatic wheel-fallback decision may relax here.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            PythonFlavors::path(dir.path()),
            "flavors = [\"python313\", \"python314\"]\n",
        )
        .unwrap();
        assert_eq!(
            PythonFlavors::load(dir.path()).unwrap().single_flavor(),
            None
        );
    }

    #[test]
    fn abi_tag_translation() {
        assert_eq!(abi_tag_for_flavor("python313").as_deref(), Some("cp313"));
        assert_eq!(abi_tag_for_flavor("python313t").as_deref(), Some("cp313t"));
        assert_eq!(abi_tag_for_flavor("python"), None);
        assert_eq!(abi_tag_for_flavor("pypy313"), None);
    }
}
