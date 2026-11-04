//! Publishing a package: the publish plan, the recipe files that are dropped or
//! restored, the tracked strays, and the commit.

use super::*;
use crate::osc::ci_retry::*;

/// What a checkout publishes to OBS, split by [`publish_plan`].
#[derive(Debug, Default, PartialEq, Eq)]
pub struct PublishPlan {
    /// Checkout-relative names to stage and commit.
    pub files: Vec<String>,
    /// Names present in the checkout that are not part of the package.
    pub skipped: Vec<String>,
}

/// Decide which files of a checkout belong in the commit.
///
/// Only the spec, the archives, the service files, the changelog and rpmlint
/// config, and the quilt patches are published: a patch when `series` or the
/// spec names it. A file a `Source:` or `Patch:` line of the spec names is
/// published whatever its extension (a `LICENSE` fetched from the upstream
/// tag): `%prep` fails without it. Anything else in the directory (backups such as
/// `*.preexisting`, a build directory, a stray file) is left out, so it never
/// reaches OBS. Dot-entries such as `.osc` are not part of the package and are
/// not listed.
pub fn publish_plan(checkout_dir: &Path) -> PublishPlan {
    let mut names: Vec<String> = fs::read_dir(checkout_dir)
        .into_iter()
        .flatten()
        .filter_map(|e| e.ok())
        .filter_map(|e| e.file_name().into_string().ok())
        .filter(|n| !n.starts_with('.'))
        .collect();
    names.sort();

    let spec_texts: Vec<String> = names
        .iter()
        .filter(|n| n.ends_with(".spec"))
        .filter_map(|n| fs::read_to_string(checkout_dir.join(n)).ok())
        .collect();
    let spec_text: String = spec_texts.concat();
    let sources = all_spec_source_names(&spec_texts);
    let series: Vec<String> = fs::read_to_string(checkout_dir.join("series"))
        .unwrap_or_default()
        .lines()
        .map(|l| l.trim().to_string())
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .collect();

    let tracked = tracked_files(checkout_dir);
    let mut plan = PublishPlan::default();
    for name in names {
        // A quilt `series` the pipeline made for its own turns is scratch: it is
        // published only when the package already has one on OBS.
        if name == "series" && !tracked.contains(&name) {
            plan.skipped.push(name);
            continue;
        }
        let recipe = if name == "_multibuild" {
            fs::read_to_string(checkout_dir.join(&name)).unwrap_or_default()
        } else {
            String::new()
        };
        if is_publishable(&name, &spec_text, &recipe, &series, &sources) {
            plan.files.push(name);
        } else {
            plan.skipped.push(name);
        }
    }
    plan
}

/// The file names the spec's `Source:`/`Patch:` lines stand for: a URL's last
/// segment, or the name a `#/name` fragment gives it. A name that holds a macro
/// keeps it, as a pattern [`matches_source_name`] reads.
pub fn spec_source_names(spec_text: &str) -> Vec<String> {
    // `Version`/`Name` are known, so a release archive of another version is
    // not named by `foo-%{version}.tar.gz`; any other macro stays a wildcard.
    let header = |tag: &str| {
        spec_text.lines().find_map(|l| {
            let (t, v) = l.split_once(':')?;
            t.trim()
                .eq_ignore_ascii_case(tag)
                .then(|| v.trim().to_string())
        })
    };
    let (name, version) = (header("Name"), header("Version"));
    let expand = |value: &str| {
        let mut out = value.to_string();
        for (macros, known) in [
            (["%{version}", "%version"], &version),
            (["%{name}", "%name"], &name),
        ] {
            if let Some(known) = known.as_deref().filter(|k| !k.contains('%')) {
                for m in macros {
                    out = out.replace(m, known);
                }
            }
        }
        out
    };
    spec_text
        .lines()
        .filter_map(|line| {
            let source = crate::spec::parse_source_line(line, &["source", "patch"], &expand)?;
            let name = source.file();
            (!name.is_empty()).then(|| name.to_string())
        })
        .collect()
}

/// Whether a source of `spec_text` is exactly the file `name`. `Name` and
/// `Version` are expanded; a source with any other macro never matches.
pub fn spec_names_source(spec_text: &str, name: &str) -> bool {
    spec_source_names(spec_text).iter().any(|n| n == name)
}

