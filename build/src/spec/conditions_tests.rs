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

#[test]
fn else_and_endif_with_blanks_or_a_comment_still_close_the_frame() {
    let m = macros(&[]);
    assert_eq!(
        taken_after("%if 1\n%else  \nb\n%endif # x\nc", &m),
        [Some(true), Some(false), Some(false), Some(true), Some(true)]
    );
    assert_eq!(
        taken_after("%if 0\n%else # y\nb\n%endif\t", &m),
        [Some(false), Some(true), Some(true), Some(true)]
    );
}

#[test]
fn elifarch_and_elifos_open_an_unknown_branch() {
    let m = macros(&[]);
    assert_eq!(
        taken_after("%if 0\n%elifarch x86_64\na\n%else\nb\n%endif", &m),
        [Some(false), None, None, None, None, Some(true)]
    );
    assert_eq!(
        taken_after("%if 0\n%elifos linux\n%endif", &m),
        [Some(false), None, Some(true)]
    );
}

#[test]
fn a_define_after_an_endif_with_a_comment_is_read() {
    let spec = "%if 0\n%global a 1\n%endif # a\n%global b 2\n";
    let defines = crate::spec::collect_simple_defines(spec);
    assert_eq!(defines.get("a"), None);
    assert_eq!(defines.get("b").map(String::as_str), Some("2"));
}

fn guards_after(text: &str, m: &HashMap<String, String>) -> Vec<String> {
    let mut stack = CondStack::new();
    for l in text.lines() {
        stack.feed(l, m);
    }
    stack.guards()
}

#[test]
fn guards_name_the_frames_not_known_to_be_taken() {
    let m = macros(&[]);
    assert_eq!(guards_after("%if 0", &m), ["%if 0 [then]"]);
    assert!(guards_after("%if 1", &m).is_empty());
    assert_eq!(guards_after("%if %{with x}", &m), ["%if %{with x} [then]"]);
    assert_eq!(
        guards_after("%if %{with x}\n%else", &m),
        ["%if %{with x} [else]"]
    );
    assert_eq!(
        guards_after("%if %{with x}\n%elif 0%{?y}", &m),
        ["%if %{with x} [elif 1: %elif 0%{?y}] {y=undefined}"]
    );
    assert_eq!(
        guards_after("%if 0\n%elif 0%{?y}\n%else", &m),
        ["%if 0 [else after %elif 0%{?y}] {y=undefined}"]
    );
    assert_eq!(
        guards_after("%ifarch x86_64", &m),
        ["%ifarch x86_64 [then]"]
    );
}

#[test]
fn guards_list_nested_frames_outermost_first_and_skip_taken_ones() {
    let m = macros(&[]);
    assert_eq!(
        guards_after("%if %{with x}\n%if 1\n%if 0", &m),
        ["%if %{with x} [then]", "%if 0 [then]"]
    );
}

#[test]
fn whitespace_in_a_guard_is_collapsed() {
    let m = macros(&[]);
    assert_eq!(
        guards_after("  %if  %{with   x}", &m),
        guards_after("%if %{with x}", &m)
    );
}

#[test]
fn a_guard_carries_the_values_of_the_macros_it_names() {
    let one = macros(&[("use_patches", "1"), ("pyver", "310")]);
    let zero = macros(&[("use_patches", "0"), ("pyver", "311")]);
    for cond in [
        "%if %{use_patches}",
        "%if 0%{use_patches}",
        "%if %{pyver} > 310",
    ] {
        assert_ne!(
            guards_after(cond, &one),
            guards_after(cond, &zero),
            "{cond}"
        );
    }
    assert_eq!(
        guards_after("%if %{use_patches}", &one),
        ["%if %{use_patches} [then] {use_patches=1}"]
    );
    // A macro the spec does not define is named as such, and an unrelated one is not.
    let other = macros(&[("use_patches", "1"), ("pyver", "999"), ("unrelated", "x")]);
    assert_eq!(
        guards_after("%if %{use_patches}", &one),
        guards_after("%if %{use_patches}", &other)
    );
    assert_eq!(
        guards_after("%if 0%{?suse_version} > 1500", &macros(&[])),
        ["%if 0%{?suse_version} > 1500 [then] {suse_version=undefined}"]
    );
}
