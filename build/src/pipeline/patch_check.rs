//! Checking patches: the stale and applied patches, the patch check against the
//! sources, and the escapes of a patch from its tree.

use super::*;
use crate::pipeline::archive::*;
use crate::pipeline::tarball::*;

/// The tree prefix a patch was made for when it names another release: the first
/// path component of its diff names is `<name>-<version>` with a version other
/// than `version`, and not the directory `top_level` the sources unpack to.
/// `a`, `b`, and the right directory are fine. `None` for a patch that fits.
pub(super) fn stale_patch_prefix(diff: &str, top_level: &str, version: &str) -> Option<String> {
    diff.lines()
        .filter_map(|l| l.strip_prefix("--- ").or_else(|| l.strip_prefix("+++ ")))
        .map(|n| n.split('\t').next().unwrap_or("").trim())
        .filter(|n| *n != "/dev/null")
        // A git diff prefixes its names with `a/` and `b/`: the directory that
        // names the release is the one under them.
        .filter_map(|n| {
            let n = n
                .strip_prefix("a/")
                .or_else(|| n.strip_prefix("b/"))
                .unwrap_or(n);
            n.split_once('/').map(|(first, _)| first)
        })
        .find(|first| {
            let Some((name, ver)) = first.rsplit_once('-') else {
                return false;
            };
            *first != top_level
                && !name.is_empty()
                && RELEASE_DIR_VERSION.is_match(ver)
                && ver != version
        })
        .map(str::to_string)
}

/// The patches `spec` declares whose diff is for another release's tree (a patch
/// inherited from a linked Factory package, python-torch's 2.3.1 one against
/// 2.14): `(file name, prefix it was made for)`. The agent cannot make such a
/// patch apply, and chasing it cost eleven rejected edits.
pub(crate) fn stale_patches(
    spec: &str,
    checkout: &Path,
    top_level: &str,
    version: &str,
) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for line in spec.lines() {
        let Some(patch) = crate::spec::parse_source_line(line, &["patch"], str::to_string) else {
            continue;
        };
        // Expanded as try_patches expands it (`foo-%{version}.patch`), so the
        // file the spec really names is judged.
        let name = expand_patch_name(&patch.url, spec);
        if name.is_empty()
            || name.contains('%')
            || Path::new(&name).file_name() != Some(name.as_ref())
        {
            continue;
        }
        if let Ok(diff) = std::fs::read(checkout.join(&name))
            && let Some(prefix) =
                stale_patch_prefix(&String::from_utf8_lossy(&diff), top_level, version)
        {
            out.push((name, prefix));
        }
    }
    out
}

/// The patch number a `%patch` line applies, or `None` for any other line.
/// Covers `%patchN`, `%patch -PN`, `%patch -P N` and the bare `%patch`, which
/// applies patch 0 as rpm does.
pub(crate) fn applied_patch_number(line: &str) -> Option<String> {
    let rest = line.trim_start().strip_prefix("%patch")?;
    let digits_len = rest
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(rest.len());
    let (digits, after) = rest.split_at(digits_len);
    if !digits.is_empty() {
        // `%patchN`: the number is glued to the macro.
        let ends_macro =
            after.is_empty() || after.starts_with(char::is_whitespace) || after.starts_with('-');
        return ends_macro.then(|| digits.to_string());
    }
    // Any option word may carry the patch number: `-PN` or `-P N`, anywhere
    // among the options (`-p1 -P 2`, `-P 2 -p1`).
    let words: Vec<&str> = after.split_whitespace().collect();
    for (i, word) in words.iter().enumerate() {
        if let Some(num_part) = word.strip_prefix("-P") {
            let digits = if num_part.is_empty() {
                words.get(i + 1).copied().unwrap_or("")
            } else {
                num_part
            };
            let n: String = digits.chars().take_while(|c| c.is_ascii_digit()).collect();
            return (!n.is_empty()).then_some(n);
        }
    }
    // No `-P` at all: the bare `%patch` applies patch 0.
    let after = after.trim_start();
    (after.is_empty() || after.starts_with('-')).then(|| "0".to_string())
}

