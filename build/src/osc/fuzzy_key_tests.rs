use super::*;

#[test]
fn treats_hyphen_underscore_and_dot_as_equivalent() {
    assert_eq!(
        fuzzy_key("python-pydantic-core"),
        fuzzy_key("python-pydantic_core")
    );
    assert_eq!(
        fuzzy_key("python-ruamel.yaml"),
        fuzzy_key("python-ruamel-yaml")
    );
    assert_eq!(
        fuzzy_key("Python-Backports.Zstd"),
        fuzzy_key("python-backports_zstd")
    );
}

#[test]
fn distinct_names_still_produce_distinct_keys() {
    assert_ne!(
        fuzzy_key("python-pydantic-core"),
        fuzzy_key("python-pydantic")
    );
}
