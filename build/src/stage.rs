//! Privileged file staging for agent turns.
//!
//! An agent turn cannot always write into the package checkout through the
//! shell (its permission gate refuses `cp` into it). A turn asks for the copy
//! with a typed `stage_files` request instead, and the pipeline performs it
//! here after validating every entry: the source must be a regular file inside
//! a tree the request names (the package's own state directory, or a dependency's
//! state directory or checkout), the destination is a bare file name in the
//! package checkout, and the file is under a size cap.

use std::path::{Component, Path, PathBuf};

use serde::Deserialize;

/// Largest single file that is staged.
pub const MAX_STAGE_BYTES: u64 = 64 * 1024 * 1024;
/// Most files one turn may stage.
pub const MAX_STAGE_FILES: usize = 16;

/// One requested copy.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct StageFile {
    /// Path relative to the package's own `state/<pkg>/` tree (`source/package-lock.json`),
    /// or, with `package`, relative to that dependency's state tree or checkout.
    pub from: String,
    /// Destination file name in the package checkout (a bare name, no directory).
    pub to: String,
    /// A package whose tree `from` is read from instead of the package's own.
    #[serde(default)]
    pub package: Option<String>,
}

/// The trees staging reads from and writes to.
pub struct StageRoots<'a> {
    /// `state/<pkg>/` of the package being worked on.
    pub own_state: &'a Path,
    /// `work_dir/state/`, for a dependency's state tree.
    pub state_root: &'a Path,
    /// `work_dir/<project>/`, the parent of every package checkout.
    pub project_dir: &'a Path,
    /// The package's checkout.
    pub checkout: &'a Path,
}

fn bare_name(name: &str, what: &str) -> Result<(), String> {
    let bad = name.is_empty()
        || name == "."
        || name == ".."
        || name.contains(['/', '\\', '\0'])
        || name.starts_with('.');
    if bad {
        return Err(format!(
            "{what} {name:?} must be a plain file name (no directory, not hidden)"
        ));
    }
    Ok(())
}

fn relative_clean(path: &str) -> Result<PathBuf, String> {
    let p = Path::new(path);
    if path.is_empty() || p.is_absolute() {
        return Err(format!("from {path:?} must be a relative path"));
    }
    if p.components().any(|c| !matches!(c, Component::Normal(_))) {
        return Err(format!("from {path:?} must not contain `..` or `.` components"));
    }
    Ok(p.to_path_buf())
}

/// The canonical source file for `rel` below one of `bases`, which must be a
/// regular file whose real path stays inside that base.
fn resolve_source(bases: &[PathBuf], rel: &Path) -> Result<PathBuf, String> {
    let mut last = format!("{} is not in the tree", rel.display());
    for base in bases {
        let Ok(canon_base) = std::fs::canonicalize(base) else {
            continue;
        };
        let Ok(real) = std::fs::canonicalize(base.join(rel)) else {
            continue;
        };
        if !real.starts_with(&canon_base) {
            last = format!(
                "{} resolves outside {} (a symlink leaves the tree)",
                rel.display(),
                base.display()
            );
            continue;
        }
        if !real.is_file() {
            last = format!("{} is not a regular file", rel.display());
            continue;
        }
        return Ok(real);
    }
    Err(last)
}

/// Validate one request and return `(source, destination)`.
pub fn validate(req: &StageFile, roots: &StageRoots<'_>) -> Result<(PathBuf, PathBuf), String> {
    bare_name(&req.to, "to")?;
    let rel = relative_clean(&req.from)?;
    let bases: Vec<PathBuf> = match req.package.as_deref() {
        None => vec![roots.own_state.to_path_buf()],
        Some(pkg) => {
            bare_name(pkg, "package")?;
            vec![roots.state_root.join(pkg), roots.project_dir.join(pkg)]
        }
    };
    let src = resolve_source(&bases, &rel)?;
    let len = std::fs::metadata(&src)
        .map_err(|e| format!("{}: {e}", rel.display()))?
        .len();
    if len > MAX_STAGE_BYTES {
        return Err(format!(
            "{} is {len} bytes, over the {MAX_STAGE_BYTES}-byte staging cap",
            rel.display()
        ));
    }
    let dest = roots.checkout.join(&req.to);
    if std::fs::symlink_metadata(&dest).is_ok_and(|m| !m.is_file()) {
        return Err(format!("{} exists and is not a regular file", req.to));
    }
    Ok((src, dest))
}

