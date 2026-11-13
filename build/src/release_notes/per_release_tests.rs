//! The recap of a jump over several releases. The Beaker fixture is a
//! changelog shaped like the python-Beaker one (1.13.0 to 1.14.1); its notes
//! are made up for the test, not the upstream text.

use super::changelog::section_between;
use super::per_release::{Found, from_releases, from_section, several};
use super::*;
use crate::osc::ReleaseNotes;

const NEWS: &str = "\
## 1.14.1

- Fix the session cookie when the secret is rotated
- Drop the deprecated `beaker.session.type` alias

## 1.14.0

- Add a memory cache backend
- Fix a race in the file lock

## 1.13.0

- Older change
";

const SEP: &str = "-------------------------------------------------------------------";

/// A `.changes` file whose newest entry is `body` and whose older one packages 1.0.
fn changes(body: &str) -> String {
    let old = format!("{SEP}\nMon Jan  1 00:00:00 UTC 2024 - A <a@b.c>\n\n- Update to 1.0\n");
    format!("{SEP}\nTue Jan  2 00:00:00 UTC 2024 - A <a@b.c>\n\n{body}\n{old}")
}

fn lines(l: &[&str]) -> Vec<String> {
    l.iter().map(|s| s.to_string()).collect()
}

fn rel(tag: &str, body: &str) -> RepoRelease {
    RepoRelease {
        tag: tag.into(),
        url: format!("https://github.com/o/r/releases/tag/{tag}"),
        body: body.into(),
    }
}

fn beaker_found() -> Vec<Found> {
    let section = section_between(NEWS, "1.13.0", "1.14.1").unwrap();
    from_section("NEWS", &section, "1.13.0", "1.14.1")
}

#[test]
fn the_changelog_splits_into_one_group_per_release_newest_first() {
    let found = beaker_found();
    let versions: Vec<_> = found.iter().map(|f| f.version.as_str()).collect();
    assert_eq!(versions, ["1.14.1", "1.14.0"]);
    let got = several("1.14.1", "1.13.0", found, Vec::new()).unwrap();
    assert_eq!(
        got,
        vec![
            (
                "1.14.1".to_string(),
                lines(&[
                    "* Fix the session cookie when the secret is rotated",
                    "* Drop the deprecated beaker.session.type alias",
                ]),
            ),
            (
                "1.14.0".to_string(),
                lines(&[
                    "* Add a memory cache backend",
                    "* Fix a race in the file lock",
                ]),
            ),
        ]
    );
}

#[test]
fn the_beaker_jump_is_written_newest_first_under_its_own_bullets() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("python-Beaker.changes");
    std::fs::write(&file, changes("- Update to 1.14.1\n")).unwrap();
    let releases: Vec<ReleaseNotes> =
        several("1.14.1", "1.13.0", beaker_found(), Vec::new()).unwrap();
    crate::osc::set_release_notes(dir.path(), &releases).unwrap();
    let text = std::fs::read_to_string(&file).unwrap();
    let want = [
        "- Update to 1.14.1:",
        "  * Fix the session cookie when the secret is rotated",
        "  * Drop the deprecated beaker.session.type alias",
        "- Update to 1.14.0:",
        "  * Add a memory cache backend",
        "  * Fix a race in the file lock",
    ]
    .join("\n");
    assert!(text.contains(&want), "{text}");
    // A second run with the same notes is the same file.
    crate::osc::set_release_notes(dir.path(), &releases).unwrap();
    assert_eq!(std::fs::read_to_string(&file).unwrap(), text);
}

#[test]
fn forge_releases_give_one_group_each_and_prose_links_its_release() {
    let repo = "https://github.com/o/r";
    let all = [
        rel("v1.14.1", "## Changes\n* Fix the cookie\n"),
        rel("v1.14.0", "A big rewrite of the cache."),
    ];
    let found = from_releases(repo, &all);
    let got = several("1.14.1", "1.13.0", Vec::new(), found).unwrap();
    assert_eq!(
        got,
        vec![
            ("1.14.1".to_string(), lines(&["* Fix the cookie"])),
            (
                "1.14.0".to_string(),
                lines(&["* see https://github.com/o/r/releases/tag/v1.14.0"]),
            ),
        ]
    );
}

#[test]
fn a_release_with_no_notes_gets_no_bullet_and_no_placeholder() {
    let repo = "https://github.com/o/r";
    let all = [
        rel("v1.14.1", "* Fix the cookie\n"),
        rel("v1.14.0", "<!-- template only -->"),
        rel("v1.13.5", "* Older fix\n"),
    ];
    let got = several("1.14.1", "1.13.0", Vec::new(), from_releases(repo, &all)).unwrap();
    let versions: Vec<_> = got.iter().map(|(v, _)| v.as_str()).collect();
    assert_eq!(versions, ["1.14.1", "1.13.5"]);
    assert!(
        got.iter()
            .all(|(_, l)| l.iter().all(|x| !x.contains("1.14.0")))
    );
}

#[test]
fn the_own_release_stays_first_even_without_notes() {
    let no_own = vec![
        Found {
            version: "1.14.1".into(),
            link: "NEWS in the sources".into(),
            body: "<!-- nothing yet -->".into(),
        },
        Found {
            version: "1.14.0".into(),
            link: "NEWS in the sources".into(),
            body: "* Add a memory cache backend\n".into(),
        },
    ];
    let got = several("1.14.1", "1.13.0", no_own, Vec::new()).unwrap();
    assert_eq!(got[0], ("1.14.1".to_string(), Vec::new()));
    assert_eq!(got[1].0, "1.14.0");
}

#[test]
fn one_release_in_the_window_is_not_a_several_release_recap() {
    let one = vec![Found {
        version: "1.14.1".into(),
        link: "NEWS in the sources".into(),
        body: "* Fix the cookie\n".into(),
    }];
    assert!(several("1.14.1", "1.14.0", one, Vec::new()).is_none());
}

#[test]
fn a_window_without_any_notes_gives_no_releases() {
    let got = several(
        "1.14.1",
        "1.13.0",
        Vec::new(),
        from_releases(
            "https://github.com/o/r",
            &[rel("v1.14.1", ""), rel("v1.14.0", "")],
        ),
    );
    assert_eq!(got, Some(Vec::new()));
}
