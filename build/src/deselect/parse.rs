use super::*;

/// The test runner the failing ids belong to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Runner {
    Pytest,
    Unittest,
    Ctest,
}

/// One failing test as the log names it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FailingTest {
    pub runner: Runner,
    /// pytest node id, `module.Class.test` for unittest, the ctest name.
    pub id: String,
    /// The log line the test was read from, the evidence.
    pub line: String,
}

/// What [`drop_failing_tests`] did.
#[derive(Debug, PartialEq, Eq)]
pub struct Dropped {
    pub spec: String,
    pub tests: Vec<FailingTest>,
    /// Failing tests not dropped (past the per-round cap).
    pub omitted: usize,
}

/// Text in a failing line or traceback that says a dependency is missing or
/// broken, not that a test's assertion failed.
pub(super) const DEPENDENCY_MARKERS: &[&str] = &[
    "ImportError",
    "ModuleNotFoundError",
    "No module named",
    "cannot import name",
    "undefined symbol",
];

/// Why the log's failure is a dependency problem rather than a test failure.
pub(super) fn dependency_problem(lines: &[String]) -> Option<String> {
    for l in lines {
        let t = l.trim();
        let hit = t.contains("errors during collection")
            || t.contains("error during collection")
            || t.starts_with("ERROR collecting")
            || t.contains("unittest.loader._FailedTest")
            || ((t.starts_with("FAILED ") || t.starts_with("ERROR "))
                && DEPENDENCY_MARKERS.iter().any(|m| t.contains(m)))
            || (t.starts_with("E ") && DEPENDENCY_MARKERS.iter().any(|m| t.contains(m)))
            || t.starts_with("ImportError:")
            || t.starts_with("ModuleNotFoundError:")
            || (t.starts_with("ERROR ")
                && t.split_whitespace()
                    .nth(1)
                    .is_some_and(|f| !f.contains("::") && f.ends_with(".py")));
        if hit {
            return Some(t.chars().take(160).collect());
        }
    }
    None
}

pub(super) fn clean_lines(log: &str) -> Vec<String> {
    log.lines()
        .map(|l| strip_timestamp(l).trim_end().to_string())
        .collect()
}

/// Whether `id` can be written into a spec and a shell line as one token, and
/// names a single test (not a file or directory).
pub(super) fn safe_pytest_id(id: &str) -> bool {
    let Some((path, rest)) = id.split_once("::") else {
        return false;
    };
    !rest.is_empty()
        && !rest.ends_with("::")
        && !path.is_empty()
        && !path.starts_with('/')
        && !path.split('/').any(|c| c == ".." || c.is_empty())
        && path.rsplit('/').next().is_some_and(|f| f.contains('.'))
        && !id.contains("...")
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "_./:[]-+,=@#()".contains(c))
}

pub(super) fn safe_ctest_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 120
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "_.+-".contains(c))
}

