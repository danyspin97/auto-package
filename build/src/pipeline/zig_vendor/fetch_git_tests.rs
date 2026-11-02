//! Git dependencies, with `git` run on the host against a local bare repository
//! through a fake [`GitRun`]: the fake maps the remote URL to the repository and
//! can refuse fetches by id, as a server may. No network. The sandboxed runner
//! is tested in `fetch_git_sandbox_tests`.

use std::cell::RefCell;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use super::fetch::{Fetch, Refusal, Request, Response, fetch_vendor};
use super::fetch_git::{self, GitRun, GitSource, fetch_commit, tree_dir};
use super::fetch_tests::Quiet;
use super::unpack::{Format, LIMITS, Limits, unpack};
use super::zon;

/// The real pixman archive: its tree, committed as it is, has this zig hash.
const PIXMAN: &[u8] = include_bytes!("testdata/pixman-0.3.0.tar.gz");
const PIXMAN_HASH: &str = "pixman-0.3.0-LClMnz2VAAAs7QSCGwLimV5VUYx0JFnX5xWU6HwtMuDX";
const WAYLAND_HASH: &str = "wayland-0.6.0-lQa1kqz8AQADQmdNJsNhLoNHcnEGEUjrOaPV-dtEnEmX";
/// A host on the built-in allowlist, and no forge: its dependencies are git's.
const HOST: &str = "gitlab.freedesktop.org";
const REMOTE: &str = "https://gitlab.freedesktop.org/ifreund/zig-pixman";
const HEX40: &str = "c183af18ae35dd5fb3dde709697c8f84bb1dc7f3";

/// Archives are not expected in these tests.
struct NoHttp;

impl Fetch for NoHttp {
    fn get(&self, url: &str) -> Result<Response, Refusal> {
        Err(format!("http was not expected: {url}").into())
    }
}

/// Runs git on the host. The remote URL is replaced by the local repository, and
/// `refuse_ids` makes a fetch by commit id fail, as a server may.
struct LocalGit {
    local: PathBuf,
    refuse_ids: bool,
    /// Extra environment for every command, as a configured filter would be.
    extra_env: Vec<(String, String)>,
    calls: RefCell<Vec<Vec<String>>>,
}

impl LocalGit {
    fn new(local: &Path) -> Self {
        LocalGit {
            local: local.to_path_buf(),
            refuse_ids: false,
            extra_env: Vec::new(),
            calls: RefCell::new(Vec::new()),
        }
    }

    fn calls(&self) -> Vec<Vec<String>> {
        self.calls.borrow().clone()
    }
}

fn is_commit_id(s: &str) -> bool {
    (s.len() == 40 || s.len() == 64) && s.bytes().all(|c| c.is_ascii_hexdigit())
}

impl GitRun for LocalGit {
    fn run(
        &self,
        _package: &str,
        host: &str,
        work: &Path,
        args: &[String],
    ) -> Result<String, String> {
        assert_eq!(host, HOST, "git may only reach the dependency's host");
        self.calls.borrow_mut().push(args.to_vec());
        let by_id =
            args.iter().any(|a| a == "fetch") && args.last().is_some_and(|a| is_commit_id(a));
        if self.refuse_ids && by_id {
            return Err("error: upload-pack: not our ref\n".to_string());
        }
        let argv: Vec<String> = args
            .iter()
            .map(|a| {
                if a == REMOTE {
                    self.local.display().to_string()
                } else {
                    a.clone()
                }
            })
            .collect();
        let mut cmd = Command::new(&argv[0]);
        cmd.args(&argv[1..])
            .current_dir(work)
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("HOME", work.join("no-home"))
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_ALLOW_PROTOCOL", "file")
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("GIT_LFS_SKIP_SMUDGE", "1");
        for (k, v) in &self.extra_env {
            cmd.env(k, v);
        }
        let out = cmd.output().map_err(|e| e.to_string())?;
        if out.status.success() {
            Ok(String::from_utf8_lossy(&out.stdout).into_owned())
        } else {
            Err(String::from_utf8_lossy(&out.stderr).into_owned())
        }
    }
}

