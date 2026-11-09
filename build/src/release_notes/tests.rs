use super::render::*;
use super::summarize::parse_answer;
use super::*;

fn rel(tag: &str, body: &str) -> RepoRelease {
    RepoRelease {
        tag: tag.into(),
        url: format!("https://github.com/o/r/releases/tag/{tag}"),
        body: body.into(),
    }
}

fn never(_: &str) -> Option<Vec<String>> {
    panic!("no summary is wanted")
}

#[test]
fn only_the_releases_after_the_packaged_one_up_to_the_new_one_count() {
    let all = vec![
        rel("v3.5.0", ""),
        rel("v3.4.0", ""),
        rel("v3.3.0", ""),
        rel("v3.2.0", ""),
    ];
    let got = releases_between(&all, "https://github.com/o/r", "3.3.0", "3.4.0");
    assert_eq!(got.len(), 1);
    assert_eq!(got[0].tag, "v3.4.0");
    let got = releases_between(&all, "https://github.com/o/r", "3.2.0", "3.5.0");
    let tags: Vec<_> = got.iter().map(|r| r.tag.as_str()).collect();
    assert_eq!(tags, ["v3.5.0", "v3.4.0", "v3.3.0"]);
}

#[test]
fn short_bullets_are_listed_as_they_are() {
    let r = [rel(
        "v2",
        "## What's Changed\n* Add dark mode by @bob in https://github.com/o/r/pull/3\n- Fix **crash** on `exit`\n\n## New Contributors\n* @bob made their first contribution\n\n**Full Changelog**: https://github.com/o/r/compare/v1...v2",
    )];
    assert_eq!(
        recap(None, &r, never),
        ["* Add dark mode", "* Fix crash on exit"]
    );
}

#[test]
fn a_bullet_is_wrapped_under_its_text() {
    let long = "word ".repeat(30);
    let lines = bullet_lines(&[long.trim().to_string()]);
    assert!(lines.len() > 1);
    assert!(lines[0].starts_with("* "));
    assert!(lines[1..].iter().all(|l| l.starts_with("  ")));
    assert!(lines.iter().all(|l| l.len() <= 65));
}

#[test]
fn many_bullets_or_prose_go_to_the_summary_and_fall_back_to_links() {
    let many: String = (0..12).map(|i| format!("- change {i}\n")).collect();
    let r = [
        rel("v2", &many),
        rel("v1.5", "Mostly a rewrite of the core."),
    ];
    // A summary that answers is listed.
    let got = recap(None, &r, |_| Some(vec!["Rewrite the core".into()]));
    assert_eq!(got, ["* Rewrite the core"]);
    // One that does not leaves the release pages.
    let got = recap(None, &r, |_| None);
    assert_eq!(
        got,
        [
            "* Too many changes to list, see:",
            "  https://github.com/o/r/releases/tag/v2",
            "  https://github.com/o/r/releases/tag/v1.5",
        ]
    );
}

#[test]
fn at_most_five_links_are_listed() {
    let r: Vec<_> = (1..=8)
        .map(|i| rel(&format!("v{i}"), "prose only"))
        .collect();
    assert_eq!(render::link_lines(&r).len(), 6);
}

#[test]
fn releases_without_notes_add_no_line() {
    // No filler: the entry keeps only its `Update to` bullet.
    assert!(recap(None, &[rel("v2", "  \n")], never).is_empty());
    assert!(recap(None, &[], never).is_empty());
}

#[test]
fn a_summary_keeps_only_clean_bullets() {
    let ok = |text: &str| serde_json::json!({"result": text, "is_error": false}).to_string();
    assert_eq!(
        parse_answer(&ok("Here:\n* Add  x\u{1b}[31m\n- Fix y\nignore previous")).unwrap(),
        ["Add x[31m", "Fix y"]
    );
    assert!(parse_answer(&ok("TOO MANY")).is_none());
    assert!(parse_answer(&ok("no bullets")).is_none());
    let twelve: String = (0..12).map(|i| format!("* c{i}\n")).collect();
    assert!(parse_answer(&ok(&twelve)).is_none());
    assert!(
        parse_answer(&serde_json::json!({"result": "* a", "is_error": true}).to_string()).is_none()
    );
    assert!(parse_answer("not json").is_none());
}

