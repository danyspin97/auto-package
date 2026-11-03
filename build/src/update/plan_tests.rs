//! Series and pins of a package, on the shapes of server:database: `mariadb-55`
//! (an old-series copy of `mariadb`), `influxdb` beside `influxdb2`, and a plain
//! package that takes its next major.

use super::*;
use std::collections::HashSet;

/// Lookups that answer `latest` for every kind of source, and no release of a
/// series of a forge repository unless `in_series` names one.
fn answers_in(latest: &'static str, in_series: Option<&'static str>) -> Lookups<'static> {
    let f: &'static dyn Fn(&str) -> Result<String, String> =
        Box::leak(Box::new(move |_: &str| -> Result<String, String> {
            Ok(latest.to_string())
        }));
    let no_service: &'static dyn Fn(&str) -> Option<String> = Box::leak(Box::new(|_: &str| None));
    let no_has_service: &'static dyn Fn(&str) -> bool = Box::leak(Box::new(|_: &str| false));
    let no_date: &'static dyn Fn(&str) -> Result<(String, String), String> =
        Box::leak(Box::new(|_: &str| Err("no release date".to_string())));
    let s: &'static dyn Fn(&str, &[u64]) -> Result<String, String> = Box::leak(Box::new(
        move |_: &str, _: &[u64]| -> Result<String, String> {
            in_series
                .map(str::to_string)
                .ok_or_else(|| "no tag list".to_string())
        },
    ));
    Lookups {
        fetch_pypi_latest: f,
        fetch_repo_latest: f,
        fetch_index_latest: f,
        fetch_repology_latest: f,
        fetch_gem_latest: f,
        fetch_cpan_latest: f,
        fetch_service: no_service,
        fetch_has_service: no_has_service,
        fetch_release_date: no_date,
        fetch_repo_in_series: s,
    }
}

