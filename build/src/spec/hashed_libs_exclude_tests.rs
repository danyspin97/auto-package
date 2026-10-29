
use super::*;

#[test]
fn the_exclude_is_added_before_the_subpackages_once() {
    let spec = "Name: python-x\n%global __provides_exclude_from ^x$\n%python_subpackages\n\n%description\nd\n";
    let out = with_hashed_libs_requires_exclude(spec);
    assert!(out.contains(&format!(
        "%global __requires_exclude {}\n%python_subpackages",
        hashed_libs_requires_exclude()
    )));
    assert_eq!(with_hashed_libs_requires_exclude(&out), out);
}

#[test]
fn an_existing_exclude_is_folded_into_one_regex() {
    let spec = "%global __requires_exclude ^lib(mpi|ucp)\\.so.*$\n%python_subpackages\n";
    let out = with_hashed_libs_requires_exclude(spec);
    assert_eq!(out.matches("__requires_exclude").count(), 1, "{out}");
    let want = format!(
        r"^(lib(mpi|ucp)\.so.*|{})$",
        hashed_libs_requires_exclude()
            .trim_start_matches('^')
            .trim_end_matches('$')
    );
    assert!(out.contains(&want), "{out}");
}
