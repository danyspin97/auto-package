//! A byte-exact replacement in a file that may not be UTF-8.
//!
//! The refresh turn's Edit tool refuses a legacy-encoded file, so a hunk in
//! one cannot be ported with it. This edits the file as bytes: the search text
//! must occur exactly once, and every other byte (line endings included) is
//! written back unchanged. Nothing is decoded or re-encoded. The file is
//! replaced by a rename from a temporary file in its directory, so it is never
//! left half written, and it keeps its permissions.

use std::io::Write;
use std::path::{Path, PathBuf};

pub mod request;

#[cfg(test)]
mod tests;

/// Why an edit did not happen. The file is untouched in every case.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// An empty search text matches everywhere: it is refused.
    #[error("the text to replace is empty")]
    EmptyOld,
    /// The search text does not occur, or occurs more than once.
    #[error("{count} matches of the text to replace in {path}, need exactly one{at}")]
    Matches {
        path: PathBuf,
        count: usize,
        /// The first byte offsets of the matches, for a several-match report.
        at: String,
    },
    /// The request names a file or scratch text it may not use.
    #[error("refused: {0}")]
    Refused(String),
    #[error("{path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

/// How many places `needle` occurs in `haystack`, overlapping ones included:
/// `aa` occurs twice in `aaa`, and replacing either would be a guess.
pub fn count_matches(haystack: &[u8], needle: &[u8]) -> Vec<usize> {
    if needle.is_empty() || needle.len() > haystack.len() {
        return Vec::new();
    }
    (0..=haystack.len() - needle.len())
        .filter(|&i| &haystack[i..i + needle.len()] == needle)
        .collect()
}

/// `content` with the one occurrence of `old` replaced by `new`. Every other
/// byte is kept as it is.
pub fn replace_once_in(content: &[u8], old: &[u8], new: &[u8]) -> Result<Vec<u8>, Error> {
    if old.is_empty() {
        return Err(Error::EmptyOld);
    }
    let found = count_matches(content, old);
    if found.len() != 1 {
        return Err(Error::Matches {
            path: PathBuf::new(),
            count: found.len(),
            at: describe_offsets(&found),
        });
    }
    let at = found[0];
    let mut out = Vec::with_capacity(content.len() - old.len() + new.len());
    out.extend_from_slice(&content[..at]);
    out.extend_from_slice(new);
    out.extend_from_slice(&content[at + old.len()..]);
    Ok(out)
}

/// The byte offsets of the first few matches, for an error message.
fn describe_offsets(found: &[usize]) -> String {
    if found.is_empty() {
        return String::new();
    }
    let shown: Vec<String> = found.iter().take(5).map(|o| o.to_string()).collect();
    let more = if found.len() > 5 { ", ..." } else { "" };
    format!(" (byte offsets {}{more})", shown.join(", "))
}

/// Replaces the one occurrence of `old` in the file at `path` with `new`, as
/// bytes. The file is rewritten atomically and keeps its permissions.
pub fn replace_once(path: &Path, old: &[u8], new: &[u8]) -> Result<(), Error> {
    let io = |source| Error::Io {
        path: path.to_path_buf(),
        source,
    };
    let content = std::fs::read(path).map_err(io)?;
    let edited = replace_once_in(&content, old, new).map_err(|e| match e {
        Error::Matches { count, at, .. } => Error::Matches {
            path: path.to_path_buf(),
            count,
            at,
        },
        other => other,
    })?;
    write_atomic(path, &edited).map_err(io)
}

/// Writes `bytes` to `path` through a temporary file in the same directory,
/// renamed over `path` once it is complete, with `path`'s permissions.
fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let dir = match path.parent() {
        Some(d) if !d.as_os_str().is_empty() => d,
        _ => Path::new("."),
    };
    let permissions = std::fs::metadata(path)?.permissions();
    let mut tmp = tempfile::NamedTempFile::new_in(dir)?;
    tmp.write_all(bytes)?;
    tmp.as_file().sync_all()?;
    tmp.as_file().set_permissions(permissions)?;
    tmp.persist(path).map_err(|e| e.error)?;
    Ok(())
}
