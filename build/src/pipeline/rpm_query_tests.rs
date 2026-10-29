use super::*;
use crate::ship_condition::{FilesCondition, Verdict, check};

fn cond(package: Option<&str>) -> FilesCondition {
    FilesCondition {
        path: "foo/utils".into(),
        package: package.map(str::to_string),
        not_in: Vec::new(),
    }
}

/// An rpms dir with empty files named like built RPMs (nested one level, as
/// OBS lays them out) and a fake `rpm` that answers from `table`:
/// file name -> (SOURCERPM, NAME, file list).
fn fake_dir(names: &[&str]) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let sub = dir.path().join("noarch");
    std::fs::create_dir_all(&sub).unwrap();
    for n in names {
        std::fs::write(sub.join(n), b"").unwrap();
    }
    dir
}

#[test]
fn the_file_lists_of_every_rpm_of_the_source_package_are_read_through_the_runner() {
    let dir = fake_dir(&[
        "python-foo-1.0-0.noarch.rpm",
        "python-foo-devel-1.0-0.noarch.rpm",
        "python-foo-1.0-0.src.rpm",
        // Names that merely contain the key, from another source package.
        "python-foo-extras-1.0-0.noarch.rpm",
        "python-food-1.0-0.noarch.rpm",
        "python-bar-1.0-0.noarch.rpm",
    ]);
    let calls = std::sync::Mutex::new(Vec::<String>::new());
    let runner = |args: &[&str], rpm: &Path| -> Option<String> {
        let file = rpm.file_name().unwrap().to_string_lossy().into_owned();
        calls
            .lock()
            .unwrap()
            .push(format!("{} {file}", args.join(" ")));
        let (source, name, files): (&str, &str, &str) = match file.as_str() {
            "python-foo-1.0-0.noarch.rpm" => (
                "python-foo-1.0-0.src.rpm",
                "python-foo",
                "/usr/lib/python3/site-packages/foo/utils/a.py\n/usr/lib/python3/site-packages/foo/__init__.py\n",
            ),
            "python-foo-devel-1.0-0.noarch.rpm" => (
                "python-foo-1.0-0.src.rpm",
                "python-foo-devel",
                "/usr/include/foo/foo.h\n",
            ),
            "python-foo-extras-1.0-0.noarch.rpm" => (
                "python-foo-extras-2.0-0.src.rpm",
                "python-foo-extras",
                "/x\n",
            ),
            "python-food-1.0-0.noarch.rpm" => ("python-food-1.0-0.src.rpm", "python-food", "/y\n"),
            _ => return None,
        };
        if args.contains(&"--qf") {
            Some(format!("{source}\n{name}\n"))
        } else if args == ["-qlp"] {
            Some(files.to_string())
        } else {
            None
        }
    };
    let got = package_files_from_rpms_with(dir.path(), "python-foo", &runner);
    let names: Vec<&str> = got.iter().map(|p| p.name.as_str()).collect();
    assert_eq!(names, ["python-foo", "python-foo-devel"]);
    assert_eq!(got[0].files.len(), 2);
    assert!(got[0].files[0].ends_with("foo/utils/a.py"));
    assert!(got.iter().all(|p| !p.from_spec && p.excludes.is_empty()));
    // The source RPM and unrelated RPMs were never queried for their files.
    let calls = calls.lock().unwrap();
    assert!(
        !calls
            .iter()
            .any(|c| c.contains(".src.rpm") || c.contains("python-bar")),
        "{calls:?}"
    );
    // And the listing feeds the condition check.
    assert_eq!(
        check(&cond(Some("python-foo")), &got, "python-foo"),
        Verdict::Satisfied
    );
    assert!(matches!(
        check(&cond(Some("python-foo-devel")), &got, "python-foo"),
        Verdict::Violated(_)
    ));
}

#[test]
fn a_failing_or_missing_rpm_yields_no_packages_and_an_empty_dir_none() {
    let dir = fake_dir(&["python-foo-1.0-0.noarch.rpm"]);
    // The binary is missing or the query fails: nothing is known, not a panic.
    assert!(package_files_from_rpms_with(dir.path(), "python-foo", &|_, _| None).is_empty());
    // A listing that fails after the header leaves the package with no files.
    let header_only = |args: &[&str], _: &Path| -> Option<String> {
        args.contains(&"--qf")
            .then(|| "python-foo-1.0-0.src.rpm\npython-foo\n".to_string())
    };
    let got = package_files_from_rpms_with(dir.path(), "python-foo", &header_only);
    assert_eq!(got.len(), 1);
    assert!(got[0].files.is_empty());
    let empty = tempfile::tempdir().unwrap();
    assert!(package_files_from_rpms_with(empty.path(), "python-foo", &header_only).is_empty());
    assert!(
        package_files_from_rpms_with(&empty.path().join("absent"), "python-foo", &header_only)
            .is_empty()
    );
}

/// Builds a real noarch RPM with a subpackage. SKIPPED (returns early,
/// printing why) where `rpm` or `rpmbuild` is not installed or cannot build.
#[test]
fn real_rpms_are_read_with_the_rpm_binary_when_it_exists() {
    let have = |bin: &str| {
        std::process::Command::new(bin)
            .arg("--version")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok_and(|s| s.success())
    };
    if !have("rpm") || !have("rpmbuild") {
        eprintln!(
            "SKIPPED real_rpms_are_read_with_the_rpm_binary_when_it_exists: rpm/rpmbuild not installed"
        );
        return;
    }
    let top = tempfile::tempdir().unwrap();
    let spec = top.path().join("python-foo.spec");
    std::fs::write(
        &spec,
        "Name: python-foo\nVersion: 1.0\nRelease: 0\nSummary: t\nLicense: MIT\nBuildArch: noarch\n\
             %description\nx\n\
             %package devel\nSummary: d\n%description devel\nd\n\
             %install\nmkdir -p %{buildroot}/usr/lib/python3/site-packages/foo/utils %{buildroot}/usr/include/foo\n\
             touch %{buildroot}/usr/lib/python3/site-packages/foo/utils/a.py %{buildroot}/usr/include/foo/foo.h\n\
             %files\n/usr/lib/python3/site-packages/foo\n%files devel\n/usr/include/foo\n",
    )
    .unwrap();
    let rpms = top.path().join("rpms");
    let built = std::process::Command::new("rpmbuild")
        .args(["-bb", "--define"])
        .arg(format!("_topdir {}", top.path().display()))
        .arg("--define")
        .arg(format!("_rpmdir {}", rpms.display()))
        .arg(&spec)
        .stdin(std::process::Stdio::null())
        .output();
    match built {
        Ok(o) if o.status.success() => {}
        other => {
            eprintln!(
                "SKIPPED real_rpms_are_read_with_the_rpm_binary_when_it_exists: rpmbuild cannot build here: {other:?}"
            );
            return;
        }
    }
    let got = package_files_from_rpms(&rpms, "python-foo");
    let mut names: Vec<&str> = got.iter().map(|p| p.name.as_str()).collect();
    names.sort();
    assert_eq!(names, ["python-foo", "python-foo-devel"], "{got:?}");
    assert_eq!(
        check(&cond(Some("python-foo")), &got, "python-foo"),
        Verdict::Satisfied
    );
    assert!(matches!(
        check(&cond(Some("python-foo-devel")), &got, "python-foo"),
        Verdict::Violated(_)
    ));
    // Another source package's name does not match these RPMs.
    assert!(package_files_from_rpms(&rpms, "python-fo").is_empty());
}
