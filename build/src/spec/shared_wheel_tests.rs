use super::*;
use ap_ecosystem::sdist::WheelLayout;

fn layout(files: &[&str]) -> WheelLayout {
    WheelLayout {
        files: files.iter().map(|f| f.to_string()).collect(),
        dist_info: files
            .iter()
            .find_map(|f| f.split('/').next().filter(|t| t.ends_with(".dist-info")))
            .map(str::to_string),
        ..Default::default()
    }
}

fn rel() -> Release {
    Release {
        name: "x".into(),
        version: "1".into(),
        summary: Some("S".into()),
        homepage: Some("https://example.org".into()),
        license: License::Spdx("MIT".into()),
        requires_python: None,
        requires_dist: Vec::new(),
        yanked: false,
        sdist: ap_ecosystem::pypi::Sdist {
            filename: String::new(),
            url: String::new(),
            sha256: String::new(),
            size: 0,
        },
    }
}

fn files_of(spec: &str) -> Vec<String> {
    let f = spec.split("%files %{python_files}\n").nth(1).unwrap();
    f.split("%changelog")
        .next()
        .unwrap()
        .lines()
        .map(str::to_string)
        .collect()
}

#[test]
fn a_directory_a_sibling_fills_is_co_owned_not_listed_whole() {
    // -base owns the regular package `pk/_mlir`; -cu13 adds the arch half.
    let base = vec![SiblingWheel {
        name: "python-pk-base".to_string(),
        files: vec![
            "pk/_mlir/__init__.py".to_string(),
            "pk/_mlir/a.py".to_string(),
        ],
        purelib: Some(false),
    }];
    let l = layout(&[
        "pk/_mlir/_ods.py",
        "pk/_mlir/ir.cpython-313-x86_64-linux-gnu.so",
        "pk/lib/libx.so",
        "pk_cu13-1.dist-info/METADATA",
    ]);
    let spec = wheel_binary_skeleton_shared(
        "pk-cu13",
        "1",
        "pk_cu13-1-cp313-cp313-manylinux_2_28_x86_64.whl",
        &rel(),
        Some(false),
        Some(&l),
        &base,
    );
    let files = files_of(&spec);
    assert_eq!(
        files[..7].to_vec(),
        vec![
            "%dir %{python_sitearch}/pk",
            "%dir %{python_sitearch}/pk/_mlir",
            "%{python_sitearch}/pk/_mlir/_ods.py",
            "%{python_sitearch}/pk/_mlir/ir.cpython-313-x86_64-linux-gnu.so",
            "%dir %{python_sitearch}/pk/_mlir/__pycache__",
            "%{python_sitearch}/pk/_mlir/__pycache__/_ods.*.pyc",
            "%{python_sitearch}/pk/lib/",
        ]
    );
    assert!(
        spec.contains("Requires:       python-base\nRequires:       python-pk-base\n"),
        "{spec}"
    );
    // Alone, the same wheel lists the package directory whole.
    let alone = wheel_binary_skeleton(
        "pk-cu13",
        "1",
        "pk_cu13-1-cp313-cp313-manylinux_2_28_x86_64.whl",
        &rel(),
        Some(false),
        Some(&l),
    );
    assert!(
        files_of(&alone).contains(&"%{python_sitearch}/pk/_mlir/".to_string()),
        "{alone}"
    );
    assert!(!alone.contains("python-pk-base"));
}

#[test]
fn the_owner_of_the_init_needs_no_requires_on_itself() {
    let l = layout(&["pk/__init__.py", "pk/b.py", "pk-1.dist-info/METADATA"]);
    let sw = |files: &[&str]| {
        vec![SiblingWheel {
            name: "python-pk-core".to_string(),
            files: files.iter().map(|f| f.to_string()).collect(),
            purelib: None,
        }]
    };
    let sib = sw(&["pk/c.py"]);
    assert!(shared_package_owners(&l, &sib).is_empty());
    let sib = sw(&["pk/__init__.py"]);
    assert!(shared_package_owners(&l, &sib).is_empty());
    let l2 = layout(&["pk/b.py"]);
    assert_eq!(
        shared_package_owners(&l2, &sib),
        vec!["python-pk-core".to_string()]
    );
}

#[test]
fn siblings_are_found_by_name_and_listings_parse() {
    let n = |f| sibling_rpm_name(f, "nvidia-cutlass-dsl-libs-cu13");
    assert_eq!(
        n("python313-nvidia-cutlass-dsl-libs-base-4.5.0-1.1.x86_64.rpm").as_deref(),
        Some("python-nvidia-cutlass-dsl-libs-base")
    );
    assert_eq!(
        n("python313-nvidia-cutlass-dsl-libs-cu13-4.5.0-1.1.x86_64.rpm"),
        None
    );
    assert_eq!(n("python313-numpy-2.0-1.1.x86_64.rpm"), None);
    assert_eq!(
        n("python313-nvidia-cutlass-dsl-libs-base-4.5.0-1.1.src.rpm"),
        None
    );
    assert_eq!(
        n("python-nvidia-cutlass-dsl-libs-base-4.5.0-1.1.noarch.rpm"),
        None
    );
    let listing = "/usr/lib/python3.13/site-packages/pk\n/usr/lib/python3.13/site-packages/pk/a.py\n\
                       /usr/share/doc/x\n/usr/lib/python3.13/site-packages/pk/sub\n\
                       /usr/lib/python3.13/site-packages/pk/sub/b.so\n";
    assert_eq!(site_package_files(listing), vec!["pk/a.py", "pk/sub/b.so"]);
    assert!(sibling_wheels(std::path::Path::new("/nonexistent-r2b"), None, "x").is_empty());
}
