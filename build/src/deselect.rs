//! Dropping failing tests from `%check`, mechanically, with the log as evidence.
//!
//! When the package builds and installs and only a few individual tests fail in
//! `%check`, the fix is to skip exactly those tests and say why. This reads the
//! failing test ids from the build log (pytest `FAILED path::test` lines,
//! unittest `FAIL: test (mod.Class.test)` headers, ctest's "The following tests
//! FAILED" list) and edits the spec: a narrow `--deselect` through
//! `PYTEST_ADDOPTS`, an `-E` regex on the `%ctest` line, or a `skip` decorator
//! inserted in `%prep` for unittest modules found in the unpacked sdist. A
//! unittest module that is not there (an installed package's tests) is skipped
//! through the runner instead: `%check` is moved to pytest (`-m unittest` is
//! rewritten, pytest added to the BuildRequires) and the test is excluded by a
//! `-k` expression in `PYTEST_ADDOPTS`; a `%check` that runs tox, nox, make
//! and the like is refused with the exact reason. Every dropped test carries a
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
                && t.split_whitespace()
                    .nth(1)
                    .is_some_and(|f| !f.contains("::") && f.ends_with(".py")));
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
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "_./:[]-+,=@#()".contains(c))
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

fn unittest_failures(lines: &[String]) -> Vec<FailingTest> {
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
    let at = addopts_insertion_point(&lines, start, end)?;
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
    if line.contains(" -E ") || line.contains("--exclude-regex") || line.trim_end().ends_with('\\')
    {
        return Err(
            "the ctest line already has an exclude regex or continues on the next line".into(),
        );
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

/// Where in `%check` the export of our own `PYTEST_ADDOPTS` goes: after one that
/// assigns the variable outright.
fn addopts_insertion_point(lines: &[&str], start: usize, end: usize) -> Result<usize, String> {
    let mut at = start;
    for (i, l) in lines[start..end].iter().enumerate() {
        let t = l.trim_start();
        let assigns = t
            .strip_prefix("export ")
            .unwrap_or(t)
            .starts_with("PYTEST_ADDOPTS=");
        if assigns
            && !t.contains("PYTEST_ADDOPTS:-")
            && !t.contains("$PYTEST_ADDOPTS")
            && !t.contains("${PYTEST_ADDOPTS")
        {
            if t.ends_with('\\') {
                return Err("the %check sets PYTEST_ADDOPTS over several lines".into());
            }
            at = start + i + 1;
        }
    }
    Ok(at)
}

/// The start of the `-k` expression export this module writes, which the next
/// round extends (pytest keeps only the last `-k`).
const K_EXPORT_HEAD: &str = "export PYTEST_ADDOPTS=\"${PYTEST_ADDOPTS:-} -k '";
const K_EXPORT_TAIL: &str = "'\"";

/// `-m unittest` in a line of `%check`.
const UNITTEST_MODULE: &str = "-m unittest";

/// The runners of `%check` that offer no deselect interface this module can
/// drive from outside: they pass their own arguments to the tests.
const CUSTOM_RUNNERS: &[&str] = &[
    "tox",
    "nox",
    "make",
    "stestr",
    "nosetests",
    "nose2",
    "behave",
    "%meson_test",
    "%cmake_build",
    "pytest-xdist-runner",
];

fn is_pytest_line(t: &str) -> bool {
    t.split_whitespace().any(|w| {
        w == "%pytest"
            || w == "pytest"
            || w.starts_with("%pytest_")
            || w.ends_with("/pytest")
            || w == "pytest3"
    }) || t.contains("-m pytest")
        || t.contains("%{python_expand") && t.contains("pytest")
}

fn is_comment(t: &str) -> bool {
    t.trim_start().starts_with('#')
}

/// `-m unittest <args>` rewritten as `-m pytest <args>`: dotted module names
/// become `--pyargs` ones, `discover -s DIR` becomes `DIR`, the flags that
/// have a pytest equivalent are mapped. `Err` names what has no equivalent.
fn unittest_line_as_pytest(line: &str) -> Result<String, String> {
    let pos = line.find(UNITTEST_MODULE).ok_or("no unittest invocation")?;
    let (prefix, rest) = (&line[..pos], &line[pos + UNITTEST_MODULE.len()..]);
    if !(rest.is_empty() || rest.starts_with(char::is_whitespace)) {
        return Err(format!(
            "the %check runs `{}`, which is not plain unittest",
            line.trim()
        ));
    }
    if rest.contains([
        '}', ';', '&', '|', '$', '"', '\'', '\\', '`', '(', ')', '<', '>',
    ]) {
        return Err(format!(
            "the unittest line `{}` has shell or macro syntax after `-m unittest` that cannot be \
             translated to pytest arguments",
            line.trim()
        ));
    }
    let mut args: Vec<String> = Vec::new();
    let mut toks = rest.split_whitespace();
    while let Some(tok) = toks.next() {
        match tok {
            "discover" => {}
            "-v" | "--verbose" => args.push("-v".into()),
            "-q" | "--quiet" => args.push("-q".into()),
            "-f" | "--failfast" => args.push("-x".into()),
            "-s" | "--start-directory" => {
                let dir = toks.next().ok_or("unittest `-s` without a directory")?;
                if dir.starts_with('/') || dir.split('/').any(|c| c == "..") {
                    return Err(format!(
                        "unittest start directory `{dir}` is not a plain relative path"
                    ));
                }
                args.push(dir.to_string());
            }
            t if !t.starts_with('-')
                && t.split('.').all(|p| {
                    !p.is_empty() && p.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
                }) =>
            {
                args.push("--pyargs".into());
                args.push(t.to_string());
            }
            other => {
                return Err(format!(
                    "the unittest option `{other}` has no pytest equivalent, so the invocation \
                     cannot be moved to pytest"
                ));
            }
        }
    }
    let mut out = format!("{prefix}-m pytest");
    for a in args {
        out.push(' ');
        out.push_str(&a);
    }
    Ok(out)
}

/// The `BuildRequires` line that adds pytest, in the spec's own style.
fn pytest_buildrequires(preamble: &[&str]) -> Result<Option<String>, String> {
    let brs: Vec<&&str> = preamble
        .iter()
        .filter(|l| {
            l.trim_start()
                .to_ascii_lowercase()
                .starts_with("buildrequires:")
        })
        .collect();
    if brs
        .iter()
        .any(|l| l.to_ascii_lowercase().contains("pytest"))
    {
        return Ok(None);
    }
    if brs.iter().any(|l| l.contains("%{python_module")) {
        return Ok(Some("BuildRequires:  %{python_module pytest}".into()));
    }
    if brs.iter().any(|l| l.contains("python3-")) {
        return Ok(Some("BuildRequires:  python3-pytest".into()));
    }
    Err(
        "pytest is not a BuildRequires and the spec's BuildRequires use neither \
         `%{python_module ...}` nor `python3-...`, so its spelling here is unknown"
            .into(),
    )
}

/// Skip installed-module unittest tests through the runner: `%check` is moved to
/// (or already is) pytest and the tests are excluded with one `-k` expression,
/// `not (module and Class and test)` for each, from `PYTEST_ADDOPTS`. `-k`
/// matches names as substrings, so a test whose name continues with more
/// characters is dropped with it; the comment says so. `Err` is exactly why the
/// `%check` cannot take it.
fn apply_unittest_through_pytest(spec: &str, tests: &[&FailingTest]) -> Result<String, String> {
    let mut lines: Vec<String> = spec.lines().map(str::to_string).collect();
    let view: Vec<&str> = lines.iter().map(String::as_str).collect();
    let (start, end) =
        section_body(&view, "%check").ok_or("the spec has no %check section to edit")?;
    let body: Vec<&str> = view[start..end]
        .iter()
        .copied()
        .filter(|l| !is_comment(l))
        .collect();
    let unittest_lines: Vec<usize> = (start..end)
        .filter(|&i| !is_comment(view[i]) && view[i].contains(UNITTEST_MODULE))
        .collect();
    let has_pytest = body.iter().any(|l| is_pytest_line(l));
    if unittest_lines.is_empty() && !has_pytest {
        let custom = body.iter().find_map(|l| {
            l.split_whitespace()
                .find(|w| CUSTOM_RUNNERS.contains(w))
                .map(|w| (w.to_string(), l.trim().to_string()))
        });
        return Err(match custom {
            Some((runner, line)) => format!(
                "the %check runs its tests through `{runner}` (`{line}`), which takes no deselect \
                 from here: the tests are not in the unpacked source (installed package tests) \
                 and the runner has no PYTEST_ADDOPTS-style hook auto-package can rely on, so \
                 they have to be skipped through that runner's own configuration"
            ),
            None => "the %check has no unittest or pytest invocation to deselect through".into(),
        });
    }
    // Another `-k` of the check would override ours (the last one wins).
    let ours = |l: &str| l.trim_start().starts_with(K_EXPORT_HEAD);
    if body.iter().any(|l| {
        !ours(l)
            && l.split_whitespace()
                .any(|w| w == "-k" || w.starts_with("-k=") || w.starts_with("--k"))
    }) {
        return Err(
            "the %check already selects tests with its own `-k`, which would override or be \
             overridden by a second `-k` expression"
                .into(),
        );
    }
    // Rewrite the unittest invocations (before the section moves).
    let mut rewritten: Vec<(usize, String)> = Vec::new();
    for &i in &unittest_lines {
        rewritten.push((i, unittest_line_as_pytest(view[i])?));
    }
    // Where pytest must be a BuildRequires.
    let first_section = view
        .iter()
        .position(|l| is_section(l))
        .unwrap_or(view.len());
    let new_br = pytest_buildrequires(&view[..first_section])?;
    let br_at = view[..first_section]
        .iter()
        .rposition(|l| {
            l.trim_start()
                .to_ascii_lowercase()
                .starts_with("buildrequires:")
        })
        .map(|i| i + 1);
    if new_br.is_some() && br_at.is_none() {
        return Err("the spec has no BuildRequires line to add pytest after".into());
    }
    let insert_at = addopts_insertion_point(&view, start, end)?;
    let existing = (start..end).find(|&i| ours(view[i]));
    let pieces: Vec<String> = tests
        .iter()
        .map(|t| {
            let parts: Vec<&str> = t.id.split('.').collect();
            format!(
                "not ({} and {} and {})",
                parts[parts.len() - 3],
                parts[parts.len() - 2],
                parts[parts.len() - 1]
            )
        })
        .collect();
    let mut comments: Vec<String> = Vec::new();
    for t in tests {
        comments.push(evidence_comment(&t.line));
        comments.push(format!(
            "# auto-package dropped {} (installed module: skipped through pytest -k, which matches names as substrings)",
            t.id
        ));
    }
    // Rewrites first: they replace lines in place, then the export is spliced.
    for (i, new) in &rewritten {
        lines[*i] = new.clone();
    }
    let (k_at, k_new): (usize, Vec<String>) = match existing {
        Some(i) => {
            let line = lines[i].trim_end();
            let head = line.find(K_EXPORT_HEAD).unwrap_or(0) + K_EXPORT_HEAD.len();
            let expr = line[head..]
                .strip_suffix(K_EXPORT_TAIL)
                .ok_or("the earlier -k export was edited, so it cannot be extended")?;
            let merged = format!(
                "{}{expr} and {}{K_EXPORT_TAIL}",
                &line[..head],
                pieces.join(" and ")
            );
            let mut v = comments.clone();
            v.push(merged);
            lines.remove(i);
            (i, v)
        }
        None => {
            let mut v = comments.clone();
            v.push(format!(
                "{K_EXPORT_HEAD}{}{K_EXPORT_TAIL}",
                pieces.join(" and ")
            ));
            (insert_at, v)
        }
    };
    for (off, l) in k_new.into_iter().enumerate() {
        lines.insert(k_at + off, l);
    }
    if let (Some(br), Some(at)) = (new_br, br_at) {
        lines.insert(at, br);
    }
    Ok(join(&lines, spec.ends_with('\n')))
}

/// Unittest failures: modules in the unpacked source get a skip decorator in
/// `%prep`; installed-package tests (not in the source) are excluded through
/// the runner, see [`apply_unittest_through_pytest`]. Returns the spec and the
/// ids that were dropped; `Err` when none could be.
fn apply_unittest(
    spec: &str,
    tests: &[&FailingTest],
    source_has: &dyn Fn(&str) -> bool,
) -> Result<(String, Vec<String>), String> {
    let lines: Vec<&str> = spec.lines().collect();
    let mut add: Vec<String> = Vec::new();
    let mut done: Vec<String> = Vec::new();
    let mut installed: Vec<&FailingTest> = Vec::new();
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
            installed.push(t);
            continue;
        };
        add.push(evidence_comment(&t.line));
        add.push(format!(
            "sed -i -E '/^class {class}\\b/,/^class /{{s/^([[:space:]]+)def {test}\\(/\\1@__import__(\"unittest\").skip(\"auto-package dropped {id}\")\\n\\1def {test}(/}}' {file}",
            id = t.id
        ));
        done.push(t.id.clone());
    }
    let mut out = spec.to_string();
    if !add.is_empty() {
        let (start, end) = section_body(&lines, "%prep").ok_or("the spec has no %prep section")?;
        let mut at = end;
        while at > start && lines[at - 1].trim().is_empty() {
            at -= 1;
        }
        out = splice(&lines, at, &add, spec.ends_with('\n'));
    }
    if !installed.is_empty() {
        match apply_unittest_through_pytest(&out, &installed) {
            Ok(s) => {
                out = s;
                done.extend(installed.iter().map(|t| t.id.clone()));
            }
            Err(why) if done.is_empty() => {
                return Err(format!(
                    "the failing unittest modules are not in the unpacked source, and they cannot \
                     be skipped through the runner either: {why}"
                ));
            }
            // The tests found in the source were dropped; the rest stay for the ladder.
            Err(_) => {}
        }
    }
    Ok((out, done))
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
    let (fresh, repeated): (Vec<FailingTest>, Vec<FailingTest>) =
        failing.into_iter().partition(|t| {
            !already
                .iter()
                .any(|a| *a == t.id || t.id.starts_with(&format!("{a}[")))
        });
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
    let mut chosen: Vec<&FailingTest> = chosen.into_iter().filter(|t| t.runner == runner).collect();
    let spec_out = match runner {
        Runner::Pytest => apply_pytest(spec, &chosen)?,
        Runner::Ctest => apply_ctest(spec, &chosen)?,
        Runner::Unittest => {
            let (s, ids) = apply_unittest(spec, &chosen, source_has)?;
            chosen.retain(|t| ids.contains(&t.id));
            s
        }
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
        let log = pytest_log(
            &[
                "tests/test_a.py::TestX::test_one",
                "tests/test_b.py::test_two[a-b]",
            ],
            40,
        );
        let got = drop_failing_tests(SPEC, &log, &none).unwrap();
        assert_eq!(got.tests.len(), 2);
        assert_eq!(got.omitted, 0);
        assert!(
            got.spec.contains(
                "# evidence: FAILED tests/test_a.py::TestX::test_one - AssertionError: boom"
            ),
            "{}",
            got.spec
        );
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
        let err =
            drop_failing_tests(SPEC, &pytest_log(&["tests/test_a.py"], 5), &none).unwrap_err();
        assert!(err.contains("no individual failing test"), "{err}");
    }

    #[test]
    fn import_and_collection_errors_are_a_dependency_problem() {
        let collection = "[  9s] ERROR collecting tests/test_a.py\n[  9s] E   ModuleNotFoundError: No module named 'numpy'\n[  9s] ===== 1 error in 0.3s =====\n";
        let err = drop_failing_tests(SPEC, collection, &none).unwrap_err();
        assert!(err.contains("dependency problem"), "{err}");
        let summary = "FAILED tests/test_a.py::test_x - ImportError: cannot import name 'y' from 'z'\n== 1 failed, 9 passed in 1s ==\n";
        assert!(
            drop_failing_tests(SPEC, summary, &none)
                .unwrap_err()
                .contains("dependency problem")
        );
        let in_test = "E   ModuleNotFoundError: No module named 'torch'\nFAILED tests/test_a.py::test_x - assert 0\n== 1 failed, 9 passed in 1s ==\n";
        assert!(drop_failing_tests(SPEC, in_test, &none).is_err());
        let unittest = "ERROR: tests.test_a (unittest.loader._FailedTest.tests.test_a)\n";
        assert!(
            drop_failing_tests(SPEC, unittest, &none)
                .unwrap_err()
                .contains("dependency problem")
        );
        let broken = "ERROR tests/test_a.py - SyntaxError: bad\n== 1 error in 0.1s ==\n";
        assert!(drop_failing_tests(SPEC, broken, &none).is_err());
    }

    #[test]
    fn a_suite_in_which_nothing_passes_is_not_dropped() {
        let err =
            drop_failing_tests(SPEC, &pytest_log(&["tests/t.py::test_a"], 0), &none).unwrap_err();
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
        assert!(
            drop_failing_tests(multiline, &pytest_log(&["tests/t.py::test_a"], 5), &none).is_err()
        );
    }

    #[test]
    fn ctest_failures_narrow_the_ctest_line() {
        let log = "[  50s] 90% tests passed, 2 tests failed out of 20\n[  50s] \n[  50s] The following tests FAILED:\n[  50s] \t  3 - test_foo.bar (Failed)\n[  50s] \t  7 - test_baz (Timeout)\n[  50s] Errors while running CTest\n";
        let spec = "%check\n%ctest\n\n%files\n";
        let got = drop_failing_tests(spec, log, &none).unwrap();
        assert_eq!(got.tests.len(), 2);
        assert!(
            got.spec.contains("%ctest -E '^(test_foo\\.bar|test_baz)$'"),
            "{}",
            got.spec
        );
        assert!(
            got.spec.contains("# evidence: 3 - test_foo.bar (Failed)"),
            "{}",
            got.spec
        );
        // A test ctest could not run is a build problem.
        let not_run = "The following tests FAILED:\n\t  1 - test_a (Not Run)\n";
        assert!(
            drop_failing_tests(spec, not_run, &none)
                .unwrap_err()
                .contains("build problem")
        );
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
        assert!(
            got.spec.contains("/^class CodecTest\\b/,/^class /"),
            "{}",
            got.spec
        );
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

    const INSTALLED_LOG: &str = "[  20s] FAIL: test_roundtrip (pkg.tests.test_codec.CodecTest.test_roundtrip)\n[  20s] ERROR: test_other (pkg.tests.test_codec.CodecTest)\n";

    fn installed_spec(check: &str) -> String {
        format!(
            "Name: python-pkg\nBuildRequires:  %{{python_module pip}}\nBuildRequires:  python-rpm-macros\n\n%prep\n%autosetup\n\n%build\n%pyproject_wheel\n\n%check\n{check}\n\n%files\n"
        )
    }

    #[test]
    fn installed_module_unittest_failures_move_the_check_to_pytest_with_a_k_expression() {
        let spec = installed_spec("%python_exec -m unittest -v pkg.tests");
        let got = drop_failing_tests(&spec, INSTALLED_LOG, &none).unwrap();
        assert_eq!(got.tests.len(), 2);
        assert!(
            got.spec
                .contains("%python_exec -m pytest -v --pyargs pkg.tests"),
            "{}",
            got.spec
        );
        assert!(
            got.spec.contains("BuildRequires:  %{python_module pytest}"),
            "{}",
            got.spec
        );
        assert!(
            got.spec.contains(
                "# evidence: FAIL: test_roundtrip (pkg.tests.test_codec.CodecTest.test_roundtrip)"
            ),
            "{}",
            got.spec
        );
        assert!(got.spec.contains("-k 'not (test_codec and CodecTest and test_roundtrip) and not (test_codec and CodecTest and test_other)'"), "{}", got.spec);
        let check = got.spec.split("%check\n").nth(1).unwrap();
        assert!(check.find("-k 'not").unwrap() < check.find("-m pytest").unwrap());
        // The BuildRequires sits with the others, before %prep.
        assert!(got.spec.find("python_module pytest").unwrap() < got.spec.find("%prep").unwrap());
        assert_eq!(dropped_count(&got.spec), 2);
        // The same tests again are refused: the drop did not take effect.
        assert!(
            drop_failing_tests(&got.spec, INSTALLED_LOG, &none)
                .unwrap_err()
                .contains("already dropped")
        );
    }

    #[test]
    fn a_later_round_extends_the_single_k_expression() {
        let spec = installed_spec("%python_exec -m unittest discover -s pkg/tests");
        let first = drop_failing_tests(&spec, INSTALLED_LOG, &none).unwrap();
        assert!(first.spec.contains("-m pytest pkg/tests"), "{}", first.spec);
        let more = "FAIL: test_new (pkg.tests.test_io.IoTest.test_new)\n";
        let second = drop_failing_tests(&first.spec, more, &none).unwrap();
        assert_eq!(second.spec.matches("-k '").count(), 1, "{}", second.spec);
        assert!(
            second
                .spec
                .contains("and not (test_io and IoTest and test_new)'"),
            "{}",
            second.spec
        );
        assert!(second.spec.contains("test_roundtrip"), "{}", second.spec);
        assert_eq!(second.spec.matches("python_module pytest").count(), 1);
    }

    #[test]
    fn an_installed_unittest_failure_under_a_pytest_check_uses_pytest_addopts() {
        let spec = installed_spec("%pytest --pyargs pkg");
        let got = drop_failing_tests(&spec, INSTALLED_LOG, &none).unwrap();
        assert!(got.spec.contains("%pytest --pyargs pkg"), "{}", got.spec);
        assert!(
            got.spec
                .contains("export PYTEST_ADDOPTS=\"${PYTEST_ADDOPTS:-} -k 'not (test_codec"),
            "{}",
            got.spec
        );
        assert!(got.spec.contains("python_module pytest"), "{}", got.spec);
        // A spec that already requires pytest gets no second one.
        let with = spec.replace(
            "python-rpm-macros",
            "python-rpm-macros\nBuildRequires:  %{python_module pytest}",
        );
        let got = drop_failing_tests(&with, INSTALLED_LOG, &none).unwrap();
        assert_eq!(got.spec.matches("python_module pytest").count(), 1);
    }

    #[test]
    fn installed_unittest_failures_go_back_to_the_ladder_with_the_exact_reason() {
        for (check, needle) in [
            ("tox -e py3", "through `tox`"),
            ("make check", "through `make`"),
            ("echo nothing", "no unittest or pytest invocation"),
            ("%python_exec -m unittest -p x pkg", "no pytest equivalent"),
            (
                "%{python_expand $python -m unittest pkg}",
                "shell or macro syntax",
            ),
            ("%pytest -k 'not slow'", "own `-k`"),
        ] {
            let err = drop_failing_tests(&installed_spec(check), INSTALLED_LOG, &none).unwrap_err();
            assert!(err.contains(needle), "{check}: {err}");
            assert!(err.contains("not in the unpacked source"), "{check}: {err}");
        }
        // No spelling of pytest's BuildRequires known.
        let bare = "Name: x\n\n%prep\n%autosetup\n\n%check\n%python_exec -m unittest\n";
        assert!(
            drop_failing_tests(bare, INSTALLED_LOG, &none)
                .unwrap_err()
                .contains("BuildRequires")
        );
    }

    #[test]
    fn source_and_installed_modules_are_dropped_each_their_own_way_in_one_round() {
        let log = "FAIL: test_a (pkg.tests.test_in.A.test_a)\nFAIL: test_b (pkg.tests.test_out.B.test_b)\n";
        let spec = installed_spec("%python_exec -m unittest pkg.tests");
        let have = |p: &str| p == "pkg/tests/test_in.py";
        let got = drop_failing_tests(&spec, log, &have).unwrap();
        assert_eq!(got.tests.len(), 2);
        assert!(got.spec.contains("sed -i -E '/^class A"), "{}", got.spec);
        assert!(
            got.spec.contains("not (test_out and B and test_b)"),
            "{}",
            got.spec
        );
        // The runner cannot take it: only the source one is dropped, the other is omitted.
        let spec = installed_spec("tox");
        let got = drop_failing_tests(&spec, log, &have).unwrap();
        assert_eq!(got.tests.len(), 1);
        assert_eq!(got.omitted, 1);
    }
}
