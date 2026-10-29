use super::*;

/// Extraction caps: a hostile or corrupt archive must not exhaust the disk
/// or the inode table. Generous enough for any real sdist or CUDA wheel.
pub(super) const MAX_ENTRIES: usize = 200_000;
/// Default cap on the bytes one extraction may write; `AUTO_PACKAGE_MAX_EXTRACT_GB`
/// (a positive number, fractions allowed) overrides it. See [`max_total_bytes`].
pub(super) const MAX_TOTAL_BYTES: u64 = 2 << 30;
pub(super) const MAX_FILE_BYTES: u64 = 2 << 30;
/// Cap on a manifest read during inspection.
pub(super) const MAX_MANIFEST_BYTES: u64 = 1 << 20;
/// Largest unpacked-to-compressed ratio accepted, past a fixed allowance: gzip
/// can expand about 1000:1, so a small upload can ask for gigabytes of disk.
const MAX_EXPANSION: u64 = 200;
const EXPANSION_ALLOWANCE: u64 = 64 << 20;

/// The total-bytes cap for one extraction.
pub(super) fn max_total_bytes() -> u64 {
    ap_core::env::env_positive_f64("AUTO_PACKAGE_MAX_EXTRACT_GB")
        .map_or(MAX_TOTAL_BYTES, |gb| (gb * (1u64 << 30) as f64) as u64)
}

/// What an extraction of `archive` is expected to write, reserved before it
/// starts: six times the archive (typical for source tarballs), at least 64 MiB
/// and at most [`max_total_bytes`].
pub(super) fn extraction_estimate(archive: &Path) -> u64 {
    let compressed = std::fs::metadata(archive).map(|m| m.len()).unwrap_or(0);
    compressed
        .saturating_mul(6)
        .max(64 << 20)
        .min(max_total_bytes())
}

/// Caps for the read-only scans of a `.tar.gz` that look for one file: entries
/// visited and decompressed bytes read, so a hostile archive cannot stall them.
pub const SCAN_MAX_ENTRIES: usize = 100_000;
pub const SCAN_MAX_BYTES: u64 = 1 << 30;

/// A `.tar.gz` reader that stops after [`SCAN_MAX_BYTES`] of decompressed data
/// (the entries past it read as a truncated archive). Pair it with
/// `.entries()?.take(SCAN_MAX_ENTRIES)`.
pub fn bounded_tar_gz<R: Read>(file: R) -> tar::Archive<std::io::Take<flate2::read::GzDecoder<R>>> {
    tar::Archive::new(flate2::read::GzDecoder::new(file).take(SCAN_MAX_BYTES))
}

/// Running totals checked against the extraction caps.
#[derive(Default)]
pub(super) struct Budget {
    pub(super) entries: usize,
    pub(super) bytes: u64,
    /// Size of the archive on disk; 0 means unknown and turns the ratio check off.
    pub(super) compressed: u64,
}

impl Budget {
    /// A budget for reading `archive`, whose size on disk bounds how much it
    /// may expand to.
    pub(super) fn for_file(archive: &Path) -> Self {
        Budget {
            compressed: std::fs::metadata(archive).map(|m| m.len()).unwrap_or(0),
            ..Default::default()
        }
    }

    /// The expansion-ratio cap: more than [`MAX_EXPANSION`] times the archive's
    /// size (after [`EXPANSION_ALLOWANCE`]) is a decompression bomb.
    fn check_ratio(&self) -> Result<(), Error> {
        if self.compressed > 0
            && self.bytes
                > self
                    .compressed
                    .saturating_mul(MAX_EXPANSION)
                    .saturating_add(EXPANSION_ALLOWANCE)
        {
            return Err(Error::LimitExceeded(
                "unpacked size is far larger than the archive (possible decompression bomb)"
                    .to_string(),
            ));
        }
        Ok(())
    }

    pub(super) fn charge(&mut self, size: u64) -> Result<(), Error> {
        self.entries += 1;
        self.bytes = self.bytes.saturating_add(size);
        if self.entries > MAX_ENTRIES {
            return Err(Error::LimitExceeded(format!(
                "more than {MAX_ENTRIES} entries"
            )));
        }
        if size > MAX_FILE_BYTES {
            return Err(Error::LimitExceeded(format!(
                "entry of {size} bytes (limit {MAX_FILE_BYTES})"
            )));
        }
        let max_total = max_total_bytes();
        if self.bytes > max_total {
            return Err(Error::LimitExceeded(format!(
                "more than {max_total} uncompressed bytes"
            )));
        }
        self.check_ratio()
    }

