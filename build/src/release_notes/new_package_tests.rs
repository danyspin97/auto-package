//! The recap of a package packaged for the first time.

use super::*;

fn rel(tag: &str, body: &str) -> RepoRelease {
    RepoRelease {
        tag: tag.into(),
        url: format!("https://github.com/o/r/releases/tag/{tag}"),
        body: body.into(),
    }
}

#[test]
fn a_first_package_takes_the_release_of_its_own_version_only() {
    let all = vec![rel("v0.7.27", "- new"), rel("v0.7.26", "- old")];
    let got = release_of(&all, "https://github.com/o/r", "0.7.27");
    assert_eq!(got.len(), 1);
    assert_eq!(got[0].tag, "v0.7.27");
    assert!(release_of(&all, "https://github.com/o/r", "0.8.0").is_empty());
    assert!(release_of(&[], "https://github.com/o/r", "0.7.27").is_empty());
}

#[test]
fn a_first_package_without_forge_or_archive_has_no_notes_and_no_problem() {
    let got = gather(None, None, None, "1.0", false);
    assert_eq!(got, Recap::default());
}

#[test]
fn a_first_package_reads_the_top_section_of_the_changelog_in_its_archive() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("pkg-1.2.0");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::write(
        src.join("CHANGELOG.md"),
        "# Changelog\r\n\r\n## 1.2.0\r\n- Add a thing\r\n- Fix a bug\r\n\r\n## 1.1.0\r\n- Older\r\n",
    )
    .unwrap();
    let archive = dir.path().join("pkg-1.2.0.tar.gz");
    let status = std::process::Command::new("tar")
        .arg("czf")
        .arg(&archive)
        .arg("-C")
        .arg(dir.path())
        .arg("pkg-1.2.0")
        .status()
        .unwrap();
    assert!(status.success());
    let got = gather(None, Some(&archive), None, "1.2.0", false);
    assert_eq!(got.releases[0].1, ["* Add a thing", "* Fix a bug"]);
    assert_eq!(got.fetch_problem, None);
}

#[test]
fn the_recap_goes_under_the_update_bullet_of_the_new_package_entry() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("p.changes"),
        "-------------------------------------------------------------------\n\
         Thu Nov 12 14:58:34 UTC 2026 - A <a@b.c>\n\n- Update to 1.2.0\n- Add gcc-c++\n\n",
    )
    .unwrap();
    let archive_dir = dir.path().join("pkg-1.2.0");
    std::fs::create_dir_all(&archive_dir).unwrap();
    std::fs::write(archive_dir.join("NEWS"), "1.2.0\n* Add a thing\n").unwrap();
    let status = std::process::Command::new("tar")
        .arg("czf")
        .arg(dir.path().join("pkg-1.2.0.tar.gz"))
        .arg("-C")
        .arg(dir.path())
        .arg("pkg-1.2.0")
        .status()
        .unwrap();
    assert!(status.success());
    let (missing, problem) = add_recap(dir.path(), None, None, "1.2.0", false).unwrap();
    assert!(!missing && problem.is_none());
    let text = std::fs::read_to_string(dir.path().join("p.changes")).unwrap();
    let at = |s: &str| text.find(s).unwrap();
    assert!(at("- Update to 1.2.0") < at("* Add a thing"));
    assert!(at("* Add a thing") < at("- Add gcc-c++"));
}

#[test]
fn without_an_agent_long_notes_name_the_file_and_call_no_model() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("pkg-1.2.0");
    std::fs::create_dir_all(&src).unwrap();
    let many: String = (0..12).map(|i| format!("- change {i}\n")).collect();
    std::fs::write(src.join("CHANGELOG.md"), format!("## 1.2.0\n{many}")).unwrap();
    let archive = dir.path().join("pkg-1.2.0.tar.gz");
    let status = std::process::Command::new("tar")
        .arg("czf")
        .arg(&archive)
        .arg("-C")
        .arg(dir.path())
        .arg("pkg-1.2.0")
        .status()
        .unwrap();
    assert!(status.success());
    let got = gather(None, Some(&archive), None, "1.2.0", true);
    assert_eq!(
        got.releases[0].1,
        ["* Too many changes to list, see CHANGELOG.md in the sources"]
    );
}
