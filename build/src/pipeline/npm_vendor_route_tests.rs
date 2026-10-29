use super::*;
use crate::pipeline::npm_vendor::*;

fn sdist(dir: &Path, files: &[&str]) {
    let file = std::fs::File::create(dir.join("pkg-1.0.tar.gz")).unwrap();
    let enc = flate2::write::GzEncoder::new(file, flate2::Compression::fast());
    let mut ar = tar::Builder::new(enc);
    for f in files {
        let mut h = tar::Header::new_gnu();
        h.set_size(2);
        h.set_mode(0o644);
        h.set_cksum();
        ar.append_data(&mut h, format!("pkg-1.0/{f}"), &b"{}"[..])
            .unwrap();
    }
    ar.into_inner().unwrap().finish().unwrap();
}

#[test]
fn first_npm_failure_is_redirected_second_is_not() {
    let dir = tempfile::tempdir().unwrap();
    sdist(dir.path(), &["package.json", "frontend/package-lock.json"]);
    let notes = dir.path().join("notes.md");
    assert!(npm_vendor_route_pending(dir.path(), &notes, "1.0"));
    append_notes(notes.clone(), &format!("x {NPM_VENDOR_NOTE_MARKER}\n"));
    assert!(!npm_vendor_route_pending(dir.path(), &notes, "1.0"));
}

#[test]
fn a_lockfile_outside_node_modules_is_what_the_route_needs() {
    let dir = tempfile::tempdir().unwrap();
    let notes = dir.path().join("notes.md");
    sdist(
        dir.path(),
        &["package.json", "node_modules/x/package-lock.json"],
    );
    assert!(!npm_vendor_route_pending(dir.path(), &notes, "1.0"));
    sdist(dir.path(), &["package-lock.json"]);
    assert!(npm_vendor_route_pending(dir.path(), &notes, "1.0"));
}