/// [`spec_source_names`] of each spec of a package on its own. A spec expands
/// its own `Name:` and `Version:`: a sibling spec at another version
/// (elfutils-debuginfod next to elfutils) names its own archive, and reading the
/// first spec's values for it would name none.
pub(super) fn all_spec_source_names(spec_texts: &[String]) -> Vec<String> {
    spec_texts
        .iter()
        .flat_map(|t| spec_source_names(t))
        .collect()
}

/// Whether `name` is the file a source `pattern` stands for. Each macro in the
/// pattern (`%{version}`, `%version`) matches any text.
pub(super) fn matches_source_name(pattern: &str, name: &str) -> bool {
    let mut parts: Vec<&str> = Vec::new();
    let mut rest = pattern;
    while let Some(at) = rest.find('%') {
        parts.push(&rest[..at]);
        let after = &rest[at + 1..];
        rest = if let Some(body) = after.strip_prefix('{') {
            // The braces close at their own depth: `%{pypi_source %{name} 1.2}`.
            let mut depth = 1usize;
            let mut end = body.len();
            for (i, c) in body.char_indices() {
                match c {
                    '{' => depth += 1,
                    '}' => {
                        depth -= 1;
                        if depth == 0 {
                            end = i + 1;
                            break;
                        }
                    }
                    _ => {}
                }
            }
            &body[end..]
        } else {
            after.trim_start_matches(|c: char| c.is_ascii_alphanumeric() || c == '_')
        };
    }
    parts.push(rest);
    let (first, tail) = parts.split_first().expect("parts is never empty");
    let Some(mut remaining) = name.strip_prefix(first) else {
        return false;
    };
    let Some((last, middle)) = tail.split_last() else {
        return remaining.is_empty();
    };
    for part in middle {
        match remaining.find(part) {
            Some(at) => remaining = &remaining[at + part.len()..],
            None => return false,
        }
    }
    remaining.ends_with(last) && remaining.len() >= last.len()
}

/// The `project`/`package` a checkout's `_link` points at (`osc branch` makes
/// one), when it has one.
pub(super) fn link_target(checkout_dir: &Path) -> Option<(String, String)> {
    let xml = fs::read_to_string(checkout_dir.join("_link")).ok()?;
    let project = crate::obs_xml::attr(&xml, "project")?;
    let package = crate::obs_xml::attr(&xml, "package")?;
    Some((project, package))
}

/// Bring back the build-recipe files (`_constraints`, `_multibuild`) a branch
/// lost to an earlier run that judged them unused, from the package the branch
/// was made from, when the spec does use them. Returns the names restored.
pub fn restore_dropped_recipe_files(checkout_dir: &Path, emit: Emit<'_>) -> Vec<String> {
    let Some((project, package)) = link_target(checkout_dir) else {
        return Vec::new();
    };
    let Ok(rd) = fs::read_dir(checkout_dir) else {
        return Vec::new();
    };
    let spec_text: String = rd
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "spec"))
        .filter_map(|p| fs::read_to_string(p).ok())
        .collect();
    if spec_text.is_empty() {
        return Vec::new();
    }
    let mut restored = Vec::new();
    for name in ["_constraints", "_multibuild"] {
        if checkout_dir.join(name).exists() {
            continue;
        }
        if let Some(text) = cat_file(&project, &package, name, emit)
            && !recipe_file_unused(name, &spec_text, &text)
            && fs::write(checkout_dir.join(name), text).is_ok()
        {
            restored.push(name.to_string());
        }
    }
    restored
}

/// Delete the build-recipe files of the checkout that its spec has no use for
/// ([`recipe_file_unused`]), so an upload never carries them and OBS drops the
/// tracked copy with the next commit. Returns the names removed.
pub fn drop_unused_recipe_files(checkout_dir: &Path) -> Vec<String> {
    let Ok(rd) = fs::read_dir(checkout_dir) else {
        return Vec::new();
    };
    let spec_text: String = rd
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "spec"))
        .filter_map(|p| fs::read_to_string(p).ok())
        .collect();
    if spec_text.is_empty() {
        return Vec::new();
    }
    ["_constraints", "_multibuild"]
        .into_iter()
        .filter(|name| {
            checkout_dir.join(name).is_file()
                && recipe_file_unused(
                    name,
                    &spec_text,
                    &fs::read_to_string(checkout_dir.join(name)).unwrap_or_default(),
                )
                && fs::remove_file(checkout_dir.join(name)).is_ok()
        })
        .map(str::to_string)
        .collect()
}

