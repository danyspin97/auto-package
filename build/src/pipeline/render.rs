//! Rendering service files and the diffs and notes written from a run.

use super::*;

/// Render the OBS `_service` XML for a Rust package.
///
/// `github_url` is the project URL (without `.git`), `version` is the crate
/// version, and `pkg_name` is used as the `cargo_vendor` `src` directory name
/// (the directory `tar_scm` creates matches the repo name).
pub(super) fn render_service_file(github_url: &str, version: &str, pkg_name: &str) -> String {
    let git_url = if github_url.ends_with(".git") {
        github_url.to_string()
    } else {
        format!("{github_url}.git")
    };
    let repo_name = github_url
        .trim_end_matches('/')
        .rsplit('/')
        .next()
        .unwrap_or(pkg_name);

    // Every value lands in element text: a `&` or `<` in a URL or a tag would
    // make the file malformed or add elements.
    let (git_url, repo_name, version) = (
        crate::obs_xml::escape(&git_url),
        crate::obs_xml::escape(repo_name),
        crate::obs_xml::escape(version),
    );
    format!(
        "<services>\n\
           <service mode=\"manual\" name=\"tar_scm\">\n\
         \x20   <param name=\"url\">{git_url}</param>\n\
         \x20   <param name=\"versionformat\">@PARENT_TAG@</param>\n\
         \x20   <param name=\"scm\">git</param>\n\
         \x20   <param name=\"revision\">v{version}</param>\n\
         \x20   <param name=\"match-tag\">*</param>\n\
         \x20   <param name=\"versionrewrite-pattern\">v(\\d+\\.\\d+\\.\\d+)</param>\n\
         \x20   <param name=\"versionrewrite-replacement\">\\1</param>\n\
         \x20   <param name=\"changesgenerate\">enable</param>\n\
         \x20   <param name=\"changesauthor\"> YOUR EMAIL HERE </param>\n\
         \x20 </service>\n\
         \x20 <service mode=\"manual\" name=\"recompress\">\n\
         \x20   <param name=\"file\">*.tar</param>\n\
         \x20   <param name=\"compression\">zst</param>\n\
         \x20 </service>\n\
         \x20 <service mode=\"manual\" name=\"set_version\"/>\n\
         \x20 <service name=\"cargo_vendor\" mode=\"manual\">\n\
         \x20    <param name=\"src\">{repo_name}</param>\n\
         \x20    <param name=\"compression\">zst</param>\n\
         \x20    <param name=\"update\">true</param>\n\
         \x20 </service>\n\
         </services>\n"
    )
}

/// What to show for an `Unclassified` failure.
///
/// The old code just took `error_lines[0]`, which is empty for a log with no
/// rpm `error:` line or Python exception at all — common for OBS failures
/// that never produced a real build log (status `Broken`, `Excluded`, a
/// buildroot resolution that aborted before scriptlets started, or an osc
/// invocation that got no bytes back at all). Fall back through progressively
/// weaker signals so the TUI/CLI shows SOMETHING actionable instead of an
/// empty string that reads like a bug in extraction itself.
pub(super) fn unclassified_raw(extracted: &crate::log::ExtractedLog) -> String {
    if let Some(first) = extracted.error_lines.iter().find(|l| !l.trim().is_empty()) {
        return first.clone();
    }
    // The excerpt is already filtered to interesting content when the
    // extractor found a failure signal of ANY kind, so prefer it to the raw
    // tail.
    if let Some(f) = &extracted.failure {
        let trimmed = f.excerpt.trim();
        if !trimmed.is_empty() {
            return tail_lines(trimmed, 5);
        }
    }
    let tail = extracted.log_tail.trim();
    if tail.is_empty() {
        return "(no build log — OBS likely rejected the build before any \
                scriptlet ran; check `osc results` for the status)"
            .to_string();
    }
    tail_lines(tail, 5)
}

/// Keep the last `n` non-empty lines of `text`.
fn tail_lines(text: &str, n: usize) -> String {
    let lines: Vec<&str> = text
        .lines()
        .filter(|l| !l.trim().is_empty())
        .rev()
        .take(n)
        .collect();
    lines.into_iter().rev().collect::<Vec<_>>().join("\n")
}

