use super::*;

fn urls(pairs: &[(&str, &str)]) -> crate::sources::SourceMap {
    let mut m = crate::sources::SourceMap::default();
    for (p, u) in pairs {
        m.insert(p, u);
    }
    m
}

#[test]
fn a_source_recipe_is_pending_when_a_wheel_is_asked_for() {
    let dir = tempfile::tempdir().unwrap();
    let spec = dir.path().join("python-torch.spec");
    std::fs::write(
        &spec,
        "Name: python-torch\nSource: torch-%{version}.tar.gz\n",
    )
    .unwrap();
    let wheel = urls(&[("torch", "wheel")]);
    assert!(wheel_policy_pending(&wheel, "torch", &spec));
    assert!(wheel_policy_pending(&wheel, "Torch", &spec));
    // Another package, an ordinary URL, or no entry: nothing pending.
    assert!(!wheel_policy_pending(&wheel, "numpy", &spec));
    assert!(!wheel_policy_pending(
        &urls(&[("torch", "https://github.com/pytorch/pytorch")]),
        "torch",
        &spec
    ));
    // The wheel spec is done.
    std::fs::write(
        &spec,
        "Name: python-torch\nSource0:        torch-2.14.1-cp313.whl\n",
    )
    .unwrap();
    assert!(!wheel_policy_pending(&wheel, "torch", &spec));
    assert!(!wheel_policy_pending(
        &wheel,
        "torch",
        &dir.path().join("absent.spec")
    ));
}

#[test]
fn switching_to_a_wheel_clears_the_source_recipe_and_keeps_the_wheel_files() {
    let dir = tempfile::tempdir().unwrap();
    for f in [
        "_service",
        "_multibuild",
        "_constraints",
        "a.patch",
        "pytorch-v2.14.0.tar.gz",
        "python-torch-rpmlintrc",
        "python-torch.spec",
        "python-torch.changes",
        "python-torch.rpmlintrc",
        "torch-2.14.1-cp313.whl",
        "_link",
    ] {
        std::fs::write(dir.path().join(f), "x").unwrap();
    }
    std::fs::create_dir(dir.path().join("pytorch")).unwrap();
    purge_source_recipe(dir.path());
    let mut left: Vec<String> = std::fs::read_dir(dir.path())
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    left.sort();
    assert_eq!(
        left,
        [
            "_link",
            "python-torch.changes",
            "python-torch.rpmlintrc",
            "python-torch.spec",
            "pytorch",
            "torch-2.14.1-cp313.whl"
        ]
    );
}