fn is_publishable(
    name: &str,
    spec_text: &str,
    recipe: &str,
    series: &[String],
    sources: &[String],
) -> bool {
    // `_multibuild` declares flavors that only a spec which reads `%flavor` builds
    // differently. One left behind when a source recipe is swapped for a wheel
    // repackage (python-torch: `standard` and `openmpi4`) makes OBS build the same
    // spec once per flavor, so it goes, and is removed from OBS with the commit.
    if recipe_file_unused(name, spec_text, recipe) {
        return false;
    }
    // A wheel repackage compiles nothing and fetches no source tree: the
    // source recipe's `_service` (tar_scm), `_constraints` (build resources) and
    // `.obsinfo` are leftovers of the recipe the wheel switch replaced.
    let wheel_spec = sources.iter().any(|s| s.ends_with(".whl"));
    if wheel_spec
        && (["_service", "_servicedata", "_constraints"].contains(&name)
            || name.ends_with(".obsinfo"))
    {
        return false;
    }
    // An archive no `Source:` line names (the previous release's tarball or
    // vendor archive, the 784 MB source tarball of a switched recipe) is not
    // needed by `%prep`. Only judged when the spec names sources at all.
    const ARCHIVES: &[&str] = &[
        ".tar.gz", ".tar.bz2", ".tar.xz", ".tar.zst", ".tar", ".tgz", ".tbz", ".txz", ".zip",
        ".crate", ".whl",
    ];
    if !sources.is_empty()
        && ARCHIVES.iter().any(|s| name.ends_with(s))
        && !sources.iter().any(|p| matches_source_name(p, name))
    {
        return false;
    }
    const FIXED: &[&str] = &[
        "_service",
        "_servicedata",
        "_multibuild",
        "_constraints",
        "_link",
        "series",
        // The audit trail of a Cargo workspace repair (see the pipeline).
        "cargo-workspace-fix.txt",
    ];
    // An rpmlintrc (`<name>-rpmlintrc`, or an older `<name>.rpmlintrc`) is published
    // only when a `Source:` names it: an unreferenced one is dropped with the commit.
    const SUFFIXES: &[&str] = &[
        ".spec", ".changes", ".obsinfo", ".tar.gz", ".tar.bz2", ".tar.xz", ".tar.zst", ".tar",
        ".tgz", ".tbz", ".txz", ".zip", ".crate",
        // A prebuilt wheel used as the source, and the pristine copy of an
        // archive the pipeline rewrote (kept so its provenance is auditable).
        ".whl", // Detached signatures and keyrings that ship beside a source archive.
        ".sig", ".asc", ".keyring",
    ];
    // `foo.tar.gz.orig`: the archive as upstream shipped it. A patch backup
    // (`foo.spec.orig`) is not published.
    let archive_original = name
        .strip_suffix(".orig")
        .is_some_and(|base| ARCHIVES.iter().any(|s| base.ends_with(s)));
    if FIXED.contains(&name)
        || archive_original
        || SUFFIXES.iter().any(|s| name.ends_with(s))
        || sources.iter().any(|p| matches_source_name(p, name))
    {
        return true;
    }
    let is_patch = name.ends_with(".patch") || name.ends_with(".diff");
    is_patch && (series.iter().any(|s| s == name) || spec_text.contains(name))
}

/// Whether `line` is a `SourceN:` line whose file name is one of `removed`.
/// A `#/` rename names the file the checkout actually holds, as in
/// `spec_source_names`. Macros are not expanded: a name written with one
/// never matches a removed file.
pub(super) fn is_removed_rpmlintrc_source(line: &str, removed: &[String]) -> bool {
    crate::spec::parse_source_line(line, &["source"], str::to_string)
        .is_some_and(|source| removed.iter().any(|r| r == source.file()))
}

/// Delete zero-byte `*-rpmlintrc` and `*.rpmlintrc` files and the `SourceN:` lines of the spec
/// that name them: an empty filter file is noise in the package. Returns the
/// names removed.
pub fn drop_empty_rpmlintrc(checkout_dir: &Path) -> Vec<String> {
    let mut removed = Vec::new();
    let Ok(rd) = fs::read_dir(checkout_dir) else {
        return removed;
    };
    let mut entries: Vec<_> = rd.flatten().collect();
    entries.sort_by_key(|e| e.file_name());
    for e in &entries {
        let name = e.file_name().to_string_lossy().to_string();
        if (name.ends_with(".rpmlintrc") || name.ends_with("-rpmlintrc"))
            && e.metadata().is_ok_and(|m| m.is_file() && m.len() == 0)
            && fs::remove_file(e.path()).is_ok()
        {
            removed.push(name);
        }
    }
    if removed.is_empty() {
        return removed;
    }
    for e in entries {
        let path = e.path();
        if path.extension().is_none_or(|x| x != "spec") {
            continue;
        }
        let Ok(text) = fs::read_to_string(&path) else {
            continue;
        };
        let kept: String = text
            .split_inclusive('\n')
            .filter(|l| !is_removed_rpmlintrc_source(l, &removed))
            .collect();
        if kept != text {
            let _ = fs::write(&path, kept);
        }
    }
    removed
}

