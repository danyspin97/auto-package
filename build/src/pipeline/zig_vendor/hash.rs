//! Zig's package hash: the content hash a dependency's `build.zig.zon` declares
//! (`.hash = "name-version-<44 chars>"`), computed from the files of the
//! dependency after its `.paths` filter.
//!
//! The 33 bytes behind the 44 characters are a 4-byte id (the low 32 bits of
//! the package fingerprint, little endian), the total size of the regular
//! files as a u32 little endian, and the first 25 bytes of a SHA-256 over the
//! per-file digests, sorted by path. The base64 is the URL-safe alphabet with
//! no padding. A file's digest is `sha256(path ‖ 0x00 ‖ 0x00 ‖ content)`; a
//! symlink's is `sha256(path ‖ target)`. The executable bit is not hashed:
//! zig 0.15.2 gives the same hash for a file with mode 0644 and 0755.
//!
//! The rules were checked against zig 0.15.2 (`zig fetch` on fixture trees,
//! including a symlink). Pixman and wayland archives, whose declared hashes
//! are in river's `build.zig.zon`, are tests here. Waylock's archives were not
//! available, so the 0.16 layout and waylock's hashes are not verified.
//!
//! Zig 0.13 and earlier declared `1220` and the hex of a SHA-256 instead
//! (a multihash). Zig 0.15.2 still verifies that form, and it is the SHA-256
//! over the same sorted per-file digests, without the id and size: the three
//! trees zig printed for it (files only, nested files with an executable,
//! and a symlink) all match.

use std::io::Read;

use sha2::{Digest, Sha256};

/// The URL-safe base64 alphabet, without padding.
const B64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";

/// Length of the base64 part of a declared hash: 33 bytes, no padding.
pub(super) const DIGEST_LEN: usize = 44;

/// The per-file digests of one package, collected before the final hash.
#[derive(Default)]
pub(super) struct PackageHash {
    /// `(path, digest)`, one per file or link.
    digests: Vec<(String, [u8; 32])>,
    /// Sum of the regular files' sizes, in bytes.
    size: u64,
}

impl PackageHash {
    pub(super) fn new() -> Self {
        Self::default()
    }

    /// Add a regular file. `path` is relative to the package root, with `/`
    /// separators. The content is streamed, not held in memory.
    pub(super) fn file(&mut self, path: &str, content: &mut impl Read) -> std::io::Result<()> {
        let mut hasher = Sha256::new();
        hasher.update(path.as_bytes());
        hasher.update([0u8, 0u8]);
        let mut counted = Counter {
            inner: content,
            hasher: &mut hasher,
            bytes: 0,
        };
        std::io::copy(&mut counted, &mut std::io::sink())?;
        self.size = self.size.saturating_add(counted.bytes);
        self.digests
            .push((path.to_string(), hasher.finalize().into()));
        Ok(())
    }

    /// Add a symbolic link with its target text, as written in the link.
    pub(super) fn link(&mut self, path: &str, target: &[u8]) {
        let mut hasher = Sha256::new();
        hasher.update(path.as_bytes());
        hasher.update(target);
        self.digests
            .push((path.to_string(), hasher.finalize().into()));
    }

    /// The declared-hash base64 for this package and fingerprint id, or an
    /// error when the total size does not fit the 4-byte field.
    pub(super) fn finish(mut self, id: u32) -> Result<String, String> {
        let size = u32::try_from(self.size).map_err(|_| {
            format!(
                "the package holds {} bytes, over zig's 4 GiB field",
                self.size
            )
        })?;
        let total = self.sorted_digest();
        let mut raw = [0u8; 33];
        raw[..4].copy_from_slice(&id.to_le_bytes());
        raw[4..8].copy_from_slice(&size.to_le_bytes());
        raw[8..].copy_from_slice(&total[..25]);
        Ok(encode(&raw))
    }

    /// The legacy declared hash, `1220` and the hex of the whole SHA-256 over
    /// the sorted per-file digests. It has no id and no size.
    pub(super) fn finish_legacy(mut self) -> String {
        format!("1220{}", hex::encode(self.sorted_digest()))
    }

    /// The SHA-256 over the per-file digests, in path order.
    fn sorted_digest(&mut self) -> [u8; 32] {
        self.digests
            .sort_by(|a, b| a.0.as_bytes().cmp(b.0.as_bytes()));
        let mut all = Sha256::new();
        for (_, digest) in &self.digests {
            all.update(digest);
        }
        all.finalize().into()
    }
}

/// Counts the bytes read and feeds them to `hasher`.
struct Counter<'a, R: Read> {
    inner: &'a mut R,
    hasher: &'a mut Sha256,
    bytes: u64,
}

impl<R: Read> Read for Counter<'_, R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let n = self.inner.read(buf)?;
        self.hasher.update(&buf[..n]);
        self.bytes += n as u64;
        Ok(n)
    }
}

/// URL-safe base64 of `bytes`, no padding.
pub(super) fn encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b = [
            chunk[0],
            chunk.get(1).copied().unwrap_or(0),
            chunk.get(2).copied().unwrap_or(0),
        ];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        let digits = chunk.len() + 1;
        for i in 0..digits {
            let idx = (n >> (18 - 6 * i)) & 0x3f;
            out.push(B64[idx as usize] as char);
        }
    }
    out
}

/// A declared hash, in the form zig wrote it.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Declared<'a> {
    /// `name-version-<44 chars>`, as zig 0.14 and later write it.
    Current {
        name_version: &'a str,
        digest: &'a str,
    },
    /// `1220` and 64 lowercase hex digits, as zig 0.13 and earlier wrote it.
    Legacy,
}

/// Which form a declared hash is in, and its parts.
pub(super) fn parse_declared(hash: &str) -> Result<Declared<'_>, String> {
    if is_legacy(hash) {
        return Ok(Declared::Legacy);
    }
    let (name_version, digest) = split_declared(hash)?;
    Ok(Declared::Current {
        name_version,
        digest,
    })
}

/// A legacy declared hash: `1220` and 64 lowercase hex digits, which is the
/// multihash of a SHA-256 (`0x12` is SHA-256, `0x20` the 32-byte length).
fn is_legacy(hash: &str) -> bool {
    hash.len() == 68
        && hash.starts_with("1220")
        && hash[4..]
            .bytes()
            .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
}

/// Split a declared hash into its `name-version` prefix and its 44-character
/// digest. The digest may contain `-`, so the split is from the end.
pub(super) fn split_declared(hash: &str) -> Result<(&str, &str), String> {
    let len = hash.len();
    if len < DIGEST_LEN + 2 || !hash.is_char_boundary(len - DIGEST_LEN) {
        return Err(format!("`{hash}` is not a zig package hash"));
    }
    let (head, digest) = hash.split_at(len - DIGEST_LEN);
    let Some(name_version) = head.strip_suffix('-') else {
        return Err(format!("`{hash}` is not a zig package hash"));
    };
    if name_version.is_empty()
        || !digest
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_')
    {
        return Err(format!("`{hash}` is not a zig package hash"));
    }
    // The name becomes a directory name in vendor/p, so it stays to a safe
    // set: no `/`, and no other character a path or a shell would read.
    if !name_version
        .bytes()
        .all(|c| c.is_ascii_alphanumeric() || b"._+-".contains(&c))
    {
        return Err(format!(
            "`{hash}` has a character outside [A-Za-z0-9._+-] in its name"
        ));
    }
    Ok((name_version, digest))
}