    /// Copy one entry's content to `out`, bounded by what the budget still
    /// allows rather than by the (attacker-controlled) declared size.
    ///
    /// `declared` is what [`Budget::charge`] already took for this entry. A
    /// header that understates the real size would otherwise let one entry
    /// expand to the per-file cap and sidestep the total cap; here the bytes
    /// actually written are what count.
    pub(super) fn copy_entry(
        &mut self,
        declared: u64,
        input: &mut impl Read,
        out: &mut impl std::io::Write,
    ) -> Result<(), Error> {
        let before = self.bytes.saturating_sub(declared);
        let allowed = MAX_FILE_BYTES.min(max_total_bytes().saturating_sub(before));
        // Read one byte past the allowance so overshoot is detectable.
        let written =
            std::io::copy(&mut input.take(allowed.saturating_add(1)), out).map_err(|e| {
                Error::Io {
                    path: std::path::PathBuf::new(),
                    source: e,
                }
            })?;
        if written > allowed {
            return Err(Error::LimitExceeded(format!(
                "entry expands past its declared {declared} bytes beyond the extraction limit"
            )));
        }
        self.bytes = before.saturating_add(written.max(declared));
        self.check_ratio()
    }
}

/// Extract the sdist tarball into `dest`.
///
/// Every entry in the archive is unpacked under `dest`. Well-formed sdists
/// contain exactly one top-level directory (e.g. `markupsafe-3.0.3/`); after
/// extraction that directory will be at `dest/<top-level>/`.
///
/// Dispatches on the file extension — `.tar.gz`/`.tgz`, `.tar.bz2`/`.tbz2`,
/// `.tar.zst`/`.tzst`, `.zip`, and `.whl` are all accepted. A `.whl` (and a
/// bare `.zip`) is a zip archive with no wrapping top-level directory — it
/// unpacks flat into `dest` itself. The gzip, bzip2 and zstd paths all share
/// the tar iterator; only the outer decoder differs.
///
/// `dest` must be absent or empty. The archive is unpacked beside it and
/// renamed into place only once complete, so a failed or interrupted unpack
/// never leaves a `dest` that looks like a finished extraction.
pub fn extract(tarball: &Path, dest: &Path) -> Result<(), Error> {
    extract_where(tarball, dest, |_| true)
}

/// Refuse a symlink whose location or target can reach outside `root`.
///
/// Walks the link's directory and then its target one component at a time,
/// keeping the path relative to `root`. Every directory it steps through must
/// be a real directory, not a link, so the walk is exact and a `..` can only
/// pop a real directory. The final component of the target may be a link:
/// it was checked when it was created.
fn check_link(root: &Path, link: &Path, target: &Path) -> Result<(), Error> {
    let escape = || Error::LinkEscapes {
        link: link.to_path_buf(),
        target: target.to_path_buf(),
    };
    if target.has_root() {
        return Err(escape());
    }
    let is_link_on_disk = |rel: &Path| {
        std::fs::symlink_metadata(root.join(rel)).is_ok_and(|m| m.file_type().is_symlink())
    };

    let mut rel = std::path::PathBuf::new();
    for c in link.parent().unwrap_or(Path::new("")).components() {
        match c {
            Component::Normal(p) => {
                if is_link_on_disk(&rel.join(p)) {
                    return Err(escape());
                }
                rel.push(p);
            }
            Component::CurDir => {}
            _ => return Err(escape()),
        }
    }

    let parts: Vec<Component<'_>> = target.components().collect();
    for (i, c) in parts.iter().enumerate() {
        match c {
            Component::Normal(p) => {
                let last = i + 1 == parts.len();
                if !last && is_link_on_disk(&rel.join(p)) {
                    return Err(escape());
                }
                rel.push(p);
            }
            Component::CurDir => {}
            Component::ParentDir => {
                if !rel.pop() {
                    return Err(escape());
                }
            }
            _ => return Err(escape()),
        }
    }
    Ok(())
}

/// [`extract`] restricted to the entries `keep` accepts. The path it is given
/// is the entry's path inside the archive (including the top-level directory).
/// Skipped entries are still decompressed on the way past and counted against
/// the extraction budget, but never written.
pub fn extract_where(
    tarball: &Path,
    dest: &Path,
    keep: impl Fn(&Path) -> bool,
) -> Result<(), Error> {
    // At most a couple of unpacks at a time, each with its expected size
    // reserved against the free space of the destination's filesystem.
    let _slot = crate::space::extraction_slot();
    let _lease = crate::space::lease(
        &format!("extracting {}", tarball.display()),
        dest.parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new(".")),
        extraction_estimate(tarball),
    )?;
    let mut partial = dest.as_os_str().to_owned();
    partial.push(".partial");
    let partial = PathBuf::from(partial);
    let _ = std::fs::remove_dir_all(&partial);
    let result = extract_into(tarball, &partial, &keep).and_then(|()| {
        // Only an empty `dest` is replaced; anything else is an error here.
        let _ = std::fs::remove_dir(dest);
        std::fs::rename(&partial, dest).map_err(|e| Error::Io {
            path: dest.to_path_buf(),
            source: e,
        })
    });
    if result.is_err() {
        let _ = std::fs::remove_dir_all(&partial);
    }
    result
}

