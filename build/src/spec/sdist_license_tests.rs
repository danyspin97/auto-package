
use super::*;

fn tarball(dir: &std::path::Path, files: &[(&str, &[u8])]) -> std::path::PathBuf {
    let path = dir.join("x-1.0.tar.gz");
    let gz = flate2::write::GzEncoder::new(
        std::fs::File::create(&path).unwrap(),
        flate2::Compression::fast(),
    );
    let mut b = tar::Builder::new(gz);
    for (name, data) in files {
        let mut h = tar::Header::new_gnu();
        h.set_size(data.len() as u64);
        h.set_mode(0o644);
        h.set_cksum();
        b.append_data(&mut h, name, *data).unwrap();
    }
    b.into_inner().unwrap().finish().unwrap();
    path
}

#[test]
fn the_top_level_licence_is_found_and_deeper_ones_are_not() {
    let dir = tempfile::tempdir().unwrap();
    let t = tarball(
        dir.path(),
        &[
            ("x-1.0/src/LICENSE", b"deep".as_slice()),
            ("x-1.0/LICENSE.txt", b"mit".as_slice()),
            ("x-1.0/README.md", b"r".as_slice()),
        ],
    );
    let (name, bytes) = sdist_top_level_license(&t).unwrap();
    assert_eq!(
        (name.as_str(), bytes.as_slice()),
        ("LICENSE.txt", b"mit".as_slice())
    );
}

#[test]
fn the_spec_gets_source1_prep_copy_and_license_entry() {
    let spec = "Source0:        x.whl
Source99:       r
%prep
# n

%files %{python_files}
%{python_sitelib}/x/
";
    let out = with_license_source(spec, "LICENSE");
    assert!(
        out.contains(
            "Source0:        x.whl
# the wheel has no licence text; this one is the sdist's
Source1:        LICENSE
"
        ),
        "{out}"
    );
    assert!(
        out.contains(
            "%prep
cp -p %{SOURCE1} LICENSE
"
        ),
        "{out}"
    );
    assert!(
        out.contains(
            "%files %{python_files}
%license LICENSE
"
        ),
        "{out}"
    );
    assert_eq!(with_license_source(&out, "LICENSE"), out);
}
