//! What the passes of the validation round change in a spec. A pass that
//! removes or rewrites a line is reported, as an event and as a note the next
//! agent turn reads: an agent that does not know the tool removed a line adds
//! it again, and the next validation removes it again.

use super::*;
use crate::pipeline::render::append_notes;
use ap_core::event::StepId;

/// The lines one pass of the validation round changed.
#[derive(Debug, PartialEq)]
pub(in crate::pipeline) struct SpecChange {
    pub(in crate::pipeline) rule: &'static str,
    pub(in crate::pipeline) removed: Vec<String>,
    pub(in crate::pipeline) added: Vec<String>,
}

/// The spec as the passes leave it, and the passes that changed it, in order.
#[derive(Debug)]
pub(in crate::pipeline) struct SpecPasses {
    pub(in crate::pipeline) text: String,
    pub(in crate::pipeline) changes: Vec<SpecChange>,
}

impl SpecPasses {
    pub(in crate::pipeline) fn new(text: &str) -> Self {
        Self {
            text: text.to_string(),
            changes: Vec::new(),
        }
    }

    /// Run `pass` on the text; a change it makes is recorded under `rule`.
    pub(in crate::pipeline) fn apply(
        mut self,
        rule: &'static str,
        pass: impl FnOnce(&str) -> String,
    ) -> Self {
        let next = pass(&self.text);
        if next != self.text {
            let (removed, added) = line_diff(&self.text, &next);
            self.changes.push(SpecChange {
                rule,
                removed,
                added,
            });
        }
        self.text = next;
        self
    }
}

/// The non-blank lines `before` has and `after` does not, and the other way
/// round, each line counted as many times as it occurs.
fn line_diff(before: &str, after: &str) -> (Vec<String>, Vec<String>) {
    let mut left: Vec<&str> = before.lines().collect();
    let mut added: Vec<&str> = Vec::new();
    for line in after.lines() {
        match left.iter().position(|l| *l == line) {
            Some(i) => {
                left.remove(i);
            }
            None => added.push(line),
        }
    }
    let shown = |lines: Vec<&str>| -> Vec<String> {
        lines
            .into_iter()
            .filter(|l| !l.trim().is_empty())
            .map(str::to_string)
            .collect()
    };
    (shown(left), shown(added))
}

/// How long a line is quoted in an event or a note.
const QUOTED_CHARS: usize = 160;

fn quoted(lines: &[String]) -> String {
    if lines.is_empty() {
        return "nothing".to_string();
    }
    lines
        .iter()
        .map(|l| {
            let short: String = l.trim().chars().take(QUOTED_CHARS).collect();
            let ellipsis = if l.trim().chars().count() > QUOTED_CHARS {
                "..."
            } else {
                ""
            };
            format!("`{short}{ellipsis}`")
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// Report each change: a warning naming the pass, and a note in the package's
/// journal, which the next agent turn reads.
pub(in crate::pipeline) fn report_changes(
    changes: &[SpecChange],
    rpm_name: &str,
    step: Option<StepId>,
    reporter: &dyn Reporter,
    notes: PathBuf,
) {
    if changes.is_empty() {
        return;
    }
    let mut section = String::from(
        "\n## Spec lines the tool changed\nThe tool changed these lines after your last \
         turn. It removes a line it finds unused again on the next validation, so keep a \
         line only with what needs it (a `%check` that runs it, a test that imports it).\n",
    );
    for change in changes {
        let (removed, added) = (quoted(&change.removed), quoted(&change.added));
        reporter.report(&Event::Warning {
            step,
            message: format!(
                "{rpm_name}: {} changed the spec: removed {removed}, added {added}",
                change.rule
            ),
        });
        section.push_str(&format!(
            "- {}: removed {removed}, added {added}\n",
            change.rule
        ));
    }
    append_notes(notes, &section);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_pass_that_drops_a_line_is_recorded_with_that_line() {
        let spec = "Name: foo\nBuildRequires: %{python_module pytest}\n%files\n";
        let passes = SpecPasses::new(spec).apply("drop_it", |s| {
            s.replace("BuildRequires: %{python_module pytest}\n", "")
        });
        assert_eq!(passes.text, "Name: foo\n%files\n");
        assert_eq!(
            passes.changes,
            vec![SpecChange {
                rule: "drop_it",
                removed: vec!["BuildRequires: %{python_module pytest}".to_string()],
                added: Vec::new(),
            }]
        );
    }

    #[test]
    fn a_pass_that_changes_nothing_records_nothing() {
        let passes = SpecPasses::new("Name: foo\n").apply("noop", str::to_string);
        assert!(passes.changes.is_empty());
    }

    #[test]
    fn a_rewritten_line_is_both_removed_and_added() {
        let (removed, added) = line_diff("Requires: a >=1\n\n", "Requires: a >= 1\n");
        assert_eq!(removed, vec!["Requires: a >=1"]);
        assert_eq!(added, vec!["Requires: a >= 1"]);
    }

    #[test]
    fn a_long_line_is_shortened_in_the_note() {
        let long = "x".repeat(QUOTED_CHARS + 5);
        assert!(quoted(&[long]).ends_with("...`"));
    }

    #[test]
    fn a_change_is_warned_about_and_left_in_the_journal() {
        struct Collect(std::sync::Mutex<Vec<String>>);
        impl Reporter for Collect {
            fn report(&self, event: &Event) {
                if let Event::Warning { message, .. } = event {
                    self.0.lock().unwrap().push(message.clone());
                }
            }
        }
        let dir = tempfile::tempdir().unwrap();
        let notes = dir.path().join("notes.md");
        let change = SpecChange {
            rule: "drop_unused_pytest_build_requires",
            removed: vec!["BuildRequires: %{python_module pytest}".to_string()],
            added: Vec::new(),
        };
        let reporter = Collect(std::sync::Mutex::new(Vec::new()));
        report_changes(&[change], "python-foo", None, &reporter, notes.clone());
        let warned = reporter.0.lock().unwrap().clone();
        assert_eq!(warned.len(), 1, "{warned:?}");
        assert!(warned[0].contains("python-foo: drop_unused_pytest_build_requires"));
        let journal = std::fs::read_to_string(&notes).unwrap();
        assert!(
            journal.contains("## Spec lines the tool changed"),
            "{journal}"
        );
        assert!(journal.contains("removed `BuildRequires: %{python_module pytest}`"));
    }
}
