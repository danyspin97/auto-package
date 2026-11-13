//! The fill of a missing recap. The changelog is a made-up fixture shaped like
//! the python-ConfigArgParse one (origin 1.7.5, branch 1.9.0); the recap of
//! every case comes from the fixture, never from the network.

use super::changelog::section_between;
use super::fill::fill_with;
use super::per_release::{from_section, several};
use super::*;
use std::cell::Cell;
use std::path::Path;

const NEWS: &str = "\
## 1.9.0

- Add a new option
- Fix the parser on Python 3.13

## 1.8.0

- Support TOML files
- Fix the help text

## 1.7.5

- Older change
";

const SEP: &str = "-------------------------------------------------------------------";

const OLD_ENTRY: &str = "Mon Jan  1 00:00:00 UTC 2024 - A <a@b.c>\n\n- Update to 1.7.5\n";

fn spec(version: &str) -> String {
    format!("Name: python-ConfigArgParse\nVersion: {version}\nRelease: 0\nSummary: x\n")
}

fn origin_spec() -> String {
    spec("1.7.5")
}

/// The `.changes` of the branch: a new entry `body` above the published one.
fn changes(body: &str) -> String {
    format!("{SEP}\nTue Jan  2 00:00:00 UTC 2024 - A <a@b.c>\n\n{body}\n{SEP}\n{OLD_ENTRY}")
}

fn origin_changes() -> String {
    format!("{SEP}\n{OLD_ENTRY}")
}

fn no_model(_: &str) -> Option<String> {
    None
}

fn no_pypi(_: &str) -> Option<String> {
    None
}

fn lines(l: &[&str]) -> Vec<String> {
    l.iter().map(|s| s.to_string()).collect()
}

/// A recap that reads the fixture's sections between `old` and `new`, as the
/// changelog of the sources does (one group per release, newest first).
fn changelog_of(
    news: &'static str,
) -> impl Fn(Option<&str>, Option<&Path>, &str, &str, bool) -> Recap {
    move |_, _, old, new, _| {
        let found = section_between(news, old, new)
            .map(|s| from_section("NEWS", &s, old, new))
            .unwrap_or_default();
        let releases = several(new, old, found, Vec::new(), no_model).unwrap_or_default();
        Recap {
            releases,
            fetch_problem: None,
        }
    }
}

/// A checkout with `<pkg>.spec` at `version` and `<pkg>.changes` as `text`.
fn checkout(version: &str, text: &str) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("python-ConfigArgParse.spec"), spec(version)).unwrap();
    std::fs::write(dir.path().join("python-ConfigArgParse.changes"), text).unwrap();
    dir
}

fn read(dir: &tempfile::TempDir) -> String {
    std::fs::read_to_string(dir.path().join("python-ConfigArgParse.changes")).unwrap()
}

fn fill(
    dir: &tempfile::TempDir,
    recap: &dyn Fn(Option<&str>, Option<&Path>, &str, &str, bool) -> Recap,
    no_agent: bool,
) -> Result<Fill, String> {
    fill_with(
        dir.path(),
        "python-ConfigArgParse",
        &origin_changes(),
        &origin_spec(),
        no_agent,
        &no_pypi,
        recap,
    )
}

#[test]
fn a_bare_bullet_gets_one_bullet_per_release_newest_first() {
    let dir = checkout("1.9.0", &changes("- Update to 1.9.0"));
    let recap = changelog_of(NEWS);
    let got = fill(&dir, &recap, true).unwrap();
    assert_eq!(
        got,
        Fill::Written {
            old: "1.7.5".into(),
            new: "1.9.0".into()
        }
    );
    let out = read(&dir);
    let own = out.find("- Update to 1.9.0:\n  * Add a new option");
    let older = out.find("- Update to 1.8.0:\n  * Support TOML files");
    let (own, older) = (
        own.unwrap_or_else(|| panic!("{out}")),
        older.unwrap_or_else(|| panic!("{out}")),
    );
    assert!(own < older, "{out}");
    assert!(out.ends_with(&format!("{OLD_ENTRY}")), "{out}");
}

#[test]
fn a_single_release_gets_its_notes_in_todays_form() {
    let dir = checkout("1.9.0", &changes("- Update to 1.9.0\n- Fix the build"));
    let recap = |_: Option<&str>, _: Option<&Path>, _: &str, _: &str, _: bool| Recap {
        releases: vec![("1.9.0".to_string(), lines(&["* Add a new option"]))],
        fetch_problem: None,
    };
    assert!(matches!(fill(&dir, &recap, true), Ok(Fill::Written { .. })));
    let out = read(&dir);
    assert!(
        out.contains("- Update to 1.9.0\n  * Add a new option\n- Fix the build\n"),
        "{out}"
    );
    assert!(!out.contains("Update to 1.9.0:"), "{out}");
}

