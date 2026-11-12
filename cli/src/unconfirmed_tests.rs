use super::*;
use ap_build::pipeline::CandidateSource::{Agent, Search};

fn cand(url: &str, stars: Option<u64>, releases: Option<u64>) -> Candidate {
    Candidate {
        stars,
        releases,
        archived: Some(false),
        fork: Some(false),
        ..Candidate::new(url, Search)
    }
}

fn pleamar() -> Vec<Candidate> {
    let mut wm = cand("https://github.com/k4ditano/pleamar-wm", Some(23), None);
    wm.fork = Some(true);
    vec![
        cand("https://github.com/k4ditano/pleamar", Some(222), Some(30)),
        cand("https://github.com/ramiroAGI/pleamar", Some(0), Some(0)),
        wm,
    ]
}

fn strip(s: &str) -> String {
    let mut out = String::new();
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            for e in chars.by_ref() {
                if e == 'm' {
                    break;
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}

#[test]
fn the_plain_table_lists_one_candidate_per_row() {
    let got = table("pleamar", &pleamar(), 100, false);
    let want = "`pleamar` has no PyPI release and nothing in openSUSE; the search found these \
repositories but nothing confirms them:
  github.com/k4ditano/pleamar     222 stars  30 releases
  github.com/ramiroAGI/pleamar      0 stars  no releases
  github.com/k4ditano/pleamar-wm   23 stars               fork
Pick one and run: auto-package build https://github.com/k4ditano/pleamar";
    assert_eq!(got, want);
}

#[test]
fn colour_adds_only_escapes_and_paints_the_command_and_the_flags() {
    let plain = table("pleamar", &pleamar(), 100, false);
    let on = table("pleamar", &pleamar(), 100, true);
    assert_eq!(strip(&on), plain);
    assert!(on.contains("\x1b[33mfork\x1b[39m"));
    assert!(on.contains("\x1b[32mauto-package build https://github.com/k4ditano/pleamar\x1b[39m"));
    assert!(!plain.contains('\x1b'));
}

#[test]
fn an_archived_repository_is_flagged() {
    let mut c = cand("https://github.com/o/x", Some(1), Some(1));
    c.archived = Some(true);
    let t = table("x", &[c], 100, false);
    assert!(t.contains("  1 star  1 release  archived\n"), "{t}");
}

#[test]
fn unknown_facts_leave_the_row_short() {
    let c = Candidate::new("https://gitlab.com/o/x", Agent);
    let t = table("x", &[c], 100, false);
    assert!(t.contains("\n  gitlab.com/o/x\nPick one"), "{t}");
}

#[test]
fn a_narrow_width_moves_the_facts_under_the_url() {
    let t = table("pleamar", &pleamar(), 40, false);
    assert!(
        t.contains("  github.com/k4ditano/pleamar\n    222 stars  30 releases\n"),
        "{t}"
    );
    for l in t.lines().skip(1).filter(|l| !l.starts_with("Pick")) {
        assert!(unicode_width::UnicodeWidthStr::width(l) <= 40, "{l}");
    }
}

#[test]
fn a_cjk_repository_name_keeps_the_columns_aligned() {
    let list = vec![
        cand("https://github.com/o/日本語", Some(5), Some(2)),
        cand("https://github.com/o/ab", Some(10), Some(3)),
    ];
    let t = table("x", &list, 100, true);
    let plain = strip(&t);
    let col = |l: &str| {
        let i = l.find("stars").unwrap();
        unicode_width::UnicodeWidthStr::width(&l[..i])
    };
    let rows: Vec<&str> = plain.lines().filter(|l| l.contains("stars")).collect();
    assert_eq!(col(rows[0]), col(rows[1]), "{plain}");
}

#[test]
fn the_error_is_downcast_for_the_terminal_and_the_tui() {
    let e: color_eyre::Report = Unconfirmed {
        name: "pleamar".into(),
        candidates: pleamar(),
    }
    .into();
    assert_eq!(
        crate::name_clash::error_text(&e, false),
        table("pleamar", &pleamar(), 100, false)
    );
    let line = crate::name_clash::tui_failure("pleamar", &e);
    assert!(!line.contains('\n'));
    assert!(
        line.contains("https://github.com/k4ditano/pleamar, https://github.com/ramiroAGI/pleamar")
    );
    assert!(line.contains("`auto-package build https://github.com/k4ditano/pleamar`"));
    assert_eq!(e.to_string(), table("pleamar", &pleamar(), 100, false));
}

#[test]
fn the_message_for_a_name_with_no_repository_is_unchanged() {
    assert_eq!(
        no_repository_message("zzz"),
        "zzz has no usable PyPI release and no discoverable public source \
         repository. Pass --url, --source-url, or a repository URL as the \
         positional argument to point it at the source."
    );
}
