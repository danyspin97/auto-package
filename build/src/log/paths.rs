//! Classifying the paths a build log mentions.
//!
//! Run 1's log mentions 859 distinct `*.py` paths and exactly one of them
//! mattered. Selecting well is mostly a matter of knowing which of three very
//! different things a path is:
//!
//! - the unpacked sdist, which is *our source* and is readable;
//! - the buildroot copy, which is almost always byte-compilation chatter;
//! - a dependency under `/usr/lib/pythonX.Y/site-packages`, which we must never
//!   try to read out of the sdist — its *presence* is diagnostic, the file is
//!   not.

use std::sync::LazyLock;
use std::sync::atomic::{AtomicBool, Ordering};

use regex::Regex;

/// What a path found in a build log actually refers to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PathClass {
    /// Inside `…/BUILD/<pkg>-build/<sdist-root>/` — the unpacked sdist.
    /// `relative` is sdist-relative and can be handed to `read_sdist_file`.
    BuildTree { relative: String },

    /// Inside `…/BUILDROOT/…` — the installed copy. `module_path` is the part
    /// below `site-packages`, which maps back to source only approximately.
    BuildRoot { module_path: String },

    /// A dependency or the interpreter itself. Never readable from the sdist.
    /// `component` is the top-level name (`setuptools`, `pip`, …).
    System { component: String },

    /// Temporary rpm scriptlets and anything else uninteresting.
    Other,
}

impl PathClass {
    /// Whether `read_sdist_file` can serve this path.
    pub fn is_readable_from_sdist(&self) -> bool {
        matches!(self, PathClass::BuildTree { .. })
    }

    /// Ranking for "which file should the model see first".
    ///
    /// Lower sorts earlier. Source beats installed copies beats dependencies:
    /// a `System` frame is where run 1's traceback *started*, and following it
    /// leads nowhere.
    pub fn priority(&self) -> u8 {
        match self {
            PathClass::BuildTree { .. } => 0,
            PathClass::BuildRoot { .. } => 1,
            PathClass::System { .. } => 2,
            PathClass::Other => 3,
        }
    }
}

/// `/usr/lib/python3.14/site-packages/` or `/usr/lib64/python3.14/…`.
static SYSTEM_SITE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^/usr/lib(?:64)?/python\d+\.\d+/(?:site|dist)-packages/(?<rest>.*)$")
        .expect("static pattern")
});

/// The `site-packages` segment wherever it appears, for buildroot paths.
static ANY_SITE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"/(?:site|dist)-packages/(?<rest>.*)$").expect("static pattern"));

/// An absolute path under a `BUILD/` directory, as a build log prints it.
static BUILD_PATH: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"/BUILD/\S+").expect("static pattern"));

/// Maps absolute build-log paths back to sdist-relative ones.
///
/// The sdist root directory name (`glances-4.3.0`) comes from the pre-build
/// tarball listing, which the pre-build phase has already computed. When it
/// is unavailable we fall back to a structural guess, but the explicit name is
/// strictly better and should be supplied whenever it is known.
pub struct PathMapper {
    sdist_root: Option<String>,
    /// Set to true the first time a path is successfully matched against sdist_root.
    /// Callers can call `did_match_root()` after extraction to detect when the
    /// supplied root name never matched anything (e.g. the bare package name was
    /// passed instead of the versioned directory name).
    matched: AtomicBool,
}

impl Default for PathMapper {
    fn default() -> Self {
        Self {
            sdist_root: None,
            matched: AtomicBool::new(false),
        }
    }
}

impl Clone for PathMapper {
    fn clone(&self) -> Self {
        Self {
            sdist_root: self.sdist_root.clone(),
            matched: AtomicBool::new(self.matched.load(Ordering::Relaxed)),
        }
    }
}

impl PathMapper {
    /// Build a mapper that knows the sdist's top-level directory name.
    pub fn with_sdist_root(root: impl Into<String>) -> Self {
        Self {
            sdist_root: Some(root.into()),
            matched: AtomicBool::new(false),
        }
    }

    /// True when at least one path was classified as `BuildTree` using the
    /// explicit sdist root. If this returns false after processing a log, the
    /// caller should emit a warning — the most likely cause is that the root
    /// was the bare package name rather than the versioned directory name.
    pub fn did_match_root(&self) -> bool {
        self.matched.load(Ordering::Relaxed)
    }

    /// Classifies every `BUILD/` path a log mentions, not only the Python
    /// frames, so that [`Self::did_match_root`] says whether the sdist root
    /// occurs anywhere in the log.
    pub fn scan_log(&self, lines: &[&str]) {
        if self.sdist_root.is_none() {
            return;
        }
        for line in lines {
            for found in BUILD_PATH.find_iter(line) {
                self.classify(found.as_str());
            }
        }
    }