#[test]
fn an_entry_whose_bullet_has_notes_is_untouched() {
    let text = changes("- Update to 1.9.0\n  * Already here\n- Fix the build");
    let dir = checkout("1.9.0", &text);
    let never = |_: Option<&str>, _: Option<&Path>, _: &str, _: &str, _: bool| -> Recap {
        panic!("the recap is not asked for an entry with notes")
    };
    assert_eq!(fill(&dir, &never, false), Ok(Fill::Nothing));
    assert_eq!(read(&dir), text);
}

#[test]
fn a_flag_line_under_the_bullet_is_not_a_note() {
    let dir = checkout(
        "1.9.0",
        &changes("- Update to 1.9.0\n  FLAG: tests skipped: -k x"),
    );
    let recap = |_: Option<&str>, _: Option<&Path>, _: &str, _: &str, _: bool| Recap {
        releases: vec![("1.9.0".to_string(), lines(&["* a"]))],
        fetch_problem: None,
    };
    assert!(matches!(fill(&dir, &recap, true), Ok(Fill::Written { .. })));
    let out = read(&dir);
    assert!(
        out.contains("- Update to 1.9.0\n  * a\n  FLAG: tests skipped: -k x\n"),
        "{out}"
    );
}

#[test]
fn no_notes_found_leaves_the_entry_and_says_why() {
    let text = changes("- Update to 1.9.0");
    let dir = checkout("1.9.0", &text);
    let none = |_: Option<&str>, _: Option<&Path>, _: &str, _: &str, _: bool| Recap {
        releases: Vec::new(),
        fetch_problem: Some("could not list the releases of x".into()),
    };
    assert_eq!(
        fill(&dir, &none, true),
        Ok(Fill::NotFound {
            old: "1.7.5".into(),
            new: "1.9.0".into(),
            problem: Some("could not list the releases of x".into()),
        })
    );
    assert_eq!(read(&dir), text, "no placeholder is written");
}

#[test]
fn no_agent_is_passed_on_to_the_recap() {
    let dir = checkout("1.9.0", &changes("- Update to 1.9.0"));
    let seen = Cell::new(None);
    let recap = |_: Option<&str>, _: Option<&Path>, _: &str, _: &str, no_agent: bool| {
        seen.set(Some(no_agent));
        Recap::default()
    };
    let _ = fill(&dir, &recap, true);
    assert_eq!(seen.get(), Some(true));
}

#[test]
fn a_second_run_changes_nothing() {
    let dir = checkout("1.9.0", &changes("- Update to 1.9.0"));
    let recap = changelog_of(NEWS);
    assert!(matches!(fill(&dir, &recap, true), Ok(Fill::Written { .. })));
    let once = read(&dir);
    assert_eq!(fill(&dir, &recap, true), Ok(Fill::Nothing));
    assert_eq!(read(&dir), once);
}

#[test]
fn a_published_entry_is_never_touched() {
    let text = changes("- Update to 1.9.0");
    let dir = checkout("1.9.0", &text);
    // The origin publishes the same entry.
    let published = format!("{SEP}\n{}", text.split_once(&format!("{SEP}\n")).unwrap().1);
    let never = |_: Option<&str>, _: Option<&Path>, _: &str, _: &str, _: bool| -> Recap {
        panic!("a published entry is not filled")
    };
    let got = fill_with(
        dir.path(),
        "python-ConfigArgParse",
        &published,
        &origin_spec(),
        true,
        &no_pypi,
        &never,
    );
    assert_eq!(got, Ok(Fill::Nothing));
    assert_eq!(read(&dir), text);
}

#[test]
fn an_entry_for_another_version_is_not_filled() {
    let text = changes("- Update to 1.8.0");
    let dir = checkout("1.9.0", &text);
    let never = |_: Option<&str>, _: Option<&Path>, _: &str, _: &str, _: bool| -> Recap {
        panic!("only the spec's version is filled")
    };
    assert_eq!(fill(&dir, &never, true), Ok(Fill::Nothing));
    assert_eq!(read(&dir), text);
}

#[test]
fn a_spec_at_the_origins_version_is_not_filled() {
    let text = changes("- Update to 1.7.5");
    let dir = checkout("1.7.5", &text);
    let never = |_: Option<&str>, _: Option<&Path>, _: &str, _: &str, _: bool| -> Recap {
        panic!("no update, no recap")
    };
    assert_eq!(fill(&dir, &never, true), Ok(Fill::Nothing));
}

