//! Embeds the revision this binary was built from (`AP_BUILD_REVISION`), so
//! the run banner, `--version` and the run log can name it. A working copy
//! with uncommitted edits gets a `-dirty` suffix, because the binary then
//! holds changes that the named commit does not. It never fails the build:
//! when no revision of this project can be read, it embeds `unknown`.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

// The pure parts (`stamp`, `dirty_state`) live in core, where their tests run.
#[allow(dead_code)]
#[path = "../core/src/build_info.rs"]
mod build_info;
use build_info::{dirty_state, stamp};

fn main() {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let root = manifest.join("..");
    let revision = project_root(&root)
        .and_then(|root| revision(&root))
        .unwrap_or_else(|| "unknown".to_string());
    println!("cargo:rustc-env=AP_BUILD_REVISION={revision}");

    println!("cargo:rerun-if-changed=build.rs");
    for path in watched_paths(manifest, &root) {
        println!("cargo:rerun-if-changed={}", path.display());
    }
}

/// The paths whose change can change the stamp: the sources (an edit makes
/// the working copy dirty), the manifests, and the VCS state that moves when
/// a commit is made. Only paths that exist are listed, because cargo reruns
/// the script on every build for a path that does not exist. `.jj/working_copy`
/// is not listed: jj rewrites it on every command, which would rerun the
/// script (and recompile `cli`) on every build.
fn watched_paths(manifest: &Path, root: &Path) -> Vec<PathBuf> {
    let mut paths = vec![
        manifest.join("src"),
        manifest.join("Cargo.toml"),
        root.join("Cargo.toml"),
        root.join("Cargo.lock"),
        root.join("core"),
        root.join("build"),
        root.join("ecosystem"),
        root.join(".git/HEAD"),
        root.join(".git/packed-refs"),
        root.join(".git/refs"),
    ];
    paths.extend(jj_repo_dir(root).map(|repo| repo.join("op_heads")));
    paths.retain(|path| path.exists());
    paths
}

/// The jj repo directory of this workspace. A secondary workspace has a
/// `.jj/repo` file holding the path of the shared repo, relative to `.jj`.
fn jj_repo_dir(root: &Path) -> Option<PathBuf> {
    let link = root.join(".jj/repo");
    if link.is_dir() {
        return Some(link);
    }
    let target = fs::read_to_string(link).ok()?;
    Some(root.join(".jj").join(target.trim()))
}

/// The project root, when it is the workspace root (the directory with
/// `Cargo.lock` and a `[workspace]` manifest). A tarball unpacked inside an
/// unrelated repository fails this and gets no revision.
fn project_root(root: &Path) -> Option<PathBuf> {
    let workspace = fs::read_to_string(root.join("Cargo.toml"))
        .ok()?
        .contains("[workspace]");
    (workspace && root.join("Cargo.lock").is_file()).then(|| root.to_path_buf())
}

/// The stamp from jj when jj's root is the project, else from git when git's
/// top-level is the project.
fn revision(root: &Path) -> Option<String> {
    if is_vcs_root("jj", &["root"], root) {
        jj_stamp(root)
    } else if is_vcs_root("git", &["rev-parse", "--show-toplevel"], root) {
        git_stamp(root)
    } else {
        None
    }
}

/// The parent of the working copy, dirty when the working copy has changes.
/// The `jj log` calls snapshot the working copy (no `--ignore-working-copy`),
/// which is what sees uncommitted edits; the snapshot is what every jj
/// command does anyway.
fn jj_stamp(root: &Path) -> Option<String> {
    let commit = output(
        "jj",
        &[
            "--color=never",
            "log",
            "-r",
            "@-",
            "--no-graph",
            "-T",
            "commit_id.short()",
        ],
        root,
    )?;
    let state = output(
        "jj",
        &[
            "--color=never",
            "log",
            "-r",
            "@",
            "--no-graph",
            "-T",
            r#"if(empty, "clean", "dirty")"#,
        ],
        root,
    )?;
    stamp(&commit, dirty_state(&state)?)
}

fn git_stamp(root: &Path) -> Option<String> {
    let commit = output("git", &["rev-parse", "--short", "HEAD"], root)?;
    let status = output("git", &["status", "--porcelain"], root)?;
    stamp(&commit, !status.trim().is_empty())
}

fn is_vcs_root(program: &str, args: &[&str], root: &Path) -> bool {
    let Some(top) = output(program, args, root) else {
        return false;
    };
    match (fs::canonicalize(top.trim()), fs::canonicalize(root)) {
        (Ok(top), Ok(root)) => top == root,
        _ => false,
    }
}

/// The stdout of a successful command run in `dir`.
fn output(program: &str, args: &[&str], dir: &Path) -> Option<String> {
    let output = Command::new(program)
        .args(args)
        .current_dir(dir)
        .stderr(Stdio::null())
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    String::from_utf8(output.stdout).ok()
}
