use super::expand_once;
use std::collections::HashMap;

#[test]
fn longer_macro_names_win_over_their_prefixes() {
    let macros: HashMap<String, String> = [("rel", "1"), ("release", "2")]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
    assert_eq!(expand_once("%release", &macros), "2");
    assert_eq!(expand_once("%rel", &macros), "1");
}

fn one(name: &str, value: &str) -> HashMap<String, String> {
    [(name.to_string(), value.to_string())].into()
}

#[test]
fn only_whole_identifiers_are_replaced() {
    let m = one("pkg", "foo");
    assert_eq!(expand_once("%pkgdir", &m), "%pkgdir");
    assert_eq!(expand_once("%pkg-1", &m), "foo-1");
    assert_eq!(expand_once("%{pkg}dir", &m), "foodir");
    assert_eq!(expand_once("%pkg_x %pkg", &m), "%pkg_x foo");
}

#[test]
fn a_source_with_an_undefined_macro_is_not_a_real_url() {
    let spec = "Name: foo\nVersion: 1\n%define pkg foo\n\
                    Source0: https://x.org/%{name}-%version_tag.tar.gz\n\
                    Source1: https://x.org/%pkgdir/a.tar.gz\n\
                    Source2: https://x.org/%pkg/%{pkg}-%{version}.tar.gz\n";
    let got = super::expanded_sources(spec);
    assert_eq!(got.len(), 1, "{got:?}");
    assert_eq!(got[0].url, "https://x.org/foo/foo-1.tar.gz");
}

#[test]
fn unknown_and_special_macros_are_left_alone() {
    let m = one("name", "foo");
    for keep in [
        "%%name",
        "%{?name}",
        "%{!?name}",
        "%(echo %name)",
        "%{x y}",
        "100%",
    ] {
        assert_eq!(expand_once(keep, &m), keep, "{keep}");
    }
    assert_eq!(expand_once("%{name}-%version_tag", &m), "foo-%version_tag");
}
