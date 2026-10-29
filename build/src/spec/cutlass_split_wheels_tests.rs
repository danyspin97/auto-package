
use super::*;
use std::collections::{BTreeMap, BTreeSet};
use std::io::Write;

/// `(short name, file list, Root-Is-Purelib, platform tag)`.
const WHEELS: [(&str, &str, bool, &str); 4] = [
    (
        "base",
        include_str!("../testdata/cutlass_dsl_libs_base.txt"),
        false,
        "cp313-cp313-manylinux_2_28_x86_64",
    ),
    (
        "core",
        include_str!("../testdata/cutlass_dsl_libs_core.txt"),
        true,
        "py3-none-any",
    ),
    (
        "cu12",
        include_str!("../testdata/cutlass_dsl_libs_cu12.txt"),
        false,
        "cp313-cp313-manylinux_2_28_x86_64",
    ),
    (
        "cu13",
        include_str!("../testdata/cutlass_dsl_libs_cu13.txt"),
        false,
        "cp313-cp313-manylinux_2_28_x86_64",
    ),
];

fn write_wheel(dir: &std::path::Path, short: &str, names: &str, purelib: bool, tag: &str) {
    std::fs::create_dir_all(dir).unwrap();
    let path = dir.join(format!("nvidia_cutlass_dsl_libs_{short}-4.7.1-{tag}.whl"));
    let mut zw = zip::ZipWriter::new(std::fs::File::create(path).unwrap());
    let opts = zip::write::SimpleFileOptions::default();
    for n in names.lines().filter(|l| !l.is_empty()) {
        zw.start_file(n, opts).unwrap();
        let body: String = if n.ends_with(".dist-info/WHEEL") {
            format!("Wheel-Version: 1.0\nRoot-Is-Purelib: {purelib}\nTag: {tag}\n")
        } else if n.ends_with(".dist-info/METADATA") {
            format!(
                "Metadata-Version: 2.4\nName: nvidia-cutlass-dsl-libs-{short}\nVersion: 4.7.1\n"
            )
        } else {
            "x".into()
        };
        zw.write_all(body.as_bytes()).unwrap();
    }
    zw.finish().unwrap();
}

/// A checkouts directory with the four packages' wheels, created in `order`.
fn checkouts(order: &[usize]) -> tempfile::TempDir {
    let tmp = tempfile::tempdir().unwrap();
    for &i in order {
        let (short, names, purelib, tag) = WHEELS[i];
        write_wheel(
            &tmp.path()
                .join(format!("python-nvidia-cutlass-dsl-libs-{short}")),
            short,
            names,
            purelib,
            tag,
        );
    }
    // Unrelated checkouts are no siblings.
    write_wheel(
        &tmp.path().join("python-nvidia-nccl-cu13"),
        "base",
        "x/y.py\n",
        true,
        "py3-none-any",
    );
    tmp
}

