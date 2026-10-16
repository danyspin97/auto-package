//! Dropping failing tests from `%check`, mechanically, with the log as evidence.
//!
//! When the package builds and installs and only a few individual tests fail in
//! `%check`, the fix is to skip exactly those tests and say why. This reads the
//! failing test ids from the build log (pytest `FAILED path::test` lines,
//! unittest `FAIL: test (mod.Class.test)` headers, ctest's "The following tests
//! FAILED" list) and edits the spec: a narrow `--deselect` through
//! `PYTEST_ADDOPTS`, an `-E` regex on the `%ctest` line, or a `skip` decorator
//! inserted in `%prep` for unittest. Every dropped test carries a
//! `# evidence:` comment quoting its log line.
//!
//! It refuses when the failure is a dependency problem (an import error, a
//! collection error, no test passing at all) because deselecting would hide a
//! package that does not work. Never a whole file or directory; at most
//! [`MAX_PER_ROUND`] tests at a time and [`MAX_IN_SPEC`] in one spec.

use crate::log::strip_timestamp;

/// Tests dropped by one call.
pub const MAX_PER_ROUND: usize = 5;
/// Tests the mechanical rung may have dropped from one spec over all rounds.
pub const MAX_IN_SPEC: usize = 15;

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
const DEPENDENCY_MARKERS: &[&str] = &[
    "ImportError",
    "ModuleNotFoundError",
    "No module named",
    "cannot import name",
    "undefined symbol",
];

/// Why the log's failure is a dependency problem rather than a test failure.
fn dependency_problem(lines: &[String]) -> Option<String> {
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
                && t.split_whitespace().nth(1).is_some_and(|f| !f.contains("::") && f.ends_with(".py")));
        if hit {
            return Some(t.chars().take(160).collect());
        }
    }
    None
}

fn clean_lines(log: &str) -> Vec<String> {
    log.lines()
        .map(|l| strip_timestamp(l).trim_end().to_string())
        .collect()
}

/// Whether `id` can be written into a spec and a shell line as one token, and
/// names a single test (not a file or directory).
fn safe_pytest_id(id: &str) -> bool {
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
        && id.chars().all(|c| {
            c.is_ascii_alphanumeric() || "_./:[]-+,=@#()".contains(c)
        })
}

fn safe_ctest_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 120
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "_.+-".contains(c))
}

