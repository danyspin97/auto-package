use super::*;

#[test]
fn escape_sequences_from_upstream_are_not_printed() {
    // An OSC title change and a clipboard write, as a build log might carry.
    let text = "ok\u{1b}]0;pwned\u{7}\n\tnext\u{1b}]52;c;ZGF0YQ==\u{1b}\\";
    let out = printable(text);
    assert!(!out.contains('\u{1b}') && !out.contains('\u{7}'), "{out:?}");
    assert_eq!(out, "ok\n\tnext");
}

#[test]
fn c1_controls_are_dropped_too() {
    // U+009B is the single-character CSI.
    assert_eq!(printable("a\u{9b}2Jb"), "a2Jb");
}

#[test]
fn plain_text_is_borrowed_unchanged() {
    assert!(matches!(
        printable("plain\ttext\n"),
        std::borrow::Cow::Borrowed(_)
    ));
}

#[test]
fn human_output_keeps_its_own_colours_only() {
    let msg = "\u{1b}[31mred\u{1b}[0m \u{1b}[2J\u{1b}]0;x\u{7}";
    let out = strip_controls(msg, true);
    assert_eq!(out, "\u{1b}[31mred\u{1b}[0m ");
    assert_eq!(printable("\u{1b}[2Kdone"), "done");
}