/// Whether two patch numbers name the same patch: `Patch01:` and `%patch1` are
/// both patch 1. Numbers that do not parse compare as text.
pub(crate) fn same_patch_number(a: &str, b: &str) -> bool {
    match (a.parse::<u32>(), b.parse::<u32>()) {
        (Ok(x), Ok(y)) => x == y,
        _ => a == b,
    }
}

/// `spec` without the `PatchN:` lines (and `%patchN` applications) of `names`.
pub(super) fn without_patches(spec: &str, names: &[String]) -> String {
    let mut nums: Vec<String> = Vec::new();
    let mut out = String::new();
    for line in spec.lines() {
        if let Some(patch) =
            crate::spec::parse_source_line(line, &["patch"], |v| expand_patch_name(v, spec))
            && names.contains(&patch.url)
        {
            // A bare `Patch:` is patch 0, as rpm reads it.
            nums.push(patch.index.to_string());
            continue;
        }
        out.push_str(line);
        out.push('\n');
    }
    if nums.is_empty() {
        return spec.to_string();
    }
    let mut kept = String::new();
    for line in out.lines() {
        if !applied_patch_number(line)
            .is_some_and(|n| nums.iter().any(|m| same_patch_number(m, &n)))
        {
            kept.push_str(line);
            kept.push('\n');
        }
    }
    kept
}

/// What trying the spec's patches against the unpacked source found.
#[derive(Debug, Default, PartialEq)]
pub(super) struct PatchCheck {
    /// Patches that do not apply, in spec order, each with `patch`'s complaint.
    /// Later patches were tried on a tree without them.
    pub(super) failed: Vec<(String, String)>,
    /// What could not be checked (the source archive would not unpack), which
    /// says nothing about the patches themselves.
    pub(super) unchecked: Option<String>,
}

/// The `-pN` strip level the spec applies patch `number` with (`"0"` for a bare
/// `Patch:`): the `%patchN` line that applies it when that gives one, else the
/// `%autosetup`/`%autopatch` line, else 1, the default of both macros. Only the
/// macro lines are read, so a `-p1` anywhere else in the spec is not a level.
pub(crate) fn spec_patch_level(spec: &str, number: &str) -> u32 {
    // rpm takes both `-p0` and the spaced `-p 0` (`%autosetup -p 0` is level 0).
    let level_of = |line: &str| -> Option<u32> {
        let mut toks = line.split_whitespace().skip(1);
        while let Some(tok) = toks.next() {
            let level = if tok == "-p" {
                toks.next().and_then(|t| t.parse::<u32>().ok())
            } else {
                tok.strip_prefix("-p").and_then(|t| t.parse::<u32>().ok())
            };
            if level.is_some() {
                return level;
            }
        }
        None
    };
    let mut general = None;
    for line in spec.lines() {
        let first = line.split_whitespace().next().unwrap_or("");
        // The `%patch` line that applies `number`, in any of its forms (see
        // `applied_patch_number`): `%patchN`, `%patch -PN`, `%patch -P N`, or
        // the bare `%patch` for patch 0.
        if first.starts_with("%patch")
            && applied_patch_number(line).is_some_and(|n| same_patch_number(&n, number))
        {
            if let Some(level) = level_of(line) {
                return level;
            }
        } else if general.is_none() && matches!(first, "%autosetup" | "%autopatch" | "%setup") {
            general = level_of(line);
        }
    }
    general.unwrap_or(1)
}

