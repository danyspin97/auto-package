//! OBS/osc integration: package creation, checkout, source sync, and build.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime};

use ap_core::event::Event;

use crate::obs_status;
use crate::proc::{self, Captured, Emit};

mod branch;
pub use branch::*;
mod names;
pub use names::*;
mod error;
pub use error::*;

mod failure;
#[cfg(test)]
mod transient_text_tests;
pub use failure::*;

#[cfg(test)]
mod extract_unresolvable_tests;

#[cfg(test)]
mod fuzzy_key_tests;

mod distro;
#[cfg(test)]
mod strip_devel_wrapping_tests;
pub use distro::*;

#[cfg(test)]
mod distro_provides_tests;
mod repo_paths;
pub use repo_paths::*;
mod published;
pub use published::*;

#[cfg(test)]
mod tracked_stray_tests;

#[cfg(test)]
mod pending_changes_tests;

#[cfg(test)]
mod published_binary_tests;

static PROJECT_PACKAGES_CACHE: OnceLock<
    Mutex<HashMap<String, Arc<std::collections::HashSet<String>>>>,
> = OnceLock::new();

/// Per-project count of invalidations. A fetch that started before an
/// invalidation holds a listing that may predate the package just committed;
/// the reader compares the generation it saw before fetching and drops the
/// result if it moved, instead of caching the stale set for the whole run.
static PROJECT_PACKAGES_GENERATION: OnceLock<Mutex<HashMap<String, u64>>> = OnceLock::new();

fn project_generation(project: &str) -> u64 {
    PROJECT_PACKAGES_GENERATION
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(project)
        .copied()
        .unwrap_or(0)
}

/// Insert `pkgs` for `project` unless it was invalidated since `generation`
/// was read. The generation is checked under the cache lock, and
/// invalidation bumps it before taking that lock, so no interleaving lets a
/// stale set land after the removal.
fn insert_if_current(
    project: &str,
    generation: u64,
    pkgs: &Arc<std::collections::HashSet<String>>,
) {
    let cache = PROJECT_PACKAGES_CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    let mut guard = cache.lock().unwrap_or_else(|e| e.into_inner());
    if project_generation(project) == generation {
        guard.insert(project.to_owned(), Arc::clone(pkgs));
    }
}

/// Return the set of package names that exist in `project`.
///
/// The result is cached per project name so repeated calls within a process
/// skip the network round-trip — callers look names up once per dependency,
/// so without caching a single batch can mean dozens of `osc ls` round-trips.
/// Callers that successfully commit a *new* package into `project` MUST call
/// [`invalidate_project_packages_cache`] afterward: a long `auto-package
/// build` run adds packages to the same project continuously, and this
/// cache has no TTL and no other invalidation path. Without that call, the
/// very first lookup permanently freezes the answer for the rest of the
/// process — every package built after it would look like it still doesn't
/// exist to every dependent package's availability check, for the entire
/// remainder of the run, no matter how many more succeed.
///
/// Errors when the listing cannot be fetched, so a caller cannot mistake
/// "OBS did not answer" for "the package is absent".
pub fn try_list_project_packages(
    project: &str,
    emit: Emit<'_>,
) -> Result<Arc<std::collections::HashSet<String>>, String> {
    list_project_packages(project, emit)
        .map_err(|e| format!("could not list packages of {project}: {e}"))
}

/// [`try_list_project_packages`] keeping the failure as an [`Error`], so that
/// its transience (a timeout, an OBS 5xx) reaches the caller.
pub fn list_project_packages(
    project: &str,
    emit: Emit<'_>,
) -> Result<Arc<std::collections::HashSet<String>>, Error> {
    let cache = PROJECT_PACKAGES_CACHE.get_or_init(|| Mutex::new(HashMap::new()));

    {
        let guard = cache.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(pkgs) = guard.get(project) {
            return Ok(Arc::clone(pkgs));
        }
    }

    let generation = project_generation(project);
    // A failed listing must not be cached: an empty set would make every
    // package look absent from OBS for the rest of the run. Try up to three
    // times with a short backoff, then report the error so the next call
    // tries again.
    let mut listing = fetch_project_packages(project, emit);
    for backoff in [1, 2] {
        if listing.is_ok() || crate::proc::shutting_down() {
            break;
        }
        std::thread::sleep(Duration::from_secs(backoff));
        listing = fetch_project_packages(project, Emit::quiet());
    }
    let pkgs = listing.map(Arc::new)?;
    insert_if_current(project, generation, &pkgs);
    Ok(pkgs)
}

/// Drop `project`'s cached package listing so the next [`try_list_project_packages`]
/// call re-fetches it. Call this right after successfully building/committing
/// a package into `project` — see [`try_list_project_packages`]'s doc comment for
/// why skipping this silently breaks every later availability check for the
/// rest of the run, not just the one for the package just built.
pub fn invalidate_project_packages_cache(project: &str) {
    // The sources changed, so the shared build results of the project may too.
    RESULTS_CACHE.invalidate(project);
    forget_published_binaries();
    // Bump first: a fetch in flight must see the change even if it reaches
    // `insert_if_current` before the removal below.
    *PROJECT_PACKAGES_GENERATION
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .entry(project.to_owned())
        .or_insert(0) += 1;
    if let Some(cache) = PROJECT_PACKAGES_CACHE.get() {
        cache
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(project);
    }
}

#[cfg(test)]
mod project_packages_cache_tests;

/// Enumerate the files committed to an OBS package.
///
/// Returns `None` when `osc ls` fails (project/package doesn't exist, auth
/// broken) rather than panicking, same as [`cat_file`]: the update sweep
/// uses that `None` to skip the package and record a SpecFetchFailed instead
/// of killing the whole scan.
///
/// Used so the update sweep can discover the real `.spec` filename rather
/// than guess `<pkg>.spec` — AppCo's versioned-library convention gives one
/// OBS package per upstream version (`pytorch+2.4`, `pytorch+2.5`), each
/// holding a plain `<base-name>.spec` (`pytorch.spec`) that has no relation
/// to the OBS package name, so the guess is not enough on its own.
pub fn list_package_files(project: &str, pkg: &str, emit: Emit<'_>) -> Option<Vec<String>> {
    if ![project, pkg].iter().all(|n| is_valid_obs_name(n)) {
        return None;
    }
    let out = osc_query(&["ls", project, pkg], emit, &format!("ls {project}/{pkg}"))?;
    if !out.success() {
        return None;
    }
    Some(
        out.stdout
            .lines()
            .map(|l| l.trim().to_string())
            .filter(|l| !l.is_empty())
            .collect(),
    )
}

/// The spec file of OBS package `pkg` given its file listing: the one `*.spec`
/// it holds. A package with several, or none, falls back to `<pkg>.spec`.
fn spec_file_name(files: &[String], pkg: &str) -> String {
    let specs: Vec<&String> = files.iter().filter(|f| f.ends_with(".spec")).collect();
    match specs.as_slice() {
        [only] => only.to_string(),
        _ => format!("{pkg}.spec"),
    }
}

/// The spec file of OBS package `pkg` in `project`, found from its listing (see
/// [`spec_file_name`]). `<pkg>.spec` when the listing cannot be read.
fn package_spec_name(project: &str, pkg: &str, emit: Emit<'_>) -> String {
    match list_package_files(project, pkg, emit) {
        Some(files) => spec_file_name(&files, pkg),
        None => format!("{pkg}.spec"),
    }
}

#[cfg(test)]
mod spec_file_name_tests;

/// Fetch a single file's contents from an OBS package without checking out.
///
/// Returns `None` when the package doesn't exist, the file isn't in it, or
/// `osc cat` otherwise fails — the update sweep uses that `None` to skip the
/// package instead of blowing up the whole run on a stray missing spec.
/// A file name passed to `osc` as a positional argument. A leading `-` would
/// be read as an option, and a `/` would leave the package directory.
fn is_safe_file_arg(name: &str) -> bool {
    !name.is_empty() && !name.starts_with('-') && !name.contains('/') && !name.contains('\0')
}

pub fn cat_file(project: &str, pkg: &str, filename: &str, emit: Emit<'_>) -> Option<String> {
    if ![project, pkg].iter().all(|n| is_valid_obs_name(n)) || !is_safe_file_arg(filename) {
        return None;
    }
    let out = osc_query(
        &["cat", project, pkg, filename],
        emit,
        &format!("cat {project}/{pkg} {filename}"),
    )?;
    if !out.success() {
        return None;
    }
    Some(out.stdout)
}

fn fetch_project_packages(
    project: &str,
    emit: Emit<'_>,
) -> Result<std::collections::HashSet<String>, Error> {
    if !is_valid_obs_name(project) {
        return Err(Error::NotActionable(format!(
            "refusing to use project {project:?}: not a valid OBS name"
        )));
    }
    let out = proc::capture_with_timeout(
        "osc",
        &["ls", project],
        &anywhere(),
        emit,
        // A whole distro project (openSUSE:Factory) is a long listing.
        Duration::from_secs(180),
    )
    .map_err(during(format!("ls {project}")))?;
    if !out.success() {
        return Err(Error::OscFailed {
            cmd: format!("ls {project}"),
            code: out.code,
            stderr: out.stderr.trim().to_string(),
        });
    }
    Ok(parse_package_listing(&out.stdout))
}

/// Package names from `osc ls <project>`, in the spelling OBS stores them
/// (OBS names are case-sensitive: `python-APScheduler`). A multibuild package
/// is listed as `<source>:<flavor>` (`apache-arrow:python-pyarrow`) and its
/// flavor is itself a package that gets built and published (python313-pyarrow),
/// so the flavor name counts as available too — otherwise a dependency Factory
/// already ships looks absent and gets queued to be built again.
fn parse_package_listing(stdout: &str) -> std::collections::HashSet<String> {
    let mut out = std::collections::HashSet::new();
    for name in stdout.lines().map(str::trim).filter(|s| !s.is_empty()) {
        if let Some((_, flavor)) = name.split_once(':')
            && flavor.starts_with("python")
        {
            out.insert(flavor.to_string());
        }
        out.insert(name.to_string());
    }
    out
}

/// Create an initial `.changes` entry for a new package using `osc vc`.
///
/// Only runs when the `.changes` file does not yet exist. `osc vc` reads the
/// committer identity from `~/.config/osc/oscrc` and writes a datestamped
/// entry at the top of the file.
pub fn create_initial_changes(
    checkout_dir: &Path,
    pkg_name: &str,
    emit: Emit<'_>,
) -> Result<(), Error> {
    let changes_name = format!("{pkg_name}.changes");
    if checkout_dir.join(&changes_name).exists() {
        return Ok(());
    }
    add_changes_entry(checkout_dir, "Initial packaging", emit)
}

/// Add a `.changes` entry saying `message`, with `osc vc`: the committer
/// identity comes from the osc configuration, and a missing `.changes` file is
/// created. A version bump records its own entry this way, so a package
/// updated by the tool does not reach OBS (or a submit request) without one.
/// Idempotent: when the newest entry already says `message` (an earlier run
/// added it), nothing is added.
pub fn add_changes_entry(checkout_dir: &Path, message: &str, emit: Emit<'_>) -> Result<(), Error> {
    if let Ok(rd) = fs::read_dir(checkout_dir) {
        let mut files: Vec<PathBuf> = rd
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|x| x == "changes"))
            .collect();
        files.sort();
        // A version given up on (a wrong upstream, later corrected) left its
        // entry at the top of the file; it was never published, so it goes.
        if let Some(file) = files.first()
            && let Some(name) = file.file_name()
            && let (Ok(working), Ok(pristine)) = (
                fs::read_to_string(file),
                fs::read_to_string(checkout_dir.join(".osc").join("sources").join(name)),
            )
            && let Some(cleaned) = without_unpublished_update_entry(&working, &pristine, message)
        {
            let _ = fs::write(file, cleaned);
        }
        if let Some(text) = files.first().and_then(|f| fs::read_to_string(f).ok())
            && newest_changes_entry_says(&text, message)
        {
            return Ok(());
        }
    }
    let out = proc::capture("osc", &["vc", "-m", message], checkout_dir, emit)
        .map_err(during(format!("vc in {}", checkout_dir.display())))?;
    if !out.success() {
        return Err(Error::OscFailed {
            cmd: format!("vc -m {message:?} in {}", checkout_dir.display()),
            code: out.code,
            stderr: out.stderr,
        });
    }
    merge_pending_changes_file(checkout_dir);
    Ok(())
}

/// [`add_changes_entry`] for a change made while packaging `version`: when the
/// newest entry is this release's own `- Update to <version>` entry, the bullet
/// joins it instead of opening an entry of its own, so a package that took
/// several rounds of fixes still reads as one update. Any other newest entry
/// (an earlier release, a new package) gets a fresh entry as before.
pub fn add_release_changes_entry(
    checkout_dir: &Path,
    version: &str,
    message: &str,
    emit: Emit<'_>,
) -> Result<(), Error> {
    // One bullet per line of `message`: each is folded into the entry in turn.
    for bullet in message.lines().map(str::trim).filter(|l| !l.is_empty()) {
        add_release_bullet(checkout_dir, version, bullet, emit)?;
    }
    Ok(())
}

/// One bullet of [`add_release_changes_entry`].
fn add_release_bullet(
    checkout_dir: &Path,
    version: &str,
    message: &str,
    emit: Emit<'_>,
) -> Result<(), Error> {
    if let Ok(rd) = fs::read_dir(checkout_dir) {
        let mut files: Vec<PathBuf> = rd
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|x| x == "changes"))
            .collect();
        files.sort();
        if let Some(file) = files.first()
            && let Ok(text) = fs::read_to_string(file)
            && let Some(folded) = with_bullet_in_release_entry(&text, version, message)
        {
            return fs::write(file, folded).map_err(|e| {
                Error::NotActionable(format!("cannot write {}: {e}", file.display()))
            });
        }
    }
    add_changes_entry(checkout_dir, message, emit)
}

/// Sets the flag lines of this release's `.changes` entry: each `lines` item
/// becomes an indented continuation line under the `Update to <version>` bullet
/// of the newest entry, so the bullet stays one bullet. Flag lines an earlier
/// pass wrote are replaced, so the entry says what the package is now. When
/// the newest entry is not this release's, each line is added as a bullet of
/// its own, unless the file already has it.
pub fn set_flag_lines(
    checkout_dir: &Path,
    version: &str,
    lines: &[String],
    emit: Emit<'_>,
) -> Result<(), Error> {
    let file = newest_changes_file(checkout_dir);
    if let Some(file) = &file
        && let Ok(text) = fs::read_to_string(file)
        && let Some(folded) = with_flag_lines_in_release_entry(&text, version, lines)
    {
        if folded != text {
            return fs::write(file, folded).map_err(|e| {
                Error::NotActionable(format!("cannot write {}: {e}", file.display()))
            });
        }
        return Ok(());
    }
    let present = file
        .as_ref()
        .and_then(|f| fs::read_to_string(f).ok())
        .unwrap_or_default();
    for line in lines {
        if !present.lines().any(|l| l.trim() == format!("- {line}")) {
            add_changes_entry(checkout_dir, line, emit)?;
        }
    }
    Ok(())
}

/// The first `.changes` file of `checkout_dir`, in name order.
fn newest_changes_file(checkout_dir: &Path) -> Option<PathBuf> {
    let mut files: Vec<PathBuf> = fs::read_dir(checkout_dir)
        .ok()?
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "changes"))
        .collect();
    files.sort();
    files.into_iter().next()
}

/// `changes` with its `Update to <version>` entry's flag lines set to `lines`
/// (see [`set_flag_lines`]). `None` when the newest entry is not that release's.
fn with_flag_lines_in_release_entry(
    changes: &str,
    version: &str,
    lines: &[String],
) -> Option<String> {
    let is_separator = |l: &str| l.len() >= 20 && l.bytes().all(|b| b == b'-');
    let all: Vec<&str> = changes.lines().collect();
    let first = all.iter().position(|l| is_separator(l.trim()))?;
    let end = all[first + 1..]
        .iter()
        .position(|l| is_separator(l.trim()))
        .map_or(all.len(), |i| first + 1 + i);
    let bullet_text = |l: &str| l.trim().trim_start_matches(['-', '*', ' ']).to_string();
    let update = format!("Update to {version}");
    if !all[first + 1..end]
        .iter()
        .map(|l| bullet_text(l))
        .any(|t| t == update)
    {
        return None;
    }
    // A continuation line of an earlier flag set: two spaces, then `FLAG: `.
    let stale = |l: &str| l.starts_with("  ") && l.trim_start().starts_with("FLAG: ");
    let kept: Vec<&str> = all[first + 1..end]
        .iter()
        .copied()
        .filter(|l| !stale(l))
        .collect();
    // The flags go under the `Update to` bullet: after its last line, before
    // the next bullet of the entry (a bullet starts at column 0).
    let is_bullet = |l: &str| !l.starts_with(' ') && (l.starts_with("- ") || l.starts_with("* "));
    let at = kept
        .iter()
        .position(|l| bullet_text(l) == update)
        .unwrap_or(0);
    let block_end = kept[at + 1..]
        .iter()
        .position(|l| is_bullet(l))
        .map_or(kept.len(), |i| at + 1 + i);
    let last = kept[..block_end]
        .iter()
        .rposition(|l| !l.trim().is_empty())
        .map_or(0, |i| i + 1);
    let mut out: Vec<String> = all[..=first].iter().map(|l| l.to_string()).collect();
    out.extend(kept[..last].iter().map(|l| l.to_string()));
    out.extend(lines.iter().map(|l| format!("  {l}")));
    out.extend(kept[last..].iter().map(|l| l.to_string()));
    out.extend(all[end..].iter().map(|l| l.to_string()));
    let mut text = out.join("\n");
    text.push('\n');
    Some(text)
}

/// `changes` with `- message` added to the newest entry, when that entry says
/// `Update to <version>`. `None` otherwise; unchanged text when the entry
/// already has the bullet.
fn with_bullet_in_release_entry(changes: &str, version: &str, message: &str) -> Option<String> {
    let is_separator = |l: &str| l.len() >= 20 && l.bytes().all(|b| b == b'-');
    let lines: Vec<&str> = changes.lines().collect();
    let first = lines.iter().position(|l| is_separator(l.trim()))?;
    let end = lines[first + 1..]
        .iter()
        .position(|l| is_separator(l.trim()))
        .map_or(lines.len(), |i| first + 1 + i);
    let entry = &lines[first + 1..end];
    let bullet_text = |l: &&str| l.trim().trim_start_matches(['-', '*', ' ']).to_string();
    let update = format!("Update to {version}");
    if !entry.iter().map(bullet_text).any(|t| t == update) {
        return None;
    }
    if entry.iter().map(bullet_text).any(|t| t == message) {
        return Some(changes.to_string());
    }
    let last = lines[first + 1..end]
        .iter()
        .rposition(|l| !l.trim().is_empty())
        .map_or(first, |i| first + 1 + i);
    let mut out: Vec<String> = lines[..=last].iter().map(|l| l.to_string()).collect();
    out.push(format!("- {message}"));
    out.extend(lines[last + 1..].iter().map(|l| l.to_string()));
    let mut text = out.join("\n");
    text.push('\n');
    Some(text)
}

fn is_changes_separator(l: &str) -> bool {
    let l = l.trim();
    l.len() >= 20 && l.bytes().all(|b| b == b'-')
}

/// One bullet list per upload: the entries `osc vc` added to `current` since
/// `committed` (the `.changes` OBS has) become the newest entry alone, headed by
/// its header, with the bullets of all of them, oldest first. `None` when there
/// is nothing to merge: fewer than two new entries, or `current` does not end
/// with the committed text (somebody edited the old entries).
fn merge_pending_changes(current: &str, committed: &str) -> Option<String> {
    let new = current.strip_suffix(committed)?;
    let mut blocks: Vec<(String, Vec<&str>)> = Vec::new();
    for line in new.lines() {
        if is_changes_separator(line) {
            blocks.push((String::new(), Vec::new()));
        } else if let Some((header, body)) = blocks.last_mut() {
            if header.is_empty() {
                *header = line.to_string();
            } else {
                body.push(line);
            }
        } else if !line.trim().is_empty() {
            return None;
        }
    }
    if blocks.len() < 2 {
        return None;
    }
    let mut bullets: Vec<String> = Vec::new();
    for (_, body) in blocks.iter().rev() {
        let text = body.join("\n");
        let text = text.trim_matches('\n').trim_end();
        if !text.is_empty() && !bullets.iter().any(|b| b == text) {
            bullets.push(text.to_string());
        }
    }
    let mut out = format!(
        "{}\n{}\n\n{}\n\n",
        "-".repeat(67),
        blocks[0].0,
        bullets.join("\n")
    );
    out.push_str(committed);
    Some(out)
}

