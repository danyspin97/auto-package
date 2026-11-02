//! A git dependency: the commit its `url` pins, fetched with `git` and exported
//! as a tree without `.git`, which the caller then hashes like an archive.
//!
//! `git` runs through a [`GitRun`], which the caller passes in. The tests run it
//! on the host against a local repository.
//!
//! Only the pinned commit is fetched. A server that refuses a fetch by id gets a
//! shallow fetch of its branches and tags instead, and the commit must be one of
//! them, and the server may advertise at most [`MAX_REFS`] refs. The tree is
//! listed and checked before it is checked out, since a checkout writes it.
//! Hooks, filters and submodules never run or are never fetched.

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

use super::unpack::{Limits, Link, link_stays_inside};

/// The git binary, at the path the sandbox's PATH gives it.
const GIT: &str = "/usr/bin/git";
/// The bare repository, inside a fetch's scratch directory.
const GITDIR: &str = "git";
/// The checked-out tree, inside a fetch's scratch directory.
const TREE: &str = "tree";
/// Where a server's branches and tags land, so they never touch `FETCH_HEAD`.
const REFS: [&str; 2] = [
    "+refs/heads/*:refs/ap/heads/*",
    "+refs/tags/*:refs/ap/tags/*",
];
/// The most refs a server may advertise to the fallback fetch. Each fetched ref
/// becomes a loose file, so this bounds the files the fallback writes.
const MAX_REFS: usize = 4096;

/// A git dependency's pin.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct GitSource {
    /// The host name: what the allowlist and the sandbox name.
    pub(super) host: String,
    /// The repository's URL, `https://` with no query and no fragment.
    pub(super) remote: String,
    /// The path after the host, `owner/repo` for a forge.
    pub(super) path: String,
    /// The full commit id pinned by `#`, lowercase: 40 or 64 hex digits.
    pub(super) commit: String,
}

/// Runs one `git` command. `args[0]` is the binary, the rest its arguments, and
/// the paths in them are relative to `work`. `host` is the only host the command
/// may reach; `package` is who asked, for the refusal records.
pub(super) trait GitRun {
    /// The command's standard output, or its failure with the error output.
    fn run(
        &self,
        package: &str,
        host: &str,
        work: &Path,
        args: &[String],
    ) -> Result<String, String>;
}

/// A [`GitRun`] that runs nothing: every command fails. For tests of what is not
/// git's: a dependency that needs git is refused by them.
#[cfg(test)]
pub(super) struct NoGit;

#[cfg(test)]
impl GitRun for NoGit {
    fn run(
        &self,
        _package: &str,
        _host: &str,
        _work: &Path,
        args: &[String],
    ) -> Result<String, String> {
        Err(format!("git is not available here: {args:?}"))
    }
}

/// Parse the part of a `git+https://` URL after the scheme, `host/path[?ref=…]#commit`.
/// `url` is the whole URL, for the messages. A `?ref=` names nothing that is
/// fetched: the commit does, so it is only checked for its shape.
pub(super) fn parse(rest: &str, url: &str) -> Result<GitSource, String> {
    let (location, commit) = rest.split_once('#').ok_or_else(|| {
        format!(
            "git dependency `{url}` has no #commit: a branch, a tag or a ?ref= does not pin \
             the code, so the build is not reproducible; pin the full commit id"
        )
    })?;
    if !is_commit(commit) {
        return Err(format!(
            "git dependency `{url}`: #{commit} is not a full commit id"
        ));
    }
    let location = match location.split_once('?') {
        Some((loc, query)) => {
            if !query.starts_with("ref=") || query.contains(['&', '#']) {
                return Err(format!("git dependency `{url}`: only ?ref= is allowed"));
            }
            loc
        }
        None => location,
    };
    let (host, path) = location
        .split_once('/')
        .ok_or_else(|| format!("git dependency `{url}` has no path after the host"))?;
    if host.is_empty()
        || !host
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'.')
    {
        return Err(format!(
            "git dependency `{url}`: `{host}` is not a plain host name (no port, no user name)"
        ));
    }
    if path.split('/').any(|part| !is_segment(part)) {
        return Err(format!(
            "git dependency `{url}`: the path has a part that is empty or has odd characters"
        ));
    }
    Ok(GitSource {
        host: host.to_ascii_lowercase(),
        remote: format!("https://{location}"),
        path: path.to_string(),
        commit: commit.to_ascii_lowercase(),
    })
}

