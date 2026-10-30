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
    assert_eq!(recap(&r, never), ["* Add dark mode", "* Fix crash on exit"]);
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
    let got = recap(&r, |_| Some(vec!["Rewrite the core".into()]));
    assert_eq!(got, ["* Rewrite the core"]);
    // One that does not leaves the release pages.
    let got = recap(&r, |_| None);
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
fn releases_without_notes_say_so() {
    assert_eq!(
        recap(&[rel("v2", "  \n")], never),
        ["* No upstream changelog found or provided"]
    );
    assert_eq!(
        recap(&[], never),
        ["* No upstream changelog found or provided"]
    );
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