/// The head of the real `python-ExifRead.spec` (3.5.1): its `URL:` is a PyPI
/// page and its `Source:` a PyPI file, so no forge is named by the spec.
const EXIF_SPEC: &str = "Name:           python-ExifRead
Version:        3.5.1
Release:        0
Summary:        Read Exif metadata from tiff and jpeg files
URL:            https://pypi.python.org/pypi/ExifRead/%{version}
Source:         https://files.pythonhosted.org/packages/source/e/exifread/exifread-%{version}.tar.gz
";

/// The real `python-LTTL.spec` (2.1.0) names its repository in `URL:`, but its
/// `Source:` is a hashed PyPI path, which names no project.
const LTTL_SPEC: &str = "Name:           python-LTTL
Version:        2.1.0
Release:        0
Summary:        Tools for text analysis
URL:            https://github.com/axanthos/LTTL
Source:         https://files.pythonhosted.org/packages/8c/f1/1c6f3b1329ea4bd3df01769bd1f05ffb53bd5e3a3c6f161c69d64d4f1b17/lttl-%{version}.tar.gz
";

#[test]
fn the_pypi_project_of_a_spec_is_named_by_its_source_or_its_flavour() {
    use super::fill::pypi_name_of;
    assert_eq!(
        pypi_name_of(EXIF_SPEC, "python-ExifRead").as_deref(),
        Some("exifread")
    );
    assert_eq!(
        pypi_name_of(LTTL_SPEC, "python-LTTL").as_deref(),
        Some("LTTL")
    );
    assert_eq!(pypi_name_of(EXIF_SPEC, "eric"), Some("exifread".into()));
    assert_eq!(
        pypi_name_of(
            "Name: eric\nVersion: 1\nSource: https://x.org/eric.tgz\n",
            "eric"
        ),
        None
    );
}

#[test]
fn the_repository_of_a_pypi_spec_is_the_one_pypi_records() {
    // ExifRead 3.5.1 in its real shape: the spec names no forge, PyPI records
    // the exif-py repository, and the notes are read from it.
    let dir = checkout("1.9.0", &changes("- Update to 1.9.0"));
    std::fs::write(
        dir.path().join("python-ConfigArgParse.spec"),
        EXIF_SPEC
            .replace("ExifRead", "ConfigArgParse")
            .replace("3.5.1", "1.9.0"),
    )
    .unwrap();
    let asked = std::cell::RefCell::new(Vec::new());
    // The project URLs PyPI serves for exifread (its `Repository` key).
    let pypi_json = r#"{"info": {"name": "ExifRead", "home_page": null, "project_urls": {
        "Changelog": "https://github.com/ianare/exif-py/blob/master/ChangeLog.rst",
        "Repository": "https://github.com/ianare/exif-py"}}}"#;
    let pypi = |name: &str| {
        asked.borrow_mut().push(name.to_string());
        ap_ecosystem::pypi::source_url_from_json(pypi_json)
    };
    let seen = std::cell::RefCell::new(None);
    let recap = |repo: Option<&str>, _: Option<&Path>, _: &str, _: &str, _: bool| {
        *seen.borrow_mut() = repo.map(str::to_string);
        Recap::default()
    };
    let got = fill_with(
        dir.path(),
        "python-ConfigArgParse",
        &origin_changes(),
        &origin_spec(),
        true,
        &pypi,
        &recap,
    );
    assert!(matches!(got, Ok(Fill::NotFound { .. })), "{got:?}");
    assert_eq!(asked.borrow().as_slice(), ["exifread"]);
    assert_eq!(
        seen.borrow().as_deref(),
        Some("https://github.com/ianare/exif-py")
    );
}

#[test]
fn a_forge_named_by_the_spec_is_used_without_asking_pypi() {
    let dir = checkout("1.9.0", &changes("- Update to 1.9.0"));
    std::fs::write(
        dir.path().join("python-ConfigArgParse.spec"),
        LTTL_SPEC
            .replace("python-LTTL", "python-ConfigArgParse")
            .replace("2.1.0", "1.9.0"),
    )
    .unwrap();
    let never = |_: &str| -> Option<String> { panic!("the spec names the repository") };
    let seen = std::cell::RefCell::new(None);
    let recap = |repo: Option<&str>, _: Option<&Path>, _: &str, _: &str, _: bool| {
        *seen.borrow_mut() = repo.map(str::to_string);
        Recap::default()
    };
    let _ = fill_with(
        dir.path(),
        "python-ConfigArgParse",
        &origin_changes(),
        &origin_spec(),
        true,
        &never,
        &recap,
    );
    assert_eq!(
        seen.borrow().as_deref(),
        Some("https://github.com/axanthos/LTTL")
    );
}
