use super::built_locally;

#[test]
fn only_a_successful_local_build_counts_as_available() {
    let dir = tempfile::tempdir().unwrap();
    let pkg = dir.path().join("python-cuda-bindings");
    std::fs::create_dir_all(&pkg).unwrap();
    std::fs::write(
        pkg.join("state.json"),
        r#"{"last_outcome":"success","last_built_version":"13.4.3"}"#,
    )
    .unwrap();
    assert_eq!(
        built_locally(dir.path(), "python-cuda-bindings").as_deref(),
        Some("13.4.3")
    );
    assert_eq!(built_locally(dir.path(), "python-missing"), None);
    // The guessed spelling must not matter: `_` and `-` are the same project.
    std::fs::create_dir_all(dir.path().join("python-typing_extensions")).unwrap();
    std::fs::write(
        dir.path()
            .join("python-typing_extensions")
            .join("state.json"),
        r#"{"last_outcome":"success","last_built_version":"4.15.0"}"#,
    )
    .unwrap();
    assert_eq!(
        built_locally(dir.path(), "python-typing-extensions").as_deref(),
        Some("4.15.0")
    );
    std::fs::write(
        pkg.join("state.json"),
        r#"{"last_outcome":"missing_deps","last_built_version":"13.4.3"}"#,
    )
    .unwrap();
    assert_eq!(built_locally(dir.path(), "python-cuda-bindings"), None);
}
