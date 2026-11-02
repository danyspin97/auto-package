//! The checks a git dependency's tree and refs pass before anything is checked
//! out: the listing, the attributes, the ref count, and the link targets.

use std::path::Path;

use super::fetch_git;
use super::unpack::{LIMITS, Limits};

/// One record of `git ls-tree -r -l -z`: a blob (or a link) of `size` bytes.
fn record(mode: &str, path: &str, size: u64) -> String {
    let id = "1".repeat(40);
    format!("{mode} blob {id} {size:>7}\t{path}\0")
}

#[test]
fn a_tree_listing_is_checked_before_the_checkout() {
    let ok = record("100644", "ok.txt", 3);
    assert_eq!(
        fetch_git::check_listing(&ok, LIMITS).unwrap(),
        Vec::<String>::new()
    );
    assert!(fetch_git::check_listing(&record("120000", "ok-link", 3), LIMITS).is_ok());
    let refused = |text: &str, limits: Limits| fetch_git::check_listing(text, limits).unwrap_err();
    let sub = format!("160000 commit {} {:>7}\tsub\0", "2".repeat(40), "-");
    assert!(refused(&sub, LIMITS).contains("not a file"));
    assert!(refused(&record("100644", ".git/config", 3), LIMITS).contains(".git"));
    let case = format!(
        "{}{}",
        record("100644", "A.txt", 3),
        record("100644", "a.txt", 3)
    );
    assert!(refused(&case, LIMITS).contains("differs only in case"));
    let two = format!("{}{}", record("100644", "a", 3), record("100644", "b", 3));
    let bytes = Limits {
        max_bytes: 5,
        ..LIMITS
    };
    assert!(refused(&two, bytes).contains("more than 5 bytes"));
    let entries = Limits {
        max_entries: 1,
        ..LIMITS
    };
    assert!(refused(&two, entries).contains("more than 1 entries"));
    let depth = Limits {
        max_depth: 1,
        ..LIMITS
    };
    assert!(refused(&record("100644", "a/b/c.txt", 3), depth).contains("deeper than 1 levels"));
    let attrs = format!("{}{ok}", record("100644", "sub/.gitattributes", 5));
    assert_eq!(
        fetch_git::check_listing(&attrs, LIMITS).unwrap(),
        vec!["1".repeat(40)]
    );
}

#[test]
fn an_attribute_that_changes_the_bytes_written_is_refused() {
    assert!(
        fetch_git::check_attributes("*.bin binary\n# ident in a comment\n* text=auto\n").is_ok()
    );
    for text in [
        "* ident\n",
        "*.c eol=crlf\n",
        "*.txt working-tree-encoding=UTF-16\n",
        "*.c -ident\n",
    ] {
        assert!(fetch_git::check_attributes(text).is_err(), "{text}");
    }
}

#[test]
fn the_advertised_refs_of_a_fallback_are_counted_first() {
    assert!(fetch_git::check_ref_count("a\nb\n", 2).is_ok());
    let err = fetch_git::check_ref_count("a\nb\nc\n", 2).unwrap_err();
    assert!(err.contains("3 refs"), "{err}");
}

#[test]
fn a_link_target_must_stay_inside_the_tree() {
    use super::unpack::link_stays_inside as inside;
    let p = Path::new;
    assert!(!inside(p("link"), p("/etc")));
    assert!(!inside(p("link"), p("")));
    assert!(!inside(p("link"), p("../../x")));
    assert!(!inside(p("d/link"), p("../../x")));
    assert!(!inside(p("link"), p("a/../../b")));
    assert!(inside(p("link"), p("./ok")));
    assert!(inside(p("sub/link"), p("ok")));
    assert!(inside(p("deep/er/link"), p("../ok-inside-root")));
}
