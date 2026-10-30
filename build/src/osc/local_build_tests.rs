use super::local_build::*;
use super::*;

fn settings(root: Option<&str>) -> LocalBuild {
    LocalBuild {
        root: root.map(PathBuf::from),
        min_free_gib: 10,
    }
}

#[test]
fn argv_uses_noninteractive_sudo_unless_root() {
    let rpms = Path::new("/w/rpms");
    let user = build_argv(
        &settings(None),
        "openSUSE_Tumbleweed",
        "x86_64",
        "a.spec",
        rpms,
        None,
        false,
    );
    assert_eq!(&user[..4], ["sudo", "-n", "osc", "build"]);
    assert!(user.contains(&"--keep-pkgs=/w/rpms".to_string()));
    assert!(user.contains(&"--prefer-pkgs=/w/rpms".to_string()));
    assert_eq!(
        &user[user.len() - 3..],
        ["openSUSE_Tumbleweed", "x86_64", "a.spec"]
    );
    let root = build_argv(&settings(None), "r", "a", "s.spec", rpms, None, true);
    assert_eq!(&root[..2], ["osc", "build"]);
}

#[test]
fn argv_carries_root_and_oscrc() {
    let argv = build_argv(
        &settings(Some("/big/root")),
        "r",
        "a",
        "s.spec",
        Path::new("/o"),
        Some(Path::new("/home/u/.oscrc")),
        true,
    );
    assert_eq!(&argv[..4], ["osc", "-c", "/home/u/.oscrc", "build"]);
    assert!(argv.contains(&"--root=/big/root".to_string()));
}

#[test]
fn oscrc_search_order() {
    let exists = |p: &Path| p == Path::new("/h/.oscrc") || p == Path::new("/h/.config/osc/oscrc");
    assert_eq!(
        find_oscrc(Some(Path::new("/h")), None, exists),
        Some(PathBuf::from("/h/.config/osc/oscrc"))
    );
    assert_eq!(find_oscrc(None, None, exists), None);
    assert_eq!(
        find_oscrc(Some(Path::new("/h")), Some(Path::new("/x")), |p| p
            == Path::new("/h/.oscrc")),
        Some(PathBuf::from("/h/.oscrc"))
    );
}

#[test]
fn df_output_is_parsed() {
    let out =
        "Filesystem 1024-blocks Used Available Capacity Mounted on\n/dev/x 1000 400 600 40% /\n";
    assert_eq!(parse_df_available(out), Some(600 * 1024));
    assert_eq!(parse_df_available(""), None);
    assert_eq!(parse_df_available("junk\nonly three fields\n"), None);
}

#[test]
fn free_space_floor() {
    let gib = 1u64 << 30;
    let root = Path::new("/var/tmp/build-root");
    assert!(free_space_refusal(10 * gib, 10, root).is_none());
    let msg = free_space_refusal(9 * gib, 10, root).unwrap();
    assert!(msg.contains("floor is 10 GiB") && msg.contains("/var/tmp/build-root"));
    assert!(free_space_refusal(0, 0, root).is_none());
}

#[test]
fn nearest_existing_walks_up() {
    let got = nearest_existing(Path::new("/a/b/c"), |p| p == Path::new("/a"));
    assert_eq!(got, Some(PathBuf::from("/a")));
    assert_eq!(nearest_existing(Path::new("/a/b"), |_| false), None);
}

#[test]
fn argv_keeps_a_path_with_a_space_in_one_argument() {
    let argv = build_argv(
        &settings(Some("/big disk/root")),
        "openSUSE Tumbleweed",
        "x86_64",
        "my pkg.spec",
        Path::new("/w/my rpms/local"),
        None,
        false,
    );
    for want in [
        "--root=/big disk/root",
        "--prefer-pkgs=/w/my rpms/local",
        "--keep-pkgs=/w/my rpms/local",
    ] {
        assert!(argv.contains(&want.to_string()), "{want} in {argv:?}");
    }
    assert_eq!(argv.last().map(String::as_str), Some("my pkg.spec"));
    assert_eq!(argv[argv.len() - 3], "openSUSE Tumbleweed");
}

#[test]
fn environment_problems_are_told_from_package_failures() {
    assert!(
        environment_problem("sudo: a password is required")
            .unwrap()
            .contains("NOPASSWD")
    );
    assert!(
        environment_problem("[  2s] sudo: no tty present and no askpass program specified")
            .is_some()
    );
    assert!(
        environment_problem("error: unpacking failed: write failed - No space left on device")
            .is_some()
    );
    assert!(environment_problem("error: Bad exit status from /var/tmp/rpm-tmp (%build)").is_none());
}