/// Try the spec's `PatchN:` files, in order, on a scratch copy of the source
/// tarball the way `%autosetup -p1` will: each is first run with
/// `patch -p1 --dry-run`, and only one that passes is applied, so a patch that
/// fails costs the ones after it nothing. Nothing is checked when there are no
/// patches, no tarball, or the spec has no `%autosetup`; a patch the spec
/// applies with another `-pN` is reported as not checked. `Err` is for
/// failures of the check itself (no scratch directory, no `patch` binary) with
/// nothing found failing before it.
pub(super) fn try_patches(spec_path: &Path, spec: &str) -> Result<PatchCheck, String> {
    // (patch number, file name): a bare `Patch:` is number 0.
    let patches: Vec<(String, String)> = spec
        .lines()
        .filter_map(|l| crate::spec::parse_source_line(l, &["patch"], str::to_string))
        .map(|patch| (patch.index.to_string(), patch.url.trim().to_string()))
        .collect();
    if patches.is_empty() || !spec.contains("%autosetup") {
        return Ok(PatchCheck::default());
    }
    // Each patch with the `-pN` level the spec applies it with. Only a `-p1`
    // patch is checked here: the dry run below is a `-p1` run, and the other
    // levels would need their own path rules.
    let patches: Vec<(String, String, u32)> = patches
        .into_iter()
        .map(|(number, file)| {
            let level = spec_patch_level(spec, &number);
            (number, file, level)
        })
        .collect();
    let Some(checkout) = spec_path.parent() else {
        return Ok(PatchCheck::default());
    };
    let version = crate::spec::parse_header(spec).map(|h| h.version);
    let Some(tarball) = find_tarball(checkout, version.as_deref().unwrap_or_default()) else {
        return Ok(PatchCheck::default());
    };
    let scratch = tempfile::tempdir().map_err(|e| e.to_string())?;
    // Unpack only what the patches touch: `patch` reads nothing else, and
    // unpacking the whole sdist for every build is the cost to avoid.
    let mut targets: Vec<String> = Vec::new();
    for (_, file, level) in &patches {
        if *level != 1 {
            continue;
        }
        let name = expand_patch_name(file, spec);
        // Only a bare file name is a patch in this checkout (see below); a
        // `../x` must not be read from outside it.
        if Path::new(&name).file_name() != Some(name.as_ref()) {
            continue;
        }
        if let Ok(diff) = std::fs::read(checkout.join(&name)) {
            targets.extend(patch_targets(&String::from_utf8_lossy(&diff)));
        }
    }
    let wanted = |p: &Path| targets.iter().any(|t| p.ends_with(t));
    if let Err(e) = ap_ecosystem::sdist::extract_where(&tarball, scratch.path(), wanted) {
        return Ok(PatchCheck {
            failed: Vec::new(),
            unchecked: Some(format!(
                "could not unpack {} to test the patches, so they were not checked: {e}",
                tarball.display()
            )),
        });
    }
    // A single top-level directory is what `%autosetup -n` enters.
    let entries: Vec<_> = std::fs::read_dir(scratch.path())
        .map_err(|e| e.to_string())?
        .filter_map(|e| e.ok())
        .collect();
    let root = match entries.as_slice() {
        [only] if only.path().is_dir() => only.path(),
        _ => scratch.path().to_path_buf(),
    };
    let mut check = PatchCheck::default();
    let mut notes: Vec<String> = Vec::new();
    for (_, raw, level) in &patches {
        let name = expand_patch_name(raw, spec);
        // Patch names come from spec text. Only a bare file name is a patch in
        // this checkout; `/etc/x` or `../x` would read outside it.
        if Path::new(&name).file_name() != Some(name.as_ref()) {
            continue;
        }
        if *level != 1 {
            notes.push(format!(
                "{name} applies with -p{level}, so it was not checked"
            ));
            continue;
        }
        let patch_file = checkout.join(&name);
        if !patch_file.is_file() {
            // Not on disk even after the macros are expanded: the patch is not
            // checked, and that is said rather than skipped silently.
            notes.push(format!("{name} is not on disk, so it was not checked"));
            continue;
        }
        // An absolute path: `patch` runs in the scratch tree, and a project
        // name with `:` in a relative path would read as a remote host to
        // some tools.
        let patch_file = std::fs::canonicalize(&patch_file).unwrap_or(patch_file);
        let run = |dry: bool| {
            let mut args: Vec<std::ffi::OsString> =
                ["-p1", "--fuzz=0", "-f", "-s", "--no-backup-if-mismatch"]
                    .iter()
                    .map(Into::into)
                    .collect();
            if dry {
                args.push("--dry-run".into());
            }
            args.push("-i".into());
            args.push(patch_file.clone().into_os_string());
            crate::proc::capture_with_timeout(
                "patch",
                &args,
                &root,
                Emit::quiet(),
                Duration::from_secs(120),
            )
            .map_err(|e| {
                let spec_name = spec_path.file_name().unwrap_or_default().to_string_lossy();
                format!("could not run patch for {name} ({spec_name}): {e}")
            })
        };
        let verdict = run(true).and_then(|dry| if dry.success() { run(false) } else { Ok(dry) });
        let out = match verdict {
            Ok(out) => out,
            // A patch the check could not run (no `patch`, a timeout) says
            // nothing about this one. The failures already found are still
            // failures, so they are returned with the rest said to be unchecked;
            // with none found the check itself is the error.
            Err(e) => {
                if check.failed.is_empty() {
                    return Err(e);
                }
                notes.push(format!("{e}; the patches from {name} on were not checked"));
                break;
            }
        };
        if !out.success() {
            let detail = out.stdout + &out.stderr;
            check
                .failed
                .push((name, detail.trim().chars().take(600).collect::<String>()));
        }
    }
    if !notes.is_empty() {
        check.unchecked = Some(notes.join("; "));
    }
    Ok(check)
}