/// `git` with the identity the test commits use, in `dir`. Panics on failure.
fn git_in(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("/usr/bin/git")
        .args(["-c", "user.name=t", "-c", "user.email=t@example.org"])
        .args([
            "-c",
            "commit.gpgsign=false",
            "-c",
            "init.defaultBranch=main",
        ])
        .args(args)
        .current_dir(dir)
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("HOME", dir)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// Commit everything under `src` (a repository, made if needed) and push `main`
/// to the bare repository `srv` (made if needed). Returns the commit.
fn commit_all(src: &Path, srv: &Path) -> String {
    if !src.join(".git").exists() {
        git_in(src, &["init", "-q"]);
    }
    if !srv.exists() {
        git_in(src, &["init", "-q", "--bare", srv.to_str().unwrap()]);
    }
    git_in(src, &["add", "--force", "--all"]);
    git_in(src, &["commit", "-q", "--allow-empty", "-m", "commit"]);
    git_in(
        src,
        &["push", "-q", srv.to_str().unwrap(), "HEAD:refs/heads/main"],
    );
    git_in(src, &["rev-parse", "HEAD"])
}

/// A source repository in a temp dir: `src` is its working tree, `srv` the bare
/// repository it is pushed to, `commit` the commit pushed last.
struct Fixture {
    dir: tempfile::TempDir,
    src: PathBuf,
    srv: PathBuf,
    commit: String,
}

fn repo_with(files: &[(&str, &[u8])]) -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("src");
    fs::create_dir(&src).unwrap();
    for (rel, bytes) in files {
        let p = src.join(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, bytes).unwrap();
    }
    let srv = dir.path().join("srv.git");
    let commit = commit_all(&src, &srv);
    Fixture {
        dir,
        src,
        srv,
        commit,
    }
}

/// The pixman archive unpacked as a tree and committed: its files are the
/// archive's, so the zig hash is [`PIXMAN_HASH`].
fn pixman_fixture() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("src");
    unpack(PIXMAN, Format::TarGz, &src, LIMITS).unwrap();
    let srv = dir.path().join("srv.git");
    let commit = commit_all(&src, &srv);
    Fixture {
        dir,
        src,
        srv,
        commit,
    }
}

fn source(commit: &str) -> GitSource {
    fetch_git::parse(&format!("{HOST}/ifreund/zig-pixman#{commit}"), "u").unwrap()
}

/// A root manifest with one dependency, pixman, at `url` declaring `hash`.
fn root_with(url: &str, hash: &str) -> zon::Manifest {
    zon::parse(&format!(
        ".{{ .name = .root, .version = \"1.0.0\", .fingerprint = 0x3610a686c4bba334, \
         .dependencies = .{{ .pixman = .{{ .url = \"{url}\", .hash = \"{hash}\" }} }} }}"
    ))
    .unwrap()
}

/// Fetch `root` with `git`, under a fresh temp dir.
fn fetch_with(
    git: &LocalGit,
    root: &zon::Manifest,
) -> (tempfile::TempDir, Result<Vec<super::fetch::Dep>, String>) {
    let dir = tempfile::tempdir().unwrap();
    let req = Request {
        package: "pkg",
        fetch: &NoHttp,
        git,
        work: dir.path(),
        limits: LIMITS,
        reporter: &Quiet,
        pause: std::time::Duration::ZERO,
    };
    let result = fetch_vendor(&req, root).map_err(|e| e.msg);
    (dir, result)
}

#[test]
fn a_git_dependency_on_another_host_is_fetched_and_its_hash_checked() {
    let fx = pixman_fixture();
    let git = LocalGit::new(&fx.srv);
    let url = format!("git+{REMOTE}#{}", fx.commit);
    let (_dir, result) = fetch_with(&git, &root_with(&url, PIXMAN_HASH));
    let deps = result.unwrap();
    assert_eq!(deps[0].archive_url, format!("{REMOTE}#{}", fx.commit));
    assert!(!deps[0].root.join(".git").exists());
    assert!(deps[0].root.join("build.zig.zon").is_file());
    // By id, shallow, with the commit last and no submodules.
    let fetch = git
        .calls()
        .into_iter()
        .find(|c| c.iter().any(|a| a == "fetch"))
        .unwrap();
    assert!(fetch.contains(&"--depth=1".to_string()), "{fetch:?}");
    assert!(fetch.contains(&"--no-recurse-submodules".to_string()));
    assert_eq!(fetch.last(), Some(&fx.commit));
}

