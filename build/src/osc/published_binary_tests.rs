use super::*;
use crate::osc::build_retry::*;
use crate::osc::publish::*;

#[test]
fn the_match_names_the_projects_and_the_arch() {
    let m = published_binary_match(
        &["openSUSE:Factory".to_string(), "devel:x".to_string()],
        "ffmpeg-8-devel",
        NameMatch::Exactly,
        "x86_64",
    );
    assert_eq!(
        m,
        "@name='ffmpeg-8-devel' and (@project='openSUSE:Factory' or @project='devel:x') and (@arch='x86_64' or @arch='noarch')"
    );
    assert!(
        published_binary_match(&["p".into()], "a'b", NameMatch::Containing, "x86_64")
            .contains("contains(@name,'ab')")
    );
}

/// python-torch: the Factory recipe's `_multibuild` stayed after the spec became a
/// wheel repackage that has no flavor of its own.
#[test]
fn a_multibuild_the_spec_does_not_use_is_not_published() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("_multibuild"), "<multibuild/>").unwrap();
    std::fs::write(
        dir.path().join("python-x.spec"),
        "Name: python-x\n%python_subpackages\n",
    )
    .unwrap();
    let plan = publish_plan(dir.path());
    assert!(
        plan.skipped.contains(&"_multibuild".to_string()),
        "{plan:?}"
    );
    assert!(plan.files.contains(&"python-x.spec".to_string()));
    std::fs::write(
        dir.path().join("python-x.spec"),
        "Name: python-x\n%if \"%{flavor}\" == \"openmpi4\"\n%endif\n",
    )
    .unwrap();
    assert!(
        publish_plan(dir.path())
            .files
            .contains(&"_multibuild".to_string())
    );
}

#[test]
fn unused_constraints_and_multibuild_are_deleted_from_a_source_recipe() {
    let dir = tempfile::tempdir().unwrap();
    for f in ["_constraints", "_multibuild"] {
        std::fs::write(dir.path().join(f), "<x/>").unwrap();
    }
    let spec = "Name: python-x\nBuildArch: noarch\nSource0: x-1.tar.gz\n%python_subpackages\n";
    std::fs::write(dir.path().join("python-x.spec"), spec).unwrap();
    assert!(
        publish_plan(dir.path())
            .skipped
            .contains(&"_constraints".to_string())
    );
    assert_eq!(
        drop_unused_recipe_files(dir.path()),
        ["_constraints", "_multibuild"]
    );
    assert!(!dir.path().join("_constraints").exists());
    // A native build keeps its constraints; a spec that names them does too.
    std::fs::write(dir.path().join("_constraints"), "<x/>").unwrap();
    std::fs::write(
        dir.path().join("python-x.spec"),
        "Name: python-x\nSource0: x.tar.gz\n",
    )
    .unwrap();
    assert!(drop_unused_recipe_files(dir.path()).is_empty());
    std::fs::write(
        dir.path().join("python-x.spec"),
        "Name: python-x\nBuildArch: noarch\nBuildRequires: memory-constraints\n",
    )
    .unwrap();
    assert!(drop_unused_recipe_files(dir.path()).is_empty());
}

#[test]
fn rpmlint_logs_are_stashed_per_package_and_taken_once() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("rpmlint.log"), "a: E: bad-x y\n").unwrap();
    stash_rpmlint_log(dir.path(), "python-a");
    assert!(!dir.path().join("rpmlint.log").exists());
    std::fs::write(dir.path().join("rpmlint.log"), "flavor: W: x\n").unwrap();
    stash_rpmlint_log(dir.path(), "python-a:openmpi");
    std::fs::write(dir.path().join("rpmlint.log"), "b: E: z\n").unwrap();
    stash_rpmlint_log(dir.path(), "python-ab");
    let taken = take_rpmlint_logs(dir.path(), "python-a");
    assert_eq!(taken.len(), 2, "{taken:?}");
    assert_eq!(taken[0].0, "python-a");
    assert!(take_rpmlint_logs(dir.path(), "python-a").is_empty());
    assert_eq!(rpmlint_errors(&taken[0].1), ["a: E: bad-x y"]);
    assert!(rpmlint_errors(&taken[1].1).is_empty());
    // A log with an invalid byte is still taken, and leaves the stash.
    std::fs::write(dir.path().join("rpmlint.log"), b"c.rpm: E: bad \xff\n").unwrap();
    stash_rpmlint_log(dir.path(), "python-c");
    let bad = take_rpmlint_logs(dir.path(), "python-c");
    assert_eq!(rpmlint_errors(&bad[0].1).len(), 1);
    assert!(take_rpmlint_logs(dir.path(), "python-c").is_empty());
}

