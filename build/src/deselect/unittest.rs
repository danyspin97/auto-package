use super::*;

/// `-m unittest` in a line of `%check`.
pub(super) const UNITTEST_MODULE: &str = "-m unittest";

/// The runners of `%check` that offer no deselect interface this module can
/// drive from outside: they pass their own arguments to the tests.
pub(super) const CUSTOM_RUNNERS: &[&str] = &[
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

pub(super) fn is_pytest_line(t: &str) -> bool {
    t.split_whitespace().any(|w| {
        w == "%pytest"
            || w == "pytest"
            || w.starts_with("%pytest_")
            || w.ends_with("/pytest")
            || w == "pytest3"
    }) || t.contains("-m pytest")
        || t.contains("%{python_expand") && t.contains("pytest")
}

pub(super) fn is_comment(t: &str) -> bool {
    t.trim_start().starts_with('#')
}

/// `-m unittest <args>` rewritten as `-m pytest <args>`: dotted module names
/// become `--pyargs` ones, `discover -s DIR` becomes `DIR`, the flags that
/// have a pytest equivalent are mapped. `Err` names what has no equivalent.
pub(super) fn unittest_line_as_pytest(line: &str) -> Result<String, String> {
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
pub(super) fn pytest_buildrequires(preamble: &[&str]) -> Result<Option<String>, String> {
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
pub(super) fn apply_unittest_through_pytest(
    spec: &str,
    tests: &[&FailingTest],
) -> Result<String, String> {
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
pub(super) fn apply_unittest(
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
