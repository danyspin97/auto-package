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
                    "* Drop the deprecated beaker.session.type alias",
                    "* Fix the session cookie when the secret is rotated",
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
        "  * Drop the deprecated beaker.session.type alias",
        "  * Fix the session cookie when the secret is rotated",
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

fn release(v: usize, bullets: Vec<String>) -> super::per_release::Release {
    super::per_release::Release {
        version: format!("1.{v}.0"),
        link: format!("https://example.org/r/{v}"),
        bullets,
        prose: None,
    }
}

fn items(n: usize, tag: &str) -> Vec<String> {
    (0..n).map(|k| format!("Change {tag}{k}")).collect()
}

#[test]
fn a_recap_within_the_limits_is_not_shrunk() {
    let releases = vec![release(2, items(12, "a")), release(1, items(12, "b"))];
    let lists = super::per_release::fitting(&releases).unwrap();
    assert_eq!(lens(&lists), [12, 12]);
}

#[test]
fn the_oldest_releases_shrink_first_to_six_bullets() {
    let releases: Vec<_> = (1..=4).rev().map(|v| release(v, items(12, "x"))).collect();
    assert!(super::per_release::fitting(&releases).is_none());
    let lists = super::per_release::shrunk(&releases);
    // 48 lines do not fit 40: the oldest goes to 6, which is enough.
    assert_eq!(lens(&lists), [12, 12, 6, 6]);
}

#[test]
fn then_each_older_release_becomes_its_link_oldest_first() {
    let releases: Vec<_> = (1..=6).rev().map(|v| release(v, items(12, "x"))).collect();
    let lists = super::per_release::shrunk(&releases);
    assert_eq!(lens(&lists), [12, 6, 6, 6, 6, 1]);
    assert_eq!(lists[5], ["see https://example.org/r/1"]);
    assert!(lists.iter().map(Vec::len).sum::<usize>() <= 40);
}

#[test]
fn the_newest_release_keeps_its_full_allowance() {
    let releases: Vec<_> = (1..=6).rev().map(|v| release(v, items(20, "x"))).collect();
    let lists = super::per_release::shrunk(&releases);
    assert_eq!(lists[0].len(), 12);
    assert!(lists.iter().skip(1).all(|l| l.len() <= 6));
}

#[test]
fn a_security_line_is_never_cut_or_dropped() {
    let long = format!("Fix CVE-2024-1234 when {}", "the path is odd ".repeat(20));
    let mut old = items(11, "o");
    old.push(long.clone());
    let releases = vec![
        release(4, items(12, "n")),
        release(3, items(12, "m")),
        release(2, old),
        release(1, items(12, "p")),
    ];
    let lists = super::per_release::shrunk(&releases);
    assert_eq!(lens(&lists), [12, 12, 6, 6]);
    assert!(lists[2].contains(&long), "{:?}", lists[2]);
    let all = super::per_release::fitting(&[release(1, vec![long.clone()])]).unwrap();
    assert_eq!(all[0], [long]);
}

#[test]
fn a_bullet_is_cut_to_a_hundred_characters_at_a_word() {
    let bullet = "word ".repeat(60);
    let lists = super::per_release::fitting(&[release(1, vec![bullet.trim().into()])]).unwrap();
    assert!(lists[0][0].chars().count() <= 100, "{:?}", lists[0]);
    assert!(lists[0][0].ends_with("..."));
    assert!(lists[0][0].trim_end_matches("...").ends_with("word"));
}

#[test]
fn breaking_changes_come_first_then_security_fixes() {
    let bullets = vec![
        "Add a cache".to_string(),
        "Remove the legacy session class".to_string(),
        "Fix CVE-2024-1 in the parser".to_string(),
    ];
    let lists = super::per_release::fitting(&[release(1, bullets)]).unwrap();
    assert_eq!(
        lists[0],
        [
            "Remove the legacy session class",
            "Fix CVE-2024-1 in the parser",
            "Add a cache"
        ]
    );
}

#[test]
fn prose_is_not_listed_without_condensing_and_shrinks_to_its_link() {
    let prose = super::per_release::Release {
        version: "1.14.0".into(),
        link: "https://example.org/r/0".into(),
        bullets: Vec::new(),
        prose: Some("A big rewrite.\nSecurity: fixes CVE-2024-9 in the loader.".into()),
    };
    assert!(super::per_release::fitting(std::slice::from_ref(&prose)).is_none());
    let lists = super::per_release::shrunk(&[release(2, items(1, "n")), prose]);
    assert_eq!(
        lists[1],
        [
            "Security: fixes CVE-2024-9 in the loader.",
            "see https://example.org/r/0"
        ]
    );
}

fn lens(lists: &[Vec<String>]) -> Vec<usize> {
    lists.iter().map(Vec::len).collect()
}