/// [`merge_pending_changes`] on the checkout's `.changes`, against the copy OBS
/// has (`.osc/sources/`; none for a file not committed yet). Best effort: a
/// file that cannot be read or written is left as `osc vc` made it.
fn merge_pending_changes_file(checkout_dir: &Path) {
    let Ok(rd) = fs::read_dir(checkout_dir) else {
        return;
    };
    let mut files: Vec<PathBuf> = rd
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "changes"))
        .collect();
    files.sort();
    let Some(file) = files.first() else {
        return;
    };
    let Ok(current) = fs::read_to_string(file) else {
        return;
    };
    let committed = file
        .file_name()
        .and_then(|n| fs::read_to_string(checkout_dir.join(".osc/sources").join(n)).ok())
        .unwrap_or_default();
    // No copy of what OBS holds although OBS tracks the file: every entry would
    // look new and the history would be merged into one.
    if committed.is_empty()
        && file
            .file_name()
            .is_some_and(|n| tracked_files(checkout_dir).contains(&n.to_string_lossy().to_string()))
    {
        return;
    }
    if let Some(merged) = merge_pending_changes(&current, &committed) {
        let _ = fs::write(file, merged);
    }
}

/// `working` without its newest entry, when that entry is not in `pristine`
/// (what OBS holds) and records an `- Update to <other version>` that is not
/// the `message` about to be recorded. `None` when there is nothing to drop.
fn without_unpublished_update_entry(
    working: &str,
    pristine: &str,
    message: &str,
) -> Option<String> {
    if !message.starts_with("Update to ") {
        return None;
    }
    let is_separator = |l: &str| l.len() >= 20 && l.bytes().all(|b| b == b'-');
    let lines: Vec<&str> = working.lines().collect();
    let first = lines.iter().position(|l| is_separator(l.trim()))?;
    let next = lines[first + 1..]
        .iter()
        .position(|l| is_separator(l.trim()))
        .map(|i| first + 1 + i)?;
    let entry = lines[first..next].join("\n");
    let says_other_update = lines[first..next].iter().any(|l| {
        let t = l.trim().trim_start_matches(['-', '*', ' ']);
        t.starts_with("Update to ") && t != message
    });
    if !says_other_update || pristine.contains(entry.trim_end()) {
        return None;
    }
    let mut out = lines[..first].join("\n");
    if !out.is_empty() {
        out.push('\n');
    }
    out.push_str(&lines[next..].join("\n"));
    out.push('\n');
    Some(out)
}

/// Longest line of a `.changes` entry the style checks accept.
const CHANGES_WIDTH: usize = 67;

/// A long `- Did this and did that` bullet as two bullets, when what follows
/// the ` and ` starts with a verb of its own (a second change, not the second
/// half of one object: "Drop a and b" stays whole).
fn split_bullet(line: &str) -> Option<[String; 2]> {
    const VERBS: &[&str] = &[
        "add", "drop", "remove", "fix", "use", "switch", "enable", "disable", "install", "move",
        "replace", "require", "build", "ship", "split", "rename", "update", "set", "pass", "patch",
        "keep", "stop", "skip", "make", "list", "own", "package", "provide",
    ];
    let lead = line.len() - line.trim_start().len();
    let rest = &line[lead..];
    let text = rest.strip_prefix("- ")?;
    let (first, second) = text.split_once(" and ")?;
    let (verb, _) = second.split_once(' ')?;
    if !VERBS.contains(&verb.to_ascii_lowercase().as_str()) || first.split_whitespace().count() < 2
    {
        return None;
    }
    let mut cap = second.chars();
    let second: String = cap.next()?.to_uppercase().chain(cap).collect();
    let first = first.trim_end_matches(',');
    let pad = &line[..lead];
    Some([format!("{pad}- {first}"), format!("{pad}- {second}")])
}

/// `line` broken at word boundaries into lines of at most [`CHANGES_WIDTH`]
/// characters, each continuation indented to sit under the text of a bullet
/// (`- text`, `  * text`) or under the line it continues.
fn wrap_changes_line(line: &str) -> Vec<String> {
    if line.chars().count() <= CHANGES_WIDTH {
        return vec![line.to_string()];
    }
    if let Some(parts) = split_bullet(line) {
        return parts.iter().flat_map(|p| wrap_changes_line(p)).collect();
    }
    let lead = line.len() - line.trim_start().len();
    let rest = &line[lead..];
    let bullet = ["- ", "* "]
        .iter()
        .find(|b| rest.starts_with(**b))
        .map_or(0, |b| b.len());
    let first_prefix = &line[..lead + bullet];
    let indent = " ".repeat(lead + bullet);
    let mut out = Vec::new();
    let mut current = first_prefix.to_string();
    let mut empty = true;
    for word in rest[bullet..].split_whitespace() {
        let add = if empty { 0 } else { 1 } + word.chars().count();
        if !empty && current.chars().count() + add > CHANGES_WIDTH {
            out.push(std::mem::replace(&mut current, indent.clone()));
            empty = true;
        }
        if !empty {
            current.push(' ');
        }
        current.push_str(word);
        empty = false;
    }
    out.push(current);
    out
}

/// `working` with the lines of its newest entry wrapped, or `None` when that
/// entry is already short enough or is one OBS already holds (`pristine`):
/// a published entry is never rewritten.
fn wrapped_newest_entry(working: &str, pristine: &str) -> Option<String> {
    let is_separator = |l: &str| l.len() >= 20 && l.bytes().all(|b| b == b'-');
    let lines: Vec<&str> = working.lines().collect();
    let first = lines.iter().position(|l| is_separator(l.trim()))?;
    let end = lines[first + 1..]
        .iter()
        .position(|l| is_separator(l.trim()))
        .map_or(lines.len(), |i| first + 1 + i);
    // The header line (date - author) is the one after the separator.
    let body_from = (first + 2).min(end);
    if !lines[body_from..end]
        .iter()
        .any(|l| l.chars().count() > CHANGES_WIDTH)
    {
        return None;
    }
    let entry = lines[first..end].join("\n");
    if pristine.contains(entry.trim_end()) {
        return None;
    }
    let mut out: Vec<String> = lines[..body_from].iter().map(|l| l.to_string()).collect();
    for l in &lines[body_from..end] {
        out.extend(wrap_changes_line(l));
    }
    out.extend(lines[end..].iter().map(|l| l.to_string()));
    let mut text = out.join("\n");
    text.push('\n');
    Some(text)
}

/// Wrap the long lines of the `.changes` entry this run wrote (one OBS does
/// not hold yet) so the package passes the changelog style checks.
pub fn wrap_unpublished_changes(checkout_dir: &Path) {
    let Ok(rd) = fs::read_dir(checkout_dir) else {
        return;
    };
    for file in rd
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "changes"))
    {
        let Some(name) = file.file_name() else {
            continue;
        };
        let pristine = fs::read_to_string(checkout_dir.join(".osc").join("sources").join(name))
            .unwrap_or_default();
        if let Ok(working) = fs::read_to_string(&file)
            && let Some(wrapped) = wrapped_newest_entry(&working, &pristine)
        {
            let _ = fs::write(&file, wrapped);
        }
    }
}

/// Whether the newest entry of a `.changes` file (the text between its first
/// two separator lines) has `message` as one of its lines, bullet or not.
fn newest_changes_entry_says(changes: &str, message: &str) -> bool {
    let is_separator = |l: &str| l.len() >= 20 && l.bytes().all(|b| b == b'-');
    let mut started = false;
    for line in changes.lines().map(str::trim) {
        if is_separator(line) {
            if started {
                return false;
            }
            started = true;
        } else if started && line.trim_start_matches(['-', '*', ' ']) == message {
            return true;
        }
    }
    false
}

/// The error `ensure_package` reports when the project listing cannot be read.
/// A transient failure keeps its classification so that the caller retries;
/// any other one stops the build, as it always has.
fn listing_failure_for_create(project: &str, e: Error) -> Error {
    if e.is_transient() {
        e
    } else {
        Error::NotActionable(format!("could not list packages of {project}: {e}"))
    }
}

/// Ensure the package exists in `project`, creating it if not.
///
/// Creates it by pushing package meta directly (`osc meta pkg ... -F -`,
/// which the server creates implicitly if it doesn't exist yet) rather than
/// `osc mkpac`. `mkpac` infers "already registered" from local working-copy
/// state — a stray directory left over from a previous, incomplete attempt,
/// or a project checkout that still remembers the package from one — and
/// happily reports success without the package ever having been created on
/// the server. That false success surfaces much later as a 404 on `osc ci`.
/// Pushing meta directly has no such local state to get confused by: the
/// server is the only thing that decides whether the package exists.
pub fn ensure_package(
    project: &str,
    pkg_name: &str,
    work_dir: &Path,
    emit: Emit<'_>,
) -> Result<(), Error> {
    check_obs_name("project", project)?;
    check_obs_name("package", pkg_name)?;
    // `osc ls project/pkg` exits non-zero when the package does not exist.
    let check = proc::capture_with_timeout(
        "osc",
        &["ls", &format!("{project}/{pkg_name}")],
        work_dir,
        emit,
        OSC_QUERY_TIMEOUT,
    )
    .map_err(during(format!("ls {project}/{pkg_name}")))?;

    if check.success() {
        return Ok(());
    }
    // Only a 404 means "not there yet". Any other failure (network, auth,
    // a 5xx) says nothing about existence, and pushing meta then could
    // overwrite the title/description of a package that is in fact there.
    if !is_not_found(&check.stderr) {
        return Err(Error::OscFailed {
            cmd: format!("ls {project}/{pkg_name}"),
            code: check.code,
            stderr: check.stderr,
        });
    }
    // OBS names are case-sensitive, so the failed lookup above does not rule
    // out a package that differs only in case or separators
    // (`python-apscheduler` vs. `python-APScheduler`). Creating one would
    // leave the project with two packages for one upstream, so refuse and let
    // the caller use the existing name.
    let listing =
        list_project_packages(project, emit).map_err(|e| listing_failure_for_create(project, e))?;
    if let Some(existing) = listing
        .iter()
        .filter(|p| fuzzy_key(p) == fuzzy_key(pkg_name))
        .min()
    {
        return Err(Error::NotActionable(format!(
            "{project} already has {existing}; refusing to create {pkg_name} next to it"
        )));
    }

    let meta = format!(
        "<package name=\"{}\" project=\"{}\">\n  \
         <title/>\n  <description/>\n</package>\n",
        crate::obs_xml::escape(pkg_name),
        crate::obs_xml::escape(project)
    );
    // A fresh, uniquely named file in the private directory: a name built
    // from the package in the work directory could collide with a concurrent
    // run or an existing file. Removed when `meta_file` drops.
    let write_meta = || -> std::io::Result<tempfile::NamedTempFile> {
        let mut file = tempfile::Builder::new()
            .prefix("meta-")
            .suffix(".xml")
            .tempfile_in(proc::private_dir()?)?;
        std::io::Write::write_all(&mut file, meta.as_bytes())?;
        Ok(file)
    };
    let meta_file = write_meta().map_err(|e| Error::OscFailed {
        cmd: format!("meta pkg {project} {pkg_name} (writing temp meta file)"),
        code: -1,
        stderr: e.to_string(),
    })?;

    let out = proc::capture_with_timeout(
        "osc",
        &[
            "meta",
            "pkg",
            project,
            pkg_name,
            "-F",
            &meta_file.path().to_string_lossy(),
        ],
        work_dir,
        emit,
        OSC_QUERY_TIMEOUT,
    );
    drop(meta_file);
    let out = out.map_err(during(format!("meta pkg {project} {pkg_name}")))?;

    if !out.success() {
        return Err(Error::OscFailed {
            cmd: format!("meta pkg {project} {pkg_name} -F -"),
            code: out.code,
            stderr: out.stderr,
        });
    }

    Ok(())
}

/// What an osc failure says, for a message: an uncaught Python exception is
/// reduced to its last lines (the traceback names osc's own source, not the
/// cause), and a full disk is called what it is, since every package that
/// downloads or checks out then fails the same way.
fn osc_error_text(stderr: &str) -> String {
    let text = match stderr.find("Traceback (most recent call last):") {
        Some(at) => {
            let lines: Vec<&str> = stderr[at..]
                .lines()
                .filter(|l| !l.trim().is_empty())
                .collect();
            lines[lines.len().saturating_sub(2)..].join("\n")
        }
        None => stderr.trim_end().to_string(),
    };
    if child_output_says_disk_full(stderr) {
        format!("{text}\nthe disk is full: free space under the work directory and run again")
    } else {
        text
    }
}

/// Check out `project/pkg_name` into `into_dir`.
///
/// Returns the path of the checkout directory, which is
/// `into_dir/<project>/<pkg_name>/`.
pub fn checkout(
    project: &str,
    pkg_name: &str,
    into_dir: &Path,
    emit: Emit<'_>,
) -> Result<PathBuf, Error> {
    check_obs_name("project", project)?;
    check_obs_name("package", pkg_name)?;
    let checkout_dir = into_dir.join(project).join(pkg_name);

    // The decision below (discard an unfinished checkout, re-use or check out)
    // and the osc run it leads to hold this directory's lock, so a concurrent
    // call for the same package waits instead of renaming a checkout that is
    // still being written. Other packages are not held up. The process-wide
    // lock further down is taken inside this one, never the other way round.
    let dir_lock = checkout_dir_lock(&checkout_dir);
    let _dir_guard = dir_lock.lock().unwrap_or_else(|e| e.into_inner());

    // Moved-aside copies of an unfinished checkout are kept for a week, then
    // they go: each can hold a large partial download.
    prune_unfinished_asides(&checkout_dir, None, Some(UNFINISHED_ASIDE_MAX_AGE));

    // A checkout osc never finished (killed, or out of disk space mid-download)
    // tracks nothing, and `osc up` on it cannot recover: it trips over the files
    // the half-run already wrote. It is checked out again instead.
    discard_unfinished_checkout(&checkout_dir, emit);

    // Re-use an existing checkout rather than re-cloning every run.
    if checkout_dir.exists() {
        let out = osc_up_recovering(&checkout_dir, emit)?;

        if !out.success() {
            // 404 means `ensure_package`'s meta push hasn't propagated to
            // this checkout's view of the server yet — use the directory
            // as-is rather than failing on a lag that resolves itself.
            if is_not_found(&out.stderr) {
                return Ok(checkout_dir);
            }
            return Err(Error::OscFailed {
                cmd: format!("up {project}/{pkg_name} in {}", checkout_dir.display()),
                code: out.code,
                stderr: out.stderr,
            });
        }

        return Ok(checkout_dir);
    }

    // Every `osc co` into the same directory rewrites the shared
    // `<project>/.osc/_packages` file (write `.new`, rename). With several
    // packages checking out in parallel (`--jobs`, or roots added from the
    // TUI) the renames race and one fails with FileNotFoundError, so
    // serialise checkouts process-wide.
    static CHECKOUT_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _guard = CHECKOUT_LOCK.lock().unwrap_or_else(|e| e.into_inner());

    let out = proc::capture_with_timeout(
        "osc",
        &["co", &format!("{project}/{pkg_name}")],
        into_dir,
        emit,
        OSC_TRANSFER_TIMEOUT,
    )
    .map_err(during(format!("co {project}/{pkg_name}")))?;

    if !out.success() {
        // Whatever osc wrote before it failed would make the next run's `osc up`
        // fail on it.
        discard_unfinished_checkout(&checkout_dir, emit);
        return Err(Error::OscFailed {
            cmd: format!("co {project}/{pkg_name}"),
            code: out.code,
            stderr: out.stderr,
        });
    }

    Ok(checkout_dir)
}

/// The lock for one checkout directory, created on first use and kept for the
/// life of the process (one per package checked out).
fn checkout_dir_lock(checkout_dir: &Path) -> Arc<Mutex<()>> {
    static LOCKS: OnceLock<Mutex<HashMap<PathBuf, Arc<Mutex<()>>>>> = OnceLock::new();
    LOCKS
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .entry(checkout_dir.to_path_buf())
        .or_insert_with(|| Arc::new(Mutex::new(())))
        .clone()
}

/// Remove `checkout_dir` when it is a checkout that was never completed: osc
/// leaves `.osc/_in_update` while it downloads and removes it at the end, and
/// `.osc/_files` lists nothing until the download has been recorded. A working
/// copy that tracks files was completed once and keeps its local changes.
///
/// Files in an unfinished checkout that nothing tracks may be local work (a
/// new package's spec or `.changes` never committed), so such a checkout is
/// moved aside to `<name>.unfinished-<millis>` next to it instead of deleted.
///
/// Only the newest moved-aside copy of a package is kept, and its `.osc`
/// metadata is dropped: the copy is no longer a checkout, so nothing scanning
/// for package directories (by `.osc`) picks it up.
fn discard_unfinished_checkout(checkout_dir: &Path, emit: Emit<'_>) {
    if !checkout_dir.join(".osc").join("_in_update").exists()
        || !tracked_files(checkout_dir).is_empty()
    {
        return;
    }
    if has_entries_besides_osc(checkout_dir) {
        let aside = unfinished_aside_path(checkout_dir);
        match fs::rename(checkout_dir, &aside) {
            Ok(()) => {
                let osc_dir = aside.join(".osc");
                if let Err(e) = fs::remove_dir_all(&osc_dir) {
                    ap_core::diag!("  could not remove {}: {e}", osc_dir.display());
                }
                prune_unfinished_asides(checkout_dir, Some(&aside), None);
                emit.reporter.report(&Event::Warning {
                    step: emit.step,
                    message: format!(
                        "{} is an unfinished checkout with untracked files; moved to {} and checking it out again",
                        checkout_dir.display(),
                        aside.display()
                    ),
                });
            }
            Err(e) => ap_core::diag!("  could not move {} aside: {e}", checkout_dir.display()),
        }
        return;
    }
    match fs::remove_dir_all(checkout_dir) {
        Ok(()) => emit.reporter.report(&Event::Warning {
            step: emit.step,
            message: format!(
                "{} is the remains of a checkout osc never finished; checking it out again",
                checkout_dir.display()
            ),
        }),
        Err(e) => ap_core::diag!("  could not remove {}: {e}", checkout_dir.display()),
    }
}

/// Whether `checkout_dir` holds anything but its `.osc` metadata directory.
fn has_entries_besides_osc(checkout_dir: &Path) -> bool {
    fs::read_dir(checkout_dir).is_ok_and(|entries| {
        entries
            .flatten()
            .any(|e| e.file_name() != std::ffi::OsStr::new(".osc"))
    })
}

/// How long a moved-aside unfinished checkout is kept.
const UNFINISHED_ASIDE_MAX_AGE: Duration = Duration::from_secs(7 * 24 * 60 * 60);

/// Delete the moved-aside copies of `checkout_dir` (`<name>.unfinished-*`
/// siblings), except `keep`. With `max_age`, only copies last modified longer
/// ago than that are deleted.
fn prune_unfinished_asides(checkout_dir: &Path, keep: Option<&Path>, max_age: Option<Duration>) {
    let (Some(parent), Some(name)) = (checkout_dir.parent(), checkout_dir.file_name()) else {
        return;
    };
    let prefix = format!("{}.unfinished-", name.to_string_lossy());
    let Ok(rd) = fs::read_dir(parent) else {
        return;
    };
    for entry in rd.flatten() {
        let path = entry.path();
        if !entry.file_name().to_string_lossy().starts_with(&prefix)
            || keep.is_some_and(|k| k == path)
            || !entry.file_type().is_ok_and(|t| t.is_dir())
        {
            continue;
        }
        if let Some(max_age) = max_age {
            let old_enough = entry
                .metadata()
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| std::time::SystemTime::now().duration_since(t).ok())
                .is_some_and(|age| age > max_age);
            if !old_enough {
                continue;
            }
        }
        if let Err(e) = fs::remove_dir_all(&path) {
            ap_core::diag!("  could not remove {}: {e}", path.display());
        }
    }
}

/// A free sibling path `<checkout_dir>.unfinished-<millis>` (suffixed with a
/// counter if that name is taken).
fn unfinished_aside_path(checkout_dir: &Path) -> PathBuf {
    let name = checkout_dir
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned();
    let millis = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis());
    let parent = checkout_dir.parent().unwrap_or(Path::new("."));
    let mut candidate = parent.join(format!("{name}.unfinished-{millis}"));
    let mut n = 1;
    while candidate.exists() {
        candidate = parent.join(format!("{name}.unfinished-{millis}-{n}"));
        n += 1;
    }
    candidate
}

/// Run `osc up` from `checkout_dir` to update the working copy.
///
/// Non-fatal: an already-up-to-date or brand-new checkout exits non-zero; we
/// ignore those cleanly.
pub fn pull(checkout_dir: &Path, emit: Emit<'_>) -> Result<(), Error> {
    let out = osc_up_recovering(checkout_dir, emit)?;
    if !out.success() {
        // 404 means `ensure_package`'s meta push hasn't propagated to this
        // checkout's view of the server yet — the directory is valid,
        // nothing to pull.
        if !is_benign_up_output(&out.stdout)
            && !is_benign_up_output(&out.stderr)
            && !is_not_found(&out.stdout)
            && !is_not_found(&out.stderr)
        {
            return Err(Error::OscFailed {
                cmd: format!("up in {}", checkout_dir.display()),
                code: out.code,
                stderr: out.stderr,
            });
        }
    }
    Ok(())
}