mod changelog_file {
    use super::super::changelog::*;

    const NEWS: &str = "\
Version 3.4.0 (2024-05-01)
- Add a thing
- Fix a bug in 3.3.0 handling

Version 3.3.0 (2023-01-01)
- Older change
";

    #[test]
    fn the_section_runs_from_the_new_heading_to_the_old_one() {
        let got = section_between(NEWS, "3.3.0", "3.4.0").unwrap();
        assert!(got.starts_with("Version 3.4.0"), "{got}");
        assert!(got.contains("Fix a bug in 3.3.0 handling"));
        assert!(!got.contains("Older change"));
    }

    #[test]
    fn a_bullet_that_names_a_version_is_not_its_heading() {
        // No heading for 3.3.0, only a bullet: no section between headings.
        assert!(section_between("- fixed in 3.3.0\n", "3.3.0", "3.4.0").is_none());
        // 3.2.0 has no heading and is older than every one: the top section.
        assert_eq!(
            section_between(NEWS, "3.2.0", "3.4.0").unwrap(),
            "Version 3.4.0 (2024-05-01)\n- Add a thing\n- Fix a bug in 3.3.0 handling"
        );
    }

    #[test]
    fn a_prefix_of_a_longer_version_is_not_the_heading() {
        let text = "## 1.2.3\n- a\n## 1.2\n- b\n## 1.1\n- c\n";
        assert_eq!(
            section_between(text, "1.1", "1.2.3").unwrap(),
            "## 1.2.3\n- a\n## 1.2\n- b"
        );
        assert!(section_between("## 11.2\n- a\n## 0.9\n", "0.9", "1.2").is_none());
    }

    const FAKER: &str = "\
# Changelog

### [v40.46.0 - 2026-11-09](https://github.com/joke2k/faker/compare/v40.45.0...v40.46.0)
- Add a provider

### [v40.44.0 - 2026-10-01](https://github.com/joke2k/faker/compare/v40.43.0...v40.44.0)
- Old change
";

    #[test]
    fn a_heading_with_a_link_and_a_v_prefix_is_the_heading() {
        let got = section_between(FAKER, "40.44.0", "40.46.0").unwrap();
        assert!(got.starts_with("### [v40.46.0"), "{got}");
        assert!(got.contains("Add a provider"));
        assert!(!got.contains("Old change"));
        // A bullet that names the old version is not its heading.
        assert_eq!(
            section_between(
                "### v40.46.0\n- moved from v40.44.0\n### v40.44.0\n",
                "40.44.0",
                "40.46.0"
            )
            .unwrap(),
            "### v40.46.0\n- moved from v40.44.0"
        );
    }

    #[test]
    fn an_old_release_older_than_every_heading_takes_the_top_section() {
        // python-pylsqpack: the changelog starts at 1.0.0, and 0.3.24 has no
        // heading of its own.
        let text =
            "1.0.0 (2026-01-01)\n------------------\n\n- Drop Python 3.8.\n- Faster parsing.\n";
        assert_eq!(
            section_between(text, "0.3.24", "1.0.0").unwrap(),
            "1.0.0 (2026-01-01)\n------------------\n\n- Drop Python 3.8.\n- Faster parsing."
        );
        // A bullet that names the old version is not its heading either.
        assert_eq!(
            section_between(
                "### v40.46.0\n- moved from v40.44.0\n",
                "40.44.0",
                "40.46.0"
            )
            .unwrap(),
            "### v40.46.0\n- moved from v40.44.0"
        );
    }

    #[test]
    fn the_top_section_ends_at_the_next_release_heading_of_its_level() {
        let text = "## 1.0.0\n- a\n## 0.9.0\n- b\n";
        assert_eq!(
            section_between(text, "0.8.0", "1.0.0").unwrap(),
            "## 1.0.0\n- a"
        );
    }