/// Names OBS already tracks for this checkout (`.osc/_files`).
pub(super) fn tracked_files(checkout_dir: &Path) -> Vec<String> {
    let xml = fs::read_to_string(checkout_dir.join(".osc").join("_files")).unwrap_or_default();
    xml.split("<entry ")
        .skip(1)
        .filter_map(|chunk| {
            crate::obs_xml::attr(&chunk[..chunk.find('>').unwrap_or(chunk.len())], "name")
        })
        .collect()
}

/// Bring back what the spec names as a `Source:`/`Patch:` and OBS tracks but the
/// checkout lost (an earlier run's `osc rm`, a file the agent deleted): without
/// it `%prep` fails, and the commit would carry the deletion. Returns the names
/// restored with `osc revert`.
pub fn restore_named_sources(checkout_dir: &Path, emit: Emit<'_>) -> Vec<String> {
    let spec_texts: Vec<String> = fs::read_dir(checkout_dir)
        .into_iter()
        .flatten()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().ends_with(".spec"))
        .filter_map(|e| fs::read_to_string(e.path()).ok())
        .collect();
    // Only a name that is exact: a wildcard would also bring back the previous
    // release's archive, which a version update removes on purpose.
    let sources: Vec<String> = all_spec_source_names(&spec_texts)
        .into_iter()
        .filter(|n| !n.contains('%'))
        .collect();
    let mut restored = Vec::new();
    for name in tracked_files(checkout_dir) {
        if checkout_dir.join(&name).exists()
            || !is_safe_file_arg(&name)
            || !sources.iter().any(|p| matches_source_name(p, &name))
        {
            continue;
        }
        match proc::capture("osc", &["revert", name.as_str()], checkout_dir, emit) {
            Ok(out) if out.success() => restored.push(name),
            Ok(out) => ap_core::diag!("  osc revert {name}: {}", first_line(&out.stderr)),
            Err(e) => ap_core::diag!("  osc revert {name}: {e}"),
        }
    }
    restored
}

/// Files OBS tracks that [`publish_plan`] says are not part of the package (an
/// archive no `Source:` names any more, a dropped rpmlintrc, a leftover),
/// scheduled for removal with `osc rm`. They stay on OBS otherwise, as the
/// 42 MB sdist a wheel switch replaced. Returns the names removed, which the
/// commit must list.
pub fn remove_tracked_strays(
    checkout_dir: &Path,
    skipped: &[String],
    emit: Emit<'_>,
) -> Vec<String> {
    let tracked = tracked_files(checkout_dir);
    let mut removed = Vec::new();
    for name in skipped {
        if !tracked.contains(name) || !is_safe_file_arg(name) {
            continue;
        }
        match proc::capture("osc", &["rm", "--force", name.as_str()], checkout_dir, emit) {
            Ok(out) if out.success() => removed.push(name.clone()),
            Ok(out) => ap_core::diag!("  osc rm {name}: {}", first_line(&out.stderr)),
            Err(e) => ap_core::diag!("  osc rm {name}: {e}"),
        }
    }
    // A tracked file that is gone from the checkout was deleted on purpose (the
    // old release's tarball when a package moves to a new version): the commit
    // must carry the deletion, or OBS keeps both releases' archives. `osc
    // addremove <name>` marks it, `osc ci <name>` commits it.
    for name in &tracked {
        if !checkout_dir.join(name).exists() && !removed.contains(name) && !skipped.contains(name) {
            removed.push(name.clone());
        }
    }
    removed
}

/// The names of `skipped` that `named` lists: files the publish plan leaves out
/// although the expanded spec names them. Committing without them drops a
/// source `%prep` needs, so the commit refuses them.
pub fn named_but_skipped(named: &[String], skipped: &[String]) -> Vec<String> {
    skipped
        .iter()
        .filter(|name| named.contains(name))
        .cloned()
        .collect()
}

