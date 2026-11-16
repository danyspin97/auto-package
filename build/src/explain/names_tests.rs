use super::*;

fn write(path: &Path, text: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

const RUN: &str = "20261027-175210-aaaa-1-0";

/// A work directory whose state directories are `state` (one `state.json`
/// each, with that `obs_name`) and whose one run built `events_pkg`, the name
/// its events carry.
fn project(state: &[&str], events_pkg: &str) -> tempfile::TempDir {
    let t = tempfile::tempdir().unwrap();
    let d = t.path();
    let run = [
        r#"{"event":"run_started","run_id":"R","argv":["auto-package","update"],"work_dir":"/w"}"#
            .to_string(),
        format!(
            r#"{{"pkg":"{events_pkg}","event":"step_started","step":1,"kind":"Build","attempt":1,"round":0,"detail":null}}"#
        ),
        format!(
            r#"{{"pkg":"{events_pkg}","event":"step_finished","step":1,"outcome":"Ok","elapsed":1000}}"#
        ),
        format!(
            r#"{{"pkg":"{events_pkg}","event":"package_finished","version":"0.7","outcome":"Built"}}"#
        ),
    ];
    write(
        &d.join("runs").join(RUN).join("run.jsonl"),
        &(run.join("\n") + "\n"),
    );
    for dir in state {
        write(
            &d.join("state").join(dir).join("state.json"),
            &format!(
                r#"{{"last_outcome":"success","obs_name":"{}"}}"#,
                events_pkg
            ),
        );
    }
    t
}

fn explain_as(d: &Path, package: &str) -> Result<Explanation, String> {
    explain(&Query {
        project_dir: d,
        package,
        run: None,
        package_notes: String::new(),
        notes_file: None,
    })
}

#[test]
fn the_exact_name_finds_its_state_and_its_run() {
    let t = project(&["python-Genshi"], "python-Genshi");
    let ex = explain_as(t.path(), "python-Genshi").unwrap();
    assert_eq!(ex.package, "python-Genshi");
    assert_eq!(ex.obs_name, "python-Genshi");
    assert_eq!(ex.run.as_deref(), Some(RUN));
    assert_eq!(
        ex.tries.as_ref().map(|t| t.pkg.as_str()),
        Some("python-Genshi")
    );
}

#[test]
fn another_case_finds_the_package_and_names_the_obs_spelling() {
    let t = project(&["python-genshi"], "python-Genshi");
    let ex = explain_as(t.path(), "python-genshi").unwrap();
    assert_eq!(ex.package, "python-genshi", "the name as given is kept");
    assert_eq!(
        ex.obs_name, "python-Genshi",
        "the OBS name the events carry"
    );
    assert_eq!(ex.run.as_deref(), Some(RUN));
    assert_eq!(ex.attempts.len(), 1);
    assert_eq!(
        ex.tries.as_ref().map(|t| t.pkg.as_str()),
        Some("python-Genshi")
    );
    let text = ansi(&render(&ex, 80), false);
    assert!(
        text.lines()
            .any(|l| l.contains("OBS name") && l.contains("python-Genshi")),
        "{text}"
    );
}

#[test]
fn an_underscore_and_a_dash_are_the_same_package() {
    let t = project(&["python-pydantic_core"], "python-pydantic_core");
    let ex = explain_as(t.path(), "python-pydantic-core").unwrap();
    assert_eq!(ex.obs_name, "python-pydantic_core");
    assert_eq!(ex.run.as_deref(), Some(RUN));
    assert_eq!(ex.attempts.len(), 1);
}

#[test]
fn a_name_that_only_drops_separators_is_another_package() {
    // `pythonwhois` is not `python-whois`: the loose key of `fuzzy_key` would
    // match them, the PEP 503 key does not.
    let t = project(&["python-whois"], "python-whois");
    let err = explain_as(t.path(), "pythonwhois").unwrap_err();
    assert!(err.contains("nothing about pythonwhois"), "{err}");
}

#[test]
fn two_spellings_that_differ_in_separators_are_listed_not_guessed() {
    let t = project(
        &["python-pydantic_core", "python-pydantic-core"],
        "python-pydantic-core",
    );
    // The exact spelling of either directory is that package.
    assert!(explain_as(t.path(), "python-pydantic-core").is_ok());
    let err = explain_as(t.path(), "python.pydantic.core").unwrap_err();
    assert!(err.contains("python-pydantic-core"), "{err}");
    assert!(err.contains("python-pydantic_core"), "{err}");
    assert!(err.contains("exact spelling"), "{err}");
}

#[test]
fn two_directories_that_differ_only_in_case_read_the_lowercase_one_and_say_so() {
    let t = project(&["python-Genshi", "python-genshi"], "python-Genshi");
    let ex = explain_as(t.path(), "PYTHON-GENSHI").unwrap();
    assert!(
        ex.unknown.iter().any(
            |u| u.contains("python-Genshi, python-genshi") && u.contains("reads python-genshi")
        ),
        "{:?}",
        ex.unknown
    );
    assert_eq!(ex.run.as_deref(), Some(RUN));
}

#[test]
fn the_work_directory_of_a_name_is_found_in_its_spelling() {
    let t = tempfile::tempdir().unwrap();
    let root = t.path();
    write(
        &root.join("alpha-x86_64/state/python-genshi/state.json"),
        "{}",
    );
    write(
        &root.join("beta-x86_64/state/python-other/state.json"),
        "{}",
    );
    let dir = resolve_project_dir(root, None, "x86_64", "python-Genshi").unwrap();
    assert_eq!(dir, root.join("alpha-x86_64"));
    let err = resolve_project_dir(root, None, "x86_64", "python-missing").unwrap_err();
    assert!(err.contains("no state for python-missing"), "{err}");
}

#[test]
fn the_package_notes_file_is_found_in_its_spelling() {
    let t = tempfile::tempdir().unwrap();
    let notes = t.path();
    write(&notes.join("python-genshi.md"), "# notes\n");
    write(&notes.join("python-genshi.md.lock"), "");
    assert_eq!(
        package_notes_name(notes, "python-Genshi")
            .unwrap()
            .as_deref(),
        Some("python-genshi")
    );
    assert_eq!(package_notes_name(notes, "python-other").unwrap(), None);
    write(&notes.join("python_genshi.md"), "# other\n");
    let err = package_notes_name(notes, "python.genshi").unwrap_err();
    assert!(err.contains("exact spelling"), "{err}");
}