pub(super) fn unified_diff(before: &str, after: &str) -> String {
    let mut d = String::new();
    for line in diff::lines(before, after) {
        match line {
            diff::Result::Left(l) => {
                d.push_str("- ");
                d.push_str(l);
                d.push('\n');
            }
            diff::Result::Right(r) => {
                d.push_str("+ ");
                d.push_str(r);
                d.push('\n');
            }
            diff::Result::Both(_, _) => {}
        }
    }
    d
}

/// Render the OBS `_service` XML for a maturin-backed Python package.
///
/// `normalized_name` is the PyPI package name with hyphens replaced by
/// underscores (e.g. `outlines_core`), matching the sdist tarball filename.
/// `extra_manifests` are further Cargo manifests (relative to the archive
/// root) to vendor alongside the root one — see [`maturin_manifest_path`].
///
/// `locked` is whether the sdist ships a `Cargo.lock`. Then the vendored registry
/// must match it: `update=true` runs `cargo update`, which re-resolves every
/// dependency, picks releases the crate was never built with (chromadb got a
/// `zip` that broke `zip-extract`) and fails outright when a requirement has
/// only yanked releases left (`zip = "^2"`).
pub(super) fn render_maturin_service_file(
    normalized_name: &str,
    extra_manifests: &[String],
    locked: bool,
    root_manifest: bool,
) -> String {
    // The manifest path comes from the sdist's pyproject.toml.
    let normalized_name = crate::obs_xml::escape(normalized_name);
    let update = !locked;
    let extra: String = extra_manifests
        .iter()
        .filter(|m| m.as_str() != "Cargo.toml")
        .map(|m| {
            format!(
                "         \x20    <param name=\"cargotoml\">{}</param>\n",
                crate::obs_xml::escape(m)
            )
        })
        .collect();
    let root = if root_manifest {
        "         \x20    <param name=\"cargotoml\">Cargo.toml</param>\n"
    } else {
        ""
    };
    format!(
        "<?xml version=\"1.0\"?>\n\
         <services>\n\
         \x20 <service mode=\"manual\" name=\"download_files\" />\n\
         \x20 <service name=\"cargo_vendor\" mode=\"manual\">\n\
         \x20    <param name=\"src\">{normalized_name}-*.tar.gz</param>\n\
         \x20    <param name=\"method\">registry</param>\n\
         \x20    <param name=\"update\">{update}</param>\n\
         \x20    <param name=\"compression\">zst</param>\n\
         \x20    <param name=\"no-root-manifest\">false</param>\n\
         {root}{extra}\
         \x20 </service>\n\
         </services>\n"
    )
}

/// The `[tool.maturin] manifest-path` of an sdist's root `pyproject.toml`,
/// relative to the archive root, when it names a manifest other than the root
/// `Cargo.toml`. `Ok(None)` when absent, default, or the manifest is itself a
/// member of the root `Cargo.toml`'s workspace; `Err` when the archive cannot
/// be unpacked (a refused link, a size limit, a corrupt file). The
/// root manifest's own `cargotoml` param already vendors every workspace
/// member through their one shared lockfile, and `obs-service-cargo` errors
/// out (bare `NotFound`, os error 2) when told to additionally vendor a
/// member path, since it looks for a lockfile next to that manifest instead
/// of at the workspace root.
pub(super) fn maturin_manifest_path(tarball: &Path) -> Result<Option<String>, String> {
    let scratch = tempfile::tempdir().map_err(|e| e.to_string())?;
    // Only the two root manifests are needed; unpacking the whole sdist for
    // them is what this does instead.
    ap_ecosystem::sdist::extract_where(tarball, scratch.path(), |p| {
        let depth = p
            .components()
            .filter(|c| matches!(c, std::path::Component::Normal(_)))
            .count();
        depth <= 2
            && matches!(
                p.file_name().and_then(|n| n.to_str()),
                Some("pyproject.toml" | "Cargo.toml")
            )
    })
    .map_err(|e| e.to_string())?;
    Ok(maturin_manifest_in(scratch.path()))
}