/// A full commit id: 40 (SHA-1) or 64 (SHA-256) hex digits.
fn is_commit(s: &str) -> bool {
    (s.len() == 40 || s.len() == 64) && s.bytes().all(|c| c.is_ascii_hexdigit())
}

/// A part of a repository path: letters, digits and `-_.~+`, not only dots.
fn is_segment(s: &str) -> bool {
    !s.is_empty()
        && !s.bytes().all(|c| c == b'.')
        && s.bytes()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'-' | b'_' | b'.' | b'~' | b'+'))
}

/// The directory of the exported tree, inside the `scratch` of [`fetch_commit`].
pub(super) fn tree_dir(scratch: &Path) -> PathBuf {
    scratch.join(TREE)
}

/// Fetch the commit `src` pins into `scratch`, a fresh directory, and check the
/// tree it exports against `limits`. The tree is [`tree_dir`]`(scratch)`: it
/// holds no `.git` and no link, since the links are read from the disk, removed
/// and returned, as the unpackers return the links of an archive. Nothing is run
/// from the checkout: no hook, no filter, no submodule.
pub(super) fn fetch_commit(
    git: &dyn GitRun,
    package: &str,
    src: &GitSource,
    scratch: &Path,
    limits: Limits,
) -> Result<Vec<Link>, String> {
    let s = Session {
        git,
        package,
        host: &src.host,
        work: scratch,
    };
    let mut init = argv(&["init", "--quiet", "--bare", "--template="]);
    if src.commit.len() == 64 {
        init.push("--object-format=sha256".to_string());
    }
    init.push(GITDIR.to_string());
    s.run(init)?;

    if let Err(by_id) = s.run(fetch_args(src, std::slice::from_ref(&src.commit))) {
        // Some servers refuse a fetch by id. Their branches and tags, shallowly,
        // are the fallback; the pinned commit must be one of them. The server's
        // refs are counted first, so a long list is refused before any is written.
        let what = |e: String| {
            format!(
                "fetching {} from {}: {by_id}; and its branches and tags: {e}",
                src.commit, src.remote
            )
        };
        let advertised = s
            .run(git_dir(&["ls-remote", "--refs", src.remote.as_str()]))
            .map_err(what)?;
        check_ref_count(&advertised, MAX_REFS).map_err(what)?;
        s.run(fetch_args(src, &REFS.map(String::from)))
            .map_err(what)?;
    }
    let kind = s
        .run(git_dir(&["cat-file", "-t", src.commit.as_str()]))
        .ok();
    if kind.as_deref().map(str::trim) != Some("commit") {
        return Err(format!(
            "commit {} is not in {}: the server did not give it by id, and it is not one of its branches or tags",
            src.commit, src.remote
        ));
    }
    within(&scratch.join(GITDIR), limits.max_bytes)?;

    // The checkout writes every blob, so the tree is checked before it runs.
    let listing = s.run(git_dir(&["ls-tree", "-r", "-l", "-z", src.commit.as_str()]))?;
    for oid in check_listing(&listing, limits)? {
        let text = s.run(git_dir(&["cat-file", "blob", oid.as_str()]))?;
        check_attributes(&text)?;
    }

    let tree = tree_dir(scratch);
    fs::create_dir(&tree).map_err(|e| format!("creating {}: {e}", tree.display()))?;
    s.run(checkout_args(&src.commit))?;
    take_links(&tree, limits)
}

/// Refuse a server that advertises more than `max` refs.
pub(super) fn check_ref_count(advertised: &str, max: usize) -> Result<(), String> {
    let n = advertised.lines().count();
    if n > max {
        return Err(format!("the server advertises {n} refs, more than {max}"));
    }
    Ok(())
}

