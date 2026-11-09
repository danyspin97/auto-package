//! Flaky `%check` tests: telling a test that fails every time from one that
//! failed once.
//!
//! When a build fails and the only thing wrong is a set of failing tests, the
//! pipeline asks OBS for one more build before the agent sees anything. Two
//! logs then say which tests are stable failures (the ones to deal with) and
//! which came and went (flaky: not a reason to change the spec).

use std::collections::BTreeSet;
use std::path::Path;

/// What two builds of the same sources say about the failing tests.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Verdict {
    /// Failed in both builds.
    pub stable: BTreeSet<String>,
    /// Failed in one build only.
    pub flaky: BTreeSet<String>,
}

impl Verdict {
    /// The agent's note for the retry: what is flaky and what is not.
    pub fn context(&self) -> String {
        let list = |s: &BTreeSet<String>| s.iter().take(20).cloned().collect::<Vec<_>>().join(", ");
        let mut text = String::from(
            "## Flaky tests (read by the pipeline)\n\nThe sources were built twice and the \
             failing tests differ between the two builds.\n",
        );
        if !self.flaky.is_empty() {
            text.push_str(&format!(
                "- Failed in ONE build only (flaky; convention 7 (section 87)): {}\n",
                list(&self.flaky)
            ));
        }
        if !self.stable.is_empty() {
            text.push_str(&format!(
                "- Failed in BOTH builds (a real failure): {}\n",
                list(&self.stable)
            ));
        }
        text.push_str(
            "Do not \"fix\" or delete a flaky test; run it in a best-effort pass (convention 7 (section 87)).\n",
        );
        text
    }

    /// The line for the package notes.
    pub fn note(&self) -> String {
        format!(
            "flaky tests (failed in one of two builds): {}; stable failures: {}",
            self.flaky.iter().cloned().collect::<Vec<_>>().join(", "),
            self.stable.iter().cloned().collect::<Vec<_>>().join(", ")
        )
    }
}

/// `Some(verdict)` when the two builds disagree about at least one test;
/// `None` when they fail on exactly the same tests (a real failure, not flaky).
pub fn compare(first: &BTreeSet<String>, second: &BTreeSet<String>) -> Option<Verdict> {
    let stable: BTreeSet<String> = first.intersection(second).cloned().collect();
    let flaky: BTreeSet<String> = first.symmetric_difference(second).cloned().collect();
    (!flaky.is_empty()).then_some(Verdict { stable, flaky })
}

/// The tests that failed in the `%check` of a build log, or `None` when the log
/// does not read as "only tests failed" (a build error, an import problem, no
/// test names). Understands pytest, unittest and ctest (through
/// [`crate::deselect::failing_tests`]) and `mysql-test-run` (MariaDB, MySQL).
pub fn failing_ids(log: &str) -> Option<BTreeSet<String>> {
    let mtr = mtr_failures(log);
    if !mtr.is_empty() {
        return Some(mtr);
    }
    let found = crate::deselect::failing_tests(log).ok()?;
    let ids: BTreeSet<String> = found.into_iter().map(|t| t.id).collect();
    (!ids.is_empty()).then_some(ids)
}

/// The tests a `mysql-test-run.pl` log reports as `[ fail ]` (`main.alter_table
/// w3 [ fail ]`, `innodb.foo 'xengine' [ fail ]`).
fn mtr_failures(log: &str) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for line in log.lines() {
        // OBS prefixes each log line with a `[  123s]` timestamp.
        let line = match line.strip_prefix('[') {
            Some(rest) => rest.split_once(']').map_or(line, |(_, tail)| tail),
            None => line,
        }
        .trim();
        // The status is the first bracket group after the test name; a message
        // may follow it (`[ fail ]  timeout after 900 seconds`).
        let Some((head, tail)) = line.split_once('[') else {
            continue;
        };
        let Some((status, _message)) = tail.split_once(']') else {
            continue;
        };
        let status = status.trim();
        if status != "fail" && status != "timeout" {
            continue;
        }
        let Some(name) = head.split_whitespace().next() else {
            continue;
        };
        // `suite.test`: a dotted identifier with no path separators.
        let ok = name.contains('.')
            && name
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | '+'));
        if ok {
            out.insert(name.to_string());
        }
    }
    out
}

/// What a run remembers between its first failing build and the retry.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Probe {
    /// The run this belongs to: a new run asks again.
    pub run_id: String,
    pub first: BTreeSet<String>,
    /// The retry has been looked at: this run does not probe again.
    #[serde(default)]
    pub compared: bool,
}

