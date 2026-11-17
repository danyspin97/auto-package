//! The cleaning of release bodies as forges and templates write them.

use super::render::bullets_of;
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
fn emoji_headings_html_comments_and_crlf_are_cleaned() {
    let body = "<!-- Release template\r\n- do not list me\r\n-->\r\n## \u{2728} What's Changed\r\n\
                - \u{1F680} Add a **fast** mode by @bob in https://github.com/o/r/pull/9\r\n\
                - Fix the crash <!-- inline note --> on exit\r\n\
                \r\n**Full Changelog**: https://github.com/o/r/compare/v1...v2\r\n";
    assert_eq!(
        bullets_of(body),
        ["Add a fast mode", "Fix the crash on exit"]
    );
    assert_eq!(
        recap(None, &[rel("v2", body)], never),
        ["* Add a fast mode", "* Fix the crash on exit"]
    );
}

#[test]
fn an_unterminated_comment_hides_the_rest() {
    assert_eq!(bullets_of("- kept\n<!-- never closed\n- hidden"), ["kept"]);
}

#[test]
fn a_body_of_only_comments_says_nothing() {
    assert!(recap(None, &[rel("v2", "<!-- nothing -->\r\n")], never).is_empty());
}

#[test]
fn a_long_bullet_is_wrapped_within_the_changes_width() {
    let item = "Rework the layout engine so that panels resize without a redraw ".repeat(4);
    let body = format!("- {item}\n");
    let lines = recap(None, &[rel("v2", &body)], never);
    assert!(lines.len() > 1);
    assert!(lines.iter().all(|l| l.len() <= 65), "{lines:?}");
    assert!(lines[1..].iter().all(|l| l.starts_with("  ")));
}

#[test]
fn prose_without_a_model_answer_links_the_release() {
    let lines = recap(None, &[rel("v2", "A big rewrite.")], |_| None);
    assert_eq!(lines[0], "* Too many changes to list, see:");
    assert!(lines[1].contains("/releases/tag/v2"));
}

#[test]
fn bullets_that_wrap_past_the_lines_limit_give_the_link_form_not_a_cut_list() {
    let long = "word ".repeat(60);
    let body: String = (0..10).map(|k| format!("- Change {k} {long}\n")).collect();
    let lines = recap(None, &[rel("v2", &body)], |_| None);
    assert_eq!(lines[0], "* Too many changes to list, see:", "{lines:?}");
    assert!(
        lines.iter().all(|l| !l.starts_with("* Change")),
        "{lines:?}"
    );
}

#[test]
fn a_huge_bullet_is_cut_and_the_recap_stays_short() {
    let body = format!("- {}\n- short\n", "word, ".repeat(100_000));
    let lines = recap(None, &[rel("v2", &body)], never);
    assert!(lines.len() <= 6, "{lines:?}");
    assert!(lines.iter().any(|l| l.ends_with("word")), "{lines:?}");
    assert!(lines.iter().all(|l| l.len() <= 70));
    let many: Vec<String> = (0..10).map(|_| "word ".repeat(60)).collect();
    assert!(!render::fits_lines(&many));
}

#[test]
fn a_word_of_dashes_never_reaches_the_changes_file() {
    let rule = "-".repeat(30);
    let body = format!(
        "- {} {rule} {} {rule}\n- ==== ____ ok\n",
        "word ".repeat(11),
        "end"
    );
    let lines = recap(None, &[rel("v2", &body)], never);
    assert!(lines.iter().all(|l| !l.contains("----")), "{lines:?}");
    assert!(lines.iter().all(|l| !l.contains("====")), "{lines:?}");
    // Even a summary that carries one is wrapped without it.
    let wrapped = render::bullet_lines(&[format!("{} {rule} x", "word ".repeat(12))]);
    assert!(wrapped.iter().all(|l| !l.contains("----")), "{wrapped:?}");
    assert_eq!(render::bullet_lines(&["a --- b".into()]), ["* a --- b"]);
}