pub(super) fn safe_ident(s: &str) -> bool {
    !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// The counts of a pytest summary line (`=== 3 failed, 5 passed in 1.2s ===`),
/// without the ` in <seconds>s` tail. A line that only mentions a failure
/// (`FAILED t.py::t - request failed in retry loop`) is not one: a summary
/// starts with a count and ends with a duration.
pub(super) fn summary_head(t: &str) -> Option<&str> {
    if !t.starts_with(|c: char| c.is_ascii_digit()) {
        return None;
    }
    let (head, tail) = t.rsplit_once(" in ")?;
    let secs = tail.split_whitespace().next()?.strip_suffix('s')?;
    secs.parse::<f64>().ok()?;
    Some(head)
}

pub(super) fn pytest_failures(lines: &[String]) -> Result<Vec<FailingTest>, String> {
    // A suite in which nothing passed fails for a reason common to all of it.
    for l in lines {
        let t = l.trim_matches(|c: char| c == '=' || c.is_whitespace());
        let Some(head) = summary_head(t) else {
            continue;
        };
        if head.contains(" failed") {
            let passed = head
                .split(", ")
                .find_map(|part| part.strip_suffix(" passed"))
                .and_then(|n| n.trim().parse::<usize>().ok());
            if passed.is_none() || passed == Some(0) {
                return Err(format!(
                    "no test passed ({t}): the whole suite fails, which is not a test to drop"
                ));
            }
        }
    }
    let mut out: Vec<FailingTest> = Vec::new();
    for l in lines {
        let t = l.trim();
        let Some(rest) = t
            .strip_prefix("FAILED ")
            .or_else(|| t.strip_prefix("ERROR "))
        else {
            continue;
        };
        let id = rest.split_whitespace().next().unwrap_or("");
        if id.contains("::") && safe_pytest_id(id) && !out.iter().any(|f| f.id == id) {
            out.push(FailingTest {
                runner: Runner::Pytest,
                id: id.to_string(),
                line: t.to_string(),
            });
        }
    }
    Ok(out)
}

pub(super) fn unittest_failures(lines: &[String]) -> Vec<FailingTest> {
    let mut out: Vec<FailingTest> = Vec::new();
    for l in lines {
        let t = l.trim();
        let Some(rest) = t
            .strip_prefix("FAIL: ")
            .or_else(|| t.strip_prefix("ERROR: "))
        else {
            continue;
        };
        let Some((name, tail)) = rest.split_once(" (") else {
            continue;
        };
        let Some(dotted) = tail.split(')').next() else {
            continue;
        };
        let mut parts: Vec<&str> = dotted.split('.').collect();
        if parts.last() != Some(&name) {
            parts.push(name);
        }
        if parts.len() < 3 || !name.starts_with("test") || !parts.iter().all(|p| safe_ident(p)) {
            continue;
        }
        let id = parts.join(".");
        if !out.iter().any(|f| f.id == id) {
            out.push(FailingTest {
                runner: Runner::Unittest,
                id,
                line: t.to_string(),
            });
        }
    }
    out
}

pub(super) fn ctest_failures(lines: &[String]) -> Result<Vec<FailingTest>, String> {
    let Some(start) = lines.iter().position(|l| l.contains("tests FAILED:")) else {
        return Ok(Vec::new());
    };
    let mut out = Vec::new();
    for l in &lines[start + 1..] {
        let t = l.trim();
        // `3 - name (Failed)`
        let Some((num, rest)) = t.split_once(" - ") else {
            if t.is_empty() {
                continue;
            }
            break;
        };
        if num.is_empty() || !num.chars().all(|c| c.is_ascii_digit()) {
            break;
        }
        let Some((name, status)) = rest.rsplit_once(" (") else {
            continue;
        };
        let status = status.trim_end_matches(')');
        if status.contains("Not Run") {
            return Err(format!(
                "ctest could not run a test ({t}): that is a build problem, not a test to drop"
            ));
        }
        let known = [
            "Failed",
            "Timeout",
            "SEGFAULT",
            "Exception",
            "Subprocess aborted",
        ];
        if known.iter().any(|k| status.starts_with(k)) && safe_ctest_name(name) {
            out.push(FailingTest {
                runner: Runner::Ctest,
                id: name.to_string(),
                line: t.to_string(),
            });
        }
    }
    Ok(out)
}

/// The failing tests of a `%check` log that may be dropped, or why none may.
pub fn failing_tests(log: &str) -> Result<Vec<FailingTest>, String> {
    let lines = clean_lines(log);
    if let Some(l) = dependency_problem(&lines) {
        return Err(format!(
            "the failure is an import or collection error ({l}), a dependency problem that \
             dropping tests would only hide"
        ));
    }
    let mut found = pytest_failures(&lines)?;
    if found.is_empty() {
        found = unittest_failures(&lines);
    }
    if found.is_empty() {
        found = ctest_failures(&lines)?;
    }
    if found.is_empty() {
        return Err("no individual failing test could be read from the log".into());
    }
    Ok(found)
}