/// A `PatchN:` value with the spec's `%{name}` and `%{version}` (or the bare
/// `%name`, `%version`) expanded, so `foo-%{version}-fix.patch` names the file
/// `foo-1.2-fix.patch`. A spec whose header cannot be read is left as written.
pub(crate) fn expand_patch_name(raw: &str, spec: &str) -> String {
    let raw = raw.trim();
    let Some(header) = crate::spec::parse_header(spec) else {
        return raw.to_string();
    };
    raw.replace("%{name}", &header.name)
        .replace("%name", &header.name)
        .replace("%{version}", &header.version)
        .replace("%version", &header.version)
}

/// [`try_patches`] as a gate for an edit: `Err` names the first patch that does
/// not apply. The `Ok` value is what could not be checked, to be reported.
/// `Err` only for a patch that does not apply. A check that could not run
/// comes back as `Ok(Some(..))`, the same "unchecked" note an archive that
/// will not unpack gets: it says nothing about the patches.
pub(super) fn check_patches_apply(spec_path: &Path, spec: &str) -> Result<Option<String>, String> {
    let check = match try_patches(spec_path, spec) {
        Ok(check) => check,
        Err(e) => return Ok(Some(format!("the patch check could not run: {e}"))),
    };
    match check.failed.first() {
        Some((name, detail)) => Err(format!(
            "{name} does not apply to the pristine source with `patch -p1 --fuzz=0`: {detail}"
        )),
        None => Ok(check.unchecked),
    }
}

/// Whether `path` starts with a home directory reference: `~`, `~/...` or
/// `~user/...`. A `~` followed by anything else (`~build.log`) is a plain name.
fn is_home_directory_path(path: &str) -> bool {
    let Some(rest) = path.strip_prefix('~') else {
        return false;
    };
    if rest.is_empty() || rest.starts_with('/') {
        return true;
    }
    match rest.split_once('/') {
        Some((user, _)) => {
            !user.is_empty()
                && user
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == '.')
        }
        None => false,
    }
}

