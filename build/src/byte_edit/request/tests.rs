use super::*;

/// A workspace like a refresh turn's: `source` links to the unpacked sdist,
/// which holds a legacy-encoded `.result` file and a quilt `.pc` directory.
/// Scratch text is in `byte-edit/`. `outside` is a file beside the workspace.
struct Fixture {
    _dir: tempfile::TempDir,
    ws: PathBuf,
    outside: PathBuf,
}

fn fixture() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_path_buf();
    let ws = root.join("ws");
    let top = ws.join("sdist/top");
    std::fs::create_dir_all(top.join("main")).unwrap();
    std::fs::create_dir_all(top.join(".pc")).unwrap();
    std::fs::write(
        top.join("main/mysqldump.result"),
        b"caf\xe9 old\r\nline\r\n",
    )
    .unwrap();
    std::fs::write(top.join(".pc/state"), b"old\n").unwrap();
    std::os::unix::fs::symlink("sdist/top", ws.join("source")).unwrap();
    std::fs::create_dir_all(ws.join("byte-edit")).unwrap();
    std::fs::write(ws.join("byte-edit/old.txt"), "old").unwrap();
    std::fs::write(ws.join("byte-edit/new.txt"), "new").unwrap();
    std::fs::write(ws.join("notes.md"), "notes\n").unwrap();
    std::fs::write(ws.join("pkg.changes"), "changes\n").unwrap();
    let outside = root.join("outside");
    std::fs::write(&outside, b"old secret\n").unwrap();
    Fixture {
        _dir: dir,
        ws,
        outside,
    }
}

fn edit(ws: &Path, file: &str) -> Result<PathBuf, Error> {
    edit_in_workspace(
        ws,
        Path::new(file),
        Path::new("byte-edit/old.txt"),
        Path::new("byte-edit/new.txt"),
    )
}

fn main_file(ws: &Path) -> Vec<u8> {
    std::fs::read(ws.join("sdist/top/main/mysqldump.result")).unwrap()
}

#[test]
fn an_allowed_edit_changes_only_the_matched_bytes() {
    let f = fixture();
    let target = edit(&f.ws, "source/main/mysqldump.result").unwrap();
    assert!(target.ends_with("sdist/top/main/mysqldump.result"));
    assert_eq!(main_file(&f.ws), b"caf\xe9 new\r\nline\r\n");
}

#[test]
fn a_link_inside_source_is_edited_where_it_points() {
    let f = fixture();
    std::os::unix::fs::symlink(
        f.ws.join("sdist/top/main/mysqldump.result"),
        f.ws.join("sdist/top/alias"),
    )
    .unwrap();
    edit(&f.ws, "source/alias").unwrap();
    assert_eq!(main_file(&f.ws), b"caf\xe9 new\r\nline\r\n");
    let alias = f.ws.join("sdist/top/alias");
    assert!(alias.symlink_metadata().unwrap().file_type().is_symlink());
}

#[test]
fn an_absolute_file_is_refused() {
    let f = fixture();
    let abs = f.ws.join("source/main/mysqldump.result");
    let err = edit(&f.ws, &abs.display().to_string()).unwrap_err();
    assert!(matches!(err, Error::Refused(_)), "{err:?}");
    assert_eq!(main_file(&f.ws), b"caf\xe9 old\r\nline\r\n");
}

#[test]
fn a_parent_component_is_refused_even_when_it_stays_inside() {
    let f = fixture();
    for file in ["source/../notes.md", "source/main/../main/mysqldump.result"] {
        let err = edit(&f.ws, file).unwrap_err();
        assert!(matches!(err, Error::Refused(_)), "{file}: {err:?}");
    }
    assert_eq!(std::fs::read(f.ws.join("notes.md")).unwrap(), b"notes\n");
}

#[test]
fn a_file_outside_source_is_refused_whatever_its_type() {
    let f = fixture();
    for file in ["notes.md", "pkg.changes", "byte-edit/old.txt"] {
        let err = edit(&f.ws, file).unwrap_err();
        assert!(
            err.to_string().contains("resolves outside"),
            "{file}: {err}"
        );
    }
    assert_eq!(
        std::fs::read(f.ws.join("pkg.changes")).unwrap(),
        b"changes\n"
    );
}

