//! Reads a `.zip` dependency archive with the rules of the tar reader in the
//! parent module: the same names, root and limits. Zig 0.15.2 differs from
//! tar in two ways, which this reader follows. A symlink entry is written as
//! a regular file holding its target, since zig hashes it so (its hash counts
//! the target's bytes as a file). And an entry may not expand more than
//! deflate can, which is what a zip bomb does: that is a limit of this reader.
//! The archive is seekable, so it is first copied to a temporary file in the
//! work directory.

use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

use super::{Limits, ensure_dirs, entry_path, stream_cap, write_file};

const S_IFMT: u32 = 0o170000;
const S_IFDIR: u32 = 0o040000;
const S_IFREG: u32 = 0o100000;
const S_IFLNK: u32 = 0o120000;

/// The most a deflate stream expands: about 1032 bytes for each byte of input.
const MAX_RATIO: u64 = 1032;
/// Bytes a tiny entry may expand by, beyond `MAX_RATIO` times its size.
const RATIO_SLACK: u64 = 1024;

/// Whether an entry of `size` bytes can come from `compressed` bytes.
pub(in crate::pipeline::zig_vendor) fn ratio_ok(size: u64, compressed: u64) -> bool {
    size <= compressed
        .saturating_mul(MAX_RATIO)
        .saturating_add(RATIO_SLACK)
}

/// Write every entry of the zip archive `src` under `dest`. A zip archive has
/// no links, so the result has none either.
pub(super) fn write_entries(src: impl Read, dest: &Path, limits: Limits) -> Result<(), String> {
    let parent = parent_of_dest(dest);
    let mut file = tempfile::tempfile_in(parent)
        .map_err(|e| format!("creating a temporary file in {}: {e}", parent.display()))?;
    std::io::copy(&mut src.take(stream_cap(limits)), &mut file)
        .map_err(|e| format!("reading the zip archive: {e}"))?;
    file.seek(SeekFrom::Start(0))
        .map_err(|e| format!("reading the zip archive: {e}"))?;
    let mut ar = zip::ZipArchive::new(file).map_err(|e| format!("reading the zip archive: {e}"))?;
    if ar.len() > limits.max_entries {
        return Err(format!("more than {} entries", limits.max_entries));
    }
    let mut bytes: u64 = 0;
    for n in 0..ar.len() {
        let mut entry = ar
            .by_index(n)
            .map_err(|e| format!("reading the zip archive: {e}"))?;
        let name = entry.name().to_string();
        let mode = entry.unix_mode();
        let kind = mode.map_or(S_IFREG, |m| m & S_IFMT);
        let rel = entry_path(Path::new(&name))?;
        if rel.as_os_str().is_empty() {
            if kind == S_IFDIR || entry.is_dir() {
                continue;
            }
            return Err(format!("the entry {name} has no name"));
        }
        // The top-level name counts as one level, as in `max_depth`'s doc.
        if rel.components().count() > limits.max_depth + 1 {
            return Err(format!(
                "{} is deeper than {} levels",
                rel.display(),
                limits.max_depth
            ));
        }
        if kind == S_IFDIR || entry.is_dir() {
            ensure_dirs(dest, &rel)?;
        } else if kind == S_IFREG || kind == S_IFLNK || kind == 0 {
            let size = entry.size();
            if !ratio_ok(size, entry.compressed_size()) {
                return Err(format!(
                    "{} expands far more than deflate can, which is a zip bomb",
                    rel.display()
                ));
            }
            bytes = bytes.saturating_add(size);
            if bytes > limits.max_bytes {
                return Err(format!("more than {} bytes of files", limits.max_bytes));
            }
            // One byte more than the declared size is read, so an entry that
            // holds more is seen, and the reader reaches its end, where the
            // zip crate checks the CRC.
            let mut limited = (&mut entry).take(size.saturating_add(1));
            write_file(dest, &rel, &mut limited, mode.unwrap_or(0o644))?;
            if limited.limit() == 0 {
                return Err(format!(
                    "{} holds more bytes than its declared size",
                    rel.display()
                ));
            }
        } else {
            return Err(format!(
                "{} is a device, a fifo or another special entry, which zig_vendor refuses",
                rel.display()
            ));
        }
    }
    Ok(())
}

/// The directory a temporary file goes in: the work directory that holds
/// `dest`.
fn parent_of_dest(dest: &Path) -> &Path {
    match dest.parent() {
        Some(p) if !p.as_os_str().is_empty() => p,
        _ => Path::new("."),
    }
}
