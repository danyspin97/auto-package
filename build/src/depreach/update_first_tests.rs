use super::*;
use crate::spec::build_requirements;

fn need(spec: &str, names: &[&str], origin: &str, new: &str) -> DepNeed {
    need_with(spec, names, origin, &[], new)
}

/// `need` with what the origin's providers give for the capabilities.
fn need_with(
    spec: &str,
    names: &[&str],
    origin: &str,
    provided: &[(&str, &str)],
    new: &str,
) -> DepNeed {
    let names: Vec<String> = names.iter().map(|n| n.to_string()).collect();
    let provided: Vec<(String, String)> = provided
        .iter()
        .map(|(n, v)| (n.to_string(), v.to_string()))
        .collect();
    dependency_need(&build_requirements(spec), &names, origin, &provided, new)
}

#[test]
fn a_bound_above_the_origin_that_the_update_meets_needs_the_update() {
    assert_eq!(
        need(
            "BuildRequires:  libfoo-devel >= 2.1\n",
            &["libfoo-devel"],
            "2.0",
            "2.3"
        ),
        DepNeed::Update("libfoo-devel >= 2.1".into())
    );
    assert_eq!(
        need(
            "BuildRequires: libfoo-devel > 2.0\n",
            &["libfoo-devel"],
            "2.0",
            "2.0.1"
        ),
        DepNeed::Update("libfoo-devel > 2.0".into())
    );
}

#[test]
fn a_bound_the_origin_meets_needs_nothing() {
    assert_eq!(
        need(
            "BuildRequires: libfoo-devel >= 1.4\n",
            &["libfoo-devel"],
            "2.0",
            "2.3"
        ),
        DepNeed::Origin
    );
}

#[test]
fn an_unversioned_requirement_needs_nothing() {
    assert_eq!(
        need(
            "BuildRequires: libfoo-devel\n",
            &["libfoo-devel"],
            "1.0",
            "9.0"
        ),
        DepNeed::Origin
    );
}

#[test]
fn an_exact_pin_on_the_new_version_needs_the_update() {
    assert_eq!(
        need(
            "BuildRequires: libfoo-devel = 2.3\n",
            &["libfoo-devel"],
            "2.0",
            "2.3"
        ),
        DepNeed::Update("libfoo-devel = 2.3".into())
    );
    // A pin with a release is compared by its version part.
    assert_eq!(
        need(
            "BuildRequires: libfoo-devel = 2.3-1\n",
            &["libfoo-devel"],
            "2.0",
            "2.3"
        ),
        DepNeed::Update("libfoo-devel = 2.3-1".into())
    );
}

#[test]
fn a_bound_both_versions_meet_needs_nothing() {
    assert_eq!(
        need(
            "BuildRequires: libfoo-devel >= 1.0\nBuildRequires: libfoo-devel < 3\n",
            &["libfoo-devel"],
            "2.0",
            "2.3"
        ),
        DepNeed::Origin
    );
}

#[test]
fn versions_are_ordered_as_versions_not_as_strings() {
    // As strings "1.10" < "1.9"; as versions 1.10 is newer.
    assert_eq!(
        need(
            "BuildRequires: libfoo-devel >= 1.9\n",
            &["libfoo-devel"],
            "1.10",
            "1.11"
        ),
        DepNeed::Origin
    );
    assert_eq!(
        need(
            "BuildRequires: libfoo-devel >= 1.10\n",
            &["libfoo-devel"],
            "1.9",
            "1.10"
        ),
        DepNeed::Update("libfoo-devel >= 1.10".into())
    );
}

#[test]
fn a_bound_neither_version_meets_is_unmet() {
    assert_eq!(
        need(
            "BuildRequires: libfoo-devel >= 5\n",
            &["libfoo-devel"],
            "2.0",
            "2.3"
        ),
        DepNeed::Unmet("libfoo-devel >= 5".into())
    );
}

