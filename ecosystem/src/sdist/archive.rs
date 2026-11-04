use super::*;

/// How an archive is wrapped on disk.
///
/// PyPI's historical default is `.tar.gz`, but every other shape in this list
/// still turns up: `.zip` is an older sdist form the index still accepts,
/// `.tar.bz2` lives on in pre-2015 uploads, and `.tar.zst` is what the Rust
/// `ap_service` emits when it hands tarballs back for inspection. Previously
/// the whole module hard-coded gzip and only wheels took the unzip path, so
/// any of these other formats crashed inspection before the pipeline could
/// even report what it had received.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ArchiveKind {
    TarGz,
    TarBz2,
    TarZstd,
    /// An `.tar.xz`: NVIDIA's redistributable archives.
    TarXz,
    /// A `.zip` sdist. Container-wise indistinguishable from `.whl`; kept
    /// separate for diagnostics.
    Zip,
    /// A Python wheel — zip container, no top-level directory wrapper.
    Wheel,
}

pub(super) fn classify(path: &Path) -> Option<ArchiveKind> {
    let s = path.to_string_lossy().to_lowercase();
    if s.ends_with(".tar.gz") || s.ends_with(".tgz") {
        Some(ArchiveKind::TarGz)
    } else if s.ends_with(".tar.bz2") || s.ends_with(".tbz2") {
        Some(ArchiveKind::TarBz2)
    } else if s.ends_with(".tar.zst") || s.ends_with(".tzst") {
        Some(ArchiveKind::TarZstd)
    } else if s.ends_with(".tar.xz") || s.ends_with(".txz") {
        Some(ArchiveKind::TarXz)
    } else if s.ends_with(".whl") {
        Some(ArchiveKind::Wheel)
    } else if s.ends_with(".zip") {
        Some(ArchiveKind::Zip)
    } else {
        None
    }
}

/// Open a compressed-tar archive as a boxed streaming reader.
///
/// Returned `Read` is a reader over the uncompressed tar bytes, ready for
/// `tar::Archive::new`. The kind is passed in rather than re-sniffed so one
/// callsite can decide what the archive is and both `extract` and
/// `inspect_preferring` reuse the same answer.
pub(super) fn open_tar(
    path: &Path,
    kind: ArchiveKind,
) -> Result<tar::Archive<Box<dyn Read>>, Error> {
    Ok(tar::Archive::new(open_reader(path, kind)?))
}

/// The uncompressed bytes of a compressed tar, as a streaming reader.
pub(super) fn open_reader(path: &Path, kind: ArchiveKind) -> Result<Box<dyn Read>, Error> {
    let file = std::fs::File::open(path).map_err(|e| Error::Io {
        path: path.to_path_buf(),
        source: e,
    })?;
    let reader: Box<dyn Read> = match kind {
        ArchiveKind::TarGz => Box::new(GzDecoder::new(file)),
        ArchiveKind::TarBz2 => Box::new(bzip2::read::BzDecoder::new(file)),
        ArchiveKind::TarZstd => {
            let dec = zstd::Decoder::new(file).map_err(|e| Error::Io {
                path: path.to_path_buf(),
                source: e,
            })?;
            Box::new(dec)
        }
        ArchiveKind::TarXz => Box::new(lzma_rust2::XzReader::new(file, true)),
        ArchiveKind::Zip | ArchiveKind::Wheel => {
            return Err(Error::Io {
                path: path.to_path_buf(),
                source: std::io::Error::other("zip-shaped archive is not a tar; call the zip path"),
            });
        }
    };
    Ok(reader)
}
