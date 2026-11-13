//! The minimum a source declares, written on an update's added dependency, through
//! the validation passes (convention 75).

use super::{minimum_gaps, normalize_spec_text};
use ap_core::event::Event;
use ap_core::sink::Reporter;

/// configupdater 3.2 (the origin) and the 4.1.1 update the agent wrote, with
/// hatchling and hatch-vcs named and no bound (the case this rule is for).
const ORIGIN: &str = "Name: python-ConfigUpdater\nVersion: 3.2\nRelease: 0\n\
    Summary: A config\nLicense: MIT\nSource0: ConfigUpdater-3.2.tar.gz\n\
    BuildRequires:  %{python_module setuptools}\n\n%description\nA config.\n\n\
    %prep\n%setup -q\n\n%build\n\n%install\n\n%files\n%{_bindir}/foo\n\n%changelog\n";
const UPDATED: &str = "Name: python-ConfigUpdater\nVersion: 4.1.1\nRelease: 0\n\
    Summary: A config\nLicense: MIT\nSource0: ConfigUpdater-4.1.1.tar.gz\n\
    BuildRequires:  %{python_module hatch-vcs}\n\
    BuildRequires:  %{python_module hatchling}\n\n%description\nA config.\n\n\
    %prep\n%setup -q\n\n%build\n\n%install\n\n%files\n%{_bindir}/foo\n\n%changelog\n";

/// A source tree whose pyproject.toml declares the 4.1.1 build-system minimums.
fn source() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("pyproject.toml"),
        "[build-system]\nrequires = [\"hatchling>=1.27\", \"hatch-vcs>=0.5\"]\n\
         build-backend = \"hatchling.build\"\n",
    )
    .unwrap();
    dir
}

#[test]
fn an_update_writes_the_minimums_upstream_declares() {
    let source = source();
    let out = normalize_spec_text(UPDATED, true, Some(ORIGIN), Some(source.path()), "x").text;
    assert!(out.contains("%{python_module hatch-vcs >= 0.5}"), "{out}");
    assert!(out.contains("%{python_module hatchling >= 1.27}"), "{out}");
}

#[test]
fn the_minimum_is_recorded_under_its_rule() {
    let source = source();
    let passes = normalize_spec_text(UPDATED, true, Some(ORIGIN), Some(source.path()), "x");
    let change = passes
        .changes
        .iter()
        .find(|c| c.rule == "add_upstream_minimums")
        .expect("the minimum is recorded");
    assert_eq!(change.added.len(), 2, "{change:?}");
}

#[test]
fn a_fresh_spec_is_not_given_minimums_by_this_rule() {
    let source = source();
    let out = normalize_spec_text(UPDATED, false, None, Some(source.path()), "x").text;
    assert!(!out.contains(">= 1.27"), "{out}");
}

#[test]
fn an_inherited_line_without_its_minimum_is_reported_once() {
    use super::super::UpdateMode;
    use std::collections::BTreeSet;

    struct Collect(std::sync::Mutex<Vec<String>>);
    impl Reporter for Collect {
        fn report(&self, event: &Event) {
            if let Event::Warning { message, .. } = event {
                self.0.lock().unwrap().push(message.clone());
            }
        }
    }

    let origin = "Name: python-ConfigUpdater\nVersion: 4.0\n\
        BuildRequires:  %{python_module hatchling}\n";
    let spec = "Name: python-ConfigUpdater\nVersion: 4.1.1\n\
        BuildRequires:  %{python_module hatchling}\n";
    let source = source();
    let mut update = Some(UpdateMode {
        origin_spec: Some(origin.to_string()),
        reported: BTreeSet::new(),
    });
    let reporter = Collect(std::sync::Mutex::new(Vec::new()));
    minimum_gaps(
        spec,
        &mut update,
        Some(source.path()),
        &reporter,
        "python-x",
    );
    minimum_gaps(
        spec,
        &mut update,
        Some(source.path()),
        &reporter,
        "python-x",
    );
    let warned = reporter.0.lock().unwrap().clone();
    assert_eq!(warned.len(), 1, "{warned:?}");
    assert!(warned[0].contains("hatchling >= 1.27"), "{warned:?}");
    // The line is the origin's: nothing is written to the spec by this report.
    assert!(!spec.contains(">="));
}