fn extract_into(tarball: &Path, dest: &Path, keep: &dyn Fn(&Path) -> bool) -> Result<(), Error> {
    let kind = classify(tarball).ok_or_else(|| Error::Io {
        path: tarball.to_path_buf(),
        source: std::io::Error::other(format!(
            "unrecognised archive extension on {}: expected one of \
             .tar.gz/.tgz, .tar.bz2/.tbz2, .tar.zst/.tzst, .zip, .whl",
            tarball.display(),
        )),
    })?;
    match kind {
        ArchiveKind::TarGz | ArchiveKind::TarBz2 | ArchiveKind::TarZstd | ArchiveKind::TarXz => {
            let mut archive = open_tar(tarball, kind)?;
            // Both are off by default; stated so a setuid bit or an xattr
            // from an untrusted archive can never be applied.
            archive.set_preserve_permissions(false);
            archive.set_unpack_xattrs(false);
            std::fs::create_dir_all(dest).map_err(|e| Error::Io {
                path: dest.to_path_buf(),
                source: e,
            })?;
            let mut budget = Budget::for_file(tarball);
            // `unpack_in` keeps tar-rs's traversal and symlink protections.
            for entry in archive.entries().map_err(Error::Tar)? {
                let mut entry = entry.map_err(Error::Tar)?;
                // Charged before `keep`: a skipped entry is still decompressed.
                budget.charge(entry.size())?;
                if !keep(&entry.path().map_err(Error::Tar)?) {
                    continue;
                }
                if let Some(reason) = escaping_link(&entry) {
                    tracing::warn!("skipping tar entry: {reason}");
                    continue;
                }
                // Each symlink is also checked against what is already on
                // disk. `unpack_in` only canonicalizes the parent of the entry
                // it is writing, so a link planted earlier can redirect a
                // later one out of the tree even when each link looks fine
                // on its own.
                if entry.header().entry_type().is_symlink() {
                    let link = entry.path().map_err(Error::Tar)?.into_owned();
                    if let Some(target) = entry.link_name().map_err(Error::Tar)? {
                        check_link(dest, &link, &target)?;
                    }
                }
                // A hard link's target is relative to the archive root and
                // is opened through whatever directories are on disk, so a
                // symlinked directory planted earlier would redirect it.
                // Checked as a link at the root.
                if entry.header().entry_type().is_hard_link()
                    && let Some(target) = entry.link_name().map_err(Error::Tar)?
                {
                    let link = entry.path().map_err(Error::Tar)?.into_owned();
                    let at_root = link.file_name().map(Path::new).unwrap_or(&link);
                    check_link(dest, at_root, &target)?;
                }
                entry.unpack_in(dest).map_err(Error::Tar)?;
            }
            Ok(())
        }
        ArchiveKind::Zip | ArchiveKind::Wheel => extract_zip(tarball, dest, keep),
    }
}

/// Why a symlink or hard-link entry must not be materialised, or `None` when
/// it is not a link or stays inside the extraction root.
///
/// tar-rs creates the link with whatever target the archive names, so an
/// absolute or `../` target would leave a link in the build tree that points
/// outside it, and later steps that follow it would read or write there.
/// The check is lexical: it cannot follow links that do not exist yet.
fn escaping_link<R: Read>(entry: &tar::Entry<'_, R>) -> Option<String> {
    use std::path::Component;
    let kind = entry.header().entry_type();
    if !kind.is_symlink() && !kind.is_hard_link() {
        return None;
    }
    let name = entry.path().ok()?.into_owned();
    let target = match entry.link_name() {
        Ok(Some(t)) => t.into_owned(),
        _ => return Some(format!("link {} has no readable target", name.display())),
    };
    let refuse = || {
        Some(format!(
            "link {} -> {} points outside the extraction root",
            name.display(),
            target.display()
        ))
    };
    if target.is_absolute() {
        return refuse();
    }
    // A symlink target is relative to the link's directory; a hard-link
    // target is relative to the archive root.
    let mut parts: Vec<&std::ffi::OsStr> = Vec::new();
    if kind.is_symlink() {
        let parent = name.parent().unwrap_or(Path::new(""));
        for c in parent.components() {
            match c {
                Component::Normal(p) => parts.push(p),
                Component::ParentDir if parts.pop().is_none() => return refuse(),
                _ => {}
            }
        }
    }
    for c in target.components() {
        match c {
            Component::Normal(p) => parts.push(p),
            Component::ParentDir => {
                if parts.pop().is_none() {
                    return refuse();
                }
            }
            Component::CurDir => {}
            _ => return refuse(),
        }
    }
    None
}