/// The first path a patch's file headers name that would reach outside the
/// tree it is applied to: an absolute path, a `..` component, or a leading `~`.
/// Unified (`---`/`+++`), context (`***`/`---`), `Index:`, `diff --git` and
/// rename/copy headers are read. The `a/` or `b/` prefix and any timestamp after
/// a tab are stripped first, and `/dev/null` (the add/delete marker) is allowed. Returns a description of the
/// offending path, or `None` if every header stays inside the tree.
pub(super) fn patch_escapes_tree(patch_text: &str) -> Option<String> {
    let lines: Vec<&str> = patch_text.lines().collect();
    let mut raw_paths: Vec<&str> = Vec::new();
    for (i, line) in lines.iter().enumerate() {
        if let Some(rest) = line.strip_prefix("--- ") {
            // `---` opens a file header only when a `+++` follows it; anywhere
            // else it is a removed line of a hunk.
            if let Some(new) = lines.get(i + 1).and_then(|n| n.strip_prefix("+++ ")) {
                raw_paths.push(rest);
                raw_paths.push(new);
            }
        } else if let Some(rest) = line.strip_prefix("*** ") {
            // A context-diff file header is `*** old` followed by `--- new`. A
            // hunk's range line (`*** 1,3 ****`) is not a header.
            if !rest.trim_end().ends_with("****")
                && let Some(new) = lines.get(i + 1).and_then(|n| n.strip_prefix("--- "))
            {
                raw_paths.push(rest);
                raw_paths.push(new);
            }
        } else if let Some(rest) = line.strip_prefix("Index: ") {
            raw_paths.push(rest);
        } else if let Some(rest) = line.strip_prefix("diff --git ") {
            raw_paths.extend(rest.split_whitespace());
        } else {
            for key in ["rename from ", "rename to ", "copy from ", "copy to "] {
                if let Some(rest) = line.strip_prefix(key) {
                    raw_paths.push(rest);
                }
            }
        }
    }
    raw_paths.into_iter().find_map(|raw| {
        // A timestamp follows a tab; quotes are how git spells odd names.
        let raw = raw
            .split('\t')
            .next()
            .unwrap_or_default()
            .trim()
            .trim_matches('"');
        // `/dev/null` may carry a space-separated timestamp, so only its first
        // word is compared.
        let first_word = raw.split_whitespace().next().unwrap_or_default();
        if raw.is_empty() || first_word == "/dev/null" {
            return None;
        }
        // A `~` is a home directory only when it leads the path as written: after
        // an `a/` or `b/` prefix it is an ordinary name (`a/~build.log`).
        let (path, prefixed) = match raw.strip_prefix("a/").or_else(|| raw.strip_prefix("b/")) {
            Some(rest) => (rest, true),
            None => (raw, false),
        };
        let reason = if path.starts_with('/') {
            "an absolute path"
        } else if !prefixed && is_home_directory_path(path) {
            "a home-directory path"
        } else if path.split('/').any(|c| c == "..") {
            "a path with a `..` component"
        } else {
            return None;
        };
        Some(format!("header path `{path}` is {reason}"))
    })
}

/// What the checkout held before the turn, for deciding which of its patches
/// the turn answers for.
pub(super) enum PatchBaseline<'a> {
    /// The pre-turn snapshot. A patch the turn left byte-identical is not judged.
    Snapshot(&'a CheckoutSnapshot),
    /// No pre-turn copy, but OBS publishes these names (`.osc/_files`). A
    /// published patch is what was committed, not this run's edit, so only the
    /// unpublished patches are judged.
    NotPublished(Vec<String>),
    /// No pre-turn copy at all. The patches OBS publishes are not judged (they
    /// are what was committed, as with `NotPublished`, and are skipped without a
    /// note); every other patch is judged as the turn's doing.
    NoBaseline(Vec<String>),
}