/// Archives in the checkout that [`publish_plan`] left out and OBS does not
/// track (an sdist fetched for a version that was then given up, the previous
/// release's tarball), deleted from the checkout so they do not pile up there
/// or show as added. An archive `named` by the expanded spec is never deleted,
/// whatever the plan said. Returns the names deleted.
pub fn discard_untracked_archives(
    checkout_dir: &Path,
    skipped: &[String],
    named: &[String],
) -> Vec<String> {
    const ARCHIVES: &[&str] = &[
        ".tar.gz", ".tar.bz2", ".tar.xz", ".tar.zst", ".tar", ".tgz", ".tbz", ".txz", ".zip",
        ".crate", ".whl",
    ];
    let tracked = tracked_files(checkout_dir);
    let mut gone = Vec::new();
    for name in skipped {
        if tracked.contains(name)
            || named.contains(name)
            || !is_safe_file_arg(name)
            || !ARCHIVES.iter().any(|s| name.ends_with(s))
        {
            continue;
        }
        // Scheduled for addition by an earlier `osc add`: unschedule it first.
        let _ = proc::capture(
            "osc",
            &["revert", name.as_str()],
            checkout_dir,
            Emit::quiet(),
        );
        if fs::remove_file(checkout_dir.join(name)).is_ok() {
            gone.push(name.clone());
        }
    }
    gone
}

/// Commit the package to OBS, triggering a remote build.
///
/// Only `files` are staged and committed. `osc addremove` and `osc ci` both
/// take file arguments, so a file outside the list is neither tracked nor
/// pushed, even when it is modified. When `ci` fails, the whole of its output
/// is written to `log` (when given) and the error names that file.
pub fn commit(
    checkout_dir: &Path,
    message: &str,
    files: &[String],
    log: Option<&Path>,
    emit: Emit<'_>,
) -> Result<(), Error> {
    if files.is_empty() {
        return Err(Error::NothingToPublish(checkout_dir.to_path_buf()));
    }
    if let Some(bad) = files.iter().find(|f| !is_safe_file_arg(f)) {
        return Err(Error::NotActionable(format!(
            "refusing to commit file {bad:?}: not a plain file name"
        )));
    }

    // A `write_atomic` temp file left by a killed run must not linger in the
    // checkout where a later add could pick it up.
    ap_core::fsx::sweep_stale_temp_files(checkout_dir, std::time::Duration::from_secs(24 * 3600));

    // Track new and deleted files among the listed ones. A failure is not
    // fatal (a new package may have nothing to add), but `ci` then commits
    // without them, so say so.
    let mut add = vec!["addremove".to_string()];
    add.extend(files.iter().cloned());
    match proc::capture("osc", &add, checkout_dir, emit) {
        Ok(out) if !out.success() => ap_core::diag!(
            "  osc addremove in {}: exit {}: {}",
            checkout_dir.display(),
            out.code,
            first_line(&out.stderr)
        ),
        Err(e) => ap_core::diag!("  osc addremove in {}: {e}", checkout_dir.display()),
        Ok(_) => {}
    }

    let mut ci = vec!["ci".to_string(), "-m".to_string(), message.to_string()];
    ci.extend(files.iter().cloned());
    // A transient server error (a 5xx, a timeout) is tried again; the commit
    // did not happen, so the same `ci` is the right retry.
    with_ci_retries(
        || ci_once(checkout_dir, message, &ci, log, emit),
        std::thread::sleep,
    )
}

/// What a failed `osc` call says: stderr, then stdout, as their last ten distinct
/// lines, without control characters. osc prints some failures on stdout only
/// (a malformed `_service` is printed there before `exit 1`) and some warnings
/// on stderr, so both are read; the caller keeps the full output in a log.
pub(crate) fn ci_failure_detail(stdout: &str, stderr: &str) -> String {
    let mut seen = std::collections::HashSet::new();
    let mut lines: Vec<String> = Vec::new();
    for text in [stderr, stdout] {
        let text = ap_core::sink::printable(text);
        for line in text
            .lines()
            .map(str::trim_end)
            .filter(|l| !l.trim().is_empty())
        {
            if seen.insert(line.to_string()) {
                lines.push(line.to_string());
            }
        }
    }
    if lines.is_empty() {
        return "osc printed nothing on stdout or stderr".to_string();
    }
    lines[lines.len().saturating_sub(10)..].join("\n")
}