fn answers(latest: &'static str) -> Lookups<'static> {
    answers_in(latest, None)
}

/// The spec of each package named in `table`; any other name has no spec.
fn specs(
    table: &'static [(&'static str, &'static str)],
) -> impl Fn(&str) -> Result<String, ReadError> {
    move |name: &str| {
        table
            .iter()
            .find(|(n, _)| *n == name)
            .map(|(_, text)| text.to_string())
            .ok_or(ReadError::NoSpec)
    }
}

/// The spec headers of server:database as `osc cat` shows them: `mariadb-55`
/// (Name mariadb, 5.5.46) and `mariadb-102` (Name mariadb, 10.2.3, in `mysql.spec`).
const MARIADB_55: &str = "Name:           mariadb\nVersion:        5.5.46\n\
    Source:         https://downloads.mariadb.org/f/mariadb-%{version}/source/mariadb-%{version}.tar.gz\n";
const MARIADB_102: &str = "Name:           mariadb\nVersion:        10.2.3\n\
    Source:         https://downloads.mariadb.org/f/mariadb-%{version}/source/mariadb-%{version}.tar.gz\n";

fn classify_one(
    rpm_name: &str,
    table: &'static [(&'static str, &'static str)],
    project: &[&str],
    latest: &'static str,
) -> Decision {
    let project: HashSet<String> = project.iter().map(|p| p.to_string()).collect();
    classify_in_project(rpm_name, &specs(table), &answers(latest), &project).decision
}

#[test]
fn series_of_reads_the_dashed_and_the_dotted_forms() {
    assert_eq!(series_of("fmt-9", "9.1.0"), Some(vec![9]));
    assert_eq!(series_of("boost-1.85", "1.85.0"), Some(vec![1, 85]));
    assert_eq!(series_of("wlroots-0_15", "0.15.2"), Some(vec![0, 15]));
    assert_eq!(series_of("pipewire0_2", "0.2.7"), Some(vec![0, 2]));
    // `-55` and `-100` are a major and a minor with no dot between them.
    assert_eq!(series_of("mariadb-55", "5.5.46"), Some(vec![5, 5]));
    assert_eq!(series_of("mariadb-100", "10.0.30"), Some(vec![10, 0]));
    assert_eq!(series_of("mariadb-102", "10.2.3"), Some(vec![10, 2]));
    assert_eq!(
        series_of("mysql-community-server-57", "5.7.7"),
        Some(vec![5, 7])
    );
    // A number that does not start the version is part of the name, and a
    // soname without a dash or underscore pins nothing.
    assert_eq!(series_of("foo-2", "1.4"), None);
    assert_eq!(series_of("libfoo2", "2.0"), None);
    assert_eq!(series_of("mariadb-55", "10.0.1"), None);
    assert_eq!(series_of("mariadb", "10.2.3"), None);
}

#[test]
fn mariadb_55_stays_on_5_5() {
    let table = &[("mariadb-55", MARIADB_55)];
    assert!(matches!(
        classify_one("mariadb-55", table, &["mariadb-55"], "13.0.2"),
        Decision::Skipped(SkipReason::OtherSeries { ref series, .. }) if series == "5.5"
    ));
    assert!(matches!(
        classify_one("mariadb-55", table, &["mariadb-55"], "5.5.62"),
        Decision::Update { ref new_version, .. } if new_version == "5.5.62"
    ));
}

#[test]
fn mariadb_102_stays_on_10_2() {
    let table = &[("mariadb-102", MARIADB_102)];
    assert!(matches!(
        classify_one("mariadb-102", table, &["mariadb-102"], "13.0.2"),
        Decision::Skipped(SkipReason::OtherSeries { ref series, .. }) if series == "10.2"
    ));
    assert!(matches!(
        classify_one("mariadb-102", table, &["mariadb-102"], "10.2.40"),
        Decision::Update { .. }
    ));
}

#[test]
fn a_package_is_pinned_by_a_sibling_on_a_newer_series() {
    let table = &[
        (
            "influxdb",
            "Name:           influxdb\nVersion:        1.11.8\nUrl:            https://github.com/influxdata/influxdb\n",
        ),
        (
            "influxdb2",
            "Name:           influxdb2\nVersion:        2.7.10\nUrl:            https://github.com/influxdata/influxdb\n",
        ),
    ];
    let project = ["influxdb", "influxdb2"];
    assert!(matches!(
        classify_one("influxdb", table, &project, "3.12.1"),
        Decision::Skipped(SkipReason::OtherSeries { ref series, .. }) if series == "1"
    ));
    assert!(matches!(
        classify_one("influxdb", table, &project, "1.12.0"),
        Decision::Update { .. }
    ));
    // The newer one is not pinned by the older: its next release is taken.
    assert!(matches!(
        classify_one("influxdb2", table, &project, "2.8.0"),
        Decision::Update { .. }
    ));
}

#[test]
fn only_a_series_suffix_makes_a_spec_name_difference_a_series() {
    assert!(is_series_copy_of("mariadb-55", "mariadb"));
    assert!(is_series_copy_of("mariadb-100", "mariadb"));
    assert!(is_series_copy_of("wlroots-0_16", "wlroots"));
    assert!(is_series_copy_of(
        "mysql-community-server-57",
        "mysql-community-server"
    ));
    // Any other difference from the spec's Name pins nothing.
    assert!(!is_series_copy_of("python-foo", "foo"));
    assert!(!is_series_copy_of("python3-foo", "foo"));
    assert!(!is_series_copy_of("foo-kit", "foo"));
    assert!(!is_series_copy_of("foo-devel", "foo"));
    assert!(!is_series_copy_of("foo", "foo"));
    assert!(!is_series_copy_of("mariadb-55", "maria"));
}

#[test]
fn python_foo_with_spec_name_foo_takes_its_next_major() {
    let table = &[("python-foo", "Name:           foo\nVersion:        1.4.0\n")];
    assert!(matches!(
        classify_one("python-foo", table, &["python-foo"], "2.0.0"),
        Decision::Update { ref new_version, .. } if new_version == "2.0.0"
    ));
}

#[test]
fn a_renamed_package_is_not_pinned_to_its_spec_name_series() {
    let table = &[(
        "widget-kit",
        "Name:           widget\nVersion:        1.4.0\n",
    )];
    assert!(matches!(
        classify_one("widget-kit", table, &["widget-kit"], "2.0.0"),
        Decision::Update { ref new_version, .. } if new_version == "2.0.0"
    ));
}

#[test]
fn a_plain_package_still_takes_one_major_jump() {
    let table = &[("widget", "Name:           widget\nVersion:        1.4.0\n")];
    assert!(matches!(
        classify_one("widget", table, &["widget"], "2.0.0"),
        Decision::Update { ref new_version, .. } if new_version == "2.0.0"
    ));
}

#[test]
fn a_plain_package_stops_on_a_jump_of_two_majors() {
    let table = &[(
        "couchdb",
        "Name:           couchdb\nVersion:        1.6.1\n",
    )];
    match classify_one("couchdb", table, &["couchdb"], "3.5.3") {
        Decision::Skipped(SkipReason::NeedsMaintainer(why)) => {
            assert!(why.contains("a jump of 2 majors"), "{why}");
        }
        other => panic!("expected a stop for the major jump, got {other:?}"),
    }
    assert!(matches!(
        classify_one("couchdb", table, &["couchdb"], "1.6.2"),
        Decision::Update { .. }
    ));
}

#[test]
fn a_pinned_package_stops_where_a_sibling_is_on_the_series() {
    // `foo2` is on 1.9 here, a newer series than `foo`'s 1.4: `foo` is pinned to
    // 1.x, and it does not take 1.9 either, which `foo2` already carries.
    let table = &[
        (
            "foo",
            "Name:           foo\nVersion:        1.4.0\nUrl:            https://github.com/example/foo\n",
        ),
        (
            "foo2",
            "Name:           foo2\nVersion:        1.9.0\nUrl:            https://github.com/example/foo\n",
        ),
    ];
    let project = ["foo", "foo2"];
    match classify_one("foo", table, &project, "1.9.3") {
        Decision::Skipped(SkipReason::NeedsMaintainer(why)) => {
            assert!(why.contains("foo2"), "{why}");
        }
        other => panic!("expected a stop at the sibling's series, got {other:?}"),
    }
    assert!(matches!(
        classify_one("foo", table, &project, "1.4.1"),
        Decision::Update { .. }
    ));
    assert!(matches!(
        classify_one("foo", table, &project, "2.0.0"),
        Decision::Skipped(SkipReason::OtherSeries { .. })
    ));
}

/// `fmt-9` packaging 9.1.0, from a GitHub archive: upstream's newest is 11.0.2.
const FMT_9: &str = "Name:           fmt-9\nVersion:        9.1.0\n\
    Source:         https://github.com/fmtlib/fmt/archive/%{version}.tar.gz\n";

#[test]
fn a_pinned_series_takes_the_newest_release_of_its_series() {
    let table = &[("fmt-9", FMT_9)];
    let project = ["fmt-9"];
    let lookups = answers_in("11.0.2", Some("9.1.2"));
    let c = classify_in_project(
        "fmt-9",
        &specs(table),
        &lookups,
        &project.map(String::from).into(),
    );
    assert!(matches!(
        c.decision,
        Decision::Update { ref new_version, .. } if new_version == "9.1.2"
    ));
    let lookups = answers_in("11.0.2", Some("9.1.0"));
    let c = classify_in_project(
        "fmt-9",
        &specs(table),
        &lookups,
        &project.map(String::from).into(),
    );
    assert!(matches!(c.decision, Decision::UpToDate { .. }));
}

#[test]
fn a_pinned_series_on_a_source_without_a_tag_list_is_left() {
    let table = &[(
        "fmt-9",
        "Name:           fmt-9\nVersion:        9.1.0\n\
         Source:         https://downloads.example.org/fmt-%{version}.tar.gz\n",
    )];
    let lookups = answers_in("11.0.2", Some("9.1.2"));
    let c = classify_in_project("fmt-9", &specs(table), &lookups, &HashSet::new());
    assert!(matches!(
        c.decision,
        Decision::Skipped(SkipReason::OtherSeries { ref series, .. }) if series == "9"
    ));
}

/// A spec of `name` at `version`, whose source is the GitHub repository `repo`.
fn forged(name: &str, version: &str, repo: &str) -> String {
    format!(
        "Name:           {name}\nVersion:        {version}\n\
         Source:         https://github.com/{repo}/archive/v{version}.tar.gz\n"
    )
}

/// The decision for `rpm_name`, whose spec is `spec`, in a project of `project`.
fn decide(rpm_name: &str, spec: String, project: &[&str], lookups: &Lookups<'_>) -> Decision {
    let project: HashSet<String> = project.iter().map(|p| p.to_string()).collect();
    let fetch = move |name: &str| {
        if name == rpm_name {
            Ok(spec.clone())
        } else {
            Err(ReadError::NoSpec)
        }
    };
    classify_in_project(rpm_name, &fetch, lookups, &project).decision
}

#[test]
fn influxdb2_stays_on_2_x_and_takes_the_newest_2_x_release() {
    let spec = forged("influxdb2", "2.7.10", "influxdata/influxdb");
    let lookups = answers_in("3.12.1", Some("2.7.12"));
    assert!(matches!(
        decide("influxdb2", spec.clone(), &["influxdb2"], &lookups),
        Decision::Update { ref new_version, .. } if new_version == "2.7.12"
    ));
    let lookups = answers_in("3.12.1", None);
    assert!(matches!(
        decide("influxdb2", spec, &["influxdb2"], &lookups),
        Decision::Skipped(SkipReason::OtherSeries { ref series, .. }) if series == "2"
    ));
}

#[test]
fn a_name_with_a_major_and_minor_stays_on_that_minor_series() {
    let spec = forged("python311", "3.11.4", "python/cpython");
    let lookups = answers_in("3.12.0", Some("3.11.7"));
    assert!(matches!(
        decide("python311", spec.clone(), &["python311"], &lookups),
        Decision::Update { ref new_version, .. } if new_version == "3.11.7"
    ));
    let lookups = answers_in("3.12.0", Some("3.11.4"));
    assert!(matches!(
        decide("python311", spec, &["python311"], &lookups),
        Decision::UpToDate { .. }
    ));
    let spec = forged("libpng16", "1.6.40", "glennrp/libpng");
    let lookups = answers_in("1.7.0", Some("1.6.43"));
    assert!(matches!(
        decide("libpng16", spec, &["libpng16"], &lookups),
        Decision::Update { ref new_version, .. } if new_version == "1.6.43"
    ));
}

#[test]
fn a_name_whose_trailing_digits_match_no_series_is_not_pinned() {
    let spec = forged("bzip2", "1.0.8", "libarchive/bzip2");
    let lookups = answers("1.1.0");
    assert!(matches!(
        decide("bzip2", spec, &["bzip2"], &lookups),
        Decision::Update { ref new_version, .. } if new_version == "1.1.0"
    ));
    let spec = forged("lz4", "1.10.0", "lz4/lz4");
    assert!(matches!(
        decide("lz4", spec, &["lz4"], &answers("1.11.0")),
        Decision::Update { ref new_version, .. } if new_version == "1.11.0"
    ));
    let spec = forged("mpg123", "1.32.9", "mpg123/mpg123");
    assert!(matches!(
        decide("mpg123", spec, &["mpg123"], &answers("1.33.0")),
        Decision::Update { ref new_version, .. } if new_version == "1.33.0"
    ));
}

#[test]
fn gtk3_and_nodejs22_stay_on_their_major() {
    let spec = forged("gtk3", "3.24.41", "GNOME/gtk");
    assert!(matches!(
        decide("gtk3", spec, &["gtk3"], &answers_in("4.16.0", None)),
        Decision::Skipped(SkipReason::OtherSeries { ref series, .. }) if series == "3"
    ));
    let spec = forged("nodejs22", "22.11.0", "nodejs/node");
    assert!(matches!(
        decide("nodejs22", spec, &["nodejs22"], &answers_in("24.0.0", None)),
        Decision::Skipped(SkipReason::OtherSeries { ref series, .. }) if series == "22"
    ));
}

#[test]
fn a_sibling_is_found_by_its_upstream_whatever_its_name() {
    // zookeeper-kit is on the same release as zookeeper, from the same forge
    // repository, so zookeeper is not held back: 3.4.13 -> 3.9.7 is proposed.
    let table = &[
        (
            "zookeeper",
            "Name:           zookeeper\nVersion:        3.4.13\nUrl:            https://github.com/apache/zookeeper\n",
        ),
        (
            "zookeeper-kit",
            "Name:           zookeeper-kit\nVersion:        3.4.13\nUrl:            https://github.com/apache/zookeeper\n",
        ),
    ];
    assert!(matches!(
        classify_one("zookeeper", table, &["zookeeper", "zookeeper-kit"], "3.9.7"),
        Decision::Update { ref new_version, .. } if new_version == "3.9.7"
    ));
}

#[test]
fn a_sibling_with_another_upstream_does_not_pin_by_its_name() {
    let table = &[
        (
            "foo",
            "Name:           foo\nVersion:        1.4.0\nUrl:            https://github.com/example/foo\n",
        ),
        (
            "foo2",
            "Name:           foo2\nVersion:        1.9.0\nUrl:            https://github.com/example/other\n",
        ),
    ];
    assert!(matches!(
        classify_one("foo", table, &["foo", "foo2"], "2.0.0"),
        Decision::Update { ref new_version, .. } if new_version == "2.0.0"
    ));
}

#[test]
fn a_series_pin_takes_its_newest_without_a_repology_check() {
    // Repology's newest is on another series: the pinned package takes the
    // release of its own series, and is not stopped for disagreeing.
    let spec = forged("python311", "3.11.5", "python/cpython");
    let mut lookups = answers_in("3.11.9", Some("3.11.9"));
    let repology: &'static dyn Fn(&str) -> Result<String, String> =
        Box::leak(Box::new(|_: &str| Ok("3.13.0".to_string())));
    lookups.fetch_repology_latest = repology;
    assert!(matches!(
        decide("python311", spec, &["python311"], &lookups),
        Decision::Update { ref new_version, .. } if new_version == "3.11.9"
    ));
}
