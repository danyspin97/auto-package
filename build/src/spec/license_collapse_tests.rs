use super::collapse_dist_info_license_lines;

#[test]
fn many_license_files_become_the_directory() {
    let mut spec = String::from("%files %{python_files}\n%{python_sitearch}/torch/\n");
    for i in 0..5 {
        spec.push_str(&format!(
                "%license %{{python_sitearch}}/torch-%{{version}}.dist-info/licenses/third_party/l{i}/LICENSE\n"
            ));
    }
    spec.push_str("%{python_sitearch}/torch-%{version}.dist-info/\n\n%changelog\n");
    let out = collapse_dist_info_license_lines(&spec);
    let d = "%{python_sitearch}/torch-%{version}.dist-info/licenses";
    assert_eq!(
        out,
        format!(
            "%files %{{python_files}}\n%{{python_sitearch}}/torch/\n%exclude {d}\n%license {d}\n\
                 %{{python_sitearch}}/torch-%{{version}}.dist-info/\n\n%changelog\n"
        )
    );
    assert_eq!(collapse_dist_info_license_lines(&out), out);
    let two = "%license a.dist-info/licenses/x\n%license a.dist-info/licenses/y\n";
    assert_eq!(collapse_dist_info_license_lines(two), two);
}
