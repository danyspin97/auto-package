//! Rewriting tarballs: the cargo workspace fix, the repack, and the member checks.

use super::*;

/// The source paths a unified or git diff names, as `patch -p1` resolves them:
/// the `---` and `+++` names and the `diff --git a/x b/y` names with their first
/// component dropped, and the `rename`/`copy` names (source and destination).
/// `/dev/null` (a created or removed file) names nothing.
///
/// `rename from`/`copy from` names are written without the `a/`/`b/` prefix, and
/// whether `patch` drops a component from them is not something to rely on, so
/// both the name as written and the one with its first component dropped are
/// targets. Naming one file too many only extracts it; naming the source side
/// too few fails the dry run of a valid pure rename.
pub(crate) fn patch_targets(diff: &str) -> Vec<String> {
    let mut stripped_names: Vec<&str> = Vec::new();
    let mut bare_names: Vec<&str> = Vec::new();
    for line in diff.lines() {
        if let Some(name) = line
            .strip_prefix("--- ")
            .or_else(|| line.strip_prefix("+++ "))
        {
            stripped_names.push(name.split('\t').next().unwrap_or("").trim());
        } else if let Some(rest) = line.strip_prefix("diff --git ") {
            stripped_names.extend(rest.split_whitespace());
        } else if let Some(name) = ["rename from ", "rename to ", "copy from ", "copy to "]
            .iter()
            .find_map(|p| line.strip_prefix(p))
        {
            bare_names.push(name.trim());
        }
    }
    stripped_names
        .into_iter()
        .filter(|name| *name != "/dev/null")
        .map(drop_first_component)
        .chain(
            bare_names
                .into_iter()
                .filter(|name| !name.is_empty() && *name != "/dev/null")
                .flat_map(|name| [name.to_string(), drop_first_component(name)]),
        )
        .filter(|rest| !rest.is_empty())
        .collect()
}

/// A patch name with its first directory component dropped (`-p1`). A name with
/// no directory part has none to drop, so it is its own target (`--- setup.py.orig`
/// / `+++ setup.py` patches `setup.py`).
fn drop_first_component(name: &str) -> String {
    match name.split_once('/') {
        Some((_, rest)) => rest.to_string(),
        None => name.to_string(),
    }
}