    /// The sdist root name this mapper was configured with, if any.
    pub fn sdist_root(&self) -> Option<&str> {
        self.sdist_root.as_deref()
    }

    pub fn classify(&self, path: &str) -> PathClass {
        // Order matters. A buildroot path also lives under `/BUILD/` on
        // openSUSE — the real layout is
        // `…/BUILD/<pkg>-build/BUILDROOT/usr/lib/…` — so testing for the
        // sdist tree first would misfile every installed file as source.
        if let Some(rest) = after_marker(path, "/BUILDROOT/") {
            let module_path = ANY_SITE
                .captures(rest)
                .map(|c| c["rest"].to_string())
                .unwrap_or_else(|| rest.to_string());
            return PathClass::BuildRoot { module_path };
        }

        if let Some(rel) = self.sdist_relative(path) {
            return PathClass::BuildTree { relative: rel };
        }

        if let Some(caps) = SYSTEM_SITE.captures(path) {
            let component = caps["rest"]
                .split('/')
                .next()
                .unwrap_or_default()
                .trim_end_matches(".py")
                .to_string();
            return PathClass::System { component };
        }

        PathClass::Other
    }

    /// The sdist-relative form of a BUILD-tree path, if it is one.
    fn sdist_relative(&self, path: &str) -> Option<String> {
        let _after_build = after_marker(path, "/BUILD/")?;

        // Preferred: we know what the tarball unpacks to.
        if let Some(root) = &self.sdist_root {
            let marker = format!("/{root}/");
            // The first place the root name occurs below BUILD: the root sits
            // right under the `-build` wrapper, and a subdirectory of the same
            // name inside it is source too, not the path to cut at.
            let tail = &path[path.rfind("/BUILD/")?..];
            if let Some(at) = tail.find(&marker) {
                let relative = &tail[at + marker.len()..];
                if has_parent_component(relative) {
                    return None;
                }
                self.matched.store(true, Ordering::Relaxed);
                return Some(relative.to_string());
            }
            // Path is under /BUILD/ but not under the sdist root — a generated
            // wheel, a scratch dir. Not our source.
            return None;
        }

        // Fallback: `<pkg>-<ver>-build/<sdist-root>/<relative>`. rpm creates
        // the `-build` wrapper; upstream's tarball creates the directory
        // inside it.
        let after_build = after_marker(path, "/BUILD/")?;
        let mut parts = after_build.splitn(3, '/');
        let wrapper = parts.next()?;
        let _sdist_root = parts.next()?;
        let relative = parts.next()?;
        if !wrapper.ends_with("-build") || relative.is_empty() || has_parent_component(relative) {
            return None;
        }
        Some(relative.to_string())
    }
}

/// Whether a path has a `..` component. Such a path, taken from a log, can
/// climb out of the unpacked sdist, so it is not source of this package.
fn has_parent_component(path: &str) -> bool {
    path.split('/').any(|c| c == "..")
}

