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

fn no_model(_: &str) -> Option<String> {
    None
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
    let got = several("1.14.1", "1.13.0", found, Vec::new(), no_model).unwrap();
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
        several("1.14.1", "1.13.0", beaker_found(), Vec::new(), no_model).unwrap();
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
    let got = several("1.14.1", "1.13.0", Vec::new(), found, no_model).unwrap();
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

fn found_of(version: &str, link: &str, body: &str) -> Found {
    Found {
        version: version.into(),
        link: link.into(),
        body: body.into(),
    }
}

#[test]
fn each_version_takes_its_changelog_entry_and_the_forge_fills_the_rest() {
    let changelog = vec![found_of("2.0", "NEWS in the sources", "- changelog two")];
    let forge = vec![
        found_of("2.0", "https://example.org/r/2.0", "- forge two"),
        found_of("1.0", "https://example.org/r/1.0", "- forge one"),
    ];
    let got = several("2.0", "0.9", changelog, forge, no_model).unwrap();
    assert_eq!(
        got,
        vec![
            ("2.0".to_string(), lines(&["* changelog two"])),
            ("1.0".to_string(), lines(&["* forge one"])),
        ]
    );
}

#[test]
fn a_changelog_that_covers_the_window_is_read_whatever_the_forge_says() {
    let changelog = vec![
        found_of("2.0", "NEWS in the sources", "- changelog two"),
        found_of("1.0", "NEWS in the sources", "- changelog one"),
    ];
    let forge = vec![
        found_of("2.0", "https://example.org/r/2.0", "- forge two"),
        found_of("1.0", "https://example.org/r/1.0", "- forge one"),
    ];
    let got = several("2.0", "0.9", changelog, forge, no_model).unwrap();
    assert_eq!(
        got,
        vec![
            ("2.0".to_string(), lines(&["* changelog two"])),
            ("1.0".to_string(), lines(&["* changelog one"])),
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
    let got = several(
        "1.14.1",
        "1.13.0",
        Vec::new(),
        from_releases(repo, &all),
        no_model,
    )
    .unwrap();
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
    let got = several("1.14.1", "1.13.0", no_own, Vec::new(), no_model).unwrap();
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
    assert!(several("1.14.1", "1.14.0", one, Vec::new(), no_model).is_none());
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
        no_model,
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
fn a_release_whose_bullets_wrap_past_the_lines_limit_shrinks_to_its_link() {
    let long: Vec<String> = (0..10)
        .map(|k| format!("Change {k} {}", "word ".repeat(60).trim_end()))
        .collect();
    let releases = vec![release(2, long), release(1, items(3, "b"))];
    assert!(super::per_release::fitting(&releases).is_none());
    let lists = super::per_release::shrunk(&releases);
    assert_eq!(lists[0], ["see https://example.org/r/2"]);
    assert_eq!(lists[1], items(3, "b"));
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
fn exactly_the_total_of_releases_is_listed_as_they_are() {
    let releases: Vec<_> = (1..=40).rev().map(|v| release(v, items(1, "x"))).collect();
    let lists = super::per_release::shrunk(&releases);
    assert_eq!(lens(&lists), vec![1; 40]);
}

#[test]
fn past_the_total_the_newest_releases_stay_and_the_rest_is_one_see_line() {
    for count in [41, 100] {
        let releases: Vec<_> = (1..=count)
            .rev()
            .map(|v| release(v, items(1, "x")))
            .collect();
        let lists = super::per_release::shrunk(&releases);
        assert!(lists.iter().map(Vec::len).sum::<usize>() <= 40, "{count}");
        // The newest 39 are listed; the rest is named by the one next down.
        assert_eq!(lens(&lists[..38]), vec![1; 38], "{count}");
        let next = count - 39;
        assert_eq!(
            lists[38],
            [
                format!("see https://example.org/r/{}", count - 38),
                format!("see https://example.org/r/{next} for the older releases"),
            ],
            "{count}"
        );
        assert!(lists[39..].iter().all(Vec::is_empty), "{count}");
    }
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
fn a_long_bullet_with_no_sentence_or_clause_end_is_kept_whole() {
    let bullet = "word ".repeat(60);
    let lists = super::per_release::fitting(&[release(1, vec![bullet.trim().into()])]).unwrap();
    assert_eq!(lists[0][0], bullet.trim());
}

#[test]
fn a_long_bullet_is_cut_at_its_clause_end_without_an_ellipsis() {
    let bullet = format!("Add a parser option, {}", "word ".repeat(60));
    let lists = super::per_release::fitting(&[release(1, vec![bullet.trim().into()])]).unwrap();
    assert_eq!(lists[0][0], "Add a parser option");
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

fn pair(version: &str, l: &[&str]) -> (String, Vec<String>) {
    (version.to_string(), lines(l))
}

/// The Beaker jump from the forge, with 1.14.0 as prose (it needs the model).
fn prose_jump() -> Vec<Found> {
    from_releases(
        "https://github.com/o/r",
        &[
            rel(
                "v1.14.1",
                "- Drop the deprecated beaker.session.type alias\n- Fix the session cookie\n",
            ),
            rel(
                "v1.14.0",
                "A rewrite of the cache layer.\n\nSecurity: fixes CVE-2024-9 in the file lock.",
            ),
        ],
    )
}

const GOOD_REPLY: &str = "## 1.14.1\n* Fix the session cookie\n\
    * Breaking: the beaker.session.type alias is gone\n\
    ## 1.14.0\n* Rewrite the cache layer\n* Fix CVE-2024-9 in the file lock\n";

#[test]
fn a_model_recap_that_holds_is_used_breaking_changes_first() {
    let got = several("1.14.1", "1.13.0", Vec::new(), prose_jump(), |notes| {
        assert!(notes.contains("## 1.14.0"), "{notes}");
        Some(GOOD_REPLY.to_string())
    })
    .unwrap();
    assert_eq!(
        got,
        vec![
            pair(
                "1.14.1",
                &[
                    "* Breaking: the beaker.session.type alias is gone",
                    "* Fix the session cookie",
                ]
            ),
            pair(
                "1.14.0",
                &[
                    "* Fix CVE-2024-9 in the file lock",
                    "* Rewrite the cache layer"
                ]
            ),
        ]
    );
}

#[test]
fn a_model_recap_that_breaks_a_rule_falls_back_to_the_shrunk_lists() {
    let fallback = several("1.14.1", "1.13.0", Vec::new(), prose_jump(), no_model).unwrap();
    assert_eq!(
        fallback[1],
        pair(
            "1.14.0",
            &[
                "* Security: fixes CVE-2024-9 in the file lock.",
                "* see https://github.com/o/r/releases/tag/v1.14.0",
            ]
        )
    );
    let long = "x ".repeat(90);
    let many: String = (0..13).map(|i| format!("* item {i}\n")).collect();
    let bad = [
        // A release is dropped.
        "## 1.14.1\n* Fix the cookie\n".to_string(),
        // A security reference of the notes is dropped.
        "## 1.14.1\n* Fix the cookie\n## 1.14.0\n* Rewrite the cache layer\n".to_string(),
        // A release name that is not one of the notes.
        "## 1.14.1\n* Fix the cookie\n## 1.14.2\n* Rewrite\n".to_string(),
        // More bullets than a release may list.
        format!("## 1.14.1\n{many}## 1.14.0\n* Fix CVE-2024-9 in the file lock\n"),
        // A bullet longer than its limit.
        format!("## 1.14.1\n* {long}\n## 1.14.0\n* Fix CVE-2024-9 in the file lock\n"),
        // Prose where bullets are required.
        "## 1.14.1\nFixes the cookie.\n## 1.14.0\n* Fix CVE-2024-9 in the file lock\n".to_string(),
        // A release with no bullet.
        "## 1.14.1\n## 1.14.0\n* Fix CVE-2024-9 in the file lock\n".to_string(),
        // A bullet before any release.
        "* Fix the cookie\n## 1.14.1\n## 1.14.0\n* Fix CVE-2024-9 in the file lock\n".to_string(),
    ];
    for reply in bad {
        let got = several("1.14.1", "1.13.0", Vec::new(), prose_jump(), |_| {
            Some(reply.clone())
        });
        assert_eq!(got.as_ref(), Some(&fallback), "reply {reply:?} was used");
    }
}

#[test]
fn the_total_limit_holds_for_a_model_recap() {
    let releases: Vec<_> = (1..=4)
        .rev()
        .map(|v| super::per_release::Release {
            version: format!("1.{v}.0"),
            link: String::new(),
            bullets: vec!["Fix a thing".into()],
            prose: None,
        })
        .collect();
    let answer = |n: usize| {
        (1..=4)
            .rev()
            .map(|v| format!("## 1.{v}.0\n{}", "* Fix a thing\n".repeat(n)))
            .collect::<String>()
    };
    assert!(super::agent_recap::checked(&releases, &answer(11)).is_err());
    assert!(super::agent_recap::checked(&releases, &answer(10)).is_ok());
}

#[test]
fn a_model_is_not_asked_when_the_notes_fit() {
    let found = from_releases(
        "https://github.com/o/r",
        &[
            rel("v1.14.1", "- Fix the cookie\n"),
            rel("v1.14.0", "- Add a cache\n"),
        ],
    );
    let got = several("1.14.1", "1.13.0", Vec::new(), found, |_| {
        panic!("the notes fit: no model call")
    })
    .unwrap();
    assert_eq!(got.len(), 2);
}

#[test]
fn a_release_with_no_notes_gets_no_bullet_in_a_model_recap() {
    let found = from_releases(
        "https://github.com/o/r",
        &[
            rel("v1.14.1", "A rewrite of the cache."),
            rel("v1.14.0", "<!-- template -->"),
            rel("v1.13.5", "- Fix the lock\n"),
        ],
    );
    let reply = "## 1.14.1\n* Rewrite the cache layer\n## 1.13.5\n* Fix the lock\n";
    let got = several("1.14.1", "1.13.0", Vec::new(), found, |_| {
        Some(reply.to_string())
    })
    .unwrap();
    assert_eq!(
        got,
        vec![
            pair("1.14.1", &["* Rewrite the cache layer"]),
            pair("1.13.5", &["* Fix the lock"]),
        ]
    );
}

#[test]
fn the_prompt_states_the_limits_once_from_caps() {
    let p = super::summarize::release_prompt("notes");
    assert!(p.contains("at most 12 bullets per release"), "{p}");
    assert!(p.contains("at most 40 bullets in all"), "{p}");
    assert!(p.contains("at most 160 characters"), "{p}");
    assert!(p.contains("`Breaking: `"), "{p}");
}

#[test]
fn no_agent_gives_the_shrunk_recap_and_calls_no_model() {
    let dir = tempfile::tempdir().unwrap();
    let top = dir.path().join("pkg-1.14.1");
    std::fs::create_dir(&top).unwrap();
    std::fs::write(
        top.join("NEWS"),
        "## 1.14.1\n- Fix the cookie\n\n## 1.14.0\nA rewrite of the cache layer.\n\n## 1.13.0\n- Older\n",
    )
    .unwrap();
    let archive = dir.path().join("pkg-1.14.1.tar.gz");
    let status = std::process::Command::new("tar")
        .arg("czf")
        .arg(&archive)
        .arg("-C")
        .arg(dir.path())
        .arg("pkg-1.14.1")
        .status()
        .unwrap();
    assert!(status.success());
    let got = super::gather(None, Some(&archive), Some("1.13.0"), "1.14.1", true);
    assert_eq!(
        got.releases,
        vec![
            pair("1.14.1", &["* Fix the cookie"]),
            pair("1.14.0", &["* see NEWS in the sources"]),
        ]
    );
}