/// The manifest path named by the unpacked sdist at `scratch`, if any (see
/// [`maturin_manifest_path`]).
fn maturin_manifest_in(scratch: &Path) -> Option<String> {
    let entries: Vec<_> = std::fs::read_dir(scratch)
        .ok()?
        .filter_map(|e| e.ok())
        .collect();
    let root = match entries.as_slice() {
        [only] if only.path().is_dir() => only.path(),
        _ => scratch.to_path_buf(),
    };
    let text = std::fs::read_to_string(root.join("pyproject.toml")).ok()?;
    let doc: toml::Value = toml::from_str(&text).ok()?;
    let path = doc
        .get("tool")?
        .get("maturin")?
        .get("manifest-path")?
        .as_str()?
        .trim_start_matches("./")
        .to_string();
    if path == "Cargo.toml" || path.starts_with('/') || path.contains("..") {
        return None;
    }
    if is_cargo_workspace_member(&root.join("Cargo.toml"), &path) {
        return None;
    }
    Some(path)
}

/// Whether `manifest_rel` (relative to the archive root) names a path listed
/// in `root_cargo_toml`'s `[workspace] members` (plain entries and simple
/// `dir/*` globs — the forms Cargo itself supports for member lists).
fn is_cargo_workspace_member(root_cargo_toml: &Path, manifest_rel: &str) -> bool {
    let Some(member_dir) = Path::new(manifest_rel).parent() else {
        return false;
    };
    let Ok(text) = std::fs::read_to_string(root_cargo_toml) else {
        return false;
    };
    let Ok(doc) = toml::from_str::<toml::Value>(&text) else {
        return false;
    };
    let Some(workspace) = doc.get("workspace") else {
        return false;
    };
    let Some(members) = workspace.get("members").and_then(|m| m.as_array()) else {
        return false;
    };
    let excluded = workspace
        .get("exclude")
        .and_then(|e| e.as_array())
        .is_some_and(|a| {
            a.iter()
                .filter_map(|v| v.as_str())
                .any(|p| Path::new(p) == member_dir)
        });
    if excluded {
        return false;
    }
    members
        .iter()
        .filter_map(|m| m.as_str())
        .any(|pattern| match pattern.strip_suffix("/*") {
            Some(prefix) => member_dir.parent() == Some(Path::new(prefix)),
            None => Path::new(pattern) == member_dir,
        })
}

/// What the turn read to reach its edit (log lines, file excerpts), at most
/// three items of 200 characters each, as an `Evidence:` line for the journal.
/// The notes agent summarises the journal and otherwise has only the reasoning,
/// so its notes say "the exact error is not recorded".
pub(super) fn evidence_lines(evidence: &[String]) -> String {
    let shown: Vec<String> = evidence
        .iter()
        .take(3)
        .map(|e| {
            let flat = e.split_whitespace().collect::<Vec<_>>().join(" ");
            match flat.char_indices().nth(200) {
                Some((at, _)) => format!("{}...", &flat[..at]),
                None => flat,
            }
        })
        .filter(|e| !e.is_empty())
        .collect();
    if shown.is_empty() {
        String::new()
    } else {
        format!("Evidence: {}\n", shown.join(" | "))
    }
}

/// A turn's reasoning on one line of at most 400 characters, for the notes.
pub(super) fn brief(text: &str) -> String {
    let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
    match flat.char_indices().nth(400) {
        Some((at, _)) => format!("{}...", &flat[..at]),
        None => flat,
    }
}

pub(super) fn append_notes(notes_path: PathBuf, section: &str) {
    // Append, never read-modify-write: a failed read would replace the whole
    // journal with just this section.
    use std::io::Write;
    let result = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&notes_path)
        .and_then(|mut f| f.write_all(section.as_bytes()));
    if let Err(e) = result {
        ap_core::diag!("  cannot append to {}: {e}", notes_path.display());
    }
}