#[test]
fn a_server_that_refuses_a_fetch_by_id_gets_its_branches_and_the_commit() {
    let fx = pixman_fixture();
    let mut git = LocalGit::new(&fx.srv);
    git.refuse_ids = true;
    let url = format!("git+{REMOTE}#{}", fx.commit);
    let (_dir, result) = fetch_with(&git, &root_with(&url, PIXMAN_HASH));
    assert!(result.is_ok(), "{result:?}");
    assert!(
        git.calls()
            .iter()
            .any(|c| c.iter().any(|a| a == "+refs/heads/*:refs/ap/heads/*"))
    );
}

#[test]
fn a_refusing_server_must_have_the_commit_among_its_branches_and_tags() {
    let fx = pixman_fixture();
    let old = fx.commit.clone();
    fs::write(fx.src.join("NEW"), b"later\n").unwrap();
    commit_all(&fx.src, &fx.srv);
    let mut git = LocalGit::new(&fx.srv);
    git.refuse_ids = true;
    let url = format!("git+{REMOTE}#{old}");
    let (_dir, result) = fetch_with(&git, &root_with(&url, PIXMAN_HASH));
    let err = result.unwrap_err();
    assert!(err.contains(&format!("commit {old} is not in")), "{err}");
    assert!(err.contains("branches or tags"), "{err}");
}

#[test]
fn a_commit_the_server_does_not_have_is_refused() {
    let fx = pixman_fixture();
    let git = LocalGit::new(&fx.srv);
    let missing = "0".repeat(40);
    let url = format!("git+{REMOTE}#{missing}");
    let (_dir, result) = fetch_with(&git, &root_with(&url, PIXMAN_HASH));
    let err = result.unwrap_err();
    assert!(
        err.contains(&format!("commit {missing} is not in")),
        "{err}"
    );
}

#[test]
fn a_sha256_commit_is_fetched_from_a_sha256_repository() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("src");
    fs::create_dir(&src).unwrap();
    let srv = dir.path().join("srv.git");
    git_in(
        dir.path(),
        &[
            "init",
            "-q",
            "--bare",
            "--object-format=sha256",
            srv.to_str().unwrap(),
        ],
    );
    git_in(&src, &["init", "-q", "--object-format=sha256"]);
    fs::write(src.join("a.txt"), b"sha256\n").unwrap();
    let commit = commit_all(&src, &srv);
    assert_eq!(commit.len(), 64);
    let scratch = dir.path().join("scratch");
    fs::create_dir(&scratch).unwrap();
    let git = LocalGit::new(&srv);
    let links = fetch_commit(&git, "pkg", &source(&commit), &scratch, LIMITS).unwrap();
    assert!(links.is_empty());
    let tree = tree_dir(&scratch);
    assert_eq!(fs::read(tree.join("a.txt")).unwrap(), b"sha256\n");
}

#[test]
fn a_dependency_pinned_by_ref_alone_is_refused_before_git_runs() {
    let git = LocalGit::new(Path::new("/nonexistent/repo"));
    let url = format!("git+{REMOTE}?ref=main");
    let (_dir, result) = fetch_with(&git, &root_with(&url, PIXMAN_HASH));
    let err = result.unwrap_err();
    assert!(err.contains("has no #commit"), "{err}");
    assert!(git.calls().is_empty());
}

#[test]
fn a_git_host_off_the_allowlist_is_refused_before_git_runs() {
    let git = LocalGit::new(Path::new("/nonexistent/repo"));
    let url = format!("git+https://evil.example.net/o/r#{HEX40}");
    let (_dir, result) = fetch_with(&git, &root_with(&url, PIXMAN_HASH));
    let err = result.unwrap_err();
    assert!(err.contains("evil.example.net"), "{err}");
    assert!(err.contains("--allow-host evil.example.net"), "{err}");
    assert!(git.calls().is_empty());
}