/// The baseline for a call site: the snapshot when there is one, otherwise the
/// names OBS publishes for the checkout.
pub(super) fn patch_baseline<'a>(
    snapshot: Option<&'a CheckoutSnapshot>,
    spec_path: &Path,
) -> PatchBaseline<'a> {
    match snapshot {
        Some(snap) => PatchBaseline::Snapshot(snap),
        None => PatchBaseline::NoBaseline(
            spec_path
                .parent()
                .map(obs_tracked_names)
                .unwrap_or_default(),
        ),
    }
}

/// The file names OBS tracks for a checkout, from `.osc/_files`.
pub(super) fn obs_tracked_names(checkout: &Path) -> Vec<String> {
    let xml = std::fs::read_to_string(checkout.join(".osc").join("_files")).unwrap_or_default();
    xml.split("<entry ")
        .skip(1)
        .filter_map(|chunk| {
            let head = &chunk[..chunk.find('>').unwrap_or(chunk.len())];
            let start = head.find("name=\"")? + "name=\"".len();
            let end = start + head[start..].find('"')?;
            Some(head[start..end].to_string())
        })
        .collect()
}

/// The patch files the gate reads: every top-level `*.patch` or `*.diff` in the
/// checkout (only those in `publishable`, when given), plus each bare file name
/// the spec gives as a `PatchN:` line. Returns the first one that escapes the
/// tree and that the baseline makes the turn's concern: its file name, and the
/// problem as an error naming the file.
pub(super) fn patch_escape_offender(
    spec_path: &Path,
    spec: &str,
    baseline: &PatchBaseline<'_>,
    publishable: Option<&[String]>,
) -> Option<(String, String)> {
    let checkout = spec_path.parent()?;
    let mut names: Vec<String> = Vec::new();
    if let Ok(entries) = std::fs::read_dir(checkout) {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if (name.ends_with(".patch") || name.ends_with(".diff"))
                && entry.path().is_file()
                && publishable.is_none_or(|p| p.contains(&name))
            {
                names.push(name);
            }
        }
    }
    for line in spec.lines() {
        let Some(patch) = crate::spec::parse_source_line(line, &["patch"], str::to_string) else {
            continue;
        };
        let name = expand_patch_name(&patch.url, spec);
        // A name with a directory part is not a file of this checkout.
        if Path::new(&name).file_name() == Some(name.as_ref()) {
            names.push(name);
        }
    }
    names.sort();
    names.dedup();
    let published: &[String] = match baseline {
        PatchBaseline::Snapshot(_) => &[],
        PatchBaseline::NotPublished(p) | PatchBaseline::NoBaseline(p) => p,
    };
    names.into_iter().find_map(|name| {
        let is_published = published.contains(&name);
        let bytes = std::fs::read(checkout.join(&name)).ok()?;
        match baseline {
            PatchBaseline::Snapshot(snap) => {
                if snap.holds_unchanged(&name, &bytes) {
                    return None;
                }
            }
            PatchBaseline::NotPublished(_) | PatchBaseline::NoBaseline(_) => {
                if is_published {
                    return None;
                }
            }
        }
        let why = patch_escapes_tree(&String::from_utf8_lossy(&bytes))?;
        let origin = match baseline {
            PatchBaseline::Snapshot(_) => "was added or changed by this turn",
            PatchBaseline::NotPublished(_) => "is not published by OBS yet",
            PatchBaseline::NoBaseline(_) => "is a new or changed patch",
        };
        let problem = format!(
            "{name} {origin}, and its headers would write outside the source tree: {why}. \
             A patch may only name paths relative to the source tree: regenerate it with \
             paths like `a/src/x.c` and `b/src/x.c`, not absolute or `..` paths."
        );
        Some((name, problem))
    })
}

/// [`patch_escape_offender`] without the file name: the problem alone.
#[cfg(test)]
pub(super) fn patch_escape_problem(
    spec_path: &Path,
    spec: &str,
    baseline: &PatchBaseline<'_>,
) -> Option<String> {
    patch_escape_offender(spec_path, spec, baseline, None).map(|(_, problem)| problem)
}