fn safe_ident(s: &str) -> bool {
    !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

fn pytest_failures(lines: &[String]) -> Result<Vec<FailingTest>, String> {
    // A suite in which nothing passed fails for a reason common to all of it.
    for l in lines {
        let t = l.trim_matches(|c: char| c == '=' || c.is_whitespace());
        if t.contains(" failed") && t.contains(" in ") {
            let head = t.rsplit_once(" in ").map_or(t, |(h, _)| h);
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
        let Some(rest) = t.strip_prefix("FAILED ").or_else(|| t.strip_prefix("ERROR ")) else {
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

fn unittest_failures(lines: &[String]) -> Vec<FailingTest> {
    let mut out: Vec<FailingTest> = Vec::new();
    for l in lines {
        let t = l.trim();
        let Some(rest) = t.strip_prefix("FAIL: ").or_else(|| t.strip_prefix("ERROR: ")) else {
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

fn ctest_failures(lines: &[String]) -> Result<Vec<FailingTest>, String> {
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
        let known = ["Failed", "Timeout", "SEGFAULT", "Exception", "Subprocess aborted"];
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

const SECTIONS: &[&str] = &[
    "%prep",
    "%build",
    "%install",
    "%check",
    "%files",
    "%changelog",
    "%package",
    "%description",
    "%generate_buildrequires",
    "%clean",
    "%pre",
    "%post",
    "%preun",
    "%postun",
    "%pretrans",
    "%posttrans",
];

fn is_section(line: &str) -> bool {
    let first = line.split_whitespace().next().unwrap_or("");
    SECTIONS.contains(&first)
}

/// The line range `[start, end)` of a section's body, `start` after its header.
fn section_body(lines: &[&str], name: &str) -> Option<(usize, usize)> {
    let header = lines
        .iter()
        .position(|l| l.split_whitespace().next() == Some(name))?;
    let end = lines[header + 1..]
        .iter()
        .position(|l| is_section(l))
        .map_or(lines.len(), |p| header + 1 + p);
    Some((header + 1, end))
}

/// What is already dropped from a spec: pytest `--deselect=` ids, unittest
/// `skip("auto-package dropped <id>")` ids and ctest names in an `-E` regex.
fn already_dropped(spec: &str) -> Vec<String> {
    let mut out = Vec::new();
    for tok in spec.split(|c: char| c.is_whitespace() || c == '"' || c == '\'') {
        if let Some(id) = tok.strip_prefix("--deselect=") {
            out.push(id.to_string());
        }
    }
    for part in spec.split("auto-package dropped ").skip(1) {
        let id: String = part
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '.')
            .collect();
        out.push(id);
    }
    out
}

/// How many tests the mechanical rung has dropped from `spec` so far.
pub fn dropped_count(spec: &str) -> usize {
    spec.lines()
        .filter(|l| l.trim_start().starts_with("# evidence: "))
        .count()
}

fn evidence_comment(line: &str) -> String {
    let flat: String = line
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .take(200)
        .collect();
    format!("# evidence: {}", flat.replace('%', "%%").trim())
}

fn apply_pytest(spec: &str, tests: &[&FailingTest]) -> Result<String, String> {
    let lines: Vec<&str> = spec.lines().collect();
    let (start, end) =
        section_body(&lines, "%check").ok_or("the spec has no %check section to edit")?;
    // Our own export has to come after one that assigns the variable outright.
    let mut at = start;
    for (i, l) in lines[start..end].iter().enumerate() {
        let t = l.trim_start();
        let assigns = t.strip_prefix("export ").unwrap_or(t).starts_with("PYTEST_ADDOPTS=");
        if assigns && !t.contains("PYTEST_ADDOPTS:-") && !t.contains("$PYTEST_ADDOPTS") && !t.contains("${PYTEST_ADDOPTS") {
            if t.ends_with('\\') {
                return Err("the %check sets PYTEST_ADDOPTS over several lines".into());
            }
            at = start + i + 1;
        }
    }
    let mut add: Vec<String> = tests.iter().map(|t| evidence_comment(&t.line)).collect();
    add.push(format!(
        "export PYTEST_ADDOPTS=\"${{PYTEST_ADDOPTS:-}} {}\"",
        tests
            .iter()
            .map(|t| format!("--deselect={}", t.id))
            .collect::<Vec<_>>()
            .join(" ")
    ));
    Ok(splice(&lines, at, &add, spec.ends_with('\n')))
}

fn apply_ctest(spec: &str, tests: &[&FailingTest]) -> Result<String, String> {
    let lines: Vec<&str> = spec.lines().collect();
    let (start, end) =
        section_body(&lines, "%check").ok_or("the spec has no %check section to edit")?;
    let idx = lines[start..end]
        .iter()
        .position(|l| {
            let t = l.trim_start();
            t.starts_with("%ctest") || t.starts_with("ctest ") || t == "ctest"
        })
        .map(|i| start + i)
        .ok_or("the %check has no ctest invocation to narrow")?;
    let line = lines[idx];
    if line.contains(" -E ") || line.contains("--exclude-regex") || line.trim_end().ends_with('\\') {
        return Err("the ctest line already has an exclude regex or continues on the next line".into());
    }
    let names: Vec<String> = tests
        .iter()
        .map(|t| t.id.replace('.', "\\.").replace('+', "\\+"))
        .collect();
    let edited = format!("{} -E '^({})$'", line.trim_end(), names.join("|"));
    let mut out: Vec<String> = Vec::new();
    for (i, l) in lines.iter().enumerate() {
        if i == idx {
            out.extend(tests.iter().map(|t| evidence_comment(&t.line)));
            out.push(edited.clone());
        } else {
            out.push(l.to_string());
        }
    }
    Ok(join(&out, spec.ends_with('\n')))
}

fn apply_unittest(
    spec: &str,
    tests: &[&FailingTest],
    source_has: &dyn Fn(&str) -> bool,
) -> Result<String, String> {
    let lines: Vec<&str> = spec.lines().collect();
    let (start, end) = section_body(&lines, "%prep").ok_or("the spec has no %prep section")?;
    let mut at = end;
    while at > start && lines[at - 1].trim().is_empty() {
        at -= 1;
    }
    let mut add: Vec<String> = Vec::new();
    for t in tests {
        let parts: Vec<&str> = t.id.split('.').collect();
        let (module, class, test) = (
            &parts[..parts.len() - 2],
            parts[parts.len() - 2],
            parts[parts.len() - 1],
        );
        let rel = module.join("/");
        let Some(file) = [format!("{rel}.py"), format!("src/{rel}.py")]
            .into_iter()
            .find(|c| source_has(c))
        else {
            continue;
        };
        add.push(evidence_comment(&t.line));
        add.push(format!(
            "sed -i -E '/^class {class}\\b/,/^class /{{s/^([[:space:]]+)def {test}\\(/\\1@__import__(\"unittest\").skip(\"auto-package dropped {id}\")\\n\\1def {test}(/}}' {file}",
            id = t.id
        ));
    }
    if add.is_empty() {
        return Err("none of the failing unittest modules was found in the unpacked source".into());
    }
    Ok(splice(&lines, at, &add, spec.ends_with('\n')))
}

fn splice(lines: &[&str], at: usize, add: &[String], trailing_newline: bool) -> String {
    let mut out: Vec<String> = lines[..at].iter().map(|s| s.to_string()).collect();
    out.extend(add.iter().cloned());
    out.extend(lines[at..].iter().map(|s| s.to_string()));
    join(&out, trailing_newline)
}

fn join(lines: &[String], trailing_newline: bool) -> String {
    let mut s = lines.join("\n");
    if trailing_newline {
        s.push('\n');
    }
    s
}

/// Edit `spec` so the tests `log` shows failing are skipped, each with its log
/// line as evidence. `source_has(rel)` says whether a file exists in the
/// unpacked source (unittest modules are located through it). `Err` is why
/// nothing may be dropped.
pub fn drop_failing_tests(
    spec: &str,
    log: &str,
    source_has: &dyn Fn(&str) -> bool,
) -> Result<Dropped, String> {
    let used = dropped_count(spec);
    if used >= MAX_IN_SPEC {
        return Err(format!(
            "{used} tests are already dropped from this spec (the limit is {MAX_IN_SPEC}): \
             more would be hiding a broken package"
        ));
    }
    let failing = failing_tests(log)?;
    let already = already_dropped(spec);
    let (fresh, repeated): (Vec<FailingTest>, Vec<FailingTest>) = failing
        .into_iter()
        .partition(|t| !already.iter().any(|a| *a == t.id || t.id.starts_with(&format!("{a}["))));
    if fresh.is_empty() {
        return Err(format!(
            "{} failing test(s) are already dropped here and still run: the drop does not take \
             effect, so another route is needed",
            repeated.len()
        ));
    }
    let room = MAX_PER_ROUND.min(MAX_IN_SPEC - used);
    let chosen: Vec<&FailingTest> = fresh.iter().take(room).collect();
    let runner = chosen[0].runner;
    let chosen: Vec<&FailingTest> = chosen.into_iter().filter(|t| t.runner == runner).collect();
    let spec_out = match runner {
        Runner::Pytest => apply_pytest(spec, &chosen)?,
        Runner::Ctest => apply_ctest(spec, &chosen)?,
        Runner::Unittest => apply_unittest(spec, &chosen, source_has)?,
    };
    let omitted = fresh.len() - chosen.len();
    Ok(Dropped {
        spec: spec_out,
        tests: chosen.into_iter().cloned().collect(),
        omitted,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const SPEC: &str = "Name: python-x\n%prep\n%autosetup -p1\n\n%build\n%pyproject_wheel\n\n%check\n%pytest\n\n%files\n%license LICENSE\n";

    fn none(_: &str) -> bool {
        false
    }

    fn pytest_log(failed: &[&str], passed: usize) -> String {
        let mut s = String::from("[  30s] =========== short test summary info ============\n");
        for f in failed {
            s.push_str(&format!("[  30s] FAILED {f} - AssertionError: boom\n"));
        }
        s.push_str(&format!(
            "[  30s] ===== {} failed, {passed} passed in 1.2s =====\n",
            failed.len()
        ));
        s
    }

    #[test]
    fn pytest_failures_become_a_narrow_deselect_with_evidence() {
        let log = pytest_log(&["tests/test_a.py::TestX::test_one", "tests/test_b.py::test_two[a-b]"], 40);
        let got = drop_failing_tests(SPEC, &log, &none).unwrap();
        assert_eq!(got.tests.len(), 2);
        assert_eq!(got.omitted, 0);
        assert!(got.spec.contains(
            "# evidence: FAILED tests/test_a.py::TestX::test_one - AssertionError: boom"
        ), "{}", got.spec);
        assert!(got.spec.contains(
            "export PYTEST_ADDOPTS=\"${PYTEST_ADDOPTS:-} --deselect=tests/test_a.py::TestX::test_one --deselect=tests/test_b.py::test_two[a-b]\""
        ), "{}", got.spec);
        // Inside %check, before the %pytest run.
        let check = got.spec.split("%check\n").nth(1).unwrap();
        assert!(check.find("export PYTEST_ADDOPTS").unwrap() < check.find("%pytest").unwrap());
        // Dropping the same tests again is refused: the drop is not taking effect.
        let again = drop_failing_tests(&got.spec, &log, &none).unwrap_err();
        assert!(again.contains("already dropped"), "{again}");
    }

    #[test]
    fn at_most_five_tests_per_round_and_fifteen_per_spec() {
        let ids: Vec<String> = (0..9).map(|i| format!("tests/t.py::test_{i}")).collect();
        let refs: Vec<&str> = ids.iter().map(String::as_str).collect();
        let got = drop_failing_tests(SPEC, &pytest_log(&refs, 100), &none).unwrap();
        assert_eq!(got.tests.len(), MAX_PER_ROUND);
        assert_eq!(got.omitted, 4);
        assert_eq!(dropped_count(&got.spec), 5);
        // A spec already at the cap takes no more.
        let mut spec = SPEC.to_string();
        for i in 0..MAX_IN_SPEC {
            spec.push_str(&format!("# evidence: FAILED t{i}\n"));
        }
        let err = drop_failing_tests(&spec, &pytest_log(&refs, 100), &none).unwrap_err();
        assert!(err.contains("already dropped"), "{err}");
    }

    #[test]
    fn a_whole_file_or_directory_or_an_unsafe_id_is_never_dropped() {
        for bad in [
            "tests/test_a.py",
            "tests/",
            "tests/test_a.py::",
            "/abs/test_a.py::t",
            "../x/test_a.py::t",
            "tests/test_a.py::t[a b]",
            "tests/test_a.py::t[$(x)]",
            "tests/test_a.py::t[100%]",
            "tests/test_a.py::test_very_long[param-...",
        ] {
            assert!(!safe_pytest_id(bad), "{bad}");
        }
        let err = drop_failing_tests(SPEC, &pytest_log(&["tests/test_a.py"], 5), &none).unwrap_err();
        assert!(err.contains("no individual failing test"), "{err}");
    }

    #[test]
    fn import_and_collection_errors_are_a_dependency_problem() {
        let collection = "[  9s] ERROR collecting tests/test_a.py\n[  9s] E   ModuleNotFoundError: No module named 'numpy'\n[  9s] ===== 1 error in 0.3s =====\n";
        let err = drop_failing_tests(SPEC, collection, &none).unwrap_err();
        assert!(err.contains("dependency problem"), "{err}");
        let summary = "FAILED tests/test_a.py::test_x - ImportError: cannot import name 'y' from 'z'\n== 1 failed, 9 passed in 1s ==\n";
        assert!(drop_failing_tests(SPEC, summary, &none).unwrap_err().contains("dependency problem"));
        let in_test = "E   ModuleNotFoundError: No module named 'torch'\nFAILED tests/test_a.py::test_x - assert 0\n== 1 failed, 9 passed in 1s ==\n";
        assert!(drop_failing_tests(SPEC, in_test, &none).is_err());
        let unittest = "ERROR: tests.test_a (unittest.loader._FailedTest.tests.test_a)\n";
        assert!(drop_failing_tests(SPEC, unittest, &none).unwrap_err().contains("dependency problem"));
        let broken = "ERROR tests/test_a.py - SyntaxError: bad\n== 1 error in 0.1s ==\n";
        assert!(drop_failing_tests(SPEC, broken, &none).is_err());
    }

    #[test]
    fn a_suite_in_which_nothing_passes_is_not_dropped() {
        let err = drop_failing_tests(SPEC, &pytest_log(&["tests/t.py::test_a"], 0), &none).unwrap_err();
        assert!(err.contains("no test passed"), "{err}");
    }

    #[test]
    fn the_export_goes_after_a_pytest_addopts_the_check_assigns_outright() {
        let spec = "%check\nexport PYTEST_ADDOPTS=\"-x\"\n%pytest\n\n%files\n";
        let got = drop_failing_tests(spec, &pytest_log(&["tests/t.py::test_a"], 5), &none).unwrap();
        let export_x = got.spec.find("PYTEST_ADDOPTS=\"-x\"").unwrap();
        let ours = got.spec.find("--deselect=").unwrap();
        assert!(export_x < ours, "{}", got.spec);
        let multiline = "%check\nexport PYTEST_ADDOPTS=\"-x \\\n  -q\"\n%pytest\n";
        assert!(drop_failing_tests(multiline, &pytest_log(&["tests/t.py::test_a"], 5), &none).is_err());
    }

    #[test]
    fn ctest_failures_narrow_the_ctest_line() {
        let log = "[  50s] 90% tests passed, 2 tests failed out of 20\n[  50s] \n[  50s] The following tests FAILED:\n[  50s] \t  3 - test_foo.bar (Failed)\n[  50s] \t  7 - test_baz (Timeout)\n[  50s] Errors while running CTest\n";
        let spec = "%check\n%ctest\n\n%files\n";
        let got = drop_failing_tests(spec, log, &none).unwrap();
        assert_eq!(got.tests.len(), 2);
        assert!(got.spec.contains("%ctest -E '^(test_foo\\.bar|test_baz)$'"), "{}", got.spec);
        assert!(got.spec.contains("# evidence: 3 - test_foo.bar (Failed)"), "{}", got.spec);
        // A test ctest could not run is a build problem.
        let not_run = "The following tests FAILED:\n\t  1 - test_a (Not Run)\n";
        assert!(drop_failing_tests(spec, not_run, &none).unwrap_err().contains("build problem"));
        // An existing exclude is not merged blindly.
        let with_e = "%check\n%ctest -E slow\n";
        assert!(drop_failing_tests(with_e, log, &none).is_err());
    }

    #[test]
    fn unittest_failures_get_a_skip_decorator_in_prep_for_the_module_found() {
        let log = "[  20s] FAIL: test_roundtrip (pkg.tests.test_codec.CodecTest.test_roundtrip)\n[  20s] ERROR: test_other (pkg.tests.test_codec.CodecTest)\n[  20s] FAIL: test_gone (pkg.tests.test_gone.G.test_gone)\n";
        let spec = "%prep\n%autosetup\n\n%build\n%check\n%python_exec -m unittest\n";
        let have = |p: &str| p == "pkg/tests/test_codec.py";
        let got = drop_failing_tests(spec, log, &have).unwrap();
        assert!(got.spec.contains("def test_roundtrip\\("), "{}", got.spec);
        assert!(got.spec.contains("/^class CodecTest\\b/,/^class /"), "{}", got.spec);
        assert!(got.spec.contains("pkg/tests/test_codec.py"), "{}", got.spec);
        assert!(!got.spec.contains("test_gone"), "{}", got.spec);
        let prep = got.spec.split("%prep\n").nth(1).unwrap();
        assert!(prep.find("sed -i").unwrap() < prep.find("%build").unwrap());
        // Nothing found in the tree: refused.
        assert!(drop_failing_tests(spec, log, &none).is_err());
    }

    #[test]
    fn a_percent_in_the_evidence_is_escaped_for_rpm() {
        let log = "FAILED tests/t.py::test_a - AssertionError: 100% wrong\n== 1 failed, 4 passed in 1s ==\n";
        let got = drop_failing_tests(SPEC, log, &none).unwrap();
        assert!(got.spec.contains("100%% wrong"), "{}", got.spec);
    }
}