#[test]
fn a_package_test_printing_an_environment_message_is_not_one() {
    // A test's own output, in the middle of the log, and a line that is not
    // osc's or sudo's own at the end.
    let mut log = String::from("sudo: a password is required\n");
    log.push_str("[  5s] test: No space left on device\n");
    log.push_str("[  6s] test: a password is required\n");
    for i in 0..100 {
        log.push_str(&format!("[  {i}s] ok\n"));
    }
    log.push_str("[  9s] test: No space left on device\n");
    assert!(environment_problem(&log).is_none());
}

fn log_with(dir: &Path, text: &str) -> PathBuf {
    let p = dir.join("build.log");
    fs::write(&p, text).unwrap();
    p
}

#[test]
fn exit_zero_is_success_whatever_the_log() {
    let d = tempfile::tempdir().unwrap();
    assert!(local_outcome(0, &d.path().join("none.log"), "a.spec").is_ok());
}

#[test]
fn failed_build_is_a_build_failure_for_the_fix_loop() {
    let d = tempfile::tempdir().unwrap();
    let log = log_with(
        d.path(),
        "[  10s] error: Bad exit status from /var/tmp/rpm-tmp.x (%build)\n",
    );
    match local_outcome(1, &log, "a.spec") {
        Err(Error::BuildFailed { spec, code, log: l }) => {
            assert_eq!((spec.as_str(), code, l), ("a.spec", 1, log));
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn sudo_password_is_not_actionable_and_empty_log_is_io() {
    let d = tempfile::tempdir().unwrap();
    let log = log_with(d.path(), "sudo: a password is required\n");
    assert!(matches!(
        local_outcome(1, &log, "a.spec"),
        Err(Error::NotActionable(_))
    ));
    let empty = log_with(d.path(), "  \n");
    assert!(matches!(
        local_outcome(1, &empty, "a.spec"),
        Err(Error::Io(_))
    ));
}

#[test]
fn osc_and_machine_failures_are_not_actionable() {
    let d = tempfile::tempdir().unwrap();
    for text in [
        "Traceback (most recent call last):\nurllib.error.HTTPError: HTTP Error 401: Unauthorized\n",
        "osc: Name or service not known\n",
        "Connection refused\n",
        "error: build root /var/tmp/build-root is locked by another build\n",
        "osc: unknown repository 'openSUSE_Foo'\n",
    ] {
        let log = log_with(d.path(), text);
        assert!(
            matches!(
                local_outcome(1, &log, "a.spec"),
                Err(Error::NotActionable(_))
            ),
            "{text}"
        );
    }
}

#[test]
fn a_package_failure_without_rpmbuild_markers_is_a_build_failure_only_with_them() {
    let d = tempfile::tempdir().unwrap();
    let dep = log_with(
        d.path(),
        "error: Failed build dependencies:\n\tfoo >= 2 is needed by a-1-1.src\n",
    );
    assert!(matches!(
        local_outcome(1, &dep, "a.spec"),
        Err(Error::BuildFailed { .. })
    ));
    let bare = log_with(d.path(), "something went wrong\n");
    assert!(matches!(
        local_outcome(1, &bare, "a.spec"),
        Err(Error::NotActionable(_))
    ));
}

#[test]
fn unresolvable_log_is_classified() {
    let d = tempfile::tempdir().unwrap();
    let log = log_with(
        d.path(),
        "unresolvable: nothing provides python3-foo needed by x\n",
    );
    match local_outcome(1, &log, "a.spec") {
        Err(Error::UnresolvableDeps { names, .. }) => assert_eq!(names, ["python3-foo"]),
        other => panic!("{other:?}"),
    }
}

#[test]
fn only_fresh_rpms_count() {
    let d = tempfile::tempdir().unwrap();
    fs::write(d.path().join("old.rpm"), "x").unwrap();
    std::thread::sleep(Duration::from_millis(20));
    let since = SystemTime::now();
    std::thread::sleep(Duration::from_millis(20));
    fs::create_dir(d.path().join("noarch")).unwrap();
    fs::write(d.path().join("noarch").join("new.rpm"), "x").unwrap();
    fs::write(d.path().join("new.txt"), "x").unwrap();
    assert_eq!(fresh_rpm_count(d.path(), since), 1);
}

#[test]
fn local_dispatch_never_reaches_obs() {
    // A floor no disk meets: the local path stops at its space check, while the
    // OBS path would have gone to the network.
    let d = tempfile::tempdir().unwrap();
    let local = LocalBuild {
        root: Some(d.path().join("not-yet/build-root")),
        min_free_gib: u64::MAX,
    };
    let r = build_package(
        Some(&local),
        "home:x",
        "pkg",
        "repo",
        "x86_64",
        d.path(),
        &d.path().join("build.log"),
        &d.path().join("rpms"),
        Emit::quiet(),
    );
    match r {
        Err(Error::NotActionable(m)) => assert!(m.contains("not enough free space"), "{m}"),
        other => panic!("{other:?}"),
    }
    assert!(!d.path().join("build.log").exists());
}