/// Whether `text` is osc's own "nothing to do" report from `osc up`: a whole
/// line that is `nothing to update` or `At revision <n>.`. Matching a line
/// rather than a substring keeps a real failure that merely quotes those
/// words (a path, a package name, a traceback) an error.
fn is_benign_up_output(text: &str) -> bool {
    let mut lines = text
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .peekable();
    lines.peek().is_some()
        && lines.all(|l| {
            l == "nothing to update"
                || l.strip_prefix("At revision ")
                    .and_then(|r| r.strip_suffix('.'))
                    .is_some_and(|r| !r.is_empty() && r.bytes().all(|b| b.is_ascii_digit()))
        })
}

/// Run `osc up` in `checkout_dir`, recovering in-place from the two known
/// self-inflicted working-copy failure modes before handing the (possibly
/// still-failing) result back to the caller: a prior run that died
/// mid-`up`/`ci` leaving `.osc/` inconsistent ([`is_inconsistent_wc_error`],
/// fixed by [`repair_wc`]), and an untracked local file colliding with a
/// same-named file the server gained in the meantime
/// ([`changes_conflict_file`], fixed by moving the local one aside).
///
/// Shared by [`checkout`]'s reuse path and [`pull`] so both get the same
/// recovery — `checkout`'s "osc up" on an existing directory is exactly the
/// same command `pull` runs, and the same interrupted-run history that leaves
/// `.osc/` inconsistent or a stray untracked `.changes` file behind can be hit
/// from either entry point, not just the one a fix happened to land in first.
/// Returns the final `osc up`'s output either way — success or not — so each
/// caller can keep interpreting failure (which stderr patterns are benign,
/// whether 404 is a non-error) in its own, different way.
fn osc_up_recovering(checkout_dir: &Path, emit: Emit<'_>) -> Result<Captured, Error> {
    // Each recovery runs at most once per call, so a checkout that keeps
    // failing the same way is reported, not retried forever.
    let mut repaired = false;
    let mut moved_aside = false;
    let up = || {
        proc::capture_with_timeout("osc", &["up"], checkout_dir, emit, OSC_TRANSFER_TIMEOUT)
            .map_err(during(format!("up in {}", checkout_dir.display())))
    };
    let mut out = up()?;
    loop {
        if out.success() {
            return Ok(out);
        }
        if !repaired && is_inconsistent_wc_error(&out.stderr) {
            repair_wc(checkout_dir, emit)?;
            repaired = true;
        } else if !moved_aside && let Some(name) = changes_conflict_file(&out.stderr) {
            // osc only raises this when `name` is untracked in this working
            // copy's `.osc/_files` (see `Package.__update`'s check against
            // `filenamelist_unvers`), so it is always safe to move aside — it
            // was never osc's file to begin with. The usual cause here is our
            // own `create_initial_changes` writing `<pkg>.changes` directly
            // before that revision's commit reached the server, and the
            // server later gaining its own file of the same name in the
            // meantime (e.g. a prior interrupted run that got further).
            // If the move fails the checkout is not usable: that is the
            // error to report, not a retry that will hit the same conflict.
            let backup = move_aside(checkout_dir, name)?;
            emit.reporter.report(&Event::Warning {
                step: emit.step,
                message: format!(
                    "osc up: untracked local '{name}' conflicted with a newer revision's \
                     file of the same name; moved it aside as '{}' and retrying",
                    backup.display()
                ),
            });
            moved_aside = true;
        } else {
            return Ok(out);
        }
        out = up()?;
    }
}

/// Move the untracked `name` out of `checkout_dir` so `osc up` can add the
/// server's file in its place, and return where it went.
///
/// The backup goes beside the checkout (`<project>/.preexisting/<pkg>/`), not
/// inside it: `osc addremove` runs on the package directory and would commit
/// a backup left there. A backup already present is never overwritten (a
/// checkout can go through this more than once), so each one gets a numbered
/// name instead.
fn move_aside(checkout_dir: &Path, name: &str) -> Result<PathBuf, Error> {
    let pkg = checkout_dir.file_name().unwrap_or(checkout_dir.as_os_str());
    let dir = checkout_dir
        .parent()
        .unwrap_or(checkout_dir)
        .join(".preexisting")
        .join(pkg);
    fs::create_dir_all(&dir)?;
    let mut backup = dir.join(name);
    let mut n = 1;
    while backup.exists() {
        n += 1;
        backup = dir.join(format!("{name}.{n}"));
    }
    fs::rename(checkout_dir.join(name), &backup)?;
    Ok(backup)
}

/// Extract the filename osc names in its uncaught `PackageFileConflict`
/// message from `Package.__update()` (`osc/obs_scm/package.py`): `failed to
/// add file '<name>' file/dir with the same name already exists`. Raised by
/// `osc up` when a file of that name sits on disk but untracked in this
/// working copy while the server's newer revision also adds a file with the
/// same name — an unhandled exception in osc itself, not one of its usual
/// "inconsistent working copy" messages, so `repairwc` does not apply here.
///
/// Only a `.changes` file (a bare name, not a path) is returned: that is the
/// one file this tool writes into a checkout itself, so the only conflict it
/// is safe to resolve by moving the local copy aside. Any other conflict is
/// left for the caller to report.
fn changes_conflict_file(stderr: &str) -> Option<&str> {
    let name = stderr
        .split("failed to add file '")
        .nth(1)?
        .split('\'')
        .next()?;
    (name.ends_with(".changes")
        && !name.contains('/')
        && !name.contains("..")
        && !name.starts_with('-'))
    .then_some(name)
}

#[cfg(test)]
mod failing_result_tests;

#[cfg(test)]
mod respelling_tests;

#[cfg(test)]
mod osc_error_text_tests;

#[cfg(test)]
mod unfinished_checkout_tests;

#[cfg(test)]
mod sandbox_spec_dir_tests;

#[cfg(test)]
mod xml_attr_and_status_tests;

#[cfg(test)]
mod changes_conflict_tests;

#[cfg(test)]
mod not_found_tests;

/// Return true if `checkout_dir`'s working copy has uncommitted local
/// changes (e.g. a spec version bump from a run that crashed before `osc
/// commit`). `osc status` prints one line per changed file and nothing when
/// the working copy is clean.
pub fn has_local_changes(checkout_dir: &Path, emit: Emit<'_>) -> bool {
    let Ok(out) = proc::capture("osc", &["status"], checkout_dir, emit) else {
        return true;
    };
    if out.success() {
        return !out.stdout.trim().is_empty();
    }
    if is_inconsistent_wc_error(&out.stderr) && repair_wc(checkout_dir, emit).is_ok() {
        return proc::capture("osc", &["status"], checkout_dir, emit)
            .map(|out| !out.stdout.trim().is_empty())
            .unwrap_or(true);
    }
    // `osc status` failed for a reason other than the one recognized,
    // repairable case (a network/server hiccup, say), or `repair_wc` itself
    // failed — either way `out.stdout` is from a failed invocation and
    // almost certainly empty, which must not read as an actually-clean
    // checkout ("no local changes"). The caller
    // uses this to gate skipping a rebuild (see the `AlreadyBuilt` fast path
    // in pipeline.rs), so treat "couldn't tell" as "has changes" — silently
    // skipping a needed rebuild is far worse than running an unnecessary one.
    true
}

/// Whether an `osc` command failed because the working copy's `.osc/`
/// metadata is inconsistent — the message `osc` itself prints when a prior
/// run died mid-`up`/`ci` and left it half-written, naming `osc repairwc .`
/// as the fix.
fn is_inconsistent_wc_error(stderr: &str) -> bool {
    stderr.contains("inconsistent state") || stderr.contains("repairwc")
}

/// Run `osc repairwc .` to fix a working copy `osc` itself reports as
/// inconsistent (see [`is_inconsistent_wc_error`]). Note this can remove
/// files from `.osc/` — exactly what `osc`'s own error message warns about —
/// so it is only ever called after that specific error, not speculatively.
pub fn repair_wc(checkout_dir: &Path, emit: Emit<'_>) -> Result<(), Error> {
    let out = proc::capture("osc", &["repairwc", "."], checkout_dir, emit)
        .map_err(during(format!("repairwc in {}", checkout_dir.display())))?;
    if !out.success() {
        return Err(Error::OscFailed {
            cmd: format!("repairwc in {}", checkout_dir.display()),
            code: out.code,
            stderr: out.stderr,
        });
    }
    Ok(())
}

/// Services a checkout's `_service` may name. The agent can edit the file, and
/// a service runs arbitrary code on this machine (`refresh_obs_scm` etc. would
/// too), so anything outside the set the pipeline and OBS packaging use is
/// refused.
const ALLOWED_SERVICES: &[&str] = &[
    "tar_scm",
    "obs_scm",
    "recompress",
    "set_version",
    "cargo_vendor",
    "node_modules",
    "download_files",
    "go_modules",
    "format_spec_file",
];

/// Parameters those services take. Notably absent are anything that names a
/// command or an output directory.
const ALLOWED_SERVICE_PARAMS: &[&str] = &[
    "url",
    "scm",
    "revision",
    "versionformat",
    "versionprefix",
    "match-tag",
    "versionrewrite-pattern",
    "versionrewrite-replacement",
    "changesgenerate",
    "changesauthor",
    "subdir",
    "filename",
    "file",
    "compression",
    "src",
    "update",
    "method",
    "cargotoml",
    "no-root-manifest",
    "version",
    "basename",
    "without-version",
    "protocol",
    "host",
    "path",
    "archive",
    "disablereview",
    "strategy",
    // tar_scm / obs_scm
    "exclude",
    "include",
    "extract",
    "submodules",
    "depth",
    "history-depth",
    "parent-tag",
    "package-meta",
    // cargo_vendor
    "versioned-dirs",
    "respect-lockfile",
    // node_modules
    "input",
    "node-dir",
    "omit-optional",
    "legacy-container",
    "cpio",
    "output",
    "source-offset",
    // download_files
    "enforceupstream",
    "enforcehash",
    // set_version
    "regex",
    "parse-version",
];

/// Params whose value is a path relative to the checkout.
const SERVICE_PATH_PARAMS: &[&str] = &[
    "subdir",
    "filename",
    "file",
    "src",
    "cargotoml",
    "archive",
    "exclude",
    "include",
    "extract",
    "input",
    "node-dir",
    "cpio",
    "output",
];

/// Check one `<param>` against the allowlists. `value` is the decoded text,
/// as osc will see it.
fn validate_service_param(name: &str, value: &str) -> Result<(), String> {
    let value = value.trim();
    if !ALLOWED_SERVICE_PARAMS.contains(&name) {
        return Err(format!("param `{name}` is not allowed"));
    }
    if name == "url" {
        crate::service_sandbox::check_service_url(value)?;
    }
    if name == "protocol" && value != "https" {
        return Err(format!("protocol `{value}` must be https"));
    }
    if name == "revision" && value.starts_with('-') {
        return Err(format!("revision `{value}` starts with `-`"));
    }
    if SERVICE_PATH_PARAMS.contains(&name)
        && (value.starts_with('/') || value.split('/').any(|c| c == ".."))
    {
        return Err(format!("{name} `{value}` leaves the checkout"));
    }
    Ok(())
}

/// Check a `_service` file against the allowlists. `Err` names the problem.
///
/// The file is parsed as XML and the checks run on decoded values, since osc
/// decodes character references (`&#46;&#46;/`) before using them. Anything
/// the parser cannot read, any DOCTYPE, processing instruction or entity
/// other than the five predefined ones is refused.
#[cfg(test)]
pub(crate) fn validate_service_xml(xml: &[u8]) -> Result<(), String> {
    parse_service_xml(xml).map(|_| ())
}

/// [`validate_service_xml`], returning the services in order with their
/// decoded (trimmed) params: what is run is what was checked.
pub(crate) fn parse_service_xml(
    xml: &[u8],
) -> Result<Vec<crate::service_sandbox::ServiceCall>, String> {
    use quick_xml::Reader;
    use quick_xml::events::Event;

    let xml = std::str::from_utf8(xml).map_err(|_| "not valid UTF-8".to_string())?;
    let mut reader = Reader::from_str(xml);
    // Open elements, and the name and text so far of the <param> being read.
    let mut depth: Vec<String> = Vec::new();
    let mut param: Option<(String, String)> = None;
    let mut seen_root = false;
    let mut services: Vec<crate::service_sandbox::ServiceCall> = Vec::new();
    loop {
        let event = reader
            .read_event()
            .map_err(|e| format!("malformed XML: {e}"))?;
        let (start, empty) = match &event {
            Event::Start(e) => (Some(e), false),
            Event::Empty(e) => (Some(e), true),
            _ => (None, false),
        };
        if let Some(e) = start {
            let tag = e.name().as_ref().to_string();
            let parent = depth.last().map(String::as_str);
            let ok = match (tag.as_str(), parent) {
                ("services", None) => !std::mem::replace(&mut seen_root, true),
                ("service", Some("services")) | ("param", Some("service")) => true,
                _ => false,
            };
            if !ok {
                return Err(format!("unexpected markup `<{tag}`"));
            }
            let mut name = None;
            for attr in e.attributes() {
                let attr = attr.map_err(|e| format!("malformed attribute: {e}"))?;
                if attr.key.as_ref() == "name" {
                    let v = attr
                        .normalized_value(quick_xml::XmlVersion::Implicit1_0)
                        .map_err(|e| format!("malformed attribute: {e}"))?;
                    name = Some(v.into_owned());
                }
            }
            match tag.as_str() {
                "service" => {
                    let name = name.ok_or("a <service> without a name")?;
                    if !ALLOWED_SERVICES.contains(&name.as_str()) {
                        return Err(format!("service `{name}` is not allowed"));
                    }
                    services.push(crate::service_sandbox::ServiceCall {
                        name,
                        params: Vec::new(),
                    });
                }
                "param" => {
                    let name = name.ok_or("a <param> without a name")?;
                    if empty {
                        validate_service_param(&name, "")?;
                        if let Some(s) = services.last_mut() {
                            s.params.push((name, String::new()));
                        }
                    } else {
                        param = Some((name, String::new()));
                    }
                }
                _ => {}
            }
            if !empty {
                depth.push(tag);
            }
            continue;
        }
        match event {
            Event::End(_) => {
                if depth.pop().as_deref() == Some("param")
                    && let Some((name, value)) = param.take()
                {
                    validate_service_param(&name, &value)?;
                    if let Some(s) = services.last_mut() {
                        s.params.push((name, value.trim().to_string()));
                    }
                }
            }
            Event::Text(t) => {
                if let Some((_, value)) = param.as_mut() {
                    value.push_str(&t);
                }
            }
            Event::CData(t) => {
                if let Some((_, value)) = param.as_mut() {
                    value.push_str(&t);
                }
            }
            Event::GeneralRef(r) => {
                let ch = match r.resolve_char_ref() {
                    Ok(Some(c)) => c,
                    Ok(None) => match &*r {
                        "lt" => '<',
                        "gt" => '>',
                        "amp" => '&',
                        "apos" => '\'',
                        "quot" => '"',
                        other => return Err(format!("entity `&{other};` is not allowed")),
                    },
                    Err(e) => return Err(format!("malformed reference: {e}")),
                };
                if let Some((_, value)) = param.as_mut() {
                    value.push(ch);
                }
            }
            Event::DocType(_) => return Err("a DOCTYPE is not allowed".to_string()),
            Event::PI(_) => return Err("processing instructions are not allowed".to_string()),
            Event::Eof => break,
            _ => {}
        }
    }
    if !depth.is_empty() {
        return Err("unclosed element".to_string());
    }
    Ok(services)
}

/// Whether a package that is blocked now, after `previous` was the status
/// label, has made progress worth restarting the stall timer for. A flip back
/// from `stale: ...` (the cross-check reading an older result) is not progress:
/// dtc sat for 1.5 hours flipping between `blocked` and `stale: outdated`, and
/// every flip restarted the timer so the stall limit never came.
fn resets_block_timer(previous: &str) -> bool {
    !previous.starts_with("stale")
}

/// Run the checkout's services, as `osc service ra` would, to fetch sources and
/// vendor Cargo deps.
///
/// The `_service` file is validated first (the agent may have edited it) and
/// each service runs in a sandbox without the home directory, the session bus
/// or OBS credentials, its network limited to the source hosts (see
/// [`crate::service_sandbox`]). Only `AUTO_PACKAGE_UNSAFE_SERVICE=1` runs the
/// unsandboxed `osc service ra` instead.
pub fn run_service(checkout_dir: &Path, emit: Emit<'_>) -> Result<(), Error> {
    // Fail closed: only a missing file skips validation.
    let services = match fs::read(checkout_dir.join("_service")) {
        Ok(xml) => parse_service_xml(&xml).map_err(|reason| Error::ServiceRejected { reason })?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(e) => {
            return Err(Error::ServiceRejected {
                reason: format!("cannot read _service: {e}"),
            });
        }
    };
    let out = if !crate::service_sandbox::unsafe_mode() {
        crate::service_sandbox::run_services(checkout_dir, &services, emit)?
    } else {
        proc::capture_with_timeout_env(
            "osc",
            &["service", "ra"],
            checkout_dir,
            emit,
            proc::DEFAULT_CAPTURE_TIMEOUT,
            // osc reads its credentials from the desktop keyring at start-up,
            // which it reaches through the session bus: without the address it
            // dies with "Unable to initialize SecretService" before any
            // service runs.
            Some(&proc::minimal_env(&["DBUS_SESSION_BUS_ADDRESS"])),
        )
        .map_err(during(format!("service ra in {}", checkout_dir.display())))?
    };
    if !out.success() {
        // `osc service` reports a failing service on stdout and leaves stderr
        // empty, which made the failure read as "failed (exit 1):" and nothing.
        let stderr = if out.stderr.trim().is_empty() {
            out.stdout
        } else {
            out.stderr
        };
        // gpg prints one `aka` line per user id of the signing key, which
        // buries the line that says what failed.
        let stderr = stderr
            .lines()
            .filter(|l| !l.starts_with("gpg:") || !l.contains(" aka "))
            .collect::<Vec<_>>()
            .join("\n");
        return Err(Error::OscFailed {
            cmd: format!("service ra in {}", checkout_dir.display()),
            code: out.code,
            stderr,
        });
    }
    collapse_generated_changes(checkout_dir);
    Ok(())
}

/// A `tar_scm` service with `changesgenerate` writes one entry listing every
/// upstream commit since the last release (a hundred bullets for a package
/// that was a few releases behind). That entry is cut down to `- Update to
/// <version>`, which is all a packaging changelog needs to say.
fn collapse_generated_changes(checkout_dir: &Path) {
    let Ok(rd) = fs::read_dir(checkout_dir) else {
        return;
    };
    for file in rd
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "changes"))
    {
        let Some(name) = file.file_name() else {
            continue;
        };
        let pristine = fs::read_to_string(checkout_dir.join(".osc").join("sources").join(name))
            .unwrap_or_default();
        if let Ok(working) = fs::read_to_string(&file)
            && let Some(collapsed) = collapsed_generated_entry(&working, &pristine)
        {
            let _ = fs::write(&file, collapsed);
        }
    }
}

fn collapsed_generated_entry(working: &str, pristine: &str) -> Option<String> {
    let is_separator = |l: &str| l.len() >= 20 && l.bytes().all(|b| b == b'-');
    let lines: Vec<&str> = working.lines().collect();
    let first = lines.iter().position(|l| is_separator(l.trim()))?;
    let end = lines[first + 1..]
        .iter()
        .position(|l| is_separator(l.trim()))
        .map_or(lines.len(), |i| first + 1 + i);
    let entry = lines[first..end].join("\n");
    if pristine.contains(entry.trim_end()) {
        return None;
    }
    let body_from = (first + 2).min(end);
    let head = lines[body_from..end]
        .iter()
        .find(|l| l.trim_start().starts_with("- Update to version "))?;
    let version = head
        .trim_start()
        .trim_start_matches("- Update to version ")
        .trim_end_matches(':')
        .trim();
    let sub_bullets = lines[body_from..end]
        .iter()
        .filter(|l| l.starts_with("  * ") || l.starts_with("  - "))
        .count();
    if version.is_empty() || sub_bullets < 5 {
        return None;
    }
    let mut out: Vec<String> = lines[..body_from].iter().map(|l| l.to_string()).collect();
    out.push(format!("- Update to {version}"));
    out.push(String::new());
    out.extend(lines[end..].iter().map(|l| l.to_string()));
    let mut text = out.join("\n");
    text.push('\n');
    Some(text)
}

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
fn code_macro_marker(text: &str) -> Option<&'static str> {
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
fn include_marker(text: &str) -> Option<&'static str> {
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
fn unsandboxed_marker(text: &str) -> Option<&'static str> {
    code_macro_marker(text).or_else(|| include_marker(text))
}