/// Extract a zip-shaped archive (`.whl` or `.zip`) using the `zip` crate.
///
/// Pure-Rust, no reliance on the system `unzip` binary — the previous
/// implementation shelled out, which quietly failed on build hosts without
/// InfoZip installed and left no artifact behind to diagnose from.
fn extract_zip(archive: &Path, dest: &Path, keep: &dyn Fn(&Path) -> bool) -> Result<(), Error> {
    let file = std::fs::File::open(archive).map_err(|e| Error::Io {
        path: archive.to_path_buf(),
        source: e,
    })?;
    let mut zip_archive = zip::ZipArchive::new(file).map_err(|e| Error::Io {
        path: archive.to_path_buf(),
        source: std::io::Error::other(e.to_string()),
    })?;
    std::fs::create_dir_all(dest).map_err(|e| Error::Io {
        path: dest.to_path_buf(),
        source: e,
    })?;
    let mut budget = Budget::for_file(archive);
    for i in 0..zip_archive.len() {
        let mut entry = zip_archive.by_index(i).map_err(|e| Error::Io {
            path: archive.to_path_buf(),
            source: std::io::Error::other(e.to_string()),
        })?;
        budget.charge(entry.size())?;
        // `enclosed_name` filters out absolute paths and `..` traversal — same
        // guard the tar reader applies — so a hostile wheel can't escape
        // `dest`.
        let Some(rel) = entry.enclosed_name() else {
            continue;
        };
        if !keep(&rel) {
            continue;
        }
        let out_path = dest.join(rel);
        if entry.is_dir() {
            std::fs::create_dir_all(&out_path).map_err(|e| Error::Io {
                path: out_path.clone(),
                source: e,
            })?;
            continue;
        }
        if let Some(parent) = out_path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| Error::Io {
                path: parent.to_path_buf(),
                source: e,
            })?;
        }
        // Never materialise a symlink entry: its "content" is the link
        // target, and writing it as a file (or linking out of `dest`) is
        // useless at best and a traversal vector at worst.
        if entry.is_symlink() {
            continue;
        }
        let mode = entry.unix_mode();
        let mut out_file = std::fs::File::create(&out_path).map_err(|e| Error::Io {
            path: out_path.clone(),
            source: e,
        })?;
        let declared = entry.size();
        budget
            .copy_entry(declared, &mut entry, &mut out_file)
            .map_err(|e| match e {
                Error::Io { source, .. } => Error::Io {
                    path: out_path.clone(),
                    source,
                },
                other => other,
            })?;
        drop(out_file);
        #[cfg(unix)]
        if let Some(mode) = mode {
            use std::os::unix::fs::PermissionsExt;
            // Keep rwx bits only (drop setuid/setgid/sticky), and always
            // owner-readable/writable so later steps can handle the file.
            let perms = std::fs::Permissions::from_mode((mode & 0o777) | 0o600);
            std::fs::set_permissions(&out_path, perms).map_err(|e| Error::Io {
                path: out_path.clone(),
                source: e,
            })?;
        }
    }
    Ok(())
}

/// The member paths of a tar archive (`.tar.gz`, `.tar.bz2`, `.tar.zst`,
/// `.tar.xz`) without unpacking it: directories end in `/`, links are listed
/// like files, a leading `./` is dropped and entries that are absolute or climb
/// out with `..` are skipped. Stops at the same entry cap as extraction.
pub fn list_archive_members(path: &Path) -> Result<Vec<String>, Error> {
    let kind = classify(path).filter(|k| !matches!(k, ArchiveKind::Zip | ArchiveKind::Wheel));
    let Some(kind) = kind else {
        return Err(Error::Io {
            path: path.to_path_buf(),
            source: std::io::Error::other("not a tar archive"),
        });
    };
    let mut archive = open_tar(path, kind)?;
    let mut out = Vec::new();
    for entry in archive.entries().map_err(Error::Tar)? {
        let entry = entry.map_err(Error::Tar)?;
        if out.len() >= MAX_ENTRIES {
            return Err(Error::LimitExceeded(format!(
                "more than {MAX_ENTRIES} entries"
            )));
        }
        let entry_path = entry.path().map_err(Error::Tar)?.into_owned();
        if entry_path.is_absolute()
            || entry_path
                .components()
                .any(|c| c == std::path::Component::ParentDir)
        {
            continue;
        }
        let mut name = entry_path.to_string_lossy().into_owned();
        while let Some(rest) = name.strip_prefix("./") {
            name = rest.to_string();
        }
        if name.is_empty() || name == "." {
            continue;
        }
        if entry.header().entry_type().is_dir() && !name.ends_with('/') {
            name.push('/');
        }
        out.push(name);
    }
    Ok(out)
}
