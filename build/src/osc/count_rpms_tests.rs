
use super::*;

fn at(secs: u64) -> Option<SystemTime> {
    Some(SystemTime::UNIX_EPOCH + Duration::from_secs(secs))
}

#[test]
fn only_this_packages_rpms_written_since_the_start_count() {
    let since = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000);
    let files = [
        // This package, written during the download.
        ("python-events-0.5-2.1.noarch.rpm".to_string(), at(1_010)),
        // This package, left by an earlier download.
        ("python-events-0.4-1.1.noarch.rpm".to_string(), at(900)),
        // Another package written during the download.
        ("python-other-1.0-1.1.noarch.rpm".to_string(), at(1_010)),
        // A sibling whose name extends this package: not its RPM.
        (
            "python-events-extra-1.0-1.1.noarch.rpm".to_string(),
            at(1_010),
        ),
        // Source RPM and other files.
        ("python-events-0.5-2.1.src.rpm".to_string(), at(1_010)),
        ("_buildenv".to_string(), at(1_010)),
        // Unknown mtime is not counted.
        ("python-events-0.6-1.1.noarch.rpm".to_string(), None),
    ];
    assert_eq!(count_package_rpms_since(&files, "python-events", since), 1);
    assert_eq!(count_package_rpms_since(&files, "python-other", since), 1);
    assert_eq!(count_package_rpms_since(&[], "python-events", since), 0);
}

#[test]
fn a_flavor_prefix_matches_its_own_rpms() {
    let since = SystemTime::UNIX_EPOCH;
    let files = [
        ("pkg-flavor-1.0-1.1.noarch.rpm".to_string(), at(5)),
        ("pkg-1.0-1.1.noarch.rpm".to_string(), at(5)),
    ];
    assert_eq!(count_package_rpms_since(&files, "pkg-flavor", since), 1);
}

#[test]
fn package_rpm_boundary_rule() {
    assert!(is_package_rpm("ffmpeg-8.0-1.x86_64.rpm", "ffmpeg"));
    assert!(!is_package_rpm("ffmpeg-8-libs-8.0-1.x86_64.rpm", "ffmpeg"));
    assert!(!is_package_rpm("ffmpeg-libs-8.0-1.x86_64.rpm", "ffmpeg"));
    assert!(!is_package_rpm("ffmpeg8-8.0-1.x86_64.rpm", "ffmpeg"));
    assert!(!is_package_rpm("ffmpeg-8.0-1.x86_64.src.rpm", "ffmpeg"));
    assert!(!is_package_rpm("ffmpeg-8.0-1.rpm", "ffmpeg"));
}

#[test]
fn package_rpms_since_tolerates_missing_dir() {
    assert_eq!(
        count_package_rpms_since_in_dir(
            std::path::Path::new("/does/not/exist"),
            "pkg",
            SystemTime::UNIX_EPOCH,
        ),
        0
    );
}

#[test]
fn package_rpms_since_reads_the_directory() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("pkg-1.0-1.1.noarch.rpm"), b"").unwrap();
    std::fs::write(tmp.path().join("pkg-1.0-1.1.src.rpm"), b"").unwrap();
    std::fs::write(tmp.path().join("other-1.0-1.1.noarch.rpm"), b"").unwrap();
    let before = SystemTime::now() - Duration::from_secs(3600);
    assert_eq!(
        count_package_rpms_since_in_dir(tmp.path(), "pkg", before),
        1
    );
    let after = SystemTime::now() + Duration::from_secs(3600);
    assert_eq!(count_package_rpms_since_in_dir(tmp.path(), "pkg", after), 0);
}
