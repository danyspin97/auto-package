use super::adopt_sources_from_tree;

/// python-open-webui: Source10 is the sdist's own package-lock.json.
#[test]
fn a_source_the_unpacked_tree_holds_is_copied_into_the_checkout() {
    let tmp = tempfile::tempdir().unwrap();
    let tree = tmp.path().join("sdist");
    let top = tree.join("open_webui-0.12.2");
    std::fs::create_dir_all(top.join("deep")).unwrap();
    std::fs::write(top.join("package-lock.json"), "{}").unwrap();
    std::fs::write(top.join("deep/other.json"), "{}").unwrap();
    let checkout = tmp.path().join("co");
    std::fs::create_dir_all(&checkout).unwrap();
    std::fs::write(checkout.join("keep.txt"), "mine").unwrap();
    std::fs::write(top.join("keep.txt"), "theirs").unwrap();
    let missing: Vec<String> = ["package-lock.json", "other.json", "keep.txt", "a/b", "nope"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    let copied = adopt_sources_from_tree(&missing, &tree, &checkout);
    assert_eq!(copied, ["package-lock.json"]);
    assert_eq!(
        std::fs::read_to_string(checkout.join("package-lock.json")).unwrap(),
        "{}"
    );
    assert_eq!(
        std::fs::read_to_string(checkout.join("keep.txt")).unwrap(),
        "mine"
    );
    assert!(!checkout.join("other.json").exists());
}
