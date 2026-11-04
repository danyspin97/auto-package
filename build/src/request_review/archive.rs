//! The file list of a downloaded archive, read within bounds.

use std::path::Path;
use std::time::Duration;

/// The most decompressed bytes a listing reads.
pub const LIST_MAX_BYTES: u64 = 2 << 30;

/// The most time a listing takes.
pub const LIST_MAX_TIME: Duration = Duration::from_secs(120);

/// The files of the archive `path` holds, without the top directory (as a
/// patch names them), read within [`LIST_MAX_BYTES`] and [`LIST_MAX_TIME`]. The
/// error says why there is no list: it is not a tar, or it passed a cap. An
/// archive with no file is an error too: every patch path would be outside it.
pub fn archive_members(path: &Path) -> Result<Vec<String>, String> {
    archive_members_within(path, LIST_MAX_BYTES, LIST_MAX_TIME)
}

fn archive_members_within(
    path: &Path,
    max_bytes: u64,
    max_time: Duration,
) -> Result<Vec<String>, String> {
    let members = ap_ecosystem::sdist::list_archive_members_capped(path, max_bytes, max_time)
        .map_err(|e| e.to_string())?;
    let files: Vec<String> = members
        .into_iter()
        .filter(|m| !m.ends_with('/'))
        .filter_map(|m| m.split_once('/').map(|(_, rest)| rest.to_string()))
        .collect();
    if files.is_empty() {
        return Err("it holds no file".to_string());
    }
    Ok(files)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;

    fn tar_gz(dir: &Path, files: &[(&str, usize)]) -> std::path::PathBuf {
        let path = dir.join("a.tar.gz");
        let mut b = tar::Builder::new(flate2::write::GzEncoder::new(
            std::fs::File::create(&path).unwrap(),
            flate2::Compression::fast(),
        ));
        for (name, size) in files {
            let mut h = tar::Header::new_gnu();
            h.set_path(name).unwrap();
            h.set_size(*size as u64);
            h.set_cksum();
            b.append(&h, std::io::repeat(0).take(*size as u64)).unwrap();
        }
        b.into_inner().unwrap().finish().unwrap();
        path
    }

    #[test]
    fn the_top_directory_is_cut_and_a_cap_is_an_error_with_its_reason() {
        let dir = tempfile::tempdir().unwrap();
        let p = tar_gz(dir.path(), &[("top/src/a.c", 10), ("top/zeros", 4 << 20)]);
        let long = Duration::from_secs(60);
        assert_eq!(
            archive_members_within(&p, 64 << 20, long).unwrap(),
            ["src/a.c", "zeros"]
        );
        let e = archive_members_within(&p, 1 << 20, long).unwrap_err();
        assert!(e.contains("cap"), "{e}");
        let e = archive_members_within(&p, 64 << 20, Duration::ZERO).unwrap_err();
        assert!(e.contains("time"), "{e}");
    }

    #[test]
    fn an_archive_that_is_not_a_tar_or_holds_nothing_has_no_list() {
        let dir = tempfile::tempdir().unwrap();
        let junk = dir.path().join("x.zip");
        std::fs::write(&junk, "x").unwrap();
        assert!(archive_members(&junk).is_err());
        let empty = tar_gz(dir.path(), &[]);
        assert_eq!(archive_members(&empty).unwrap_err(), "it holds no file");
    }
}