fn rel() -> Release {
    Release {
        name: "x".into(),
        version: "4.7.1".into(),
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

/// The skeleton of wheel `i`, its siblings read from `checkouts` like the
/// pipeline does.
fn skeleton(i: usize, checkouts: &std::path::Path) -> String {
    let (short, _, _, tag) = WHEELS[i];
    let own = format!("nvidia-cutlass-dsl-libs-{short}");
    let wheel_dir = checkouts.join(format!("python-{own}"));
    let file = format!("nvidia_cutlass_dsl_libs_{short}-4.7.1-{tag}.whl");
    let wheel = wheel_dir.join(&file);
    let layout = ap_ecosystem::sdist::wheel_layout(&wheel).unwrap();
    let purelib = ap_ecosystem::sdist::wheel_root_is_purelib(&wheel);
    let siblings = sibling_wheels(
        std::path::Path::new("/nonexistent-r3a"),
        wheel_dir.parent(),
        &own,
    );
    wheel_binary_skeleton_shared(
        &own,
        "4.7.1",
        &file,
        &rel(),
        purelib,
        Some(&layout),
        &siblings,
    )
}

fn files_section(spec: &str) -> Vec<String> {
    let f = spec.split("%files %{python_files}\n").nth(1).unwrap();
    f.split("%changelog")
        .next()
        .unwrap()
        .lines()
        .map(str::to_string)
        .filter(|l| !l.is_empty())
        .collect()
}

fn site_path(entry: &str) -> &str {
    entry
        .strip_prefix("%{python_sitelib}/")
        .or_else(|| entry.strip_prefix("%{python_sitearch}/"))
        .unwrap_or_else(|| panic!("not a site path: {entry}"))
}

#[test]
fn siblings_are_found_from_their_wheels_whatever_order_they_appeared_in() {
    let a = checkouts(&[0, 1, 2, 3]);
    let b = checkouts(&[3, 2, 1, 0]);
    let none = std::path::Path::new("/nonexistent-r3a");
    for (i, (short, ..)) in WHEELS.iter().enumerate() {
        let own = format!("nvidia-cutlass-dsl-libs-{short}");
        let sa = sibling_wheels(none, Some(a.path()), &own);
        let sb = sibling_wheels(none, Some(b.path()), &own);
        assert_eq!(sa, sb, "{short}");
        let names: Vec<&str> = sa.iter().map(|s| s.name.as_str()).collect();
        let mut want: Vec<String> = WHEELS
            .iter()
            .filter(|(n, ..)| n != short)
            .map(|(n, ..)| format!("python-nvidia-cutlass-dsl-libs-{n}"))
            .collect();
        want.sort();
        assert_eq!(names, want, "{short}: nccl and itself are no siblings");
        assert_eq!(skeleton(i, a.path()), skeleton(i, b.path()), "{short}");
        // dist-info is never part of what a sibling owns.
        assert!(
            sa.iter()
                .all(|s| s.files.iter().all(|f| !f.contains(".dist-info/")))
        );
    }
    let core = sibling_wheels(none, Some(a.path()), "nvidia-cutlass-dsl-libs-cu13")
        .into_iter()
        .find(|s| s.name.ends_with("-core"))
        .unwrap();
    assert_eq!(core.purelib, Some(true));
}

#[test]
fn the_first_wheel_built_already_sees_the_siblings_that_have_not_been_built() {
    // Only the sibling's wheel exists: no RPM anywhere.
    let only_core = checkouts(&[1, 3]);
    let spec = skeleton(3, only_core.path());
    assert!(
        spec.contains("Requires:       python-nvidia-cutlass-dsl-libs-core\n"),
        "{spec}"
    );
    // -cu13 shares the tree with the pure -core: both land in sitelib.
    assert!(
        spec.contains("%{python_sitelib}/nvidia_cutlass_dsl/cu13/lib/libcute_dsl_runtime.so"),
        "{spec}"
    );
    // And alone (no sibling wheel at all) it follows its own WHEEL file.
    let alone = checkouts(&[3]);
    let spec = skeleton(3, alone.path());
    assert!(
        spec.contains("%{python_sitearch}/nvidia_cutlass_dsl/"),
        "{spec}"
    );
    assert!(!spec.contains("python_sitelib"), "{spec}");
}

/// python-tokenspeed-mla's `%check`: `No module named 'cutlass.cute'`.
/// `nvidia-cutlass-dsl`'s `.pth` puts `nvidia_cutlass_dsl.__path__[0] +
/// /dsl_packages` on `sys.path`, and `nvidia_cutlass_dsl` is a namespace
/// package (no `__init__.py` anywhere), so `__path__[0]` is the FIRST site
/// directory that holds one: with the compiled wheels in sitearch (searched
/// first on openSUSE) and `-core` in sitelib, `cutlass/__init__.py` was
/// found and `cutlass/cute` was not. Merge what the four generated specs
/// install, as rpm does, and resolve the imports on both interpreters.
/// The specs of `-base` and `-cu12` were generated while `-core` was not
/// known, so they sit in sitearch; the spec fix moves them to the tree.
#[test]
fn a_sibling_spec_generated_alone_is_moved_into_the_shared_tree() {
    let tmp = checkouts(&[0, 1, 2, 3]);
    let alone = checkouts(&[2]);
    let old = skeleton(2, alone.path());
    assert!(
        old.contains("%{python_sitearch}/nvidia_cutlass_dsl"),
        "{old}"
    );
    let own = "nvidia-cutlass-dsl-libs-cu12";
    let wheel_dir = tmp.path().join(format!("python-{own}"));
    let wheel =
        wheel_dir.join("nvidia_cutlass_dsl_libs_cu12-4.7.1-cp313-cp313-manylinux_2_28_x86_64.whl");
    let layout = ap_ecosystem::sdist::wheel_layout(&wheel).unwrap();
    let siblings = sibling_wheels(
        std::path::Path::new("/nonexistent-r4b"),
        Some(tmp.path()),
        own,
    );
    let fixed = with_sibling_tree_in_sitelib(&old, &layout, &siblings);
    assert_ne!(fixed, old);
    for l in files_section(&fixed) {
        assert!(!l.contains("python_sitearch"), "{l}\n{fixed}");
    }
    let install = fixed.find("%pyproject_install").unwrap();
    let mv = fixed
        .find("%python_expand mv %{buildroot}%{$python_sitearch}/nvidia_cutlass_dsl ")
        .expect(&fixed);
    let cleanup = fixed
        .find("rm -rf %{buildroot}%{$python_sitelib}/nvidia_cutlass_dsl/cu12/include")
        .expect(&fixed);
    assert!(install < mv && mv < cleanup, "{fixed}");
    assert_eq!(
        with_sibling_tree_in_sitelib(&fixed, &layout, &siblings),
        fixed
    );
    // Without a pure sibling nothing changes.
    assert_eq!(with_sibling_tree_in_sitelib(&old, &layout, &[]), old);
}

#[test]
fn the_merged_install_of_the_four_wheels_resolves_cutlass_cute_in_one_tree() {
    let tmp = checkouts(&[0, 1, 2, 3]);
    // site directory -> files, per interpreter flavor (the macros are the
    // same for both; each flavor expands them against its own directories).
    for flavor in ["python313", "python314"] {
        let mut trees: BTreeMap<&str, BTreeSet<String>> = BTreeMap::new();
        for (i, &(short, names, purelib, _)) in WHEELS.iter().enumerate().take(4) {
            let spec = skeleton(i, tmp.path());
            // Where `%pyproject_install` puts the wheel: its WHEEL file decides.
            let mut placed: Vec<(&str, String)> = names
                .lines()
                .filter(|f| !f.is_empty() && !f.contains(".dist-info/"))
                .map(|f| (if purelib { "sitelib" } else { "sitearch" }, f.to_string()))
                .collect();
            // The relocation lines of the spec, each for every flavor.
            for line in spec.lines().filter(|l| l.contains("mv %{buildroot}")) {
                assert!(line.starts_with("%python_expand "), "{short}: {line}");
                let top = line
                    .split("%{$python_sitearch}/")
                    .nth(1)
                    .and_then(|r| r.split(' ').next())
                    .unwrap();
                for (site, f) in placed.iter_mut() {
                    if f.split('/').next() == Some(top) {
                        *site = "sitelib";
                    }
                }
            }
            // `import cutlass` runs the CUDA IR extension setup, which needs a
            // device (`cannot import name 'ir' from _mlir`): %check never runs it.
            let check = spec
                .split("%check\n")
                .nth(1)
                .unwrap()
                .split("%files")
                .next()
                .unwrap();
            assert!(!check.contains("import cutlass"), "{short}: {check}");
            assert!(
                !check.contains("import nvidia_cutlass_dsl"),
                "{short}: {check}"
            );
            // %files names the directory the file really is in.
            for l in files_section(&spec) {
                assert!(!l.contains("python_sitearch"), "{flavor} {short}: {l}");
            }
            for (site, f) in placed {
                trees.entry(site).or_default().insert(f);
            }
        }
        // One tree: nothing of the namespace is left in sitearch.
        assert_eq!(
            trees.keys().copied().collect::<Vec<_>>(),
            ["sitelib"],
            "{flavor}"
        );
        let lib = &trees["sitelib"];
        // `sys.path` of an openSUSE interpreter: sitearch before sitelib;
        // `__path__[0]` is the first directory with `nvidia_cutlass_dsl/`.
        let first = ["sitearch", "sitelib"]
            .into_iter()
            .find(|s| {
                trees
                    .get(s)
                    .is_some_and(|t| t.iter().any(|f| f.starts_with("nvidia_cutlass_dsl/")))
            })
            .unwrap();
        assert_eq!(first, "sitelib", "{flavor}");
        // Every module of the import chain is in that one tree.
        for need in [
            "nvidia_cutlass_dsl/dsl_packages/cutlass/__init__.py",
            "nvidia_cutlass_dsl/dsl_packages/cutlass/cute/__init__.py",
            "nvidia_cutlass_dsl/dsl_packages/cutlass/_mlir/_mlir_libs/__init__.py",
            "nvidia_cutlass_dsl/dsl_packages/cutlass/_mlir/ir.py",
            "nvidia_cutlass_dsl/cu13/lib/libcute_dsl_runtime.so",
            "nvidia_cutlass_dsl/cu12/lib/libcute_dsl_runtime.so",
        ] {
            assert!(lib.contains(need), "{flavor}: {need} is not in sitelib");
        }
        // And every top-level package directory resolves in the one tree.
        let tops: BTreeSet<&str> = lib.iter().filter_map(|f| f.split('/').next()).collect();
        assert_eq!(
            tops.iter().copied().collect::<Vec<_>>(),
            ["nvidia_cutlass_dsl"],
            "{flavor}"
        );
    }
}

#[test]
fn the_four_skeletons_share_one_tree_without_listing_a_file_twice() {
    let tmp = checkouts(&[0, 1, 2, 3]);
    let specs: Vec<String> = (0..4).map(|i| skeleton(i, tmp.path())).collect();
    let payload = |i: usize| -> Vec<&'static str> {
        WHEELS[i]
            .1
            .lines()
            .filter(|f| !f.contains(".dist-info/") && !f.is_empty())
            .collect()
    };

    for (i, spec) in specs.iter().enumerate() {
        let (short, _, purelib, _) = WHEELS[i];
        // One site directory for the whole tree: sitelib, where the noarch
        // -core must be; the arch wheels carry ExclusiveArch for their .so.
        assert!(
            !spec.contains("python_sitearch}/nvidia_cutlass_dsl/"),
            "{short}: {spec}"
        );
        if purelib {
            assert!(spec.contains("BuildArch:      noarch\n"), "{short}");
            assert!(!spec.contains("ExclusiveArch"), "{short}");
            assert!(!spec.contains("%python_expand mv "), "{short}: {spec}");
        } else {
            assert!(spec.contains("ExclusiveArch:  x86_64\n"), "{short}");
            // The wheel's tree (and its dist-info) is moved next to -core's.
            assert!(
                    spec.contains(
                        "%python_expand mv %{buildroot}%{$python_sitearch}/nvidia_cutlass_dsl %{buildroot}%{$python_sitelib}/\n"
                    ),
                    "{short}: {spec}"
                );
            assert!(spec.contains(&format!(
                    "%python_expand mv %{{buildroot}}%{{$python_sitearch}}/nvidia_cutlass_dsl_libs_{short}-4.7.1.dist-info "
                )), "{short}: {spec}");
        }
        // Nothing writes into another package's dist-info.
        assert!(!spec.contains("RECORD"), "{short}");
    }

    // What each package owns: exact files, plus whole directories (`.../`).
    let mut dirs_listed: Vec<BTreeSet<String>> = Vec::new();
    let mut explicit: Vec<BTreeSet<String>> = Vec::new();
    let mut whole: Vec<BTreeSet<String>> = Vec::new();
    for (i, spec) in specs.iter().enumerate() {
        let mut d = BTreeSet::new();
        let mut e = BTreeSet::new();
        let mut w = BTreeSet::new();
        for line in files_section(spec) {
            if let Some(dir) = line.strip_prefix("%dir ") {
                assert!(
                    d.insert(site_path(dir).to_string()),
                    "{}: %dir twice: {line}",
                    WHEELS[i].0
                );
            } else if line.starts_with("%exclude ")
                || line.starts_with("%license ")
                || line.contains("dist-info")
                || line.contains("/__pycache__/")
            {
                continue;
            } else if let Some(dir) = line.strip_suffix('/') {
                w.insert(site_path(dir).to_string());
            } else {
                assert!(
                    e.insert(site_path(&line).to_string()),
                    "{}: listed twice: {line}",
                    WHEELS[i].0
                );
            }
        }
        dirs_listed.push(d);
        explicit.push(e);
        whole.push(w);
    }

    // Which packages put a file below each directory.
    let mut fillers: BTreeMap<String, BTreeSet<usize>> = BTreeMap::new();
    for i in 0..4 {
        for f in payload(i) {
            let mut dir = f;
            while let Some((parent, _)) = dir.rsplit_once('/') {
                fillers.entry(parent.to_string()).or_default().insert(i);
                dir = parent;
            }
        }
    }
    // No two packages own one file: an explicit entry is one package's, a
    // whole directory contains no other package's file.
    for i in 0..4 {
        for w in &whole[i] {
            for j in (0..4).filter(|j| *j != i) {
                assert!(
                    payload(j).iter().all(|f| !f.starts_with(&format!("{w}/"))),
                    "{} lists {w}/ whole but {} has files in it",
                    WHEELS[i].0,
                    WHEELS[j].0
                );
            }
        }
        for j in (i + 1)..4 {
            assert!(
                explicit[i].is_disjoint(&explicit[j]),
                "{} and {} list the same file: {:?}",
                WHEELS[i].0,
                WHEELS[j].0,
                explicit[i].intersection(&explicit[j]).collect::<Vec<_>>()
            );
            assert!(
                dirs_listed[i].is_disjoint(&whole[j]),
                "a co-owned directory is whole in another package"
            );
        }
    }
    // A directory two packages fill is `%dir` in each of them, listed whole
    // by none.
    for (dir, who) in &fillers {
        if who.len() < 2 {
            continue;
        }
        for &i in who {
            assert!(
                dirs_listed[i].contains(dir),
                "{} fills the shared directory {dir} without owning it with %dir",
                WHEELS[i].0
            );
            assert!(
                !whole[i].contains(dir),
                "{} lists {dir}/ whole",
                WHEELS[i].0
            );
        }
    }
    // Every payload file of a package is owned by it: listed, or below a
    // whole directory (bundled headers, stubs and the static archive are
    // removed in %install, bytecode comes with its module).
    for i in 0..4 {
        for f in payload(i) {
            if f.contains("/include/") || f.contains("/lib/stubs/") || f.ends_with(".a") {
                continue;
            }
            let owned =
                explicit[i].contains(f) || whole[i].iter().any(|w| f.starts_with(&format!("{w}/")));
            assert!(owned, "{} does not own {f}\n{}", WHEELS[i].0, specs[i]);
        }
    }
    // The arch-specific .so stay where the wheel puts them relative to the
    // rest of the tree: cu12/cu13's `_cutlass_ir.cuNN...so` is in `_mlir_libs`
    // beside -base's `__init__.py`, as the wheels were built to be.
    let cu13_so = "nvidia_cutlass_dsl/dsl_packages/cutlass/_mlir/_mlir_libs/_cutlass_ir.cu13.cpython-313-x86_64-linux-gnu.so";
    assert!(explicit[3].contains(cu13_so));
    assert!(WHEELS[0].1.contains("_mlir_libs/__init__.py"));

    // The import chain: -cu13 and -base need -core (`cutlass/__init__.py`);
    // -cu13 needs -base (`_mlir_libs/__init__.py`); -core needs neither.
    assert!(specs[0].contains("Requires:       python-nvidia-cutlass-dsl-libs-core\n"));
    assert!(specs[3].contains("Requires:       python-nvidia-cutlass-dsl-libs-base\n"));
    assert!(specs[3].contains("Requires:       python-nvidia-cutlass-dsl-libs-core\n"));
    assert!(!specs[1].contains("Requires:       python-nvidia-cutlass-dsl-libs-"));
}
