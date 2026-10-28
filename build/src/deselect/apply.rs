use super::*;

/// Tests dropped by one call.
pub const MAX_PER_ROUND: usize = 5;

/// Tests the mechanical rung may have dropped from one spec over all rounds.
pub const MAX_IN_SPEC: usize = 15;

pub(super) const SECTIONS: &[&str] = &[
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

pub(crate) fn is_section(line: &str) -> bool {
    let first = line.split_whitespace().next().unwrap_or("");
    SECTIONS.contains(&first)
}

/// The line range `[start, end)` of a section's body, `start` after its header.
pub(super) fn section_body(lines: &[&str], name: &str) -> Option<(usize, usize)> {
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
pub(super) fn already_dropped(spec: &str) -> Vec<String> {
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

pub(super) fn evidence_comment(line: &str) -> String {
    let flat: String = line
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .take(200)
        .collect();
    format!("# evidence: {}", flat.replace('%', "%%").trim())
}

/// The pytest ids of `failing` that `t` is a strict prefix of. pytest's
/// `--deselect=<id>` drops every test whose node id starts with `<id>`, so
/// these would be dropped along with `t`. Non-pytest tests yield none.
pub(super) fn pytest_prefix_extensions<'a>(
    t: &FailingTest,
    failing: &'a [FailingTest],
) -> Vec<&'a str> {
    if t.runner != Runner::Pytest {
        return Vec::new();
    }
    failing
        .iter()
        .filter(|o| {
            o.runner == Runner::Pytest && o.id.len() > t.id.len() && o.id.starts_with(&t.id)
        })
        .map(|o| o.id.as_str())
        .collect()
}

pub(super) fn apply_pytest(spec: &str, tests: &[&FailingTest]) -> Result<String, String> {
    let lines: Vec<&str> = spec.lines().collect();
    let (start, end) =
        section_body(&lines, "%check").ok_or("the spec has no %check section to edit")?;
    let at = addopts_insertion_point(&lines, start, end)?;
    let mut add: Vec<String> = tests.iter().map(|t| evidence_comment(&t.line)).collect();
    add.push(
        "# auto-package: --deselect matches node ids by prefix, so each id also deselects the \
         tests whose ids start with it; the failing tests of this log that extend one are not \
         dropped with it"
            .to_string(),
    );
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

pub(super) fn apply_ctest(spec: &str, tests: &[&FailingTest]) -> Result<String, String> {
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
pub(super) fn addopts_insertion_point(
    lines: &[&str],
    start: usize,
    end: usize,
) -> Result<usize, String> {
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
pub(super) const K_EXPORT_HEAD: &str = "export PYTEST_ADDOPTS=\"${PYTEST_ADDOPTS:-} -k '";

pub(super) const K_EXPORT_TAIL: &str = "'\"";

pub(super) fn splice(lines: &[&str], at: usize, add: &[String], trailing_newline: bool) -> String {
    let mut out: Vec<String> = lines[..at].iter().map(|s| s.to_string()).collect();
    out.extend(add.iter().cloned());
    out.extend(lines[at..].iter().map(|s| s.to_string()));
    join(&out, trailing_newline)
}

pub(super) fn join(lines: &[String], trailing_newline: bool) -> String {
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
        failing.iter().cloned().partition(|t| {
            !already
                .iter()
                .any(|a| *a == t.id || t.id.starts_with(&format!("{a}[")))
        });
    // `--deselect` matches a node id by prefix: a failing test whose id another
    // failing test of this log extends would also deselect that test, unrecorded.
    let (fresh, prefixed): (Vec<FailingTest>, Vec<FailingTest>) = fresh
        .into_iter()
        .partition(|t| pytest_prefix_extensions(t, &failing).is_empty());
    if fresh.is_empty() {
        if let Some(t) = prefixed.first() {
            let by = pytest_prefix_extensions(t, &failing).join(", ");
            return Err(format!(
                "{} failing test(s) cannot be dropped exactly: pytest --deselect matches node ids \
                 by prefix, so deselecting {} would also deselect {by}, unrecorded",
                prefixed.len(),
                t.id
            ));
        }
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
    let omitted = fresh.len() + prefixed.len() - chosen.len();
    Ok(Dropped {
        spec: spec_out,
        tests: chosen.into_iter().cloned().collect(),
        omitted,
    })
}