const PROBE_FILE: &str = "flaky-probe.json";

/// The probe of `run_id` kept in the workspace directory, if any.
pub fn load_probe(dir: &Path, run_id: &str) -> Option<Probe> {
    let text = std::fs::read_to_string(dir.join(PROBE_FILE)).ok()?;
    let probe: Probe = serde_json::from_str(&text).ok()?;
    (probe.run_id == run_id).then_some(probe)
}

/// Keep `probe` in `dir`. A caller that cannot save it must not rely on it
/// being found again.
pub fn save_probe(dir: &Path, probe: &Probe) -> std::io::Result<()> {
    let text = serde_json::to_string(probe).map_err(std::io::Error::other)?;
    std::fs::write(dir.join(PROBE_FILE), text)
}

pub fn clear_probe(dir: &Path) {
    let _ = std::fs::remove_file(dir.join(PROBE_FILE));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn set(items: &[&str]) -> BTreeSet<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn tests_that_come_and_go_are_flaky_and_the_rest_are_stable() {
        let v = compare(&set(&["a", "b"]), &set(&["b", "c"])).unwrap();
        assert_eq!(v.stable, set(&["b"]));
        assert_eq!(v.flaky, set(&["a", "c"]));
        assert!(compare(&set(&["a"]), &set(&["a"])).is_none());
        // A second build that passes everything has nothing failing to compare.
        assert_eq!(compare(&set(&["a"]), &set(&[])).unwrap().flaky, set(&["a"]));
    }

    #[test]
    fn mysql_test_run_failures_are_read_from_the_log() {
        let log = "[  900s] main.alter_table                         w3 [ pass ]   1200\n\
                   [  901s] main.mdev_1234                           w1 [ fail ]\n\
                   [  902s] innodb.foo 'xengine'                     w2 [ fail ]\n\
                   [  903s] galera.slow_one                          w4 [ timeout ]\n\
                   [  904s] ==============================================================\n";
        assert_eq!(
            mtr_failures(log),
            set(&["main.mdev_1234", "innodb.foo", "galera.slow_one"])
        );
        assert_eq!(
            failing_ids(log),
            Some(set(&["main.mdev_1234", "innodb.foo", "galera.slow_one"]))
        );
    }

    #[test]
    fn a_message_after_the_status_is_still_a_failure() {
        let log = "[  901s] main.slow_one  w1 [ fail ]  timeout after 900 seconds\n\
                   [  902s] main.fine  w2 [ pass ]  [ fail ] not a status\n";
        assert_eq!(mtr_failures(log), set(&["main.slow_one"]));
    }

    #[test]
    fn saving_a_probe_reports_a_write_error() {
        let dir = tempfile::tempdir().unwrap();
        let probe = Probe {
            run_id: "r".into(),
            first: set(&["a"]),
            compared: false,
        };
        assert!(save_probe(&dir.path().join("missing"), &probe).is_err());
        assert!(save_probe(dir.path(), &probe).is_ok());
    }

    #[test]
    fn pytest_failures_and_non_test_failures_are_told_apart() {
        let log = "[  30s] FAILED tests/test_a.py::test_one - AssertionError: boom\n[  30s] ===== 1 failed, 40 passed in 2s =====\n";
        assert_eq!(failing_ids(log), Some(set(&["tests/test_a.py::test_one"])));
        assert_eq!(
            failing_ids("error: command 'gcc' failed with exit status 1\n"),
            None
        );
    }

    #[test]
    fn the_probe_is_kept_per_run() {
        let dir = tempfile::tempdir().unwrap();
        assert!(load_probe(dir.path(), "r1").is_none());
        save_probe(
            dir.path(),
            &Probe {
                run_id: "r1".into(),
                first: set(&["a"]),
                compared: false,
            },
        )
        .unwrap();
        assert_eq!(load_probe(dir.path(), "r1").unwrap().first, set(&["a"]));
        assert!(load_probe(dir.path(), "r2").is_none());
        clear_probe(dir.path());
        assert!(load_probe(dir.path(), "r1").is_none());
    }

    #[test]
    fn the_agent_note_names_flaky_and_stable_tests() {
        let v = compare(&set(&["a", "b"]), &set(&["b"])).unwrap();
        let text = v.context();
        assert!(
            text.contains("ONE build only") && text.contains("convention 7 (section 87)"),
            "{text}"
        );
        assert!(text.contains("BOTH builds"));
    }
}