#[test]
fn an_empty_rpmlintrc_goes_with_its_source_line() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("p.spec"),
        "Name: p\nSource0: p-1.tar.gz\nSource99: p.rpmlintrc\n",
    )
    .unwrap();
    std::fs::write(dir.path().join("p.rpmlintrc"), "").unwrap();
    assert_eq!(drop_empty_rpmlintrc(dir.path()), ["p.rpmlintrc"]);
    assert!(!dir.path().join("p.rpmlintrc").exists());
    let spec = std::fs::read_to_string(dir.path().join("p.spec")).unwrap();
    assert_eq!(spec, "Name: p\nSource0: p-1.tar.gz\n");
    // A rpmlintrc with content stays.
    std::fs::write(dir.path().join("q.rpmlintrc"), "addFilter(\"x\")\n").unwrap();
    assert!(drop_empty_rpmlintrc(dir.path()).is_empty());
    assert!(dir.path().join("q.rpmlintrc").exists());
}

#[test]
fn only_the_source_lines_of_removed_rpmlintrc_files_go() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("p.spec"),
        "Name: p\nSource0: p-1.tar.gz\nSource1: b.rpmlintrc\nSource2: a.rpmlintrc\n",
    )
    .unwrap();
    std::fs::write(dir.path().join("a.rpmlintrc"), "").unwrap();
    std::fs::write(dir.path().join("b.rpmlintrc"), "x".repeat(200)).unwrap();
    assert_eq!(drop_empty_rpmlintrc(dir.path()), ["a.rpmlintrc"]);
    let spec = std::fs::read_to_string(dir.path().join("p.spec")).unwrap();
    assert_eq!(spec, "Name: p\nSource0: p-1.tar.gz\nSource1: b.rpmlintrc\n");
    assert!(dir.path().join("b.rpmlintrc").exists());
}

#[test]
fn a_removed_rpmlintrc_source_is_matched_by_file_name() {
    let removed = ["a.rpmlintrc".to_string()];
    assert!(is_removed_rpmlintrc_source(
        "Source3: https://h/x/a.rpmlintrc\n",
        &removed
    ));
    assert!(is_removed_rpmlintrc_source(
        "source4: https://h/x.tar#/a.rpmlintrc\n",
        &removed
    ));
    assert!(!is_removed_rpmlintrc_source(
        "Source3: https://h/x/xa.rpmlintrc\n",
        &removed
    ));
    assert!(!is_removed_rpmlintrc_source(
        "SourceX: a.rpmlintrc\n",
        &removed
    ));
    assert!(!is_removed_rpmlintrc_source(
        "Patch1: a.rpmlintrc\n",
        &removed
    ));
}

#[test]
fn a_wheel_spec_drops_source_recipe_leftovers() {
    let dir = tempfile::tempdir().unwrap();
    let spec = "Name: python-x\nVersion: 2\nSource0: https://h/x-2-py3-none-any.whl#/x-2-py3-none-any.whl\n";
    std::fs::write(dir.path().join("python-x.spec"), spec).unwrap();
    for f in [
        "_service",
        "_constraints",
        "x-1.tar.gz",
        "x-2-py3-none-any.whl",
        "python-x.changes",
        "vendor.tar.xz",
        "x.obsinfo",
    ] {
        std::fs::write(dir.path().join(f), "x").unwrap();
    }
    let plan = publish_plan(dir.path());
    for f in [
        "_service",
        "_constraints",
        "x-1.tar.gz",
        "vendor.tar.xz",
        "x.obsinfo",
    ] {
        assert!(plan.skipped.contains(&f.to_string()), "{f}: {plan:?}");
    }
    for f in ["python-x.spec", "python-x.changes", "x-2-py3-none-any.whl"] {
        assert!(plan.files.contains(&f.to_string()), "{f}: {plan:?}");
    }
}

#[test]
fn a_source_recipe_keeps_its_service_and_named_archives() {
    let dir = tempfile::tempdir().unwrap();
    let spec = "Name: x\nVersion: 2\nSource0: x-%{version}.tar.gz\nSource1: vendor.tar.zst\n";
    std::fs::write(dir.path().join("x.spec"), spec).unwrap();
    for f in ["_service", "x-2.tar.gz", "x-1.tar.gz", "vendor.tar.zst"] {
        std::fs::write(dir.path().join(f), "x").unwrap();
    }
    let plan = publish_plan(dir.path());
    assert!(plan.files.contains(&"_service".to_string()), "{plan:?}");
    assert!(plan.files.contains(&"x-2.tar.gz".to_string()));
    assert!(plan.files.contains(&"vendor.tar.zst".to_string()));
    assert!(plan.skipped.contains(&"x-1.tar.gz".to_string()), "{plan:?}");
}

#[test]
fn binary_names_are_distinct_and_sorted() {
    let xml = r#"<collection matches="3">
  <binary name="b-devel" project="p" arch="x86_64"/>
  <binary name="a" project="p" arch="x86_64"/>
  <binary name="b-devel" project="q" arch="x86_64"/>
</collection>"#;
    assert_eq!(parse_published_binary_names(xml), ["a", "b-devel"]);
    assert!(parse_published_binary_names("<collection matches=\"0\">\n</collection>").is_empty());
}
