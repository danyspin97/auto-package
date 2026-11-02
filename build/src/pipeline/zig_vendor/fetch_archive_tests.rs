use super::fetch_tests::*;

/// `git archive --prefix=x-0.1.0/` of a small repository: a PAX global header
/// and one root directory, as codeload.github.com, Gitea and GitLab serve
/// their archives. The hash is the one `zig fetch` printed for it.
const GIT_ARCHIVE: &[u8] = include_bytes!("testdata/git-archive-0.1.0.tar.gz");
const GIT_ARCHIVE_HASH: &str = "x-0.1.0-b8JhgJwAAAB4WFQ59n2MvJppsyVnupBqVMaaNMKRtlUk";

#[test]
fn a_git_archive_with_a_pax_global_header_has_the_hash_zig_gives_it() {
    let url = "https://codeload.github.com/x/x/archive/v0.1.0.tar.gz";
    let fake = Fake::new(vec![(url, Route::Body(GIT_ARCHIVE.to_vec()))]);
    let (_dir, result) = fetch(&fake, &root(&dep("x", url, GIT_ARCHIVE_HASH)));
    assert_eq!(result.unwrap()[0].declared, GIT_ARCHIVE_HASH);
}

/// An archive whose entries are named `./x` and `src2/x`: two roots, so zig
/// strips neither. Its manifest's `.paths` keeps the same files as the git
/// archive, so zig gave it the same hash.
const DOT_MIXED: &[u8] = include_bytes!("testdata/dot-mixed-0.1.0.tar.gz");

#[test]
fn an_archive_with_dot_slash_names_has_the_hash_zig_gives_it() {
    let url = "https://codeberg.org/x/x/archive/v0.1.0.tar.gz";
    let fake = Fake::new(vec![(url, Route::Body(DOT_MIXED.to_vec()))]);
    let (_dir, result) = fetch(&fake, &root(&dep("x", url, GIT_ARCHIVE_HASH)));
    assert_eq!(result.unwrap()[0].declared, GIT_ARCHIVE_HASH);
}

#[test]
fn the_gzip_content_types_zig_reads_name_a_gzipped_tar() {
    // zig 0.15.2 reads these three as gzip, as it reads application/gzip.
    let url = "https://codeberg.org/x/x/archive/v0.1.0";
    for ct in [
        "application/tar+gzip",
        "application/x-tar-gz",
        "application/x-gtar-compressed; q=1",
    ] {
        let fake = Fake::new(vec![(url, Route::Typed(ct, GIT_ARCHIVE.to_vec()))]);
        let (_dir, result) = fetch(&fake, &root(&dep("x", url, GIT_ARCHIVE_HASH)));
        assert_eq!(result.unwrap()[0].declared, GIT_ARCHIVE_HASH, "{ct}");
    }
}