/// Whether `rpmspec` can run inside bubblewrap here. Probed once: `bwrap`
/// installed is not enough, unprivileged user namespaces are often disabled.
fn rpmspec_sandbox_works() -> bool {
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
fn sandbox_spec_dir(spec_path: &Path, cwd: &Path) -> PathBuf {
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
/// pre-flight syntax check; the one-shot [`emit_host_macro_warning`] call
/// below surfaces the gap to the user. A durable fix would require
/// containerized validation against the target repo's rpm-config.
///
/// Expanding a spec can run code (`%(...)`, `%{lua:...}`), so it runs inside
/// bubblewrap when that works. Without a sandbox, a spec containing such a
/// macro is refused rather than expanded on the host.
pub fn rpmspec_check(spec_path: &Path, emit: Emit<'_>) -> Result<String, Error> {
    emit_host_macro_warning(emit);

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

    let spec_text: String = names
        .iter()
        .filter(|n| n.ends_with(".spec"))
        .filter_map(|n| fs::read_to_string(checkout_dir.join(n)).ok())
        .collect();
    let sources = spec_source_names(&spec_text);
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
        if is_publishable(&name, &spec_text, &series, &sources) {
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
fn spec_source_names(spec_text: &str) -> Vec<String> {
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

/// Whether `name` is the file a source `pattern` stands for. Each macro in the
/// pattern (`%{version}`, `%version`) matches any text.
fn matches_source_name(pattern: &str, name: &str) -> bool {
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

/// Whether `name` is a build-recipe file (`_multibuild`, `_constraints`) that the
/// spec has no use for. `_multibuild` declares flavors, which only a spec that
/// reads `%flavor` builds differently. `_constraints` asks OBS for build
/// resources: a pure-Python (`BuildArch: noarch`) spec that does not mention
/// constraints or `%limit_build` compiles nothing that needs them, so one
/// inherited from a Factory recipe of another shape is a leftover. A spec that
/// builds native code keeps it: the agent writes it without touching the spec.
fn recipe_file_unused(name: &str, spec_text: &str) -> bool {
    match name {
        "_multibuild" => !["%{flavor}", "%flavor", "%{?flavor", "%{!?flavor"]
            .iter()
            .any(|m| spec_text.contains(m)),
        "_constraints" => {
            // Only the main package's own `BuildArch:` counts: a `noarch`
            // subpackage (mariadb's `-test`, a `-doc`) says nothing about
            // the build that `_constraints` sizes.
            let noarch = spec_text
                .lines()
                .take_while(|l| {
                    let l = l.trim_start();
                    !(l.starts_with("%package") || l.starts_with("%description"))
                })
                .any(|l| {
                    let l = l.trim();
                    l.get(..9)
                        .is_some_and(|p| p.eq_ignore_ascii_case("buildarch"))
                        && l.ends_with("noarch")
                });
            noarch && !spec_text.contains("constraints") && !spec_text.contains("%limit_build")
        }
        _ => false,
    }
}

/// The `project`/`package` a checkout's `_link` points at (`osc branch` makes
/// one), when it has one.
fn link_target(checkout_dir: &Path) -> Option<(String, String)> {
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
        if checkout_dir.join(name).exists() || recipe_file_unused(name, &spec_text) {
            continue;
        }
        if let Some(text) = cat_file(&project, &package, name, emit)
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
                && recipe_file_unused(name, &spec_text)
                && fs::remove_file(checkout_dir.join(name)).is_ok()
        })
        .map(str::to_string)
        .collect()
}

fn is_publishable(name: &str, spec_text: &str, series: &[String], sources: &[String]) -> bool {
    // `_multibuild` declares flavors that only a spec which reads `%flavor` builds
    // differently. One left behind when a source recipe is swapped for a wheel
    // repackage (python-torch: `standard` and `openmpi4`) makes OBS build the same
    // spec once per flavor, so it goes, and is removed from OBS with the commit.
    if recipe_file_unused(name, spec_text) {
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
    const SUFFIXES: &[&str] = &[
        ".spec",
        ".changes",
        ".rpmlintrc",
        ".obsinfo",
        ".tar.gz",
        ".tar.bz2",
        ".tar.xz",
        ".tar.zst",
        ".tar",
        ".tgz",
        ".tbz",
        ".txz",
        ".zip",
        ".crate",
        // A prebuilt wheel used as the source, and the pristine copy of an
        // archive the pipeline rewrote (kept so its provenance is auditable).
        ".whl",
        // Detached signatures and keyrings that ship beside a source archive.
        ".sig",
        ".asc",
        ".keyring",
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
fn is_removed_rpmlintrc_source(line: &str, removed: &[String]) -> bool {
    crate::spec::parse_source_line(line, &["source"], str::to_string)
        .is_some_and(|source| removed.iter().any(|r| r == source.file()))
}

/// Delete zero-byte `*.rpmlintrc` files and the `SourceN:` lines of the spec
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
        if name.ends_with(".rpmlintrc")
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
fn tracked_files(checkout_dir: &Path) -> Vec<String> {
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
    let spec_text: String = fs::read_dir(checkout_dir)
        .into_iter()
        .flatten()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().ends_with(".spec"))
        .filter_map(|e| fs::read_to_string(e.path()).ok())
        .collect();
    // Only a name that is exact: a wildcard would also bring back the previous
    // release's archive, which a version update removes on purpose.
    let sources: Vec<String> = spec_source_names(&spec_text)
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

/// Archives in the checkout that [`publish_plan`] left out and OBS does not
/// track (an sdist fetched for a version that was then given up, the previous
/// release's tarball), deleted from the checkout so they do not pile up there
/// or show as added. Returns the names deleted.
pub fn discard_untracked_archives(checkout_dir: &Path, skipped: &[String]) -> Vec<String> {
    const ARCHIVES: &[&str] = &[
        ".tar.gz", ".tar.bz2", ".tar.xz", ".tar.zst", ".tar", ".tgz", ".tbz", ".txz", ".zip",
        ".crate", ".whl",
    ];
    let tracked = tracked_files(checkout_dir);
    let mut gone = Vec::new();
    for name in skipped {
        if tracked.contains(name)
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
/// pushed, even when it is modified.
pub fn commit(
    checkout_dir: &Path,
    message: &str,
    files: &[String],
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
    let out = proc::capture_with_timeout("osc", &ci, checkout_dir, emit, OSC_TRANSFER_TIMEOUT)
        .map_err(during(format!("ci in {}", checkout_dir.display())))?;

    if !out.success() {
        // "nothing to do" means files are already committed — not an error.
        if out.stdout.contains("nothing to do") || out.stderr.contains("nothing to do") {
            return Ok(());
        }
        return Err(Error::OscFailed {
            cmd: format!("ci -m {message:?} in {}", checkout_dir.display()),
            code: out.code,
            stderr: out.stderr,
        });
    }
    Ok(())
}

/// Commit message of the commit that puts a baseline back.
pub const RESTORE_MESSAGE: &str = "auto-package: restore baseline after failed run";

/// Keep in `dir` what OBS holds for `checkout_dir`, before the run first writes
/// to it: the names it tracks, and a copy of each one's pristine file (osc's
/// copy in `.osc/sources`, the revision the checkout was made or last
/// committed from). A tracked name without a pristine copy is listed anyway, so
/// the restore can say it cannot put that file back.
pub fn save_baseline(checkout_dir: &Path, dir: &Path) -> std::io::Result<()> {
    let saved = dir.join("files");
    fs::create_dir_all(&saved)?;
    let tracked = tracked_files(checkout_dir);
    for name in tracked
        .iter()
        .filter(|n| is_safe_file_arg(n) && !n.starts_with('.'))
    {
        match fs::copy(
            checkout_dir.join(".osc/sources").join(name),
            saved.join(name),
        ) {
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
    }
    fs::write(dir.join("tracked"), tracked.join("\n"))?;
    fs::write(
        dir.join("checkout"),
        checkout_dir.to_string_lossy().as_bytes(),
    )
}

/// Note in `dir` the files a commit is about to send to OBS, before it is made:
/// an attempt that fails half way may still have written them.
pub fn note_attempt(dir: &Path, files: &[String]) -> std::io::Result<()> {
    use std::io::Write;
    let mut list = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join("attempted"))?;
    for name in files {
        writeln!(list, "{name}")?;
    }
    Ok(())
}

/// The files the run noted as sent to OBS in `dir` (see [`note_attempt`]).
/// Empty when nothing was written.
pub fn attempted_files(dir: &Path) -> Vec<String> {
    fs::read_to_string(dir.join("attempted"))
        .unwrap_or_default()
        .lines()
        .filter(|l| !l.is_empty())
        .map(str::to_string)
        .collect()
}

/// Where the baselines of writes made outside a pipeline run are kept, under
/// the directory the checkouts are in (a leading dot: no OBS project has one).
pub const BASELINE_ROOT: &str = ".obs-baseline";

/// The file a baseline directory holds once a restore of it has failed: the
/// reason is written in it.
const UNRESTORED: &str = "unrestored";

/// Whether `dir` holds a baseline that is not yet put back: an attempt noted
/// in it (see [`note_attempt`]) has not been undone. A run or restore that
/// succeeds removes the directory, so this is only true while an attempt may
/// still be on OBS. It also holds after a restore that was cut short (the
/// process exited), when no `UNRESTORED` reason was written.
pub fn is_pending(dir: &Path) -> bool {
    !attempted_files(dir).is_empty() || dir.join(UNRESTORED).exists()
}

/// Keep the baseline of `checkout_dir` for a write made outside a pipeline run,
/// in `root/<branch>/<pkg>`. Returns that dir.
///
/// An earlier baseline that is pending (see [`is_pending`]) is restored first,
/// since a new baseline taken now would hold the failed revision. When that
/// restore fails the error is returned and the earlier baseline stays where it
/// is. Any other earlier baseline was undone or is no longer needed, and is
/// replaced.
pub fn begin_baseline(
    root: &Path,
    checkout_dir: &Path,
    branch: &str,
    pkg: &str,
    emit: Emit<'_>,
) -> Result<PathBuf, String> {
    let dir = root.join(ap_core::run::sanitize_name(branch)).join(pkg);
    if is_pending(&dir) {
        restore_baseline(&dir, emit).map_err(|why| {
            format!(
                "the OBS baseline kept at {} was NOT restored ({why}); \
                 it is kept there, and nothing was written. If OBS is right as it \
                 is, discard it with `auto-package baseline discard`",
                dir.display()
            )
        })?;
    }
    let _ = fs::remove_dir_all(&dir);
    save_baseline(checkout_dir, &dir).map_err(|e| e.to_string())?;
    Ok(dir)
}

/// The baseline directories of `pkg` under `root`, one per branch
/// (`root/<branch>/<pkg>`, see [`begin_baseline`]).
pub fn baselines_of(root: &Path, pkg: &str) -> Vec<PathBuf> {
    fs::read_dir(root)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path().join(pkg))
        .filter(|p| p.is_dir())
        .collect()
}

/// Drop the pending baseline in `dir` without putting it back: OBS is then
/// taken as it is. Returns `false` when `dir` holds no pending baseline.
pub fn discard_pending(dir: &Path) -> std::io::Result<bool> {
    if !is_pending(dir) {
        return Ok(false);
    }
    fs::remove_dir_all(dir)?;
    Ok(true)
}

/// Put the baseline saved in `dir` back into its checkout, and commit what the
/// attempts sent to OBS with `commit`, the OBS write (the real one is
/// [`commit`]; tests record the call). A tracked file gets its saved copy back.
/// A file the attempts added is removed from the checkout, and committed as a
/// deletion, only when OBS lists it now (`.osc/_files`, which osc updates on a
/// commit that lands): a file that never reached OBS has nothing to undo.
///
/// `obs_check` runs before anything is written, with the checkout: it refuses
/// (an error) when OBS is no longer where the checkout is (see
/// [`obs_still_at_checkout`]).
pub fn restore_baseline_with(
    dir: &Path,
    obs_check: impl FnOnce(&Path) -> Result<(), String>,
    commit: impl FnOnce(&Path, &str, &[String]) -> Result<(), Error>,
) -> Result<(), String> {
    let attempted = attempted_files(dir);
    if attempted.is_empty() {
        return Ok(());
    }
    let checkout = fs::read_to_string(dir.join("checkout"))
        .map(PathBuf::from)
        .map_err(|e| format!("the checkout the baseline was saved from is unknown: {e}"))?;
    let tracked = fs::read_to_string(dir.join("tracked")).unwrap_or_default();
    let on_obs = tracked_files(&checkout);
    if let Some(bad) = attempted.iter().find(|n| !is_safe_file_arg(n)) {
        return Err(format!(
            "refusing to restore {bad:?}: not a plain file name"
        ));
    }
    obs_check(&checkout)?;
    let mut sent: Vec<String> = Vec::new();
    for name in &attempted {
        if sent.contains(name) {
            continue;
        }
        let target = checkout.join(name);
        if tracked.lines().any(|t| t == name.as_str()) {
            let saved = fs::read(dir.join("files").join(name))
                .map_err(|e| format!("{name}: no saved copy of the baseline ({e})"))?;
            ap_core::fsx::write_atomic(&target, saved).map_err(|e| format!("{name}: {e}"))?;
        } else if on_obs.contains(name) {
            match fs::remove_file(&target) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(format!("{name}: {e}")),
            }
        } else {
            continue;
        }
        sent.push(name.clone());
    }
    if sent.is_empty() {
        return Ok(());
    }
    commit(&checkout, RESTORE_MESSAGE, &sent).map_err(|e| e.to_string())
}

/// The revision OBS holds for the checkout's package must be the one the
/// checkout holds (`.osc/_files`, which osc updates when a commit lands). When
/// it is not, someone committed after the attempt, or the attempt landed but
/// osc did not record it: either way the restore would overwrite a revision
/// nobody here has looked at, so it is refused.
pub fn obs_still_at_checkout(checkout: &Path) -> Result<(), String> {
    let read = |name: &str| {
        fs::read_to_string(checkout.join(".osc").join(name))
            .map(|s| s.trim().to_string())
            .map_err(|e| format!("cannot read .osc/{name} of {}: {e}", checkout.display()))
    };
    let (project, package) = (read("_project")?, read("_package")?);
    let local = fs::read_to_string(checkout.join(".osc/_files"))
        .ok()
        .and_then(|xml| parse_directory_srcmd5(&xml))
        .ok_or_else(|| format!("the checkout of {project}/{package} records no revision"))?;
    let head = source_srcmd5(&project, &package, Emit::quiet())
        .map_err(|e| format!("cannot read the OBS revision of {project}/{package}: {e}"))?;
    if head == local {
        return Ok(());
    }
    Err(format!(
        "OBS moved on: {project}/{package} is at revision {head}, the checkout at {local}; \
         nothing was written to OBS"
    ))
}

/// [`restore_baseline_with`] committing through [`commit`], and removing `dir`
/// once it is restored. A failure leaves `dir` in place, marked as unrestored
/// (see [`begin_baseline`]). The commit may run after a shutdown: see
/// [`proc::restoring_after_shutdown`]. Refuses when OBS has moved on (see
/// [`obs_still_at_checkout`]).
pub fn restore_baseline(dir: &Path, emit: Emit<'_>) -> Result<(), String> {
    let _restoring = proc::restoring_after_shutdown();
    match restore_baseline_with(dir, obs_still_at_checkout, |checkout, message, files| {
        commit(checkout, message, files, emit)
    }) {
        Ok(()) => {
            let _ = fs::remove_dir_all(dir);
            Ok(())
        }
        Err(why) => {
            let _ = fs::write(dir.join(UNRESTORED), &why);
            Err(why)
        }
    }
}

#[cfg(test)]
mod baseline_tests;

/// Query the OBS build status for one repo/arch combination.
///
/// Reads `/build/<project>/_result` (XML) and interprets it with
/// [`obs_status::resolve`]: a bare `finished` is resolved through its
/// `<details>` verdict, and a terminal result in a `dirty` repository is only
/// trusted once the job history confirms it was built from the source
/// revision currently on the server (otherwise `Unknown("stale: ...")`).
///
/// If the API call cannot run or its answer lacks the package, the status is
/// `Unknown` for this read, so the build loop polls again instead of failing.
/// An HTTP error from the API (401, 429, 503, ...) is returned as is.
pub fn get_build_status(
    project: &str,
    pkg_name: &str,
    repo: &str,
    arch: &str,
) -> Result<BuildStatus, Error> {
    match get_build_status_xml(project, pkg_name, repo, arch, false, None)? {
        Some(status) => Ok(status),
        None => Ok(no_result_status()),
    }
}

/// The status of a package the `_result` document does not list (or could not
/// be read for).
fn no_result_status() -> BuildStatus {
    BuildStatus::Unknown("no results".to_string())
}

/// [`get_build_status`] for a package whose sources were just committed: a
/// built verdict only counts once the job history shows it came from the
/// revision now on the server (see [`obs_status::Evidence::confirm`]), so the
/// previous revision's `failed`/`succeeded` is never mistaken for the result.
/// Without the XML answer there is no job history to check, so the status is
/// `Unknown`. `committed` is when the commit was done: the `_result` document
/// is never older than that (see [`ResultsCache::get`]).
fn get_build_status_after_commit(
    project: &str,
    pkg_name: &str,
    repo: &str,
    arch: &str,
    committed: Instant,
) -> Result<BuildStatus, Error> {
    match get_build_status_xml(project, pkg_name, repo, arch, true, Some(committed))? {
        Some(status) => Ok(status),
        None => Ok(no_result_status()),
    }
}

/// What every repository/arch of `pkg_name` says, as one status: `Succeeded`
/// only when each result of the package (all repositories and architectures,
/// multibuild flavors included) is succeeded, excluded or disabled; otherwise
/// the first result that is not, with the `repo/arch` it is from. For
/// deciding whether the package may be submitted, where a build that is green
/// on `repo`/`arch` but failed on another architecture is not a good update.
/// `repo`/`arch` is read the careful way ([`get_build_status`]); the others
/// by their plain verdict, polled for a bounded time while any is still
/// scheduled, blocked or building.
pub fn get_overall_build_status(
    project: &str,
    pkg_name: &str,
    repo: &str,
    arch: &str,
) -> Result<(BuildStatus, Option<String>), Error> {
    const MAX_WAIT: Duration = Duration::from_secs(1800);
    let own = get_build_status(project, pkg_name, repo, arch)?;
    if !own.is_success() {
        return Ok((own, Some(format!("{repo}/{arch}"))));
    }
    check_obs_name("project", project)?;
    check_obs_name("pkg_name", pkg_name)?;
    let path = format!(
        "/build/{}/_result?package={}&multibuild=1",
        enc(project),
        enc(pkg_name)
    );
    let start = Instant::now();
    let mut unchanged: u32 = 0;
    let mut last: Option<(BuildStatus, Option<String>)> = None;
    loop {
        let xml = osc_api_checked(&path).map_err(|e| match e {
            ApiFailure::Http(code, msg) => Error::OscFailed {
                cmd: format!("api {path}"),
                code: i32::from(code),
                stderr: msg,
            },
            ApiFailure::Other(msg) => Error::NotActionable(msg),
        })?;
        let entries =
            obs_status::try_parse_results(&xml).map_err(|e| Error::NotActionable(e.to_string()))?;
        let overall = overall_status(&entries, pkg_name);
        let active = matches!(
            overall.0,
            BuildStatus::Scheduled | BuildStatus::Blocked | BuildStatus::Building
        ) || overall.0 == BuildStatus::Unknown("finished".to_string());
        if !active || start.elapsed() >= MAX_WAIT || crate::proc::shutting_down() {
            return Ok(overall);
        }
        unchanged = if last.as_ref() == Some(&overall) {
            unchanged.saturating_add(1)
        } else {
            0
        };
        last = Some(overall);
        sleep_interruptible(poll_delay(unchanged, 0, jitter()));
    }
}

/// The aggregate of [`get_overall_build_status`], from a `_result` document's
/// entries: only the package and its `<package>:<flavor>` multibuild entries
/// count. A result that failed outranks one still going, since waiting on the
/// latter cannot change the outcome. Excluded and disabled results are out of
/// scope; the `repo/arch` of the blocking result comes with the status.
fn overall_status(
    entries: &[obs_status::ResultEntry],
    pkg_name: &str,
) -> (BuildStatus, Option<String>) {
    let flavor_prefix = format!("{pkg_name}:");
    let statuses: Vec<(BuildStatus, String)> = entries
        .iter()
        .filter(|e| e.pkg == pkg_name || e.pkg.starts_with(&flavor_prefix))
        .map(|e| {
            (
                obs_status::resolve_lenient(e),
                format!("{}/{}", e.repo, e.arch),
            )
        })
        .collect();
    let blocks = |s: &BuildStatus| {
        !matches!(
            s,
            BuildStatus::Succeeded | BuildStatus::Excluded | BuildStatus::Disabled
        )
    };
    if let Some((bad, at)) = statuses
        .iter()
        .find(|(s, _)| blocks(s) && s.is_terminal())
        .or_else(|| statuses.iter().find(|(s, _)| blocks(s)))
    {
        return (bad.clone(), Some(at.clone()));
    }
    if statuses.iter().any(|(s, _)| s.is_success()) {
        (BuildStatus::Succeeded, None)
    } else {
        (BuildStatus::Unknown("no result built".to_string()), None)
    }
}

/// The `osc results` human-table path only — no XML, no staleness resolution
/// via job history. Used by [`obs_build`] as an independent second opinion
/// when the XML path's answer has stopped changing for a long time: the two
/// paths hit different OBS code (an XML API document vs. a
/// human-table-rendering endpoint), so if one of them is silently serving a
/// stale/cached snapshot, the other is the cheapest way to notice without
/// guessing at why.
fn get_build_status_from_results_table(
    project: &str,
    pkg_name: &str,
    repo: &str,
    arch: &str,
    emit: Emit<'_>,
) -> Result<BuildStatus, Error> {
    check_obs_name("project", project)?;
    check_obs_name("pkg_name", pkg_name)?;
    check_obs_name("repo", repo)?;
    check_obs_name("arch", arch)?;
    let cmd = format!("results --verbose -r {repo} -a {arch} {project} {pkg_name}");
    // `--verbose` is what carries a `finished` result's verdict.
    let out = proc::capture_with_timeout(
        "osc",
        &[
            "results",
            "--verbose",
            "-r",
            repo,
            "-a",
            arch,
            project,
            pkg_name,
        ],
        &anywhere(),
        emit,
        OSC_QUERY_TIMEOUT,
    )
    .map_err(during(cmd.clone()))?;

    if !out.success() {
        return Err(Error::OscFailed {
            cmd,
            code: out.code,
            stderr: out.stderr,
        });
    }

    Ok(parse_single_build_status(&out.stdout, pkg_name, repo, arch))
}

/// Per-call limit for the short, network-bound `osc` queries used while
/// polling. Generous for a healthy OBS, but finite: see
/// [`proc::capture_with_timeout`].
const OSC_QUERY_TIMEOUT: Duration = Duration::from_secs(60);

/// Bound for `osc co`, `up` and `ci`, which move a package's sources.
const OSC_TRANSFER_TIMEOUT: Duration = Duration::from_secs(15 * 60);

/// Bound for `osc buildlog` and `getbinaries`, which move whole logs and RPM
/// sets and so run longer than a source transfer.
const OSC_ARTIFACT_TIMEOUT: Duration = Duration::from_secs(30 * 60);

/// Run a short network-bound `osc` query in the current directory. A launch
/// failure or timeout is reported and yields `None`, for callers that treat a
/// failed query as "no answer" rather than as an error.
fn osc_query(args: &[&str], emit: Emit<'_>, what: &str) -> Option<Captured> {
    proc::capture_with_timeout("osc", args, &anywhere(), emit, OSC_QUERY_TIMEOUT)
        .map_err(|e| ap_core::diag!("  osc {what}: {e}"))
        .ok()
}

/// Why an `osc api` read gave no document.
#[derive(Clone)]
enum ApiFailure {
    /// The server answered with this HTTP status (401, 429, 503, ...): asking
    /// again through another `osc` command will not get a different answer.
    Http(u16, String),
    /// `osc` could not run, timed out, or failed without an HTTP status.
    Other(String),
}

/// The HTTP status in an `osc` error message, in either wording `osc`
/// versions use (`HTTP Error 503: ...`, `503 Server Error: ...`).
fn http_status(stderr: &str) -> Option<u16> {
    let digits = |s: &str| {
        let n: String = s.chars().take_while(char::is_ascii_digit).collect();
        (n.len() == 3).then(|| n.parse().ok()).flatten()
    };
    if let Some((_, rest)) = stderr.split_once("HTTP Error ") {
        return digits(rest);
    }
    stderr
        .split_whitespace()
        .zip(stderr.split_whitespace().skip(1))
        .find_map(|(code, kind)| {
            matches!(kind, "Client" | "Server")
                .then(|| digits(code))
                .flatten()
        })
}

/// `osc api <path>` stdout, or why there is none.
fn osc_api_checked(path: &str) -> Result<String, ApiFailure> {
    let out = proc::capture_with_timeout(
        "osc",
        &["api", path],
        &anywhere(),
        Emit::quiet(),
        OSC_QUERY_TIMEOUT,
    )
    .map_err(|e| ApiFailure::Other(format!("osc api {path}: {e}")))?;
    if !out.success() {
        let msg = format!("osc api {path}: exit {}: {}", out.code, out.stderr.trim());
        return Err(match http_status(&out.stderr) {
            Some(code) => ApiFailure::Http(code, msg),
            None => ApiFailure::Other(msg),
        });
    }
    Ok(out.stdout)
}

/// The end of a package's latest build log for `repo`/`arch`, where the
/// rpmlint block is, or `None` when OBS has no log for it.
pub fn fetch_build_log_tail(project: &str, pkg: &str, repo: &str, arch: &str) -> Option<String> {
    const KEEP: usize = 1 << 20;
    check_obs_name("project", project).ok()?;
    check_obs_name("package", pkg).ok()?;
    let log = osc_api(&format!(
        "/build/{}/{}/{}/{}/_log?nostream=1&start=0",
        enc(project),
        enc(repo),
        enc(arch),
        enc(pkg)
    ))?;
    if log.len() <= KEEP {
        return Some(log);
    }
    let mut from = log.len() - KEEP;
    while !log.is_char_boundary(from) {
        from += 1;
    }
    Some(log[from..].to_string())
}

/// `osc api <path>` stdout, or `None` on any failure or timeout.
fn osc_api(path: &str) -> Option<String> {
    osc_api_checked(path)
        .map_err(|e| match e {
            ApiFailure::Http(_, m) | ApiFailure::Other(m) => ap_core::diag!("  {m}"),
        })
        .ok()
}

/// Percent-encode one URL component. Package names like `libsigc++` contain
/// `+`, which a query string would otherwise read as a space, and `&` or `#`
/// would end the value outright.
fn enc(s: &str) -> std::borrow::Cow<'_, str> {
    urlencoding::encode(s)
}

/// How long a project-wide `_result` document is shared between waiters.
const RESULTS_TTL: Duration = Duration::from_secs(5);

/// How long a failed fetch is served to the waiters that ask right after it,
/// so a refusing server is asked once, not once per waiter.
const RESULTS_ERROR_TTL: Duration = Duration::from_secs(1);

/// The longest a waiter sits behind another waiter's fetch before fetching
/// itself (a little over [`OSC_QUERY_TIMEOUT`]).
const RESULTS_FLIGHT_WAIT: Duration = Duration::from_secs(70);

type ResultsKey = (String, String, String);

enum Slot {
    /// A fetch is running; `id` tells it apart from a later takeover.
    Fetching { id: u64 },
    /// The outcome of a fetch that began at `started` and ended at `at`.
    Ready {
        started: Instant,
        at: Instant,
        result: Result<Arc<str>, ApiFailure>,
    },
}

#[derive(Default)]
struct CacheState {
    slots: HashMap<ResultsKey, Slot>,
    next_flight: u64,
    /// Bumped per project by [`ResultsCache::invalidate`]: a fetch that began
    /// under an older epoch is not stored.
    epochs: HashMap<String, u64>,
}

/// The project-wide `_result` documents, one per (project, repository, arch),
/// shared by every waiter of the process. Every OBS call is an `osc`
/// subprocess, so tens of waiters polling their own package would start tens
/// of Python interpreters per interval; here they trigger one fetch per
/// [`RESULTS_TTL`] and read their package out of the same document.
struct ResultsCache {
    state: Mutex<CacheState>,
    done: std::sync::Condvar,
}

/// Clears a flight's `Fetching` slot if the fetch unwinds, so the waiters
/// behind it neither hang nor wait for a result that will never come.
struct Flight<'a> {
    cache: &'a ResultsCache,
    key: &'a ResultsKey,
    id: u64,
    landed: bool,
}

impl Drop for Flight<'_> {
    fn drop(&mut self) {
        if self.landed {
            return;
        }
        let mut st = self.cache.lock();
        if matches!(st.slots.get(self.key), Some(Slot::Fetching { id, .. }) if *id == self.id) {
            st.slots.remove(self.key);
        }
        drop(st);
        self.cache.done.notify_all();
    }
}

impl ResultsCache {
    fn new() -> Self {
        ResultsCache {
            state: Mutex::new(CacheState::default()),
            done: std::sync::Condvar::new(),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, CacheState> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// The document for `key`, from `fetch` unless a shared one is at most
    /// `ttl` old. Waiters for a key whose fetch is running wait for it instead
    /// of starting another. `newer_than` demands a document whose fetch began
    /// at or after that instant (a waiter that has just committed cannot use
    /// what was asked before): an older one is not served, and an older fetch
    /// in flight is waited out and then replaced.
    fn get(
        &self,
        key: &ResultsKey,
        ttl: Duration,
        newer_than: Option<Instant>,
        fetch: impl FnOnce() -> Result<String, ApiFailure>,
    ) -> Result<Arc<str>, ApiFailure> {
        let deadline = Instant::now() + RESULTS_FLIGHT_WAIT;
        let mut st = self.lock();
        let (id, started, epoch) = loop {
            let fresh_enough = |started: &Instant| newer_than.is_none_or(|t| *started >= t);
            match st.slots.get(key) {
                Some(Slot::Ready {
                    started,
                    at,
                    result,
                }) if fresh_enough(started)
                    && at.elapsed()
                        < if result.is_ok() {
                            ttl
                        } else {
                            ttl.min(RESULTS_ERROR_TTL)
                        } =>
                {
                    return result.clone();
                }
                Some(Slot::Fetching { .. }) => {
                    let left = deadline.saturating_duration_since(Instant::now());
                    if !left.is_zero() {
                        st = self
                            .done
                            .wait_timeout(st, left)
                            .unwrap_or_else(|e| e.into_inner())
                            .0;
                        continue;
                    }
                }
                _ => {}
            }
            let id = st.next_flight;
            st.next_flight += 1;
            let started = Instant::now();
            st.slots.insert(key.clone(), Slot::Fetching { id });
            break (id, started, st.epochs.get(&key.0).copied().unwrap_or(0));
        };
        drop(st);

        let mut flight = Flight {
            cache: self,
            key,
            id,
            landed: false,
        };
        let result = fetch().map(|doc| Arc::from(doc.as_str()));
        let mut st = self.lock();
        if matches!(st.slots.get(key), Some(Slot::Fetching { id: i, .. }) if *i == id) {
            if st.epochs.get(&key.0).copied().unwrap_or(0) == epoch {
                st.slots.insert(
                    key.clone(),
                    Slot::Ready {
                        started,
                        at: Instant::now(),
                        result: result.clone(),
                    },
                );
            } else {
                st.slots.remove(key);
            }
        }
        flight.landed = true;
        drop(st);
        self.done.notify_all();
        result
    }

    /// Forget `project`'s documents: its sources changed, so what was fetched
    /// so far describes the old state. A fetch in flight is not stored.
    fn invalidate(&self, project: &str) {
        let mut st = self.lock();
        *st.epochs.entry(project.to_string()).or_insert(0) += 1;
        st.slots
            .retain(|k, slot| k.0 != project || matches!(slot, Slot::Fetching { .. }));
    }
}

static RESULTS_CACHE: LazyLock<ResultsCache> = LazyLock::new(ResultsCache::new);

/// Every entry of the `_result` document for `pkg`'s repository/arch, when the
/// document has `pkg` in it. The project-wide document comes from `cache`;
/// one that is cut off, or lacks the package (not scheduled yet, or fetched
/// before it was added), is replaced by `fetch_package`, the document of the
/// package alone (with its multibuild flavors). A failure of the shared fetch
/// is returned as is: asking again another way meets the same refusal.
#[allow(clippy::too_many_arguments)]
fn package_entries(
    cache: &ResultsCache,
    project: &str,
    repo: &str,
    arch: &str,
    pkg: &str,
    fresh_since: Option<Instant>,
    fetch_project: impl FnOnce() -> Result<String, ApiFailure>,
    fetch_package: impl FnOnce() -> Result<String, ApiFailure>,
) -> Result<Option<Vec<obs_status::ResultEntry>>, ApiFailure> {
    let key = (project.to_string(), repo.to_string(), arch.to_string());
    let doc = cache.get(&key, RESULTS_TTL, fresh_since, fetch_project)?;
    if let Ok(entries) = obs_status::try_parse_results(&doc)
        && obs_status::entry_in(&entries, repo, arch, pkg).is_some()
    {
        return Ok(Some(entries));
    }
    let xml = fetch_package()?;
    let entries = obs_status::parse_results(&xml);
    let found = obs_status::entry_in(&entries, repo, arch, pkg).is_some();
    Ok(found.then_some(entries))
}

/// [`package_entries`] against OBS. The names must be valid OBS names.
fn obs_package_entries(
    project: &str,
    repo: &str,
    arch: &str,
    pkg: &str,
    fresh_since: Option<Instant>,
) -> Result<Option<Vec<obs_status::ResultEntry>>, ApiFailure> {
    // `multibuild=1` adds the package's `<pkg>:<flavor>` entries to the
    // single-package document; the project-wide one lists them anyway.
    let (project_path, package_path) = (
        format!(
            "/build/{}/_result?repository={}&arch={}",
            enc(project),
            enc(repo),
            enc(arch)
        ),
        format!(
            "/build/{}/_result?package={}&repository={}&arch={}&multibuild=1",
            enc(project),
            enc(pkg),
            enc(repo),
            enc(arch)
        ),
    );
    package_entries(
        &RESULTS_CACHE,
        project,
        repo,
        arch,
        pkg,
        fresh_since,
        || osc_api_checked(&project_path),
        || osc_api_checked(&package_path),
    )
}

/// `fresh_since`: only a document fetched after that instant may be used
/// (see [`ResultsCache::get`]); `None` accepts one a few seconds old.
fn get_build_status_xml(
    project: &str,
    pkg_name: &str,
    repo: &str,
    arch: &str,
    confirm: bool,
    fresh_since: Option<Instant>,
) -> Result<Option<BuildStatus>, Error> {
    if ![project, pkg_name, repo, arch]
        .iter()
        .all(|n| is_valid_obs_name(n))
    {
        return Ok(None);
    }
    let entries = match obs_package_entries(project, repo, arch, pkg_name, fresh_since) {
        Ok(Some(entries)) => entries,
        Ok(None) => return Ok(None),
        // An HTTP error (login, rate limit, outage) would repeat on the next
        // read, so it is reported rather than turned into an empty answer.
        Err(ApiFailure::Http(code, msg)) => {
            return Err(Error::OscFailed {
                cmd: format!("api /build/{project}/_result"),
                code: i32::from(code),
                stderr: msg,
            });
        }
        Err(ApiFailure::Other(msg)) => {
            ap_core::diag!("  {msg}");
            return Ok(None);
        }
    };
    let Some(entry) = obs_status::entry_in(&entries, repo, arch, pkg_name) else {
        return Ok(None);
    };
    // The source revision is the package's; every flavor builds from it.
    let mut source: Option<Option<obs_status::SourceInfo>> = None;
    let mut resolve_entry = |entry: &obs_status::ResultEntry| {
        let evidence = if obs_status::needs_evidence(entry, confirm) {
            obs_status::Evidence {
                confirm,
                source: source
                    .get_or_insert_with(|| {
                        osc_api(&format!(
                            "/source/{}/{}?view=info",
                            enc(project),
                            enc(pkg_name)
                        ))
                        .and_then(|x| obs_status::parse_source_info(&x))
                    })
                    .clone(),
                job: osc_api(&format!(
                    "/build/{}/{}/{}/_jobhistory?package={}&limit=1",
                    enc(project),
                    enc(repo),
                    enc(arch),
                    enc(&entry.pkg)
                ))
                .and_then(|x| obs_status::parse_latest_job(&x)),
            }
        } else {
            obs_status::Evidence {
                confirm,
                ..Default::default()
            }
        };
        obs_status::resolve(entry, &evidence)
    };
    let base = resolve_entry(entry);
    let flavors: Vec<BuildStatus> = obs_status::flavors_in(&entries, repo, arch, pkg_name)
        .iter()
        .map(&mut resolve_entry)
        .collect();
    Ok(Some(obs_status::combine(base, &flavors)))
}

#[cfg(test)]
mod results_cache_tests;

#[cfg(test)]
mod wait_pacing_tests;

/// Parsing half of the table read in [`get_build_status_from_results_table`],
/// split out so it is testable without shelling out to `osc`.
///
/// `osc results --verbose -r <repo> -a <arch> <project> <pkg>` prints one line
/// per result as `<repo> <arch> <pkg> <status>` (osc's `get_results` format,
/// columns in that order). The status is the rest of the line:
/// - a plain code (`succeeded`, `failed`, `scheduled`, ...), optionally with
///   `: <details>` after it, and `(unpublished)` for a build that succeeded
///   but is not published yet;
/// - `finished: <verdict>`: OBS hides the verdict behind the bare `finished`
///   code, and `--verbose` prints it as the details;
/// - `outdated (was: ...)` when the repository is dirty, or a trailing `*` in
///   the non-verbose form: the result may be for an older source revision.
///
/// Multi-line `unresolvable:` details only add lines that do not start with
/// the repo, so reading the first matching line is enough.
fn parse_single_build_status(stdout: &str, pkg_name: &str, repo: &str, arch: &str) -> BuildStatus {
    for line in stdout.lines() {
        let mut rest = line.trim_start();
        let mut columns = [""; 3];
        for col in &mut columns {
            let end = rest.find(char::is_whitespace).unwrap_or(rest.len());
            *col = &rest[..end];
            rest = rest[end..].trim_start();
        }
        if columns != [repo, arch, pkg_name] {
            continue;
        }
        return status_from_osc_text(rest.trim());
    }
    BuildStatus::Unknown("no results".to_string())
}

/// The bare status word of a result head: the trailing `*` and `(unpublished)`
/// markers removed, with whitespace before each one removed too, so that
/// `succeeded (unpublished)` gives `succeeded`.
fn strip_result_suffixes(head: &str) -> &str {
    let mut word = head;
    loop {
        let next = word
            .trim_end_matches('*')
            .trim_end_matches("(unpublished)")
            .trim_end();
        if next.len() == word.len() {
            return word;
        }
        word = next;
    }
}

/// The status part of one `osc results` line (see [`parse_single_build_status`]).
fn status_from_osc_text(text: &str) -> BuildStatus {
    // OBS marks a result as possibly older than the sources on the server:
    // the dirty-repository form, which `--verbose` spells `outdated (was: ..)`
    // and the plain form spells with a trailing `*`. Never trust those as-is:
    // reporting the stripped word as plain `Succeeded` would make the
    // `AlreadyBuilt` fast path in pipeline.rs believe a since-superseded
    // result still verifies the current revision. `Unknown` makes that path
    // fall through and resync instead.
    if text.starts_with("outdated") {
        return BuildStatus::Unknown(format!("stale: {text}"));
    }
    let (head, details) = match text.split_once(':') {
        Some((head, details)) => (head.trim(), Some(details.trim())),
        None => (text, None),
    };
    let starred = head.ends_with('*');
    let word = strip_result_suffixes(head);
    if starred {
        return BuildStatus::Unknown(format!("stale: {word}"));
    }
    if word == "finished" {
        // The verdict is the first word of the details, if osc printed it.
        return match details.and_then(|d| d.split_whitespace().next()) {
            Some(verdict) => BuildStatus::from(verdict),
            None => BuildStatus::Unknown("finished".to_string()),
        };
    }
    BuildStatus::from(word)
}

#[cfg(test)]
mod single_build_status_tests;

/// Every package's status in `project` for one repository and architecture,
/// read from the `_result` document. Multibuild flavors are folded into their
/// base package (see [`fold_flavor_statuses`]).
pub fn get_project_build_statuses(
    project: &str,
    repo: &str,
    arch: &str,
) -> Result<HashMap<String, BuildStatus>, Error> {
    check_obs_name("project", project)?;
    check_obs_name("repo", repo)?;
    check_obs_name("arch", arch)?;
    let path = format!(
        "/build/{}/_result?repository={}&arch={}",
        enc(project),
        enc(repo),
        enc(arch)
    );
    // An empty map reads as "no package has a status", so a listing that
    // cannot be read is an error rather than an empty answer.
    let xml = osc_api_checked(&path).map_err(|e| match e {
        ApiFailure::Http(code, msg) => Error::OscFailed {
            cmd: format!("api {path}"),
            code: i32::from(code),
            stderr: msg,
        },
        ApiFailure::Other(msg) => {
            Error::NotActionable(format!("cannot read the OBS results listing: {msg}"))
        }
    })?;
    // A cut-off listing must not read as a partial set of statuses.
    let statuses = project_statuses_from_xml(&xml, repo, arch)
        .map_err(|e| Error::NotActionable(format!("cannot read the OBS results listing: {e}")))?;
    Ok(fold_flavor_statuses(statuses))
}

/// A package is only as built as its worst multibuild flavor: `python-mcp` is
/// `succeeded` while its `python-mcp:test` flavor is `unresolvable` (a test
/// dependency nothing provides), and counting the base alone as built left the
/// package out of the build queue with its missing dependency never reported.
/// A flavor that is not `succeeded`/`excluded`/`disabled` passes its status to
/// the base package when the base would otherwise count as built. The flavor
/// entries stay in the map.
fn fold_flavor_statuses(
    mut statuses: HashMap<String, BuildStatus>,
) -> HashMap<String, BuildStatus> {
    let blocks = |s: &BuildStatus| {
        !matches!(
            s,
            BuildStatus::Succeeded | BuildStatus::Excluded | BuildStatus::Disabled
        )
    };
    let flavors: Vec<(String, BuildStatus)> = statuses
        .iter()
        .filter(|(k, s)| k.contains(':') && blocks(s))
        // A `test` flavor whose tests FAIL builds nothing a dependent installs,
        // and the package it belongs to cannot be fixed by building it again:
        // python-redis (`python-redis:test` failed, `python-redis` succeeded) was
        // selected as unbuilt every round, finished "already built" every
        // round, and the requested root was never reached. Only a test flavor
        // that cannot even be set up (`unresolvable`, `broken`) still counts.
        .filter(|(k, s)| !(k.ends_with(":test") && matches!(s, BuildStatus::Failed)))
        .map(|(k, s)| (k.clone(), s.clone()))
        .collect();
    for (key, status) in flavors {
        let Some((base, _)) = key.split_once(':') else {
            continue;
        };
        if let Some(b) = statuses.get_mut(base)
            && b.is_success()
        {
            *b = status;
        }
    }
    statuses
}

/// Every package's status in ONE repository/architecture of a `_result`
/// document.
///
/// The document carries a `<result>` per repository/architecture the project
/// builds for, and the same package is routinely `succeeded` in one and
/// `excluded` or `unresolvable` in another (an `i586` repository next to
/// `x86_64`); only the pair asked for may contribute, or one package's state
/// somewhere else would stand in for the state of the build being made.
fn project_statuses_from_xml(
    xml: &str,
    repo: &str,
    arch: &str,
) -> Result<HashMap<String, BuildStatus>, String> {
    Ok(obs_status::try_parse_results(xml)?
        .into_iter()
        .filter(|e| e.repo == repo && e.arch == arch)
        .map(|e| (e.pkg.clone(), obs_status::resolve_lenient(&e)))
        .collect())
}

/// Fetch the OBS build log into `log_path`.
///
/// Runs `osc buildlog <project> <package> <repo> <arch>` and writes
/// stdout to `log_path`. Returns `Ok` even when the exit code is non-zero
/// if some output was captured (partial logs are still useful).
pub fn fetch_buildlog(
    project: &str,
    pkg_name: &str,
    repo: &str,
    arch: &str,
    log_path: &Path,
    emit: Emit<'_>,
) -> Result<(), Error> {
    check_obs_name("project", project)?;
    check_obs_name("pkg_name", pkg_name)?;
    check_obs_name("repo", repo)?;
    check_obs_name("arch", arch)?;
    let out = proc::capture_with_timeout(
        "osc",
        &["buildlog", project, pkg_name, repo, arch],
        &anywhere(),
        emit,
        OSC_ARTIFACT_TIMEOUT,
    )
    .map_err(during(format!(
        "buildlog {project} {pkg_name} {repo} {arch}"
    )))?;

    proc::write_log(log_path, out.stdout.as_bytes())?;

    if !out.success() && out.stdout.is_empty() {
        return Err(Error::OscFailed {
            cmd: format!("buildlog {project} {pkg_name} {repo} {arch}"),
            code: out.code,
            stderr: out.stderr,
        });
    }
    Ok(())
}

/// Ask OBS to build `pkg_name` again from the sources it already has, and wait
/// (at most 90 s) until its status is no longer a final one. Without that wait
/// the build wait that follows would read the previous build's verdict, which
/// matches the current sources, as the answer. Returns whether the new build
/// was seen to start.
pub fn rebuild(
    project: &str,
    pkg_name: &str,
    repo: &str,
    arch: &str,
    emit: Emit<'_>,
) -> Result<bool, Error> {
    check_obs_name("project", project)?;
    check_obs_name("pkg_name", pkg_name)?;
    check_obs_name("repo", repo)?;
    check_obs_name("arch", arch)?;
    let out = proc::capture_with_timeout(
        "osc",
        &["rebuild", project, pkg_name, repo, arch],
        &anywhere(),
        emit,
        OSC_QUERY_TIMEOUT,
    )
    .map_err(during(format!(
        "rebuild {project} {pkg_name} {repo} {arch}"
    )))?;
    if !out.success() {
        return Err(Error::OscFailed {
            cmd: format!("rebuild {project} {pkg_name} {repo} {arch}"),
            code: out.code,
            stderr: out.stderr,
        });
    }
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(90) && !crate::proc::shutting_down() {
        if get_build_status(project, pkg_name, repo, arch).is_ok_and(|s| !s.is_terminal()) {
            return Ok(true);
        }
        sleep_interruptible(Duration::from_secs(5));
    }
    Ok(false)
}

/// Download built binaries from OBS into `dest_dir`.
///
/// How many of the binary RPMs OBS lists for `pkg_name` are now in `rpms_dir`.
///
/// Used after `osc getbinaries` to confirm it actually produced artifacts
/// rather than silently exiting 0 with an empty binary set (a misconfigured
/// publishing setup for the repo/arch pair). The RPMs are matched by the
/// names OBS lists, NOT by the source package name: a multi-flavor Python
/// package `python-events` ships `python313-Events-…rpm`, which no
/// `python-events-` prefix would ever match. If the listing cannot be read,
/// fall back to counting the RPMs of `pkg` written since `since` (the start of
/// this package's download), see [`count_package_rpms_since`].
fn downloaded_rpm_count(
    listing: Option<&[String]>,
    rpms_dir: &std::path::Path,
    pkg: &str,
    since: SystemTime,
) -> usize {
    match listing {
        Some(names) => names.iter().filter(|n| rpms_dir.join(n).is_file()).count(),
        None => count_package_rpms_since_in_dir(rpms_dir, pkg, since),
    }
}

/// The `.rpm` binaries OBS lists for `pkg_name` in `repo`/`arch`, or `None`
/// when the listing cannot be read.
fn binary_rpm_listing(
    project: &str,
    pkg_name: &str,
    repo: &str,
    arch: &str,
) -> Option<Vec<String>> {
    osc_api(&format!(
        "/build/{}/{}/{}/{}",
        enc(project),
        enc(repo),
        enc(arch),
        enc(pkg_name)
    ))
    .map(|xml| obs_status::parse_binary_rpms(&xml))
}

/// The package name of an RPM file name: `python313-foo-1.0-2.1.noarch.rpm`
/// is `python313-foo`.
fn rpm_base_name(file: &str) -> Option<&str> {
    let (rest, _arch) = file.strip_suffix(".rpm")?.rsplit_once('.')?;
    let (rest, _release) = rest.rsplit_once('-')?;
    let (name, _version) = rest.rsplit_once('-')?;
    Some(name)
}

/// Delete the RPMs in `rpms_dir` that an earlier build of the same package
/// left behind: files named like one of `current` (same package name),
/// including a `current` file itself, so that a download which wrote nothing
/// is not mistaken for one that did by a stale file of the same name.
/// `rpms_dir` is shared by every package, so they would otherwise pile up and
/// satisfy a dependency with an old version.
fn remove_superseded_rpms(rpms_dir: &Path, current: &[String]) {
    let names: std::collections::HashSet<&str> =
        current.iter().filter_map(|f| rpm_base_name(f)).collect();
    let Ok(entries) = fs::read_dir(rpms_dir) else {
        return;
    };
    for entry in entries.filter_map(|e| e.ok()) {
        let file = entry.file_name().to_string_lossy().into_owned();
        if file.ends_with(".src.rpm") {
            continue;
        }
        if rpm_base_name(&file).is_some_and(|n| names.contains(n))
            && let Err(e) = fs::remove_file(entry.path())
        {
            ap_core::diag!("  cannot remove the stale {file}: {e}");
        }
    }
}

/// The result a non-successful `status` of `pkg_name` comes from: the package
/// itself, or the `<pkg>:<flavor>` multibuild entry that failed while the
/// package built (`python-mcp:test`'s `%check`). The status of a multibuild
/// package is the worst of its entries, so the log that explains it is that
/// entry's, not the package's own, which can be a clean success.
fn failing_result(
    project: &str,
    pkg_name: &str,
    repo: &str,
    arch: &str,
    status: &BuildStatus,
) -> String {
    if ![project, pkg_name, repo, arch]
        .iter()
        .all(|n| is_valid_obs_name(n))
    {
        return pkg_name.to_string();
    }
    osc_api(&format!(
        "/build/{}/_result?package={}&repository={}&arch={}&multibuild=1",
        enc(project),
        enc(pkg_name),
        enc(repo),
        enc(arch)
    ))
    .and_then(|xml| failing_entry_name(&xml, repo, arch, pkg_name, status))
    .unwrap_or_else(|| pkg_name.to_string())
}

/// [`failing_result`] over a `_result` document: the package if its own entry
/// has `status`, else the first flavor entry that does.
fn failing_entry_name(
    xml: &str,
    repo: &str,
    arch: &str,
    pkg_name: &str,
    status: &BuildStatus,
) -> Option<String> {
    let base = obs_status::find_entry(xml, repo, arch, pkg_name);
    std::iter::once(base)
        .flatten()
        .chain(obs_status::flavor_entries(xml, repo, arch, pkg_name))
        .find(|e| obs_status::resolve_lenient(e) == *status)
        .map(|e| e.pkg)
        .filter(|name| is_valid_obs_name(name))
}

/// The `<pkg>:<flavor>` names of `pkg_name`'s multibuild flavors built for
/// `repo`/`arch`; empty for a plain package or when the listing cannot be read.
fn multibuild_flavors(project: &str, pkg_name: &str, repo: &str, arch: &str) -> Vec<String> {
    if ![project, pkg_name, repo, arch]
        .iter()
        .all(|n| is_valid_obs_name(n))
    {
        return Vec::new();
    }
    osc_api(&format!(
        "/build/{}/_result?package={}&repository={}&arch={}&multibuild=1",
        enc(project),
        enc(pkg_name),
        enc(repo),
        enc(arch)
    ))
    .map(|xml| {
        obs_status::flavor_entries(&xml, repo, arch, pkg_name)
            .into_iter()
            .filter(|e| {
                is_valid_obs_name(&e.pkg)
                    && !matches!(
                        obs_status::resolve_lenient(e),
                        BuildStatus::Excluded | BuildStatus::Disabled
                    )
            })
            .map(|e| e.pkg)
            .collect()
    })
    .unwrap_or_default()
}

/// Whether the file name `file` is a binary RPM of package `pkg`: `<pkg>-` and
/// then `<version>-<release>.<arch>.rpm`, the same shape `sibling_rpms` takes
/// (a version starting with a digit, exactly two `-` fields after the name).
/// A sibling whose name extends `pkg` (`pkg-libs-1.0-1.x86_64.rpm`) does not
/// match, and neither does a `.src.rpm`.
fn is_package_rpm(file: &str, pkg: &str) -> bool {
    if file.ends_with(".src.rpm") {
        return false;
    }
    let Some(rest) = file.strip_prefix(pkg).and_then(|r| r.strip_prefix('-')) else {
        return false;
    };
    let Some(stem) = rest.strip_suffix(".rpm") else {
        return false;
    };
    match stem.split('-').collect::<Vec<_>>().as_slice() {
        [version, release_arch] => {
            version.starts_with(|c: char| c.is_ascii_digit())
                && release_arch
                    .rsplit_once('.')
                    .is_some_and(|(release, arch)| !release.is_empty() && !arch.is_empty())
        }
        _ => false,
    }
}

/// How many of `files` (name, modification time; `None` when unknown) are
/// binary RPMs of `pkg` modified at or after `since`. Pure: the caller reads
/// the directory.
fn count_package_rpms_since(
    files: &[(String, Option<SystemTime>)],
    pkg: &str,
    since: SystemTime,
) -> usize {
    files
        .iter()
        .filter(|(name, modified)| {
            is_package_rpm(name, pkg) && modified.is_some_and(|m| m >= since)
        })
        .count()
}

/// [`count_package_rpms_since`] over the files currently in `rpms_dir`.
/// Returns 0 when the directory doesn't exist or can't be read.
fn count_package_rpms_since_in_dir(
    rpms_dir: &std::path::Path,
    pkg: &str,
    since: SystemTime,
) -> usize {
    let Ok(iter) = std::fs::read_dir(rpms_dir) else {
        return 0;
    };
    let files: Vec<(String, Option<SystemTime>)> = iter
        .flatten()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_file()))
        .map(|e| {
            (
                e.file_name().to_string_lossy().into_owned(),
                e.metadata().and_then(|m| m.modified()).ok(),
            )
        })
        .collect();
    count_package_rpms_since(&files, pkg, since)
}

/// Runs `osc getbinaries --destdir=<dest> <project> <package> <repo> <arch>`.
/// Errors are non-fatal: the caller logs a warning and continues.
pub fn get_binaries(
    project: &str,
    pkg_name: &str,
    repo: &str,
    arch: &str,
    dest_dir: &Path,
    emit: Emit<'_>,
) -> Result<(), Error> {
    check_obs_name("project", project)?;
    check_obs_name("pkg_name", pkg_name)?;
    check_obs_name("repo", repo)?;
    check_obs_name("arch", arch)?;
    fs::create_dir_all(dest_dir)?;
    let dest = dest_dir.to_string_lossy();
    let out = proc::capture_with_timeout(
        "osc",
        &[
            "getbinaries",
            &format!("--destdir={dest}"),
            project,
            pkg_name,
            repo,
            arch,
        ],
        &anywhere(),
        emit,
        OSC_ARTIFACT_TIMEOUT,
    )
    .map_err(during(format!(
        "getbinaries {project} {pkg_name} {repo} {arch}"
    )))?;

    if !out.success() {
        return Err(Error::OscFailed {
            cmd: format!("getbinaries {project} {pkg_name} {repo} {arch}"),
            code: out.code,
            stderr: out.stderr,
        });
    }
    Ok(())
}

/// Get the unresolvable package names (and raw detail lines) OBS already
/// knows about for a package, without triggering a new build: the `_result`
/// document first, then whatever build log already sits at `log_path`. Exposed
/// so a caller that finds a package already `Unresolvable` on restart can
/// inform the fix agent immediately instead of spending an attempt
/// rediscovering it through a fresh build.
pub fn current_unresolvable(
    project: &str,
    pkg_name: &str,
    repo: &str,
    arch: &str,
    log_path: &Path,
) -> (Vec<String>, Vec<String>) {
    // The `_result` document is what OBS says about the current revision in
    // exactly this repository and architecture; a build log may be left over
    // from an older revision, so it only runs when the document gives nothing.
    if let Some(details) = unresolvable_details(project, pkg_name, repo, arch) {
        let names = extract_unresolvable(&details);
        if !names.is_empty() {
            return (names, extract_unresolvable_detail(&details));
        }
    }
    let log_text = read_log_lossy(log_path).unwrap_or_default();
    (
        extract_unresolvable(&log_text),
        extract_unresolvable_detail(&log_text),
    )
}

/// The `<details>` text of `pkg_name`'s `unresolvable` status in this one
/// repository/architecture, or `None` when it is not unresolvable there (or
/// the status cannot be read). Other repositories' results in the same
/// document are never looked at: a package can be `unresolvable` on `i586`
/// and `succeeded` on `x86_64` at once.
pub fn unresolvable_details(
    project: &str,
    pkg_name: &str,
    repo: &str,
    arch: &str,
) -> Option<String> {
    let xml = osc_api(&format!(
        "/build/{}/_result?package={}&repository={}&arch={}",
        enc(project),
        enc(pkg_name),
        enc(repo),
        enc(arch)
    ))?;
    unresolvable_details_from_xml(&xml, repo, arch, pkg_name)
}

fn unresolvable_details_from_xml(xml: &str, repo: &str, arch: &str, pkg: &str) -> Option<String> {
    let entry = obs_status::find_entry(xml, repo, arch, pkg)?;
    (BuildStatus::from(entry.code.as_str()) == BuildStatus::Unresolvable)
        .then_some(entry.details)
        .flatten()
}

/// The packages that must be built before the unresolvable `pkg_name` can
/// build, read from OBS's own `nothing provides` details for this
/// repository/architecture and mapped back to package names by
/// [`missing_package_names`]. Empty when OBS names none (not unresolvable
/// here, a version conflict, or an unreadable status).
pub fn unresolvable_blockers(project: &str, pkg_name: &str, repo: &str, arch: &str) -> Vec<String> {
    match unresolvable_details(project, pkg_name, repo, arch) {
        Some(details) => blockers_from_details(&details),
        None => Vec::new(),
    }
}

fn blockers_from_details(details: &str) -> Vec<String> {
    // A requirement with a version bound is the spec's own constraint failing
    // against the one version the repository has, which building the named
    // package cannot fix; only unconstrained ones are packages to build.
    let unconstrained: Vec<String> = details
        .lines()
        .flat_map(nothing_provides_segments)
        .filter(|segment| segment.split_whitespace().nth(1).is_none())
        .map(|segment| format!("nothing provides {segment}"))
        .collect();
    missing_package_names(&extract_unresolvable(&unconstrained.join("\n")))
}

/// The name `dep` has in `listing` when the project has it built
/// (`succeeded`) in the repository/architecture `statuses` was read for, else
/// `None`. Names are matched by [`fuzzy_key`], with or without the `python-`
/// prefix, and the listing's own spelling is returned.
pub fn built_in_project(
    listing: &std::collections::HashSet<String>,
    statuses: &HashMap<String, BuildStatus>,
    dep: &str,
) -> Option<String> {
    let bare = fuzzy_key(dep);
    let rpm = fuzzy_key(&rpm_name(dep));
    listing
        .iter()
        .filter(|p| {
            let k = fuzzy_key(p);
            k == bare || k == rpm
        })
        .find(|p| statuses.get(*p).is_some_and(BuildStatus::is_success))
        .cloned()
}

/// Section headers of an RPM spec, for [`spec_section`].
const SPEC_SECTIONS: &[&str] = &[
    "%package",
    "%description",
    "%prep",
    "%conf",
    "%generate_buildrequires",
    "%build",
    "%install",
    "%check",
    "%pre",
    "%post",
    "%preun",
    "%postun",
    "%pretrans",
    "%posttrans",
    "%files",
    "%changelog",
];

/// The lines of every `name` section (`%files`, `%install`, ...) of `spec`,
/// header lines excluded.
fn spec_section<'a>(spec: &'a str, name: &str) -> Vec<&'a str> {
    let mut inside = false;
    let mut lines = Vec::new();
    for line in spec.lines() {
        let word = line.split_whitespace().next().unwrap_or("");
        if SPEC_SECTIONS.contains(&word) {
            inside = word == name;
            continue;
        }
        if inside {
            lines.push(line);
        }
    }
    lines
}

/// Whether `spec` is an unfinished skeleton whose package builds but ships
/// next to nothing, and why: a `%files` that lists only `%license`/`%doc`
/// (nothing the build installed is packaged), or an `%install` that is empty
/// apart from comments next to a `TODO`. Both are what the skeleton
/// generators leave for a monorepo or a wheel they could not lay out, and OBS
/// builds either one "successfully".
pub fn unfinished_spec_reason(spec: &str) -> Option<String> {
    let body = spec.split("\n%changelog").next().unwrap_or(spec);
    let is_code = |l: &&str| {
        let t = l.trim();
        !t.is_empty() && !t.starts_with('#')
    };
    let files: Vec<&str> = spec_section(body, "%files")
        .into_iter()
        .filter(is_code)
        .collect();
    let files_listed = files.iter().any(|l| {
        let t = l.trim_start();
        !["%license", "%doc", "%defattr", "%dir"]
            .iter()
            .any(|p| t.starts_with(p))
    });
    let mut reasons = Vec::new();
    if body.contains("\n%files") && !files_listed {
        reasons.push("its %files lists nothing but %license/%doc".to_string());
    }
    if !spec_section(body, "%install").iter().any(is_code)
        && body
            .lines()
            .any(|l| l.trim_start().starts_with('#') && l.contains("TODO"))
    {
        reasons.push("its %install has no commands, only TODO comments".to_string());
    }
    (!reasons.is_empty()).then(|| reasons.join(" and "))
}

/// The `<version>` of a `/source/<project>/<package>?view=info&parse=1`
/// answer, or `None` when it names none (a package with no parsable spec).
fn parse_sourceinfo_version(xml: &str) -> Option<String> {
    let rest = &xml[xml.find("<version>")? + "<version>".len()..];
    let version = rest[..rest.find("</version>")?].trim();
    (!version.is_empty()).then(|| version.to_string())
}

/// The version `pkg_name` has in the sources of `project`, read from the spec
/// OBS parsed, or `None` when it cannot be told. The version a linked project
/// ships is what a dependent's bound has to be met by, so this is how the
/// closure check learns it without checking the package out.
pub fn source_version(project: &str, pkg_name: &str) -> Option<String> {
    let xml = osc_api(&format!(
        "/source/{}/{}?view=info&parse=1",
        enc(project),
        enc(pkg_name)
    ))?;
    parse_sourceinfo_version(&xml)
}

#[cfg(test)]
mod sourceinfo_version_tests;

/// A warning for a package OBS reports `succeeded` whose result a dependent
/// cannot use, or `None` when nothing is wrong or it cannot be told. The test
/// is deterministic: the spec OBS holds is an unfinished skeleton (see
/// [`unfinished_spec_reason`]), and the RPMs it published are listed with
/// their sizes as evidence. A dependent installs such a package and still
/// finds its modules missing, which reads as "the dependency is missing".
pub fn empty_package_warning(
    project: &str,
    pkg_name: &str,
    repo: &str,
    arch: &str,
) -> Option<String> {
    let spec_name = package_spec_name(project, pkg_name, Emit::quiet());
    let spec = osc_api(&format!(
        "/source/{}/{}/{}",
        enc(project),
        enc(pkg_name),
        enc(&spec_name)
    ))?;
    let reason = unfinished_spec_reason(&spec)?;
    let sizes = osc_api(&format!(
        "/build/{}/{}/{}/{}",
        enc(project),
        enc(repo),
        enc(arch),
        enc(pkg_name)
    ))
    .map(|xml| obs_status::parse_binary_rpm_sizes(&xml))
    .unwrap_or_default();
    Some(empty_package_message(pkg_name, project, &reason, &sizes))
}

fn empty_package_message(
    pkg: &str,
    project: &str,
    reason: &str,
    sizes: &[(String, u64)],
) -> String {
    let published = if sizes.is_empty() {
        String::new()
    } else {
        let list: Vec<String> = sizes
            .iter()
            .map(|(f, b)| format!("{f}: {b} bytes"))
            .collect();
        format!(" (published: {})", list.join(", "))
    };
    format!(
        "{pkg} is built in {project}, but its spec there is an unfinished skeleton: {reason}, \
         so the RPMs it published are effectively empty{published}. A package that needs it \
         installs it and still cannot import it. Finish {pkg}'s spec, then rebuild it with \
         `auto-package build --project {project} --force <name>`"
    )
}

/// Package names to queue for building, from the RPM names a build or an
/// agent reports missing: see [`crate::depclass::triage_all`], here without
/// asking the distro (the names come from OBS saying the target has nothing
/// providing them). Callers that want to tell the user why a name was left out
/// use `triage_all` directly.
pub fn missing_package_names(rpm_names: &[String]) -> Vec<String> {
    crate::depclass::triage_all(rpm_names, &|_| false).0
}

/// Whether `name` is an RPM capability (a shared library such as
/// `libcuda.so.1()(64bit)`, a file path, an `rpmlib(...)` feature) rather than
/// a package name. OBS reports these as `nothing provides` too, but there is
/// no project to build for them: `libcuda.so.1` is the NVIDIA driver's own
/// library (convention 60), supplied at runtime, so queueing it as a package
/// can never succeed. The fix is on the package that requires it (an
/// `%global __requires_exclude`), which convention 60 tells the agent.
pub fn is_rpm_capability(name: &str) -> bool {
    let name = name.trim();
    if name.starts_with('/') || name.starts_with("rpmlib(") || name.contains("()") {
        return true;
    }
    // `lib<x>.so`, `.so.<n>`: not a name that merely contains ".so".
    name.match_indices(".so")
        .any(|(i, _)| matches!(name[i + 3..].chars().next(), None | Some('.') | Some('(')))
}

/// Whether `info` (a project meta) builds for `repo` on `arch`: the `Err`
/// says what the project does offer. A meta with no repository at all is
/// left to the "no build repositories" handling elsewhere.
fn check_repo_arch(
    info: &ProjectInfo,
    project: &str,
    repo: &str,
    arch: &str,
) -> Result<(), String> {
    if info.repositories.is_empty() {
        return Ok(());
    }
    let Some(def) = info.repositories.iter().find(|r| r.name == repo) else {
        let names: Vec<&str> = info.repositories.iter().map(|r| r.name.as_str()).collect();
        return Err(format!(
            "{project} has no repository {repo} (it has: {})",
            names.join(", ")
        ));
    };
    if !def.arches.iter().any(|a| a == arch) {
        return Err(format!(
            "{project} does not build {repo} for {arch} (it builds: {})",
            def.arches.join(", ")
        ));
    }
    Ok(())
}

/// The repository of a project that stands for `wanted` on `arch`: `wanted`
/// itself when the project has it, else the one the distribution is also known
/// by (`openSUSE_Tumbleweed` is built as `openSUSE_Factory` in Factory's devel
/// projects, and a branch takes its repositories from the project it was
/// branched from), else the only repository that builds `arch`.
fn pick_repository(info: &ProjectInfo, wanted: &str, arch: &str) -> Result<String, String> {
    let builds = |name: &str| {
        info.repositories
            .iter()
            .find(|r| r.name == name)
            .is_some_and(|r| r.arches.iter().any(|a| a == arch))
    };
    if builds(wanted) {
        return Ok(wanted.to_string());
    }
    let aliases: &[&str] = match wanted {
        "openSUSE_Tumbleweed" => &["openSUSE_Factory"],
        "openSUSE_Factory" => &["openSUSE_Tumbleweed"],
        _ => &[],
    };
    if let Some(alias) = aliases.iter().find(|a| builds(a)) {
        return Ok((*alias).to_string());
    }
    let mut serving = info
        .repositories
        .iter()
        .filter(|r| r.arches.iter().any(|a| a == arch));
    match (serving.next(), serving.next()) {
        (Some(only), None) => Ok(only.name.clone()),
        _ => Err(format!(
            "no repository for {wanted} on {arch} (it has: {})",
            info.repositories
                .iter()
                .map(|r| r.name.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        )),
    }
}

/// The repository to build for when none was asked for: Tumbleweed, else
/// Factory, else the one repository that builds `arch`, else the first by name.
/// The others that build `arch` come second, so the caller can say what it
/// passed over.
fn default_repository(info: &ProjectInfo, arch: &str) -> Option<(String, Vec<String>)> {
    let mut serving: Vec<&str> = info
        .repositories
        .iter()
        .filter(|r| r.arches.iter().any(|a| a == arch))
        .map(|r| r.name.as_str())
        .collect();
    serving.sort_unstable();
    let chosen = ["openSUSE_Tumbleweed", "openSUSE_Factory"]
        .into_iter()
        .find(|p| serving.contains(p))
        .or_else(|| serving.first().copied())?
        .to_string();
    let others = serving
        .into_iter()
        .filter(|n| *n != chosen)
        .map(str::to_string)
        .collect();
    Some((chosen, others))
}

/// The repository of `project` to build for on `arch`: the one asked for, as
/// [`resolve_repository`] reads it, or, when `requested` is `None`, the
/// project's own preferred one. The second part is the other repositories that
/// build `arch` when the choice was between several. A project that does not
/// exist yet, or whose meta cannot be read, gets `openSUSE_Tumbleweed`.
pub fn choose_repository(
    project: &str,
    requested: Option<&str>,
    arch: &str,
) -> (String, Vec<String>) {
    const FALLBACK: &str = "openSUSE_Tumbleweed";
    if let Some(wanted) = requested {
        return (resolve_repository(project, wanted, arch), Vec::new());
    }
    let Ok(Some(meta)) = get_project_meta(project, Emit::quiet()) else {
        return (FALLBACK.to_string(), Vec::new());
    };
    default_repository(&parse_project_info(&meta), arch)
        .unwrap_or_else(|| (FALLBACK.to_string(), Vec::new()))
}

/// [`pick_repository`] for a project on OBS, remembered for the run. A project
/// whose meta cannot be read keeps `wanted`, so the build's own check reports
/// what is wrong.
pub fn resolve_repository(project: &str, wanted: &str, arch: &str) -> String {
    /// (project, wanted, arch) → the repository chosen.
    type Known = HashMap<(String, String, String), String>;
    static KNOWN: Mutex<Option<Known>> = Mutex::new(None);
    let key = (project.to_string(), wanted.to_string(), arch.to_string());
    if let Some(hit) = KNOWN
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_ref()
        .and_then(|m| m.get(&key))
    {
        return hit.clone();
    }
    let Ok(Some(meta)) = get_project_meta(project, Emit::quiet()) else {
        return wanted.to_string();
    };
    let info = parse_project_info(&meta);
    if info.repositories.is_empty() {
        return wanted.to_string();
    }
    let chosen = pick_repository(&info, wanted, arch).unwrap_or_else(|_| wanted.to_string());
    KNOWN
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get_or_insert_with(HashMap::new)
        .insert(key, chosen.clone());
    chosen
}

/// Fail before waiting when the project does not build for `repo`/`arch`:
/// OBS would answer with no result at all, forever. A meta that cannot be
/// read skips the check; the wait's own "no results" limit still applies.
fn ensure_builds_for(project: &str, repo: &str, arch: &str) -> Result<(), Error> {
    let Ok(Some(meta)) = get_project_meta(project, Emit::quiet()) else {
        return Ok(());
    };
    check_repo_arch(&parse_project_info(&meta), project, repo, arch).map_err(Error::NotActionable)
}

/// How long to wait before the next status poll. A status that keeps
/// holding is polled less often (15s, then 30s after 4 unchanged polls, then
/// 60s after 8) and a failing poll backs off exponentially up to 2 minutes, so
/// a long build or an ailing server is not hammered. `jitter` in `[0, 1)`
/// spreads concurrent waiters by up to +-20%.
fn poll_delay(unchanged_polls: u32, errors: u32, jitter: f64) -> Duration {
    let base = match unchanged_polls {
        0..=3 => 15,
        4..=7 => 30,
        _ => 60,
    };
    let secs = if errors == 0 {
        base
    } else {
        (base << errors.min(4)).min(120)
    };
    Duration::from_secs_f64(secs as f64 * (0.8 + 0.4 * jitter))
}

/// The unchanged-poll count that sets the delay while waiting for a build. A
/// status that only awaits the new revision (see
/// [`obs_status::awaiting_new_revision`]) is not "holding still": the build is
/// about to start, so it never backs off past the 30s tier.
fn wait_polls(unchanged_polls: u32, awaiting_new_revision: bool) -> u32 {
    if awaiting_new_revision {
        unchanged_polls.min(7)
    } else {
        unchanged_polls
    }
}

/// How long an unchanged status holds before the independent `osc results`
/// cross-check, asked once per stuck period (see [`recheck_due`]).
const NO_PROGRESS_RECHECK: Duration = Duration::from_secs(300);
// A `stale`/`unconfirmed` status is the previous revision's verdict served
// while OBS has not taken up the commit (or has, and the document has not
// caught up): the independent read says which, so it is asked early and again
// at this pace, instead of once after minutes.
const STALE_RECHECK_FIRST: Duration = Duration::from_secs(30);
const STALE_RECHECK_EVERY: Duration = Duration::from_secs(60);

/// Whether the independent `osc results` cross-check of a non-terminal status
/// is due: `since_change` is how long the status has not changed,
/// `since_recheck` how long ago the last check of this stuck period was. A
/// status that awaits the new revision is checked after 30s and then every
/// 60s; any other unchanged status once, after 300s.
fn recheck_due(
    awaiting_new_revision: bool,
    since_change: Duration,
    since_recheck: Option<Duration>,
) -> bool {
    match (awaiting_new_revision, since_recheck) {
        (true, None) => since_change >= STALE_RECHECK_FIRST,
        (true, Some(last)) => last >= STALE_RECHECK_EVERY,
        (false, None) => since_change >= NO_PROGRESS_RECHECK,
        (false, Some(_)) => false,
    }
}

/// A pseudo-random value in `[0, 1)` from the clock, enough to de-synchronise
/// pollers without a randomness dependency.
fn jitter() -> f64 {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.subsec_nanos());
    f64::from(nanos % 1000) / 1000.0
}

/// The `<details>` OBS gives a package's result in `repo`/`arch` (what a
/// `blocked` or `scheduled` package is waiting for), if any.
fn build_details(project: &str, pkg_name: &str, repo: &str, arch: &str) -> Option<String> {
    if ![project, pkg_name, repo, arch]
        .iter()
        .all(|n| is_valid_obs_name(n))
    {
        return None;
    }
    let entries = obs_package_entries(project, repo, arch, pkg_name, None)
        .map_err(|e| match e {
            ApiFailure::Http(_, m) | ApiFailure::Other(m) => ap_core::diag!("  {m}"),
        })
        .ok()??;
    obs_status::entry_in(&entries, repo, arch, pkg_name)?
        .details
        .clone()
}

/// Sleep for `total`, waking each second to notice a shutdown request, so a
/// long poll delay does not hold up an interrupt.
pub(crate) fn sleep_interruptible(total: Duration) {
    let end = Instant::now() + total;
    while !crate::proc::shutting_down() {
        let left = end.saturating_duration_since(Instant::now());
        if left.is_zero() {
            break;
        }
        std::thread::sleep(left.min(Duration::from_secs(1)));
    }
}

/// Wait for the OBS remote build to complete, fetch the log, and download
/// built RPMs on success.
///
/// The caller must commit the package to OBS before calling this (see `commit`).
/// Polls the OBS build status (every 15 seconds, less often while it holds
/// still; see [`poll_delay`]), for at most two hours (the
/// `MAX_WAIT` below), then gives up with a timeout error. Emits a `Progress`
/// event each time the build status changes so the user can see progress.
pub fn obs_build(
    project: &str,
    pkg_name: &str,
    repo: &str,
    arch: &str,
    log_path: &Path,
    rpms_dir: &Path,
    emit: Emit<'_>,
) -> Result<(), Error> {
    build_with_retriggers(
        pkg_name,
        emit,
        BROKEN_RETRIGGER_PAUSE,
        || obs_build_once(project, pkg_name, repo, arch, log_path, rpms_dir, emit),
        || trigger_rebuild(project, pkg_name, repo, arch, emit),
    )
}

/// The retry loop of [`obs_build`], with the build and the re-trigger injected:
/// a `broken` verdict with no build log behind it is re-triggered up to
/// [`MAX_BROKEN_RETRIGGERS`] times, then stands as a plain `broken`. Any other
/// result is returned as it came.
fn build_with_retriggers(
    pkg_name: &str,
    emit: Emit<'_>,
    pause: Duration,
    mut once: impl FnMut() -> Result<(), Error>,
    mut rebuild: impl FnMut() -> Result<(), Error>,
) -> Result<(), Error> {
    let mut retriggers = 0;
    loop {
        match once() {
            Err(Error::NotActionable(msg)) if msg == BROKEN_UNCONFIRMED => {
                if retriggers >= MAX_BROKEN_RETRIGGERS {
                    return Err(Error::NotActionable(BROKEN_MESSAGE.to_string()));
                }
                retriggers += 1;
                emit.reporter.report(&Event::Warning {
                    step: emit.step,
                    message: format!(
                        "OBS says {pkg_name} is broken but has no build log to confirm it; \
                         treating it as transient and re-triggering the build \
                         ({retriggers}/{MAX_BROKEN_RETRIGGERS})"
                    ),
                });
                if let Err(e) = rebuild() {
                    emit.reporter.report(&Event::Warning {
                        step: emit.step,
                        message: format!("could not re-trigger the OBS build: {e}"),
                    });
                }
                if !pause.is_zero() {
                    sleep_interruptible(pause);
                }
            }
            other => return other,
        }
    }
}

/// The final message for a `broken` verdict.
const BROKEN_MESSAGE: &str = "OBS: package source is broken";
/// Internal marker: `broken` without a build log behind it (see [`obs_build`]).
const BROKEN_UNCONFIRMED: &str = "OBS: broken status could not be confirmed by a build log";
/// How often an unconfirmed `broken` verdict is re-triggered before it stands.
const MAX_BROKEN_RETRIGGERS: u32 = 3;
const BROKEN_RETRIGGER_PAUSE: Duration = Duration::from_secs(30);

/// Whether a `broken` verdict is backed by evidence: a non-empty build log.
fn broken_is_confirmed(log_path: &Path) -> bool {
    read_log_lossy(log_path).is_ok_and(|t| !t.trim().is_empty())
}

/// A build log as text, lossily: a log with one invalid UTF-8 byte is still
/// the evidence it was, and must not read as an empty or missing file.
fn read_log_lossy(path: &Path) -> std::io::Result<String> {
    fs::read(path).map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
}

/// The text of a build log that is the evidence for a failure. One that
/// cannot be read, or is empty (a failed `osc buildlog` leaves an empty file
/// behind), is an environment problem, reported as an I/O error.
fn usable_build_log(log_path: &Path) -> Result<String, Error> {
    read_log_lossy(log_path)
        .map_err(|e| e.to_string())
        .and_then(|text| {
            if text.trim().is_empty() {
                Err("the file is empty".to_string())
            } else {
                Ok(text)
            }
        })
        .map_err(|why| {
            Error::Io(std::io::Error::other(format!(
                "no usable build log at {}: {why}",
                log_path.display()
            )))
        })
}

/// Ask OBS to rebuild `pkg_name` (`osc rebuild`), which re-evaluates a source
/// it had flagged `broken`.
fn trigger_rebuild(
    project: &str,
    pkg_name: &str,
    repo: &str,
    arch: &str,
    emit: Emit<'_>,
) -> Result<(), Error> {
    check_obs_name("project", project)?;
    check_obs_name("pkg_name", pkg_name)?;
    check_obs_name("repo", repo)?;
    check_obs_name("arch", arch)?;
    let out = proc::capture_with_timeout(
        "osc",
        &["rebuild", project, pkg_name, repo, arch],
        &anywhere(),
        emit,
        OSC_ARTIFACT_TIMEOUT,
    )
    .map_err(during(format!(
        "rebuild {project} {pkg_name} {repo} {arch}"
    )))?;
    if !out.success() {
        return Err(Error::OscFailed {
            cmd: format!("rebuild {project} {pkg_name} {repo} {arch}"),
            code: out.code,
            stderr: out.stderr,
        });
    }
    Ok(())
}

/// Call `f` up to `attempts` times, pausing `pause` between failures.
fn retry<T, E>(
    attempts: u32,
    pause: Duration,
    mut f: impl FnMut() -> Result<T, E>,
) -> Result<T, E> {
    let mut n = 1;
    loop {
        match f() {
            Ok(v) => return Ok(v),
            Err(e) if n >= attempts => return Err(e),
            Err(_) => {
                n += 1;
                if !pause.is_zero() {
                    sleep_interruptible(pause);
                }
            }
        }
    }
}

/// `osc getbinaries` right after a green verdict can fail transiently.
const GETBINARIES_ATTEMPTS: u32 = 4;
const GETBINARIES_PAUSE: Duration = Duration::from_secs(20);

/// One package's `getbinaries`, `get` injected: retried up to `attempts` times,
/// and each try waits for free disk space first (a full disk fails the try with
/// code 28 without running `get`).
#[allow(clippy::too_many_arguments)]
fn fetch_with_retries(
    project: &str,
    name: &str,
    repo: &str,
    arch: &str,
    rpms_dir: &Path,
    emit: Emit<'_>,
    attempts: u32,
    pause: Duration,
    mut get: impl FnMut() -> Result<(), Error>,
) -> Result<(), Error> {
    retry(attempts, pause, || {
        if let Err(stderr) = crate::workspace::await_disk_space(rpms_dir, emit.reporter, emit.step)
        {
            return Err(Error::OscFailed {
                cmd: format!("getbinaries {project} {name} {repo} {arch}"),
                code: 28,
                stderr,
            });
        }
        get()
    })
}

/// Download the RPMs of `pkg_name` (and its multibuild flavors) into
/// `rpms_dir`, retrying a failing `osc getbinaries`. Returns how many of the
/// RPMs OBS lists landed.
pub fn download_built_rpms(
    project: &str,
    pkg_name: &str,
    repo: &str,
    arch: &str,
    rpms_dir: &Path,
    emit: Emit<'_>,
) -> Result<usize, Error> {
    let mut packages = vec![pkg_name.to_string()];
    packages.extend(multibuild_flavors(project, pkg_name, repo, arch));
    let mut downloaded_count = 0;
    for name in &packages {
        let listing = binary_rpm_listing(project, name, repo, arch);
        if let Some(names) = &listing {
            remove_superseded_rpms(rpms_dir, names);
        }
        // A flavor `pkg:flavor` ships RPMs named `pkg-flavor-<version>…`.
        let file_prefix = name.replace(':', "-");
        let started = SystemTime::now();
        fetch_with_retries(
            project,
            name,
            repo,
            arch,
            rpms_dir,
            emit,
            GETBINARIES_ATTEMPTS,
            GETBINARIES_PAUSE,
            || get_binaries(project, name, repo, arch, rpms_dir, emit),
        )?;
        stash_rpmlint_log(rpms_dir, name);
        downloaded_count +=
            downloaded_rpm_count(listing.as_deref(), rpms_dir, &file_prefix, started);
    }
    Ok(downloaded_count)
}

/// Directory under `rpms_dir` holding the `rpmlint.log` OBS published for each
/// package (and multibuild flavor) downloaded, until the pipeline records it.
const RPMLINT_STASH: &str = ".rpmlint";

/// `osc getbinaries` writes OBS's `rpmlint.log` next to the RPMs, where the next
/// package's download overwrites it. Move it to a file of its own for `name`.
fn stash_rpmlint_log(rpms_dir: &Path, name: &str) {
    let log = rpms_dir.join("rpmlint.log");
    if !log.is_file() {
        return;
    }
    let dir = rpms_dir.join(RPMLINT_STASH);
    if fs::create_dir_all(&dir).is_ok() {
        let _ = fs::rename(&log, dir.join(format!("{name}.log")));
    }
}

/// The stashed rpmlint logs of `pkg` and its multibuild flavors
/// (`pkg:flavor`), as `(name, text)`, removed from the stash.
pub fn take_rpmlint_logs(rpms_dir: &Path, pkg: &str) -> Vec<(String, String)> {
    let Ok(rd) = fs::read_dir(rpms_dir.join(RPMLINT_STASH)) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for e in rd.flatten() {
        let file = e.file_name().to_string_lossy().into_owned();
        let Some(name) = file.strip_suffix(".log") else {
            continue;
        };
        if (name == pkg || name.strip_prefix(pkg).is_some_and(|r| r.starts_with(':')))
            // Bytes, lossily: a log with one invalid byte is still a log, and a
            // file that never reads would stay in the stash for ever.
            && let Ok(bytes) = fs::read(e.path())
        {
            let _ = fs::remove_file(e.path());
            out.push((
                name.to_string(),
                String::from_utf8_lossy(&bytes).into_owned(),
            ));
        }
    }
    out.sort();
    out
}

/// The error lines of an rpmlint log (`pkg.rpm: E: rule detail`).
pub fn rpmlint_errors(log: &str) -> Vec<String> {
    log.lines()
        .filter(|l| l.contains(": E: "))
        .map(|l| l.trim().to_string())
        .collect()
}

fn obs_build_once(
    project: &str,
    pkg_name: &str,
    repo: &str,
    arch: &str,
    log_path: &Path,
    rpms_dir: &Path,
    emit: Emit<'_>,
) -> Result<(), Error> {
    let reporter = emit.reporter;
    let step = emit.step;
    // The caller has committed already: no `_result` document fetched before
    // this may be used for the status (see `get_build_status_after_commit`).
    let committed = Instant::now();
    ensure_builds_for(project, repo, arch)?;
    const MAX_WAIT: Duration = Duration::from_secs(4 * 3600);
    // Time spent `blocked` (waiting for other packages to build) does not
    // count against MAX_WAIT while the blocker is visibly moving, i.e. its
    // details changed within BLOCK_STALL; the wait as a whole is still
    // bounded by MAX_TOTAL_WAIT.
    const BLOCK_STALL: Duration = Duration::from_secs(600);
    const MAX_TOTAL_WAIT: Duration = Duration::from_secs(6 * 3600);

    let start = Instant::now();
    let mut prev_status = String::new();

    // A single `osc` hiccup (network blip, OBS 5xx, rate limit) during a
    // build that can run for many minutes must not abort the wait and throw
    // the build away: tolerate a few consecutive poll failures, then give up.
    const MAX_CONSECUTIVE_POLL_ERRORS: u32 = 8;
    let mut poll_errors: u32 = 0;
    // Polls in a row that saw the same status; the delay between polls grows
    // with it (see `poll_delay`).
    let mut unchanged_polls: u32 = 0;

    // The previous mechanism only ever broke silence for one specific case
    // (an `Unknown("stale: ...")` verdict). A plain non-terminal status
    // (`building`, `scheduled`, ...) that simply never changes again —
    // because the XML `_result` document this reads is serving a stale
    // snapshot while OBS has actually moved on — produced no `Progress`
    // event (nothing to report: the string didn't change) and no `Warning`
    // either, so the wait looked indistinguishable from a healthy long
    // build for up to the full `MAX_WAIT`. Reported live: the TUI sat on an
    // unchanging "building" for 1000+ seconds while OBS itself showed
    // nothing in progress.
    const NO_PROGRESS_WARNING: Duration = Duration::from_secs(120);
    let mut last_change = Instant::now();
    let mut last_warning = Instant::now();
    let mut last_recheck: Option<Instant> = None;
    let mut last_table_label = String::new();
    const UNRESOLVABLE_CONFIRM: Duration = Duration::from_secs(15);
    let mut unresolvable_since: Option<Instant> = None;
    // A repository/arch the project does not build for has no result at all,
    // which would otherwise read as "not started yet" until MAX_WAIT. A
    // freshly committed package usually shows up within seconds, but a busy
    // scheduler can take minutes: after this long the project meta is
    // consulted again, and only a repository/arch it lacks ends the wait.
    const NO_RESULTS_LIMIT: Duration = Duration::from_secs(600);
    let mut no_results_since: Option<Instant> = None;
    // A built verdict whose job history cannot be read at all is accepted
    // after this long (see `obs_status::accept_unconfirmed`).
    const UNCONFIRMED_LIMIT: Duration = Duration::from_secs(300);
    let mut unconfirmed_since: Option<Instant> = None;
    let mut blocked_credit = Duration::ZERO;
    let mut last_tick = Instant::now();
    let mut last_block_change = Instant::now();
    let mut was_blocked = false;

    let status = loop {
        if crate::proc::shutting_down() {
            return Err(Error::OscFailed {
                cmd: format!("wait {project}/{pkg_name}"),
                code: -1,
                stderr: "interrupted".to_string(),
            });
        }
        let tick = last_tick.elapsed();
        last_tick = Instant::now();
        if was_blocked && last_block_change.elapsed() < BLOCK_STALL {
            blocked_credit += tick;
        }
        let waited = start.elapsed();
        if waited.saturating_sub(blocked_credit) >= MAX_WAIT || waited >= MAX_TOTAL_WAIT {
            // Which limit tripped: the total one only when it did so alone.
            let limit = if waited >= MAX_TOTAL_WAIT {
                MAX_TOTAL_WAIT
            } else {
                MAX_WAIT
            };
            return Err(Error::OscFailed {
                cmd: format!("wait {project}/{pkg_name}"),
                code: -1,
                stderr: format!(
                    "timed out after {}s (limit {}s) waiting for OBS build (last status: {})",
                    waited.as_secs(),
                    limit.as_secs(),
                    if prev_status.is_empty() {
                        "none"
                    } else {
                        &prev_status
                    }
                ),
            });
        }

        let status = match get_build_status_after_commit(project, pkg_name, repo, arch, committed) {
            Ok(s) => {
                poll_errors = 0;
                s
            }
            Err(e) => {
                poll_errors += 1;
                if poll_errors >= MAX_CONSECUTIVE_POLL_ERRORS {
                    return Err(Error::OscFailed {
                        cmd: format!("wait {project}/{pkg_name} ({repo}/{arch})"),
                        code: -1,
                        stderr: format!(
                            "gave up after {poll_errors} consecutive failed status polls; last error: {e}"
                        ),
                    });
                }
                reporter.report(&Event::Warning {
                    step,
                    message: format!(
                        "OBS status poll failed ({poll_errors}/{MAX_CONSECUTIVE_POLL_ERRORS}), retrying: {e}"
                    ),
                });
                sleep_interruptible(poll_delay(unchanged_polls, poll_errors, jitter()));
                continue;
            }
        };
        if status == BuildStatus::Unknown("no results".to_string()) {
            let since = *no_results_since.get_or_insert_with(Instant::now);
            if since.elapsed() >= NO_RESULTS_LIMIT {
                // The project meta decides: a repository/arch it does not
                // build is a dead end, anything else is a slow scheduler
                // and MAX_WAIT bounds the wait.
                ensure_builds_for(project, repo, arch)?;
                reporter.report(&Event::Warning {
                    step,
                    message: format!(
                        "OBS has reported no build result for {pkg_name} on {repo}/{arch} \
                         for {}s although {project} builds for it; the scheduler is slow, \
                         still waiting",
                        since.elapsed().as_secs()
                    ),
                });
                no_results_since = Some(Instant::now());
            }
        } else {
            no_results_since = None;
        }
        // A verdict no job history could confirm (empty or evicted history,
        // failing API calls) is not worth waiting on forever: settle for it.
        let status = match obs_status::accept_unconfirmed(&status) {
            Some(accepted) => {
                let since = *unconfirmed_since.get_or_insert_with(Instant::now);
                if since.elapsed() >= UNCONFIRMED_LIMIT {
                    reporter.report(&Event::Warning {
                        step,
                        message: format!(
                            "no OBS job history to confirm the {accepted} result for \
                             {pkg_name} after {}s; accepting it unconfirmed",
                            since.elapsed().as_secs()
                        ),
                    });
                    accepted
                } else {
                    status
                }
            }
            None => {
                unconfirmed_since = None;
                status
            }
        };
        let status_str = status.to_string();
        // What blocks or delays the package is part of the status: a change
        // in it is progress worth showing, and keeps the polling brisk.
        let label = match status {
            BuildStatus::Blocked | BuildStatus::Scheduled => {
                match build_details(project, pkg_name, repo, arch) {
                    Some(d) => format!("{status_str}: {d}"),
                    None => status_str.clone(),
                }
            }
            _ => status_str.clone(),
        };
        was_blocked = status == BuildStatus::Blocked;

        let mut status = status;
        if label == prev_status {
            unchanged_polls = unchanged_polls.saturating_add(1);
        } else {
            unchanged_polls = 0;
        }
        if label != prev_status {
            reporter.report(&Event::Progress {
                step,
                message: format!("OBS build status: {label}"),
            });
            if was_blocked && resets_block_timer(&prev_status) {
                last_block_change = Instant::now();
            }
            prev_status = label;
            last_change = Instant::now();
            last_warning = Instant::now();
            last_recheck = None;
        } else if !status.is_terminal() {
            // Non-terminal and unchanged: make sure the user can always
            // tell this is still alive and polling, not silently wedged.
            if last_warning.elapsed() >= NO_PROGRESS_WARNING {
                reporter.report(&Event::Warning {
                    step,
                    message: format!(
                        "OBS build status still {prev_status} after {}s with no change",
                        last_change.elapsed().as_secs()
                    ),
                });
                last_warning = Instant::now();
            }
            // An independent cross-check, via a different OBS endpoint than
            // whichever one produced `status` (see
            // `get_build_status_from_results_table`'s doc comment) — catches
            // one of them silently serving a stale snapshot while the other
            // has already moved on. Once per stuck period, or, while the
            // status only says the newest sources are not built yet, soon
            // and then every minute (see `recheck_due`).
            if recheck_due(
                obs_status::awaiting_new_revision(&status),
                last_change.elapsed(),
                last_recheck.map(|t| t.elapsed()),
            ) {
                last_recheck = Some(Instant::now());
                if let Ok(table_status) = get_build_status_from_results_table(
                    project,
                    pkg_name,
                    repo,
                    arch,
                    Emit::quiet(),
                ) {
                    let table_str = table_status.to_string();
                    match obs_status::cross_check(&status, &table_status) {
                        obs_status::CrossCheck::Agrees | obs_status::CrossCheck::Ignore => {}
                        obs_status::CrossCheck::NewRevisionSeen => {
                            // The scheduler works on the new revision: the
                            // wait is progressing, so it polls briskly again.
                            unchanged_polls = 0;
                            if table_str != last_table_label {
                                reporter.report(&Event::Progress {
                                    step,
                                    message: format!(
                                        "OBS has taken up the new revision: {table_str}"
                                    ),
                                });
                                last_table_label = table_str;
                            }
                        }
                        obs_status::CrossCheck::Adopt => {
                            reporter.report(&Event::Warning {
                                step,
                                message: format!(
                                    "OBS status disagreement after {}s of no change: the API \
                                     document says {status_str}, but `osc results` says \
                                     {table_str} — trusting the independent read",
                                    last_change.elapsed().as_secs()
                                ),
                            });
                            reporter.report(&Event::Progress {
                                step,
                                message: format!("OBS build status: {table_str}"),
                            });
                            prev_status = table_str;
                            last_change = Instant::now();
                            last_warning = Instant::now();
                            last_recheck = None;
                            unchanged_polls = 0;
                            status = table_status;
                        }
                    }
                }
            }
        }

        // `unresolvable` is only the scheduler's current view: a dependency
        // published a moment later turns it back into `scheduled`. Accept it
        // as the answer only once a later poll agrees (UNRESOLVABLE_CONFIRM is
        // one poll interval, so a genuine verdict costs a single extra poll).
        if status == BuildStatus::Unresolvable {
            let since = *unresolvable_since.get_or_insert_with(Instant::now);
            if since.elapsed() >= UNRESOLVABLE_CONFIRM {
                break status;
            }
        } else {
            unresolvable_since = None;
            if status.is_terminal() {
                break status;
            }
        }

        // `stale: <verdict>` is not a final answer: that terminal result is
        // from a source revision the server may already have superseded, so
        // keep polling. Only a verdict confirmed for the current sources
        // (see `obs_status::resolve`) ends the wait; if none arrives, the
        // MAX_WAIT check above reports a timeout error.
        sleep_interruptible(poll_delay(
            wait_polls(unchanged_polls, obs_status::awaiting_new_revision(&status)),
            0,
            jitter(),
        ));
    };

    // Fetch the log regardless of outcome — partial logs help diagnose failures.
    // A missing or stale log leaves the failure without its cause, so say so.
    // Announce the command that really writes `log_path`; the polling above
    // goes through the API and runs no `osc results --watch`.
    //
    // A multibuild package whose own entry built but whose flavor did not has
    // the flavor's log as the evidence: the package's would show a success and
    // send the fix agent after a failure it cannot see.
    let log_pkg = if status.is_success() {
        pkg_name.to_string()
    } else {
        failing_result(project, pkg_name, repo, arch, &status)
    };
    if log_pkg != pkg_name {
        reporter.report(&Event::Warning {
            step,
            message: format!(
                "{pkg_name} itself built, but its multibuild flavor {log_pkg} did not: \
                 the build log is {log_pkg}'s"
            ),
        });
    }
    reporter.report(&Event::Command {
        step,
        program: "osc".to_string(),
        argv: vec![
            "buildlog".to_string(),
            project.to_string(),
            log_pkg.clone(),
            repo.to_string(),
            arch.to_string(),
        ],
        cwd: anywhere(),
        log: Some(log_path.to_path_buf()),
    });
    if let Err(e) = fetch_buildlog(project, &log_pkg, repo, arch, log_path, Emit::quiet()) {
        reporter.report(&Event::Warning {
            step,
            message: format!("could not fetch the OBS build log for {log_pkg}: {e}"),
        });
    }
    let log_bytes = log_path.metadata().map(|m| m.len()).unwrap_or(0);
    let elapsed = start.elapsed();

    if status.is_success() {
        // Download the built RPMs. A succeeded status followed by a failing
        // getbinaries must not still return success with the RPM directory
        // empty, so this call is fatal: a succeeded OBS build without any
        // locally-downloaded artifacts is NOT a success a dependent
        // build can rely on — `rpm_satisfies_dep` can't find anything,
        // the dep is reported unresolvable even though OBS says the
        // source is green, and the dependent spends attempts chasing a
        // nonexistent problem. Promote this to fatal: if OBS says
        // succeeded but we cannot pull the binaries, surface that as a
        // build failure.
        //
        // We also verify that at least one `.rpm` landed under
        // `rpms_dir` after the fetch — `osc getbinaries` can exit 0
        // with no files written when the project is misconfigured
        // (empty binary set for this repo/arch pair), which the warning
        // version silently papered over.
        let fetch_result = download_built_rpms(project, pkg_name, repo, arch, rpms_dir, emit);
        let downloaded_count = fetch_result.as_ref().copied().unwrap_or(0);
        if let Err(e) = fetch_result {
            reporter.report(&Event::CommandDone {
                step,
                code: 1,
                elapsed,
                stdout_bytes: log_bytes,
                stderr_bytes: 0,
            });
            let advice = if e.is_disk_full() {
                // The build is fine; only this machine could not take the files.
                "The build itself succeeded: free disk space and run again."
            } else {
                "A green source without downloadable artifacts is not a usable \
                 build — a dependent package would see every BuildRequires as \
                 unresolvable. Re-check the OBS project's binary publishing config \
                 for repo={repo}/arch={arch}."
            };
            return Err(Error::NotActionable(format!(
                "OBS reported {pkg_name} {DOWNLOAD_FAILED}: {e}. {}",
                advice.replace("{repo}", repo).replace("{arch}", arch)
            )));
        }
        if downloaded_count == 0 {
            reporter.report(&Event::CommandDone {
                step,
                code: 1,
                elapsed,
                stdout_bytes: log_bytes,
                stderr_bytes: 0,
            });
            return Err(Error::NotActionable(format!(
                "OBS reported {pkg_name} succeeded and `osc getbinaries` exited 0, \
                 but no .rpm files appeared under {}. The project's binary \
                 publishing for repo={repo}/arch={arch} is probably misconfigured \
                 (empty binary set) — check the OBS <publish>/<binaries> settings.",
                rpms_dir.display()
            )));
        }
        reporter.report(&Event::CommandDone {
            step,
            code: 0,
            elapsed,
            stdout_bytes: log_bytes,
            stderr_bytes: 0,
        });
        return Ok(());
    }

    reporter.report(&Event::CommandDone {
        step,
        code: 1,
        elapsed,
        stdout_bytes: log_bytes,
        stderr_bytes: 0,
    });

    Err(match status {
        BuildStatus::Unresolvable => {
            let (names, detail) = current_unresolvable(project, &log_pkg, repo, arch, log_path);
            Error::UnresolvableDeps { names, detail }
        }
        BuildStatus::Broken if !broken_is_confirmed(log_path) => {
            Error::NotActionable(BROKEN_UNCONFIRMED.to_string())
        }
        BuildStatus::Broken => {
            Error::NotActionable(match build_details(project, &log_pkg, repo, arch) {
                Some(d) => format!("{BROKEN_MESSAGE}: {d}"),
                None => BROKEN_MESSAGE.to_string(),
            })
        }
        BuildStatus::Disabled => {
            Error::NotActionable("OBS: build is disabled in project config".to_string())
        }
        BuildStatus::Excluded => {
            Error::NotActionable("OBS: build is excluded in project config".to_string())
        }
        _ => {
            // The log is the only evidence for what went wrong. One that
            // cannot be read, or is empty (a failed `osc buildlog` leaves an
            // empty file behind), is an environment problem: classifying it
            // would send the package to the fix agent as a BuildFailed.
            let log_text = usable_build_log(log_path)?;
            match classify_build_failure(&log_text) {
                FailureKind::NoRepositories => Error::NotActionable(
                    "OBS project has no build repositories — configure them in OBS first"
                        .to_string(),
                ),
                FailureKind::NotWorkingCopy => {
                    Error::NotActionable("osc checkout is not server-backed".to_string())
                }
                FailureKind::UnresolvableDeps { names, detail } => {
                    Error::UnresolvableDeps { names, detail }
                }
                FailureKind::BuildFailed => Error::BuildFailed {
                    spec: package_spec_name(project, pkg_name, emit),
                    code: 1,
                    log: log_path.to_owned(),
                },
            }
        }
    })
}

#[cfg(test)]
mod package_listing_tests;

#[cfg(test)]
mod obs_name_tests;

#[cfg(test)]
mod listing_failure_tests;

#[cfg(test)]
mod count_rpms_tests;

#[cfg(test)]
mod project_build_statuses_tests;

#[cfg(test)]
mod project_repo_paths_tests;

#[cfg(test)]
mod rpmspec_sandbox_tests;

#[cfg(test)]
mod publish_plan_tests;

#[cfg(test)]
mod service_validation_tests;

#[cfg(test)]
mod dash_name_tests;

#[cfg(test)]
mod repo_arch_tests;

#[cfg(test)]
mod poll_tests;

#[cfg(test)]
mod overall_status_tests;

#[cfg(test)]
mod superseded_rpm_tests;

#[cfg(test)]
mod redist_name_tests;

#[cfg(test)]
mod transient_broken_tests;

#[cfg(test)]
mod transient_error_tests;