/// Fix broken workspace definitions in a Cargo.toml inside a tarball.
///
/// Some packages reference workspace members (often git submodules) that
/// aren't included in GitHub's archive downloads. This removes
/// genuinely-missing members from the workspace definition so cargo vendor
/// doesn't fail.
///
/// Correctness notes from the earlier review:
///
/// * **Glob-aware membership**. Cargo's `members = ["crates/*"]` syntax is
///   a glob; literal filesystem existence of a path named `crates/*`
///   would otherwise say "not there, remove it", collapsing a perfectly valid
///   workspace to `members = []`. `member_resolves` now expands single-
///   segment wildcards and keeps the member when any filesystem entry
///   matches the pattern.
///
/// * **Original tarball preserved**. The repack must not overwrite the
///   downloaded archive in place, destroying the pristine SHA256 identity
///   the caller had just verified. The original is now kept as
///   `<name>.orig.tar.gz` next to the modified one so provenance survives.
///
/// * **Audit trail**. When any member is removed, a `cargo-workspace-fix.
///   txt` is written alongside the tarball naming each removed member;
///   that file lands in the OBS commit so the reviewer sees what changed.
pub(super) fn fix_cargo_workspace_in_tarball(
    tarball: &std::path::Path,
    pkg_name: &str,
) -> Result<(), String> {
    // Refuse before extracting: a non-gzip archive would be rewritten as gzip.
    require_gzip_tarball(tarball)?;

    // Extract to temp directory
    let temp_dir = tempfile::tempdir().map_err(|e| e.to_string())?;
    let extract_dir = temp_dir.path();

    ap_core::diag!("  checking for broken Cargo workspace in tarball");

    // Extract tarball. The sdist extractor refuses links that could reach
    // outside `extract_dir`, so nothing below can read or write past it.
    ap_ecosystem::sdist::extract(tarball, extract_dir)
        .map_err(|e| format!("extracting tarball: {}", e))?;

    // Find Cargo.toml files
    let mut fixed = false;
    let mut audit_log: Vec<(PathBuf, Vec<String>)> = Vec::new();
    for entry in walkdir::WalkDir::new(extract_dir)
        .max_depth(3)
        .into_iter()
        .filter_map(|e| e.ok())
    {
        if entry.file_name() != "Cargo.toml" {
            continue;
        }
        // Only a real file is a manifest: a link named Cargo.toml must not
        // be read or rewritten through.
        if !entry.file_type().is_file() {
            continue;
        }

        let cargo_toml_path = entry.path();
        let content = match std::fs::read_to_string(cargo_toml_path) {
            Ok(c) => c,
            Err(_) => continue,
        };

        // Parse as TOML
        let mut doc: toml::Table = match toml::from_str(&content) {
            Ok(d) => d,
            Err(_) => continue,
        };

        // Check if it has a workspace section
        let workspace = match doc.get_mut("workspace") {
            Some(toml::Value::Table(w)) => w,
            _ => continue,
        };

        // Get members array
        let members = match workspace.get_mut("members") {
            Some(toml::Value::Array(m)) => m,
            _ => continue,
        };

        // Check which members actually resolve. A member can be a literal
        // path (`crates/foo`) OR a single-segment glob (`crates/*`,
        // `vendor/foo-*`); `member_resolves` understands both so a glob
        // whose parent dir has at least one match stays in instead of
        // being stripped as if the literal path `crates/*` were missing.
        let base_dir = cargo_toml_path.parent().ok_or_else(|| {
            format!(
                "Cargo.toml in the {pkg_name} tarball has no parent directory: {}",
                cargo_toml_path.display()
            )
        })?;
        let mut existing_members = Vec::new();
        let mut removed_members = Vec::new();

        for member in members.iter() {
            if let Some(member_str) = member.as_str() {
                if member_resolves(base_dir, member_str) {
                    existing_members.push(member.clone());
                } else {
                    ap_core::diag!("  ⚠ removing missing workspace member: {}", member_str);
                    removed_members.push(member_str.to_string());
                    fixed = true;
                }
            }
        }

        if !removed_members.is_empty() {
            // Update the members array
            *members = existing_members;

            // Write back the modified Cargo.toml
            let new_content = toml::to_string_pretty(&doc)
                .map_err(|e| format!("serializing Cargo.toml: {}", e))?;
            ap_core::fsx::write_atomic(cargo_toml_path, new_content)
                .map_err(|e| format!("writing Cargo.toml: {}", e))?;

            ap_core::diag!(
                "  ✓ fixed Cargo.toml: removed {} missing workspace members",
                removed_members.len()
            );

            audit_log.push((
                cargo_toml_path
                    .strip_prefix(extract_dir)
                    .unwrap_or(cargo_toml_path)
                    .to_path_buf(),
                removed_members,
            ));
        }
    }

    if !fixed {
        ap_core::diag!("  ✓ no workspace issues found");
        return Ok(());
    }

    // Preserve the pristine downloaded archive before replacing it, so the
    // original SHA256-verified bytes are still auditable after the fact.
    // Fatal: the repack below renames over the original, so a missing backup
    // would leave the modification with no way back to the bytes it replaced.
    let orig_backup = tarball.with_file_name(format!(
        "{}.orig",
        tarball
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default()
    ));
    if let Err(e) = std::fs::copy(tarball, &orig_backup) {
        return Err(format!(
            "could not back up original tarball to {}: {e}",
            orig_backup.display()
        ));
    }

    // Audit record of what members were stripped and from which
    // Cargo.toml. Written next to the tarball so it travels with the OBS
    // commit; without this the modification is invisible to the recipe
    // reviewer.
    let audit = {
        let mut s = String::from(
            "cargo workspace fix applied by auto-package: the following \
             workspace members were removed from in-tarball Cargo.toml \
             files because nothing in the archive resolved them (likely \
             omitted git submodules).\n\
             \n\
             Original tarball preserved next to this one as the `.orig`\n\
             file; compare for the exact byte-level diff.\n\n",
        );
        for (path, members) in &audit_log {
            s.push_str(&format!("{}:\n", path.display()));
            for m in members {
                s.push_str(&format!("  - {m}\n"));
            }
        }
        s
    };
    let audit_path = tarball.with_file_name("cargo-workspace-fix.txt");
    write_or_warn(&audit_path, audit);

    // Re-pack the tarball
    ap_core::diag!("  re-packing fixed tarball");
    repack_tarball(extract_dir, tarball)?;

    ap_core::diag!("  ✓ tarball updated with fixed workspace");
    Ok(())
}

/// Pack `extract_dir` into `tarball`, replacing it.
///
/// Links are recorded as links, never followed: a symlink in the tree must
/// not turn into a copy of whatever it points at on this host. Anything that
/// is neither a file, a directory nor a link is left out.
pub(super) fn repack_tarball(extract_dir: &Path, tarball: &Path) -> Result<(), String> {
    require_gzip_tarball(tarball)?;
    let temp_tarball = tarball.with_extension("tar.gz.tmp");
    let result = write_gzip_tarball(extract_dir, &temp_tarball).and_then(|()| {
        std::fs::rename(&temp_tarball, tarball).map_err(|e| format!("replacing tarball: {}", e))
    });
    if result.is_err() {
        // Never leave a partial temp archive behind.
        let _ = std::fs::remove_file(&temp_tarball);
    }
    result
}