#[test]
fn a_bound_that_cannot_be_compared_is_unknown() {
    assert_eq!(
        need(
            "BuildRequires: libfoo-devel >= %{foo_min}\n",
            &["libfoo-devel"],
            "2.0",
            "2.3"
        ),
        DepNeed::Unknown("libfoo-devel >= %{foo_min}".into())
    );
}

#[test]
fn only_requirements_on_the_dependency_count() {
    assert_eq!(
        need(
            "BuildRequires: libbar-devel >= 9\n",
            &["libfoo-devel"],
            "2.0",
            "2.3"
        ),
        DepNeed::Origin
    );
}

#[test]
fn a_capability_is_matched_by_its_text_with_the_bound() {
    let spec = "BuildRequires: pkgconfig(foo) >= 2.1\n";
    assert_eq!(
        need(spec, &["pkgconfig(foo) >= 2.1"], "2.0", "2.3"),
        DepNeed::Update("pkgconfig(foo) >= 2.1".into())
    );
}

#[test]
fn the_update_decides_before_a_bound_nothing_meets() {
    let spec = "BuildRequires: libfoo-devel >= 9\nBuildRequires: libfoo-devel >= 2.1\n";
    assert_eq!(
        need(spec, &["libfoo-devel"], "2.0", "2.3"),
        DepNeed::Update("libfoo-devel >= 2.1".into())
    );
}

#[test]
fn a_capability_is_compared_with_the_provide_of_the_origin_not_the_package() {
    // gtk4-x11's source is 4.12.1, but the RPM in the origin provides 4.8.0.
    let spec = "BuildRequires: pkgconfig(gtk4-x11) >= 4.10\n";
    let names = ["pkgconfig(gtk4-x11) >= 4.10"];
    assert_eq!(
        need_with(
            spec,
            &names,
            "4.12.1",
            &[("pkgconfig(gtk4-x11) >= 4.10", "4.8.0")],
            "4.14.0"
        ),
        DepNeed::Update("pkgconfig(gtk4-x11) >= 4.10".into())
    );
    // Without the provide read, the package's version stands in for it.
    assert_eq!(need(spec, &names, "4.12.1", "4.14.0"), DepNeed::Origin);
}

#[test]
fn a_provide_the_origin_meets_needs_nothing_even_if_the_package_is_older() {
    // The source is 1.0, but the origin's provider gives 2.5, which meets >= 2.1.
    assert_eq!(
        need_with(
            "BuildRequires: pkgconfig(foo) >= 2.1\n",
            &["pkgconfig(foo) >= 2.1"],
            "1.0",
            &[("pkgconfig(foo) >= 2.1", "2.5")],
            "3.0"
        ),
        DepNeed::Origin
    );
}

#[test]
fn a_cmake_capability_is_compared_with_its_provide() {
    assert_eq!(
        need_with(
            "BuildRequires: cmake(Qt6Core) >= 6.5\n",
            &["cmake(Qt6Core) >= 6.5"],
            "6.7.0",
            &[("cmake(Qt6Core) >= 6.5", "6.2.4")],
            "6.8.0"
        ),
        DepNeed::Update("cmake(Qt6Core) >= 6.5".into())
    );
}

#[test]
fn a_provide_with_a_release_is_compared_without_it() {
    assert_eq!(
        need_with(
            "BuildRequires: libfoo.so.3 >= 2.1\n",
            &["libfoo.so.3 >= 2.1"],
            "2.0",
            &[("libfoo.so.3 >= 2.1", "2.0-3")],
            "2.3"
        ),
        DepNeed::Update("libfoo.so.3 >= 2.1".into())
    );
}

#[test]
fn the_update_side_is_the_new_package_version() {
    // The provide of the new release is not known before it is built, so the
    // new package version decides whether the update meets the bound.
    assert_eq!(
        need_with(
            "BuildRequires: pkgconfig(foo) >= 2.1\n",
            &["pkgconfig(foo) >= 2.1"],
            "2.0",
            &[("pkgconfig(foo) >= 2.1", "2.0")],
            "2.0.1"
        ),
        DepNeed::Unmet("pkgconfig(foo) >= 2.1".into())
    );
}