#[test]
fn a_symlink_in_source_that_leads_outside_is_refused() {
    let f = fixture();
    std::os::unix::fs::symlink(&f.outside, f.ws.join("sdist/top/escape")).unwrap();
    let err = edit(&f.ws, "source/escape").unwrap_err();
    assert!(matches!(err, Error::Refused(_)), "{err:?}");
    assert_eq!(std::fs::read(&f.outside).unwrap(), b"old secret\n");
}

#[test]
fn a_symlinked_source_dir_to_outside_is_refused() {
    let f = fixture();
    let elsewhere = f.outside.parent().unwrap().join("elsewhere");
    std::fs::create_dir_all(&elsewhere).unwrap();
    std::fs::write(elsewhere.join("x"), b"old\n").unwrap();
    std::os::unix::fs::symlink(&elsewhere, f.ws.join("sdist/top/out")).unwrap();
    // `out` is inside the source tree, but its file is not.
    let err = edit(&f.ws, "source/out/x").unwrap_err();
    assert!(matches!(err, Error::Refused(_)), "{err:?}");
}

#[test]
fn quilts_state_directory_is_refused() {
    let f = fixture();
    let err = edit(&f.ws, "source/.pc/state").unwrap_err();
    assert!(err.to_string().contains("`.pc`"), "{err}");
}

#[test]
fn a_missing_or_non_file_target_is_refused() {
    let f = fixture();
    assert!(matches!(
        edit(&f.ws, "source/absent"),
        Err(Error::Refused(_))
    ));
    assert!(matches!(edit(&f.ws, "source/main"), Err(Error::Refused(_))));
}

#[test]
fn scratch_files_must_be_under_the_workspace_and_not_source() {
    let f = fixture();
    let call = |old: &str, new: &str| {
        edit_in_workspace(
            &f.ws,
            Path::new("source/main/mysqldump.result"),
            Path::new(old),
            Path::new(new),
        )
    };
    // Under source/: the sources are not scratch space.
    std::fs::write(f.ws.join("sdist/top/s.txt"), "old").unwrap();
    let err = call("source/s.txt", "byte-edit/new.txt").unwrap_err();
    assert!(err.to_string().contains("under source/"), "{err}");
    // Absolute and escaping forms.
    for old in [
        f.ws.join("byte-edit/old.txt").display().to_string(),
        "../outside".to_string(),
    ] {
        let err = call(&old, "byte-edit/new.txt").unwrap_err();
        assert!(matches!(err, Error::Refused(_)), "{old}: {err:?}");
    }
    // A symlink in the workspace that leads outside it.
    std::os::unix::fs::symlink(&f.outside, f.ws.join("byte-edit/link")).unwrap();
    let err = call("byte-edit/link", "byte-edit/new.txt").unwrap_err();
    assert!(err.to_string().contains("resolves outside"), "{err}");
    assert_eq!(main_file(&f.ws), b"caf\xe9 old\r\nline\r\n");
}

#[test]
fn scratch_text_must_be_utf8() {
    let f = fixture();
    std::fs::write(f.ws.join("byte-edit/bad.txt"), b"\xe9").unwrap();
    let err = edit_in_workspace(
        &f.ws,
        Path::new("source/main/mysqldump.result"),
        Path::new("byte-edit/bad.txt"),
        Path::new("byte-edit/new.txt"),
    )
    .unwrap_err();
    assert!(err.to_string().contains("not UTF-8"), "{err}");
    assert_eq!(main_file(&f.ws), b"caf\xe9 old\r\nline\r\n");
}

#[test]
fn a_text_with_no_single_match_is_the_byte_edit_error() {
    let f = fixture();
    std::fs::write(f.ws.join("byte-edit/old.txt"), "absent").unwrap();
    let err = edit(&f.ws, "source/main/mysqldump.result").unwrap_err();
    assert!(matches!(err, Error::Matches { count: 0, .. }), "{err:?}");
    assert_eq!(main_file(&f.ws), b"caf\xe9 old\r\nline\r\n");
}