/// Check a tree listing (`git ls-tree -r -l -z`) before anything is checked out.
/// The checkout writes each blob's bytes, so the sum of the sizes bounds the disk
/// and the count bounds the files. Refused: more than `max_entries` entries, more
/// than `max_bytes` of blobs (a link counts its target's length), a path deeper
/// than `max_depth`, a `.git` component, two paths equal under ASCII case
/// folding, and a mode that is not a file, an executable or a link (a submodule,
/// for one). Returns the object ids of the `.gitattributes` files, to check.
pub(super) fn check_listing(listing: &str, limits: Limits) -> Result<Vec<String>, String> {
    let mut bytes: u64 = 0;
    let mut entries = 0usize;
    let mut folded = HashSet::new();
    let mut attributes = Vec::new();
    for record in listing.split('\0').filter(|r| !r.is_empty()) {
        entries += 1;
        if entries > limits.max_entries {
            return Err(format!("more than {} entries", limits.max_entries));
        }
        let unreadable = || format!("unreadable tree listing: {record}");
        let (meta, path) = record.split_once('\t').ok_or_else(unreadable)?;
        let [mode, kind, oid, size] = meta.split_whitespace().collect::<Vec<_>>()[..] else {
            return Err(unreadable());
        };
        if kind != "blob" || !matches!(mode, "100644" | "100755" | "120000") {
            return Err(format!("{path} is not a file, an executable or a link"));
        }
        let parts: Vec<&str> = path.split('/').collect();
        if parts.len() > limits.max_depth + 1 {
            return Err(format!("{path} is deeper than {} levels", limits.max_depth));
        }
        if parts.contains(&".git") {
            return Err(format!("the checkout has a .git: {path}"));
        }
        if !folded.insert(path.to_ascii_lowercase()) {
            return Err(format!("{path} differs only in case from another path"));
        }
        if parts.last() == Some(&".gitattributes") {
            attributes.push(oid.to_string());
        }
        bytes = bytes.saturating_add(size.parse::<u64>().map_err(|_| unreadable())?);
        if bytes > limits.max_bytes {
            return Err(format!("more than {} bytes of files", limits.max_bytes));
        }
    }
    Ok(attributes)
}

/// Refuse a `.gitattributes` that changes the bytes a checkout writes, so the
/// blob sizes no longer bound the disk: `ident` expands `$Id$`, `eol` may add
/// carriage returns, and `working-tree-encoding` re-encodes a file. No filter is
/// configured, so a filter named in an attribute runs nothing.
pub(super) fn check_attributes(text: &str) -> Result<(), String> {
    for line in text.lines().filter(|l| !l.trim_start().starts_with('#')) {
        for token in line.split_whitespace().skip(1) {
            let name = token
                .trim_start_matches(['-', '!'])
                .split('=')
                .next()
                .unwrap_or("");
            if matches!(name, "ident" | "eol" | "working-tree-encoding") {
                return Err(format!(
                    "a .gitattributes sets {name}, which changes the bytes a checkout writes"
                ));
            }
        }
    }
    Ok(())
}

/// One sequence of commands for one dependency.
struct Session<'a> {
    git: &'a dyn GitRun,
    package: &'a str,
    host: &'a str,
    work: &'a Path,
}

impl Session<'_> {
    fn run(&self, args: Vec<String>) -> Result<String, String> {
        self.git.run(self.package, self.host, self.work, &args)
    }
}

/// The binary and then `parts`, as owned arguments.
fn argv(parts: &[&str]) -> Vec<String> {
    std::iter::once(GIT)
        .chain(parts.iter().copied())
        .map(String::from)
        .collect()
}

/// A command on the bare repository. Hooks are off by path, whatever the config.
/// A fetch checks the objects it receives, and keeps them in one pack file
/// (`fetch.unpackLimit=1`), so the sandbox's file size limit bounds them.
fn git_dir(parts: &[&str]) -> Vec<String> {
    let mut args = vec![GIT.to_string(), format!("--git-dir={GITDIR}")];
    for config in [
        "core.hooksPath=/dev/null",
        "fetch.fsckObjects=true",
        "fetch.unpackLimit=1",
    ] {
        args.extend(["-c".to_string(), config.to_string()]);
    }
    args.extend(parts.iter().map(|p| p.to_string()));
    args
}

fn fetch_args(src: &GitSource, refspecs: &[String]) -> Vec<String> {
    let mut args = git_dir(&[
        "fetch",
        "--quiet",
        "--no-tags",
        "--no-recurse-submodules",
        "--depth=1",
        src.remote.as_str(),
    ]);
    args.extend(refspecs.iter().cloned());
    args
}