#[test]
fn a_git_tree_with_another_hash_is_refused_with_both_hashes() {
    let fx = pixman_fixture();
    let git = LocalGit::new(&fx.srv);
    let url = format!("git+{REMOTE}#{}", fx.commit);
    // The same name and version as the archive, with another digest.
    let other = format!("pixman-0.3.0-{}", &WAYLAND_HASH["wayland-0.6.0-".len()..]);
    let (_dir, result) = fetch_with(&git, &root_with(&url, &other));
    let err = result.unwrap_err();
    assert!(err.contains(PIXMAN_HASH) && err.contains(&other), "{err}");
}

#[test]
fn the_checkout_has_no_git_and_a_configured_lfs_filter_does_not_run() {
    let fx = repo_with(&[
        (
            ".gitattributes",
            b"*.bin filter=lfs diff=lfs merge=lfs -text\n",
        ),
        ("data.bin", b"the content\n"),
        ("src/lib.zig", b"// lib\n"),
    ]);
    let marker = fx.dir.path().join("marker");
    let mut git = LocalGit::new(&fx.srv);
    // A smudge filter configured in the environment: it must not run.
    git.extra_env = vec![
        ("GIT_CONFIG_COUNT".to_string(), "1".to_string()),
        (
            "GIT_CONFIG_KEY_0".to_string(),
            "filter.lfs.smudge".to_string(),
        ),
        (
            "GIT_CONFIG_VALUE_0".to_string(),
            format!("sh -c 'echo ran > {}; cat'", marker.display()),
        ),
    ];
    let scratch = fx.dir.path().join("scratch");
    fs::create_dir(&scratch).unwrap();
    let links = fetch_commit(&git, "pkg", &source(&fx.commit), &scratch, LIMITS).unwrap();
    assert!(links.is_empty());
    let tree = tree_dir(&scratch);
    assert!(!tree.join(".git").exists());
    assert_eq!(fs::read(tree.join("data.bin")).unwrap(), b"the content\n");
    assert_eq!(fs::read(tree.join("src/lib.zig")).unwrap(), b"// lib\n");
    assert!(!marker.exists(), "the smudge filter ran");
}

#[test]
fn the_links_of_a_tree_are_read_from_disk_and_taken_out_of_it() {
    let fx = repo_with(&[("src/lib.zig", b"// lib\n")]);
    std::os::unix::fs::symlink("lib.zig", fx.src.join("src/alias")).unwrap();
    std::os::unix::fs::symlink("../out/there", fx.src.join("src/far")).unwrap();
    let commit = commit_all(&fx.src, &fx.srv);
    let scratch = fx.dir.path().join("scratch");
    fs::create_dir(&scratch).unwrap();
    let git = LocalGit::new(&fx.srv);
    let mut links = fetch_commit(&git, "pkg", &source(&commit), &scratch, LIMITS).unwrap();
    links.sort();
    // The targets are as written; zig hashes them and does not follow them.
    let want = [("src/alias", "lib.zig"), ("src/far", "../out/there")]
        .map(|(l, t)| (PathBuf::from(l), PathBuf::from(t)));
    assert_eq!(links, want);
    let tree = tree_dir(&scratch);
    assert!(fs::symlink_metadata(tree.join("src/alias")).is_err());
    assert!(fs::symlink_metadata(tree.join("src/far")).is_err());
    assert!(tree.join("src/lib.zig").is_file());
}