// RPM facts, checked with the python rpm bindings (RPM 4.20.1) on a provide
// `foo = 1:1.2.3-4`: it does not meet `foo = 1.2.3`, it meets `foo >= 2.0`,
// and it does not meet `foo < 2.0`. A bound without an epoch reads it as 0.

#[test]
fn an_equal_bound_without_an_epoch_is_not_met_by_a_provide_with_one() {
    // The origin's 1:1.2.3 misses = 1.2.3 (epoch 1 against 0); the new 1.2.3
    // meets it, so the dependency is updated first.
    assert_eq!(
        need_with(
            "BuildRequires: pkgconfig(foo) = 1.2.3\n",
            &["pkgconfig(foo) = 1.2.3"],
            "1.2.3",
            &[("pkgconfig(foo) = 1.2.3", "1:1.2.3-4")],
            "1.2.3"
        ),
        DepNeed::Update("pkgconfig(foo) = 1.2.3".into())
    );
}

#[test]
fn a_lower_bound_without_an_epoch_is_met_by_a_provide_with_a_higher_epoch() {
    // 1:1.2.3 is above 0:2.0 whatever its version, so the origin meets >= 2.0.
    assert_eq!(
        need_with(
            "BuildRequires: pkgconfig(foo) >= 2.0\n",
            &["pkgconfig(foo) >= 2.0"],
            "1.2.3",
            &[("pkgconfig(foo) >= 2.0", "1:1.2.3-4")],
            "2.3"
        ),
        DepNeed::Origin
    );
}

#[test]
fn an_upper_bound_without_an_epoch_is_not_met_by_a_provide_with_a_higher_epoch() {
    // 1:1.2.3 is above 0:2.0, so the origin misses < 2.0; the new 1.5 meets it.
    assert_eq!(
        need_with(
            "BuildRequires: pkgconfig(foo) < 2.0\n",
            &["pkgconfig(foo) < 2.0"],
            "1.2.3",
            &[("pkgconfig(foo) < 2.0", "1:1.2.3-4")],
            "1.5"
        ),
        DepNeed::Update("pkgconfig(foo) < 2.0".into())
    );
}

#[test]
fn a_bound_with_an_epoch_compares_the_epoch_of_the_provide() {
    assert_eq!(
        need_with(
            "BuildRequires: pkgconfig(foo) >= 1:2.0\n",
            &["pkgconfig(foo) >= 1:2.0"],
            "2.0",
            &[("pkgconfig(foo) >= 1:2.0", "1:2.0")],
            "2.3"
        ),
        DepNeed::Origin
    );
    // A provide without an epoch counts as epoch 0, so 2.0 misses 1:2.0, and
    // the new version (2.3, no epoch either) misses it too.
    assert_eq!(
        need_with(
            "BuildRequires: pkgconfig(foo) >= 1:2.0\n",
            &["pkgconfig(foo) >= 1:2.0"],
            "2.0",
            &[("pkgconfig(foo) >= 1:2.0", "2.0")],
            "2.3"
        ),
        DepNeed::Unmet("pkgconfig(foo) >= 1:2.0".into())
    );
}

/// What `pkgconfig(foo) <bound>` needs when the origin's provide gives
/// `provide` and the update brings `new`.
fn foo_need(bound: &str, provide: &str, new: &str) -> DepNeed {
    let text = format!("pkgconfig(foo) {bound}");
    need_with(
        &format!("BuildRequires: {text}\n"),
        &[text.as_str()],
        "1.0",
        &[(text.as_str(), provide)],
        new,
    )
}