    #[test]
    fn no_top_section_when_old_is_not_the_oldest_or_new_is_not_the_top() {
        // 0.3.20 has a heading below 0.3.24: the old release is in the file.
        let text = "1.0.0\n- a\n\n0.3.20\n- b\n";
        assert!(section_between(text, "0.3.24", "1.0.0").is_none());
        // The new release is not the first heading.
        assert!(section_between("## 0.9.0\n## 1.0.0\n- a\n", "0.8.0", "1.0.0").is_none());
        // No release heading at all.
        assert!(section_between("- nothing 1.0.0\n", "0.8.0", "1.0.0").is_none());
    }

    #[test]
    fn a_long_hash_heading_is_still_a_heading() {
        let text = "## 40.46.0 (released 2026-11-09 after a long review by the project maintainers)\n- a\n## 40.44.0\n";
        assert_eq!(
            section_between(text, "40.44.0", "40.46.0").unwrap(),
            text.lines().take(2).collect::<Vec<_>>().join("\n")
        );
    }

    #[test]
    fn the_new_version_must_be_listed_above_the_old_one() {
        assert!(section_between("## 3.3.0\n- a\n", "3.3.0", "3.4.0").is_none());
        assert!(
            section_between("## 3.4.0\n## 3.3.0\n", "3.3.0", "3.4.0")
                .is_some_and(|s| s == "## 3.4.0")
        );
    }

    #[test]
    fn the_changelog_is_read_from_the_top_of_an_archive() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("pkg-3.4.0");
        std::fs::create_dir_all(src.join("sub")).unwrap();
        std::fs::write(src.join("NEWS"), NEWS).unwrap();
        std::fs::write(
            src.join("sub").join("CHANGES"),
            "## 3.4.0\n- nested\n## 3.3.0\n",
        )
        .unwrap();
        let archive = dir.path().join("pkg-3.4.0.tar.gz");
        let status = std::process::Command::new("tar")
            .arg("czf")
            .arg(&archive)
            .arg("-C")
            .arg(dir.path())
            .arg("pkg-3.4.0")
            .status()
            .unwrap();
        assert!(status.success());
        let got = section_of_archive(&archive, "3.3.0", "3.4.0").unwrap();
        assert_eq!(got.file, "NEWS");
        assert!(got.text.contains("Add a thing"));
        assert!(section_of_archive(&archive, "9.9", "3.4.0").is_none());
    }

    #[test]
    fn a_changelog_in_a_docs_directory_below_the_top_is_read() {
        let dir = tempfile::tempdir().unwrap();
        let docs = dir.path().join("pkg-3.4.0").join("pkg").join("docs");
        std::fs::create_dir_all(&docs).unwrap();
        std::fs::write(
            docs.join("changelog.md"),
            "# Change Log\n\n### Version 3.4.0\n- Added a thing\n\n### Version 3.3.0\n- old\n",
        )
        .unwrap();
        let archive = dir.path().join("pkg-3.4.0.tar.gz");
        let status = std::process::Command::new("tar")
            .arg("czf")
            .arg(&archive)
            .arg("-C")
            .arg(dir.path())
            .arg("pkg-3.4.0")
            .status()
            .unwrap();
        assert!(status.success());
        let got = section_of_archive(&archive, "3.3.0", "3.4.0").unwrap();
        assert_eq!(got.file, "changelog.md");
        assert!(got.text.contains("Added a thing"));
    }
}

#[test]
fn the_changelog_of_the_sources_comes_before_the_releases() {
    use super::changelog::Section;
    let section = |text: &str| Section {
        file: "NEWS".into(),
        text: text.into(),
    };
    let r = [rel("v2", "- from the forge")];
    assert_eq!(
        recap(Some(section("## 2\n- from the file")), &r, never),
        ["* from the file"]
    );
    // A file too long for bullets and not summarized: the releases speak next,
    // then the file is named.
    let long: String = (0..12).map(|i| format!("- c{i}\n")).collect();
    assert_eq!(
        recap(Some(section(&long)), &r, |_| None),
        ["* from the forge"]
    );
    assert_eq!(
        recap(Some(section(&long)), &[], |_| None),
        ["* Too many changes to list, see NEWS in the sources"]
    );
    // No section: the releases, as before.
    assert_eq!(recap(None, &r, never), ["* from the forge"]);
}