/// Repacking always writes gzip, so only a `.tar.gz` original may be replaced.
/// Anything else is refused and left untouched: no bz2/zstd/xz encoder is a
/// dependency of this crate, and silently changing the format would break the
/// file name the spec refers to.
fn require_gzip_tarball(tarball: &Path) -> Result<(), String> {
    if tarball.to_string_lossy().ends_with(".tar.gz") {
        Ok(())
    } else {
        Err(format!(
            "{} is not a .tar.gz archive; not repacked, left as downloaded",
            tarball.display()
        ))
    }
}

fn write_gzip_tarball(extract_dir: &Path, temp_tarball: &Path) -> Result<(), String> {
    let tar_file =
        std::fs::File::create(temp_tarball).map_err(|e| format!("creating temp tarball: {}", e))?;
    let enc = flate2::write::GzEncoder::new(tar_file, flate2::Compression::default());
    let mut tar_builder = tar::Builder::new(enc);
    tar_builder.follow_symlinks(false);

    for entry in walkdir::WalkDir::new(extract_dir) {
        // An unreadable entry must fail the rebuild: skipping it would ship a
        // tarball that silently lacks files.
        let entry = entry.map_err(|e| format!("walking {}: {e}", extract_dir.display()))?;
        let path = entry.path();
        if path == extract_dir {
            continue;
        }

        let relative = path
            .strip_prefix(extract_dir)
            .map_err(|e| format!("{}: {e}", path.display()))?;
        let file_type = entry.file_type();

        if file_type.is_file() || file_type.is_symlink() {
            tar_builder
                .append_path_with_name(path, relative)
                .map_err(|e| format!("adding file to tarball: {}", e))?;
        } else if file_type.is_dir() {
            tar_builder
                .append_dir(relative, path)
                .map_err(|e| format!("adding dir to tarball: {}", e))?;
        }
    }

    tar_builder
        .finish()
        .map_err(|e| format!("finalizing tarball: {}", e))?;
    // The gzip trailer is written by `finish`, not by dropping the encoder,
    // where a failure would go unseen and a truncated archive replace the
    // original.
    tar_builder
        .into_inner()
        .and_then(|enc| enc.finish())
        .and_then(|file| file.sync_all())
        .map_err(|e| format!("finalizing tarball: {}", e))?;
    Ok(())
}

/// Does `member` under `base_dir` resolve to anything on disk?
///
/// Cargo workspace members are either literal paths (`crates/foo`) or
/// single-segment globs (`crates/*`, `vendor/foo-*`). The literal case is
/// a direct filesystem check; the glob case reads the parent directory
/// and matches each entry name against the pattern. Returns `true` as
/// soon as any match is found.
///
/// Not a complete Cargo workspace resolver — doesn't handle `**` recursive
/// globs or Cargo's `exclude = [...]` filter — but every workspace shape
/// the running packaging corpus hits uses single-segment `*` patterns.
///
/// A member with a `*` in a non-final segment (`crates/*/bindings`) spans
/// several directories, which this does not expand, so it is kept as it is
/// (`true`) rather than judged missing and removed from the manifest.
pub(super) fn member_resolves(base_dir: &Path, member: &str) -> bool {
    if member
        .split('/')
        .rev()
        .skip(1)
        .any(|segment| segment.contains('*'))
    {
        return true;
    }
    let (parent_rel, pattern) = match member.rsplit_once('/') {
        Some((p, n)) => (p, n),
        None => ("", member),
    };
    let parent = if parent_rel.is_empty() {
        base_dir.to_path_buf()
    } else {
        base_dir.join(parent_rel)
    };
    if !parent.is_dir() {
        return false;
    }
    if !pattern.contains('*') {
        return parent.join(pattern).exists();
    }
    let Ok(iter) = std::fs::read_dir(&parent) else {
        return false;
    };
    iter.filter_map(|e| e.ok())
        .any(|e| glob_matches_single_segment(pattern, &e.file_name().to_string_lossy()))
}

/// Match a single-segment glob (one with `*` wildcards, no `/`) against a
/// candidate filename.
///
/// A single `*` matches any run of characters (including empty); multiple
/// `*`s split the pattern into literal pieces that must appear in order.
/// Does not implement `?`, `[...]` character classes, or `**`.
pub(super) fn glob_matches_single_segment(pattern: &str, candidate: &str) -> bool {
    if !pattern.contains('*') {
        return pattern == candidate;
    }
    let pieces: Vec<&str> = pattern.split('*').collect();
    let (first, last) = (pieces[0], pieces[pieces.len() - 1]);
    // The ends are anchored (and must not overlap); only the pieces between
    // them float, in order.
    if candidate.len() < first.len() + last.len()
        || !candidate.starts_with(first)
        || !candidate.ends_with(last)
    {
        return false;
    }
    let mut rest = &candidate[first.len()..candidate.len() - last.len()];
    for part in &pieces[1..pieces.len() - 1] {
        match rest.find(part) {
            Some(idx) => rest = &rest[idx + part.len()..],
            None => return false,
        }
    }
    true
}