#[test]
fn a_bound_release_is_compared_with_the_release_of_a_provide_that_has_one() {
    // `= 1.2.3-4`: an equal release is met by the origin; a greater or a less
    // one is not, and the update (1.2.3, no release) meets it.
    assert_eq!(foo_need("= 1.2.3-4", "1.2.3-4", "1.2.3"), DepNeed::Origin);
    assert_eq!(
        foo_need("= 1.2.3-4", "1.2.3-5", "1.2.3"),
        DepNeed::Update("pkgconfig(foo) = 1.2.3-4".into())
    );
    assert_eq!(
        foo_need("= 1.2.3-4", "1.2.3-3", "1.2.3"),
        DepNeed::Update("pkgconfig(foo) = 1.2.3-4".into())
    );
    // `>= 1.2.3-4`: a greater release meets it, a less one does not.
    assert_eq!(foo_need(">= 1.2.3-4", "1.2.3-5", "1.3"), DepNeed::Origin);
    assert_eq!(
        foo_need(">= 1.2.3-4", "1.2.3-3", "1.3"),
        DepNeed::Update("pkgconfig(foo) >= 1.2.3-4".into())
    );
    // The update's version has a release too: it is compared with it.
    assert_eq!(
        foo_need("= 1.2.3-4", "1.2.3-3", "1.2.3-4"),
        DepNeed::Update("pkgconfig(foo) = 1.2.3-4".into())
    );
}

#[test]
fn a_bound_without_a_release_ignores_the_release_of_a_provide() {
    assert_eq!(foo_need("= 1.2.3", "1.2.3-5", "1.3"), DepNeed::Origin);
    assert_eq!(foo_need("= 1.2.3", "1.2.3-3", "1.3"), DepNeed::Origin);
    assert_eq!(foo_need(">= 1.2.3", "1.2.3-1", "1.3"), DepNeed::Origin);
}

#[test]
fn a_bound_release_is_not_compared_with_a_provide_without_one() {
    // The provide's `1.2.3` meets `= 1.2.3-4` as RPM does: no release to compare.
    assert_eq!(foo_need("= 1.2.3-4", "1.2.3", "1.3"), DepNeed::Origin);
}

#[test]
fn an_origin_epoch_counts_against_a_bound_without_one() {
    // Epoch 1 with version 1.0 is above `>= 2.0`, which has epoch 0.
    let origin = with_epoch("1.0", Some("1"));
    let new = with_epoch("2.0", Some("1"));
    assert_eq!(
        need(
            "BuildRequires: libfoo-devel >= 2.0\n",
            &["libfoo-devel"],
            &origin,
            &new
        ),
        DepNeed::Origin
    );
}

#[test]
fn a_bound_with_an_epoch_the_origin_misses_is_not_met() {
    let origin = with_epoch("1.0", Some("1"));
    let new = with_epoch("1.5", Some("1"));
    assert_eq!(
        need(
            "BuildRequires: libfoo-devel >= 1:2.0\n",
            &["libfoo-devel"],
            &origin,
            &new
        ),
        DepNeed::Unmet("libfoo-devel >= 1:2.0".into())
    );
}

#[test]
fn the_new_version_keeps_the_origin_epoch() {
    // Without the epoch on the new version, 2.0 would be below `< 3.0` and
    // the update would be taken as meeting it; with epoch 1 it is 1:2.0.
    let origin = with_epoch("1.0", Some("1"));
    let new = with_epoch("2.0", Some("1"));
    assert_eq!(
        need(
            "BuildRequires: libfoo-devel < 3.0\n",
            &["libfoo-devel"],
            &origin,
            &new
        ),
        DepNeed::Unmet("libfoo-devel < 3.0".into())
    );
}

#[test]
fn without_an_origin_epoch_the_versions_compare_as_before() {
    assert_eq!(
        need(
            "BuildRequires: libfoo-devel >= 2.0\n",
            &["libfoo-devel"],
            "1.0",
            "2.0"
        ),
        DepNeed::Update("libfoo-devel >= 2.0".into())
    );
    assert_eq!(with_epoch("1.0", None), "1.0");
}
