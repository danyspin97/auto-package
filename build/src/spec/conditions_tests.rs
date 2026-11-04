use super::*;
use std::collections::HashMap;

fn macros(pairs: &[(&str, &str)]) -> HashMap<String, String> {
    pairs
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

/// `taken()` after feeding each line of `text`.
fn taken_after(text: &str, m: &HashMap<String, String>) -> Vec<Option<bool>> {
    let mut stack = CondStack::new();
    text.lines()
        .map(|l| {
            stack.feed(l, m);
            stack.taken()
        })
        .collect()
}

#[test]
fn integer_conditions_are_decided() {
    let m = macros(&[]);
    assert_eq!(
        taken_after("%if 0\nx\n%endif\n%if 1\ny\n%endif", &m),
        [
            Some(false),
            Some(false),
            Some(true),
            Some(true),
            Some(true),
            Some(true)
        ]
    );
}

#[test]
fn a_defined_macro_decides_and_an_undefined_one_does_not() {
    let defined = macros(&[("x", "1")]);
    let off = macros(&[("x", "0")]);
    let none = macros(&[]);
    assert_eq!(taken_after("%if 0%{?x}", &defined), [Some(true)]);
    assert_eq!(taken_after("%if 0%{?x}", &off), [Some(false)]);
    assert_eq!(taken_after("%if 0%{?x}", &none), [None]);
}

#[test]
fn an_unreadable_condition_is_unknown() {
    let m = macros(&[]);
    assert_eq!(
        taken_after("%if %{with x}\na\n%else\nb\n%endif", &m),
        [None, None, None, None, Some(true)]
    );
    assert_eq!(
        taken_after("%ifarch x86_64\n%endif", &m),
        [None, Some(true)]
    );
}

#[test]
fn else_flips_a_known_frame() {
    let m = macros(&[]);
    assert_eq!(
        taken_after("%if 1\na\n%else\nb\n%endif", &m),
        [Some(true), Some(true), Some(false), Some(false), Some(true)]
    );
    assert_eq!(
        taken_after("%if 0\na\n%else\nb\n%endif", &m),
        [Some(false), Some(false), Some(true), Some(true), Some(true)]
    );
}

#[test]
fn elif_follows_the_chain() {
    let m = macros(&[]);
    assert_eq!(
        taken_after("%if 0\n%elif 1\na\n%else\nb\n%endif", &m),
        [
            Some(false),
            Some(true),
            Some(true),
            Some(false),
            Some(false),
            Some(true)
        ]
    );
    assert_eq!(
        taken_after("%if 1\n%elif 1\na\n%endif", &m),
        [Some(true), Some(false), Some(false), Some(true)]
    );
    assert_eq!(
        taken_after("%if %{with x}\n%elif 1\n%else\n%endif", &m),
        [None, None, None, Some(true)]
    );
}

#[test]
fn nested_frames_need_every_frame() {
    let m = macros(&[]);
    assert_eq!(
        taken_after("%if 1\n%if 0\na\n%endif\nb\n%endif", &m),
        [
            Some(true),
            Some(false),
            Some(false),
            Some(true),
            Some(true),
            Some(true)
        ]
    );
    assert_eq!(
        taken_after("%if %{with x}\n%if 0\na\n%endif\n%endif", &m),
        [None, Some(false), Some(false), None, Some(true)]
    );
}

#[test]
fn a_define_under_an_unknown_frame_is_not_recorded() {
    let spec = "%global a 1\n%if %{with x}\n%global b 2\n%endif\n%if 1\n%global c 3\n%endif\n";
    let defines = crate::spec::collect_simple_defines(spec);
    assert_eq!(defines.get("a").map(String::as_str), Some("1"));
    assert_eq!(defines.get("b"), None);
    assert_eq!(defines.get("c").map(String::as_str), Some("3"));
}