/// The checkout of the pinned commit into `TREE`. Line endings are not converted,
/// and the LFS filter is named but empty, so a configured one never runs.
fn checkout_args(commit: &str) -> Vec<String> {
    let mut args = vec![
        GIT.to_string(),
        format!("--git-dir={GITDIR}"),
        format!("--work-tree={TREE}"),
    ];
    for config in [
        "core.hooksPath=/dev/null",
        "core.autocrlf=false",
        "filter.lfs.process=",
        "filter.lfs.smudge=",
        "filter.lfs.clean=",
        "filter.lfs.required=false",
    ] {
        args.extend(["-c".to_string(), config.to_string()]);
    }
    args.extend(["checkout", "--quiet", "--force", "--detach", commit].map(String::from));
    args
}

/// Refuse a directory tree larger than `cap` bytes, counting files as they are.
fn within(dir: &Path, cap: u64) -> Result<(), String> {
    fn walk(dir: &Path, cap: u64, total: &mut u64) -> Result<(), String> {
        let rd = fs::read_dir(dir).map_err(|e| format!("reading {}: {e}", dir.display()))?;
        for entry in rd {
            let entry = entry.map_err(|e| format!("reading {}: {e}", dir.display()))?;
            let meta = fs::symlink_metadata(entry.path())
                .map_err(|e| format!("reading {}: {e}", entry.path().display()))?;
            if meta.is_dir() {
                walk(&entry.path(), cap, total)?;
            } else {
                *total = total.saturating_add(meta.len());
                if *total > cap {
                    return Err(format!("the fetched objects are more than {cap} bytes"));
                }
            }
        }
        Ok(())
    }
    walk(dir, cap, &mut 0)
}

/// Refuse an exported tree the unpackers would refuse: more than `max_entries`
/// entries, nested deeper than `max_depth`, more than `max_bytes` of files, a
/// `.git`, or a special file. Each link is read from the disk with `read_link`,
/// target as written, and removed from the tree; the links are returned, the
/// way an unpacker returns them.
fn take_links(root: &Path, limits: Limits) -> Result<Vec<Link>, String> {
    let mut walk = Walk {
        limits,
        entries: 0,
        bytes: 0,
        links: Vec::new(),
    };
    walk.dir(root, root)?;
    for (rel, _) in &walk.links {
        let at = root.join(rel);
        fs::remove_file(&at).map_err(|e| format!("removing the link {}: {e}", rel.display()))?;
    }
    Ok(walk.links)
}

/// What [`take_links`] has counted and found so far.
struct Walk {
    limits: Limits,
    entries: usize,
    bytes: u64,
    links: Vec<Link>,
}

impl Walk {
    fn dir(&mut self, root: &Path, dir: &Path) -> Result<(), String> {
        let limits = self.limits;
        let rd = fs::read_dir(dir).map_err(|e| format!("reading {}: {e}", dir.display()))?;
        for entry in rd {
            let entry = entry.map_err(|e| format!("reading {}: {e}", dir.display()))?;
            let path = entry.path();
            let rel = path.strip_prefix(root).unwrap_or(&path).to_path_buf();
            self.entries += 1;
            if self.entries > limits.max_entries {
                return Err(format!("more than {} entries", limits.max_entries));
            }
            // The top-level name counts as one level, as in `max_depth`'s doc.
            if rel.components().count() > limits.max_depth + 1 {
                return Err(format!(
                    "{} is deeper than {} levels",
                    rel.display(),
                    limits.max_depth
                ));
            }
            if entry.file_name() == ".git" {
                return Err(format!("the checkout has a {}", rel.display()));
            }
            let kind = entry
                .file_type()
                .map_err(|e| format!("reading {}: {e}", rel.display()))?;
            if kind.is_dir() {
                self.dir(root, &path)?;
            } else if kind.is_file() {
                let len = entry
                    .metadata()
                    .map_err(|e| format!("reading {}: {e}", rel.display()))?
                    .len();
                self.bytes = self.bytes.saturating_add(len);
                if self.bytes > limits.max_bytes {
                    return Err(format!("more than {} bytes of files", limits.max_bytes));
                }
            } else if kind.is_symlink() {
                let target =
                    fs::read_link(&path).map_err(|e| format!("reading {}: {e}", rel.display()))?;
                if target.as_os_str().is_empty() {
                    return Err(format!("{} has no link target", rel.display()));
                }
                if !link_stays_inside(&rel, &target) {
                    return Err(format!(
                        "{} links to {}, outside the tree",
                        rel.display(),
                        target.display()
                    ));
                }
                self.links.push((rel, target));
            } else {
                return Err(format!(
                    "{} is not a file, a directory or a link",
                    rel.display()
                ));
            }
        }
        Ok(())
    }
}
