use super::extra_drop::*;

#[test]
fn a_raw_detail_line_with_an_extra_names_the_package_and_the_extra() {
    assert_eq!(
        requested_extra("nothing provides python313-setuptools_scm[toml]"),
        Some(("python313-setuptools_scm".into(), "toml".into()))
    );
    assert_eq!(
        requested_extra("nothing provides python314-pkg[a,b] >= 1.0"),
        Some(("python314-pkg".into(), "a,b".into()))
    );
    assert_eq!(
        requested_extra("unresolvable: nothing provides python313-x[toml] < 2"),
        Some(("python313-x".into(), "toml".into()))
    );
    for plain in [
        "nothing provides python313-setuptools_scm",
        "nothing provides libcuda.so.1()(64bit)",
        "nothing provides (python313-x[toml] or y)",
        "nothing provides x[]",
        "nothing provides x[t oml]",
    ] {
        assert_eq!(requested_extra(plain), None, "{plain}");
    }
}

#[test]
fn an_extra_of_a_published_package_is_a_drop_not_a_build() {
    let detail = vec![
        "nothing provides python313-setuptools_scm[toml]".to_string(),
        "nothing provides python313-missing[toml]".to_string(),
    ];
    let drops = find_extra_drops(&detail, &|name| name == "python313-setuptools_scm");
    assert_eq!(
        drops,
        [("python313-setuptools_scm".to_string(), "toml".to_string())]
    );
    let note = extra_drop_note(&drops).expect("a note");
    assert!(note.contains("Drop `[toml]`"), "{note}");
    assert!(extra_drop_note(&[]).is_none());
}
