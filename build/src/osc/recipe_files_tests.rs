use super::*;

/// The `_multibuild` of elfutils: its flavor is a sibling package, not a `%flavor`.
const ELFUTILS_MULTIBUILD: &str =
    "<multibuild>\n  <package>elfutils-debuginfod</package>\n</multibuild>\n";
const FLAVORS: &str =
    "<multibuild>\n  <flavor>standard</flavor>\n  <flavor>openmpi4</flavor>\n</multibuild>\n";

#[test]
fn a_multibuild_naming_a_package_is_kept() {
    let spec = "Name: elfutils\nVersion: 0.197\n%description\nx\n";
    assert!(!recipe_file_unused(
        "_multibuild",
        spec,
        ELFUTILS_MULTIBUILD
    ));
}

#[test]
fn a_flavor_only_multibuild_is_unused_unless_the_spec_reads_flavor() {
    let plain = "Name: torch\n%description\nx\n";
    assert!(recipe_file_unused("_multibuild", plain, FLAVORS));
    let flavored = "Name: torch\n%if %{flavor} == openmpi4\n%endif\n";
    assert!(!recipe_file_unused("_multibuild", flavored, FLAVORS));
}

#[test]
fn a_multibuild_is_kept_in_a_checkout_when_it_names_a_package() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path();
    std::fs::write(
        p.join("elfutils.spec"),
        "Name: elfutils\nVersion: 0.197\nSource: x.tar.bz2\n",
    )
    .unwrap();
    std::fs::write(p.join("_multibuild"), ELFUTILS_MULTIBUILD).unwrap();
    assert!(publish_plan(p).files.iter().any(|f| f == "_multibuild"));
    assert!(drop_unused_recipe_files(p).is_empty());
    assert!(p.join("_multibuild").exists());
}

#[test]
fn a_flavor_only_multibuild_is_dropped_from_a_checkout() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path();
    std::fs::write(p.join("torch.spec"), "Name: torch\nVersion: 1\n").unwrap();
    std::fs::write(p.join("_multibuild"), FLAVORS).unwrap();
    assert_eq!(drop_unused_recipe_files(p), vec!["_multibuild".to_string()]);
    assert!(!p.join("_multibuild").exists());
}
