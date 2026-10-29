
use super::*;

/// `/build/home:dspinella:open-webui/_result` as OBS returned it for the
/// three dependencies of the vllm run, trimmed to their statuses: every
/// one is fine on x86_64 and broken or excluded on i586.
const TWO_ARCHES: &str = r#"<resultlist state="a89fd62d90378c6fa375e52c78300d1c">
  <result project="home:dspinella:open-webui" repository="openSUSE_Tumbleweed" arch="i586" code="published" state="published">
    <status package="python-fastapi-cloud-cli" code="unresolvable">
      <details>nothing provides python314-fastar, nothing provides python313-fastar</details>
    </status>
    <status package="python-nvidia-cutlass-dsl-libs-base" code="excluded"/>
    <status package="python-cuda-bindings" code="succeeded"/>
  </result>
  <result project="home:dspinella:open-webui" repository="openSUSE_Tumbleweed" arch="x86_64" code="building" state="building">
    <status package="python-fastapi-cloud-cli" code="succeeded"/>
    <status package="python-nvidia-cutlass-dsl-libs-base" code="succeeded"/>
    <status package="python-cuda-bindings" code="succeeded"/>
  </result>
</resultlist>"#;

#[test]
fn a_package_excluded_on_one_arch_is_not_excluded_on_the_one_being_built() {
    let x86 = project_statuses_from_xml(TWO_ARCHES, "openSUSE_Tumbleweed", "x86_64").unwrap();
    assert_eq!(x86.len(), 3);
    assert!(x86.values().all(|s| *s == BuildStatus::Succeeded));
    let i586 = project_statuses_from_xml(TWO_ARCHES, "openSUSE_Tumbleweed", "i586").unwrap();
    assert_eq!(
        i586.get("python-nvidia-cutlass-dsl-libs-base"),
        Some(&BuildStatus::Excluded)
    );
    assert_eq!(
        i586.get("python-fastapi-cloud-cli"),
        Some(&BuildStatus::Unresolvable)
    );
    assert_eq!(
        i586.get("python-cuda-bindings"),
        Some(&BuildStatus::Succeeded)
    );
}

#[test]
fn a_repository_or_arch_the_project_does_not_build_for_yields_nothing() {
    for (repo, arch) in [
        ("openSUSE_Tumbleweed", "aarch64"),
        ("openSUSE_Leap_16.0", "x86_64"),
    ] {
        assert!(
            project_statuses_from_xml(TWO_ARCHES, repo, arch)
                .unwrap()
                .is_empty()
        );
    }
}

#[test]
fn a_cut_off_listing_is_an_error_not_a_partial_map() {
    let cut = &TWO_ARCHES[..TWO_ARCHES.find("arch=\"x86_64\"").unwrap() + 20];
    assert!(project_statuses_from_xml(cut, "openSUSE_Tumbleweed", "i586").is_err());
}
