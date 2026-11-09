//! Convention 75 says where a version bound goes when the dependency is a
//! `cmake()` or `pkgconfig()` provide: on the `-devel` package name.

use super::prompt::index_line;

#[test]
fn convention_75_puts_the_bound_on_the_devel_name_not_on_a_provide() {
    let text = crate::conventions_table::CONVENTIONS[74].1;
    assert!(text.starts_with("75. **Version bounds: the one rule.**"));
    let at = text
        .find("**A bound goes on the package name, not on a provide.**")
        .expect("convention 75 has the provide paragraph");
    let rest = &text[at..];
    let para = rest[..rest.find("\n   - ").unwrap_or(rest.len())]
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    for want in [
        "`cmake(X)` or `pkgconfig(X)` provide is unversioned",
        "`BuildRequires: aws-c-http-devel > 0.11.0`",
        "the version on that provide",
    ] {
        assert!(para.contains(want), "the provide paragraph lacks {want:?}");
    }
    assert!(
        index_line(75)
            .contains("a bound on a cmake()/pkgconfig() line (put it on the -devel name)"),
        "index line 75 lacks the provide trigger: {}",
        index_line(75)
    );
}

#[test]
fn convention_75_does_not_put_a_bound_on_a_pkgconfig_example() {
    let text = crate::conventions_table::CONVENTIONS[74].1;
    assert!(
        !text.contains("pkgconfig(foo) >="),
        "a pkgconfig bound remains"
    );
    assert!(text.contains("`BuildRequires: foo-devel >="));
}
