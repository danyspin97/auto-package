
use super::*;

#[test]
fn runtime_requires_become_flavor_neutral_and_build_requires_stay() {
    let spec = "BuildRequires:  %{python_module pip}\nRequires:       %{python_module click >= 8.1}\nRequires:       %{python_module python-dotenv}\nRecommends:     %{python_module torch}\nRequires:       %{python_module x if %python-base < 3.11}\n";
    let out = normalize_runtime_python_module(spec);
    assert!(out.contains("BuildRequires:  %{python_module pip}"));
    assert!(out.contains("Requires:       python-click >= 8.1"), "{out}");
    assert!(
        out.contains("Requires:       python-python-dotenv"),
        "{out}"
    );
    assert!(out.contains("Recommends:     python-torch"), "{out}");
    assert!(out.contains("%{python_module x if"), "{out}");
    assert_eq!(normalize_runtime_python_module(&out), out);
}

#[test]
fn dependency_tags_after_the_subpackages_macro_move_above_it() {
    let spec = "Name: x\n%python_subpackages\nRequires:  python-foo\nRecommends: python-bar\n\n%description\nText\n";
    let out = hoist_dependency_tags(spec);
    let sub = out.find("%python_subpackages").unwrap();
    assert!(out.find("Requires:  python-foo").unwrap() < sub, "{out}");
    assert!(out.find("Recommends: python-bar").unwrap() < sub, "{out}");
    assert!(out.contains("%description\nText"));
    assert_eq!(hoist_dependency_tags(&out), out);
}

#[test]
fn the_nvidia_licence_spelling_is_unified() {
    let out = normalize_nvidia_license("Name: x\nLicense:        LicenseRef-NvidiaProprietary\n");
    assert!(out.contains("License:        SUSE-NonFree"));
    let out = normalize_nvidia_license("License: NVIDIA Proprietary Software\n");
    assert!(out.contains(NVIDIA_PROPRIETARY));
    assert_eq!(
        normalize_nvidia_license("License: MIT\n"),
        "License:        MIT\n"
    );
}

#[test]
fn a_second_changelog_line_is_dropped() {
    assert_eq!(
        dedupe_changelog_header("a\n%changelog\n\n%changelog\n"),
        "a\n%changelog\n\n"
    );
}

#[test]
fn description_problems_catch_a_repeated_summary_and_an_emoji() {
    let spec = "Summary: A tool\n\n%description\nA tool\n\n%prep\n";
    assert_eq!(description_problems(spec).len(), 1);
    let spec = "Summary: A tool\n\n%description\nFast CLI \u{1F680}\n";
    assert_eq!(description_problems(spec).len(), 1);
    let spec = "Summary: A tool\n\n%description\nA tool that does things well.\n";
    assert!(description_problems(spec).is_empty());
}

#[test]
fn the_licence_entry_check_reads_files() {
    assert!(has_license_file_entry(
        "%files\n%license LICENSE\n%doc README\n"
    ));
    assert!(!has_license_file_entry("%files\n%doc README\n"));
    assert!(has_license_file_entry("Name: x\n"));
}