/// Perform every request. Returns the destination names copied and, for an
/// entry that failed validation (skipped), the reason.
pub fn stage_files(reqs: &[StageFile], roots: &StageRoots<'_>) -> (Vec<String>, Vec<String>) {
    let mut done = Vec::new();
    let mut refused = Vec::new();
    if reqs.len() > MAX_STAGE_FILES {
        refused.push(format!(
            "{} files requested, at most {MAX_STAGE_FILES} are staged per turn",
            reqs.len()
        ));
    }
    for req in reqs.iter().take(MAX_STAGE_FILES) {
        let result = validate(req, roots).and_then(|(src, dest)| {
            std::fs::read(&src)
                .map_err(|e| format!("reading {}: {e}", req.from))
                .and_then(|bytes| {
                    ap_core::fsx::write_atomic(&dest, bytes)
                        .map_err(|e| format!("writing {}: {e}", req.to))
                })
        });
        match result {
            Ok(()) => done.push(req.to.clone()),
            Err(e) => refused.push(format!("stage_files {} -> {}: {e}", req.from, req.to)),
        }
    }
    (done, refused)
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fx {
        t: tempfile::TempDir,
        own: PathBuf,
        state: PathBuf,
        project: PathBuf,
        checkout: PathBuf,
    }

    fn fx() -> Fx {
        let t = tempfile::tempdir().unwrap();
        let state = t.path().join("state");
        let own = state.join("python-x");
        let project = t.path().join("proj");
        let checkout = project.join("python-x");
        for d in [&own, &checkout, &state.join("dep"), &project.join("dep")] {
            std::fs::create_dir_all(d).unwrap();
        }
        std::fs::create_dir_all(own.join("sdist/top")).unwrap();
        std::fs::write(own.join("sdist/top/package-lock.json"), b"{}").unwrap();
        std::os::unix::fs::symlink(own.join("sdist"), own.join("source")).unwrap();
        std::fs::write(t.path().join("secret"), b"s").unwrap();
        Fx {
            t,
            own,
            state,
            project,
            checkout,
        }
    }

    impl Fx {
        fn roots(&self) -> StageRoots<'_> {
            StageRoots {
                own_state: &self.own,
                state_root: &self.state,
                project_dir: &self.project,
                checkout: &self.checkout,
            }
        }
    }

    fn req(from: &str, to: &str, package: Option<&str>) -> StageFile {
        StageFile {
            from: from.into(),
            to: to.into(),
            package: package.map(Into::into),
        }
    }

    #[test]
    fn copies_from_the_source_tree_through_its_symlink() {
        let f = fx();
        let (done, refused) = stage_files(
            &[req("source/top/package-lock.json", "package-lock.json", None)],
            &f.roots(),
        );
        assert_eq!(done, ["package-lock.json"]);
        assert!(refused.is_empty(), "{refused:?}");
        assert_eq!(
            std::fs::read(f.checkout.join("package-lock.json")).unwrap(),
            b"{}"
        );
    }

    #[test]
    fn copies_from_a_dependencys_checkout() {
        let f = fx();
        std::fs::write(f.project.join("dep/dep.spec"), b"Name: dep").unwrap();
        let (done, refused) = stage_files(&[req("dep.spec", "dep.spec", Some("dep"))], &f.roots());
        assert_eq!((done.len(), refused.len()), (1, 0), "{refused:?}");
    }

    #[test]
    fn refuses_traversal_absolute_and_nested_destinations() {
        let f = fx();
        for r in [
            req("../secret", "x", None),
            req("source/../../secret", "x", None),
            req("/etc/passwd", "x", None),
            req("source/top/package-lock.json", "../x", None),
            req("source/top/package-lock.json", "sub/x", None),
            req("source/top/package-lock.json", ".osc", None),
            req("source/top/package-lock.json", "", None),
            req("x", "x", Some("../dep")),
        ] {
            let (done, refused) = stage_files(&[r.clone()], &f.roots());
            assert!(done.is_empty() && refused.len() == 1, "{r:?}: {done:?} {refused:?}");
        }
    }

    #[test]
    fn refuses_a_symlink_leaving_the_tree_and_a_directory() {
        let f = fx();
        std::os::unix::fs::symlink(f.t.path().join("secret"), f.own.join("escape")).unwrap();
        let (done, refused) =
            stage_files(&[req("escape", "x", None), req("sdist", "y", None)], &f.roots());
        assert!(done.is_empty(), "{done:?}");
        assert_eq!(refused.len(), 2, "{refused:?}");
    }

    #[test]
    fn refuses_a_file_over_the_cap_and_too_many_files() {
        let f = fx();
        let big = f.own.join("big");
        std::fs::File::create(&big)
            .unwrap()
            .set_len(MAX_STAGE_BYTES + 1)
            .unwrap();
        let (done, refused) = stage_files(&[req("big", "big", None)], &f.roots());
        assert!(done.is_empty() && refused[0].contains("cap"), "{refused:?}");
        let many: Vec<StageFile> = (0..MAX_STAGE_FILES + 1)
            .map(|i| req("source/top/package-lock.json", &format!("f{i}"), None))
            .collect();
        let (done, refused) = stage_files(&many, &f.roots());
        assert_eq!(done.len(), MAX_STAGE_FILES);
        assert_eq!(refused.len(), 1);
    }
}
