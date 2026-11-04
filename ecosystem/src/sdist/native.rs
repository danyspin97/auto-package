//! The native build system of an archive: CMake, meson or autotools, at the
//! archive root or in the one subdirectory that holds the project.

use super::*;

/// A native build system, in the order a root with several is read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NativeKind {
    Cmake,
    Meson,
    Autotools,
}

/// Where a native build lives and what it is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativeBuild {
    pub kind: NativeKind,
    /// The subdirectory of the archive's top-level directory that holds the
    /// build file, or `None` for the archive root. `fortune-mod` keeps its
    /// `CMakeLists.txt` in `fortune-mod/` of the repository snapshot.
    pub dir: Option<String>,
}

/// Subdirectories that hold a build of their own for something other than
/// the project (tests, documentation, bundled libraries).
const NOT_THE_PROJECT: &[&str] = &[
    "test",
    "tests",
    "doc",
    "docs",
    "example",
    "examples",
    "contrib",
    "third_party",
    "thirdparty",
    "vendor",
    "external",
    "deps",
    "subprojects",
];

fn kind_of(file: &str) -> Option<NativeKind> {
    match file {
        "CMakeLists.txt" => Some(NativeKind::Cmake),
        "meson.build" => Some(NativeKind::Meson),
        "configure.ac" | "configure.in" | "Makefile.am" => Some(NativeKind::Autotools),
        _ => None,
    }
}

/// The first kind among `kinds` in [`NativeKind`] order.
fn preferred(kinds: impl Iterator<Item = NativeKind>) -> Option<NativeKind> {
    kinds.min_by_key(|k| match k {
        NativeKind::Cmake => 0,
        NativeKind::Meson => 1,
        NativeKind::Autotools => 2,
    })
}

/// The native build of an archive with these `files` (paths from the
/// archive, below `top_level_dir` when it has one).
///
/// The root wins. A build file one level down counts only when exactly one
/// subdirectory has one, so a project with several side builds is not
/// guessed at.
pub(super) fn find(files: &[String], top_level_dir: Option<&str>) -> Option<NativeBuild> {
    let rest = |f: &'_ String| -> Option<String> {
        match top_level_dir {
            Some(top) => Some(f.strip_prefix(top)?.strip_prefix('/')?.to_string()),
            None => Some(f.clone()),
        }
    };
    let rests: Vec<String> = files.iter().filter_map(rest).collect();
    let root = preferred(
        rests
            .iter()
            .filter(|r| !r.contains('/'))
            .filter_map(|r| kind_of(r)),
    );
    if let Some(kind) = root {
        return Some(NativeBuild { kind, dir: None });
    }
    let mut dirs: std::collections::BTreeMap<&str, Vec<NativeKind>> = Default::default();
    for r in &rests {
        if let Some((dir, file)) = r.split_once('/')
            && !file.contains('/')
            && !NOT_THE_PROJECT.contains(&dir.to_lowercase().as_str())
            && is_safe_top_level_dir(dir)
            && let Some(kind) = kind_of(file)
        {
            dirs.entry(dir).or_default().push(kind);
        }
    }
    if dirs.len() != 1 {
        return None;
    }
    let (dir, kinds) = dirs.into_iter().next()?;
    Some(NativeBuild {
        kind: preferred(kinds.into_iter())?,
        dir: Some(dir.to_string()),
    })
}