/// Everything after the last occurrence of `marker`.
fn after_marker<'a>(haystack: &'a str, marker: &str) -> Option<&'a str> {
    haystack
        .rfind(marker)
        .map(|i| &haystack[i + marker.len()..])
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The three classes of path an `%install` traceback mixes together.
    const SDIST_FRAME: &str =
        "/home/abuild/rpmbuild/BUILD/python-glances-4.3.0-build/glances-4.3.0/fastentrypoints.py";
    const SYSTEM_FRAME: &str =
        "/usr/lib/python3.14/site-packages/setuptools/command/install_scripts.py";
    const BUILDROOT_NOISE: &str = "/home/abuild/rpmbuild/BUILD/python-glances-4.3.0-build/BUILDROOT/usr/lib/python3.14/site-packages/glances/main.py";

    fn mapper() -> PathMapper {
        PathMapper::with_sdist_root("glances-4.3.0")
    }

    #[test]
    fn sdist_frame_maps_to_relative_path() {
        assert_eq!(
            mapper().classify(SDIST_FRAME),
            PathClass::BuildTree {
                relative: "fastentrypoints.py".into()
            }
        );
    }

    #[test]
    fn dependency_frame_is_not_readable() {
        // The innermost frame in run 1. Following it leads nowhere, and asking
        // for it from the sdist would be a wasted round trip.
        let class = mapper().classify(SYSTEM_FRAME);
        assert_eq!(
            class,
            PathClass::System {
                component: "setuptools".into()
            }
        );
        assert!(!class.is_readable_from_sdist());
    }

    #[test]
    fn buildroot_path_is_not_mistaken_for_source() {
        // This is the regression that matters: BUILDROOT lives *under* BUILD,
        // so a naive `/BUILD/` test files all 859 noise paths as source.
        let class = mapper().classify(BUILDROOT_NOISE);
        assert_eq!(
            class,
            PathClass::BuildRoot {
                module_path: "glances/main.py".into()
            }
        );
        assert!(!class.is_readable_from_sdist());
    }

    #[test]
    fn source_outranks_installed_outranks_dependency() {
        let m = mapper();
        let mut classes = [
            m.classify(SYSTEM_FRAME),
            m.classify(BUILDROOT_NOISE),
            m.classify(SDIST_FRAME),
        ];
        classes.sort_by_key(|c| c.priority());
        assert!(classes[0].is_readable_from_sdist());
        assert_eq!(classes[2].priority(), 2);
    }

    #[test]
    fn a_path_that_climbs_out_of_the_sdist_is_not_source() {
        let escape = "/home/abuild/rpmbuild/BUILD/python-glances-4.3.0-build/glances-4.3.0/../../../../etc/shadow";
        assert!(!mapper().classify(escape).is_readable_from_sdist());
        assert_eq!(mapper().sdist_relative(escape), None);
        assert!(!mapper().did_match_root());
        let blind = PathMapper::default();
        let blind_escape =
            "/home/abuild/rpmbuild/BUILD/python-glances-4.3.0-build/glances-4.3.0/../secret.py";
        assert_eq!(blind.sdist_relative(blind_escape), None);
        // A `..` inside a name is not a parent component.
        assert_eq!(
            mapper().sdist_relative(
                "/home/abuild/rpmbuild/BUILD/python-glances-4.3.0-build/glances-4.3.0/a..b/c.py"
            ),
            Some("a..b/c.py".to_string())
        );
    }

    #[test]
    fn fallback_works_without_a_known_sdist_root() {
        let blind = PathMapper::default();
        assert_eq!(
            blind.classify(SDIST_FRAME),
            PathClass::BuildTree {
                relative: "fastentrypoints.py".into()
            }
        );
        // …and still refuses the buildroot copy.
        assert!(!blind.classify(BUILDROOT_NOISE).is_readable_from_sdist());
    }

    #[test]
    fn nested_relative_paths_survive() {
        let p = "/home/abuild/rpmbuild/BUILD/python-glances-4.3.0-build/glances-4.3.0/glances/outputs/glances_curses.py";
        assert_eq!(
            mapper().classify(p),
            PathClass::BuildTree {
                relative: "glances/outputs/glances_curses.py".into()
            }
        );
    }

    #[test]
    fn rpm_scratch_is_other() {
        assert_eq!(
            mapper().classify("/var/tmp/rpm-tmp.Asoqjo"),
            PathClass::Other
        );
    }

    #[test]
    fn a_root_name_recurring_below_the_root_cuts_at_the_first_one() {
        // `glances-4.3.0/` is the sdist root, and a subdirectory of the same
        // name sits inside it: the source path is below the root, not below
        // the subdirectory.
        let nested = "/home/abuild/rpmbuild/BUILD/python-glances-4.3.0-build/glances-4.3.0/glances-4.3.0/main.py";
        assert_eq!(
            mapper().classify(nested),
            PathClass::BuildTree {
                relative: "glances-4.3.0/main.py".into()
            }
        );
        // Without a root name below BUILD, nothing matches.
        assert_eq!(
            mapper().classify("/home/abuild/rpmbuild/BUILD/other/x.py"),
            PathClass::Other
        );
    }

    #[test]
    fn did_match_root_tracks_successful_matches() {
        let m = mapper();
        assert!(!m.did_match_root());
        m.classify(SDIST_FRAME);
        assert!(m.did_match_root());
    }

    /// A compiler log has no Python frame, so only the scan of the whole log
    /// can see that the root occurs in it.
    #[test]
    fn a_compiler_log_matches_the_root_through_the_scan() {
        let m = mapper();
        m.scan_log(&[
            "/home/abuild/rpmbuild/BUILD/python-glances-4.3.0-build/glances-4.3.0/src/a.cpp:1:1: error: x",
            "/home/abuild/rpmbuild/BUILD/python-glances-4.3.0-build/BUILDROOT/usr/lib/a.o",
        ]);
        assert!(m.did_match_root());
    }

    #[test]
    fn the_scan_does_nothing_without_a_root() {
        let m = PathMapper::default();
        m.scan_log(&["/home/abuild/rpmbuild/BUILD/glances-4.3.0-build/glances-4.3.0/a.cpp"]);
        assert!(!m.did_match_root());
    }

    #[test]
    fn did_match_root_stays_false_for_system_frames() {
        let m = mapper();
        m.classify(SYSTEM_FRAME);
        m.classify(BUILDROOT_NOISE);
        assert!(!m.did_match_root());
    }
}