#[test]
fn the_limits_refuse_a_large_or_deep_tree() {
    let fx = repo_with(&[("a/b/c/d.txt", b"deep\n"), ("x.txt", b"x\n")]);
    let cases = [
        (
            Limits {
                max_entries: 2,
                ..LIMITS
            },
            "more than 2 entries",
        ),
        (
            Limits {
                max_depth: 1,
                ..LIMITS
            },
            "deeper than 1 levels",
        ),
        (
            Limits {
                max_bytes: 10,
                ..LIMITS
            },
            "more than 10 bytes",
        ),
    ];
    for (i, (limits, want)) in cases.into_iter().enumerate() {
        let scratch = fx.dir.path().join(format!("scratch-{i}"));
        fs::create_dir(&scratch).unwrap();
        let git = LocalGit::new(&fx.srv);
        let err = fetch_commit(&git, "pkg", &source(&fx.commit), &scratch, limits).unwrap_err();
        assert!(err.contains(want), "{err}");
    }
}

#[test]
fn unpinned_or_odd_git_urls_are_refused() {
    let c = HEX40;
    let refused = [
        format!("{HOST}/o/r"),
        format!("{HOST}/o/r?depth=1#{c}"),
        format!("{HOST}/o/r?ref=a&x=1#{c}"),
        format!("{HOST}/o/r?ref=a#{c}#x"),
        format!("{HOST}:8443/o/r#{c}"),
        format!("user@{HOST}/o/r#{c}"),
        format!("{HOST}/o/../r#{c}"),
        format!("{HOST}/o//r#{c}"),
        format!("{HOST}/o/r#{}", &c[..12]),
        format!("{HOST}/o/r#v1.0"),
        format!("{HOST}/o/r%20x#{c}"),
        format!("{HOST}/o r#{c}"),
        format!("{HOST}#{c}"),
        format!("/o/r#{c}"),
    ];
    for rest in refused {
        assert!(
            fetch_git::parse(&rest, "u").is_err(),
            "{rest} should be refused"
        );
    }
}

#[test]
fn a_tree_over_the_limit_is_refused_before_the_checkout_writes_it() {
    let fx = repo_with(&[
        ("a.bin", &[b'a'; 600_000][..]),
        ("b.bin", &[b'b'; 600_000][..]),
    ]);
    let scratch = fx.dir.path().join("scratch");
    fs::create_dir(&scratch).unwrap();
    let git = LocalGit::new(&fx.srv);
    let limits = Limits {
        max_bytes: 1 << 20,
        ..LIMITS
    };
    let err = fetch_commit(&git, "pkg", &source(&fx.commit), &scratch, limits).unwrap_err();
    assert!(err.contains("more than 1048576 bytes"), "{err}");
    assert!(!tree_dir(&scratch).exists());
}

#[test]
fn an_ident_attribute_is_refused_before_the_checkout() {
    let fx = repo_with(&[(".gitattributes", b"x.txt ident\n"), ("x.txt", b"$Id$\n")]);
    let scratch = fx.dir.path().join("scratch");
    fs::create_dir(&scratch).unwrap();
    let git = LocalGit::new(&fx.srv);
    let err = fetch_commit(&git, "pkg", &source(&fx.commit), &scratch, LIMITS).unwrap_err();
    assert!(err.contains("sets ident"), "{err}");
    assert!(!tree_dir(&scratch).exists());
}

#[test]
fn a_link_that_leaves_the_tree_is_refused() {
    let fx = repo_with(&[("src/lib.zig", b"// lib\n")]);
    std::os::unix::fs::symlink("../../../etc/passwd", fx.src.join("src/esc")).unwrap();
    let commit = commit_all(&fx.src, &fx.srv);
    let scratch = fx.dir.path().join("scratch");
    fs::create_dir(&scratch).unwrap();
    let git = LocalGit::new(&fx.srv);
    let err = fetch_commit(&git, "pkg", &source(&commit), &scratch, LIMITS).unwrap_err();
    assert!(err.contains("outside the tree"), "{err}");
}

#[test]
fn a_pinned_git_url_is_read_with_its_commit_in_lower_case_and_no_query() {
    let upper = HEX40.to_ascii_uppercase();
    let g = fetch_git::parse(&format!("{HOST}/ifreund/zig-pixman?ref=main#{upper}"), "u").unwrap();
    assert_eq!(g.host, HOST);
    assert_eq!(g.remote, REMOTE);
    assert_eq!(g.path, "ifreund/zig-pixman");
    assert_eq!(g.commit, HEX40);
}
