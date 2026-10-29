use super::*;

/// The flags of a change from `baseline` (the spec OBS holds; `None` for a
/// package OBS does not hold yet) to `fin` (the spec to be committed).
///
/// Only lines the change added are read for skipped tests, so a test that was
/// already skipped upstream is not a flag. Comments are not read: the
/// `# evidence:` lines quote the log. `CheckRemoved` and `WheelSwitch` need a
/// baseline to compare with.
pub fn from_spec_diff(baseline: Option<&str>, fin: &str) -> Vec<FixFlag> {
    let base: Vec<&str> = baseline.unwrap_or("").lines().collect();
    let fin: Vec<&str> = fin.lines().collect();
    let added = extra(&fin, &base);
    let removed: Vec<&str> = extra(&base, &fin).into_iter().map(|i| base[i]).collect();
    let mut out = Vec::new();
    if baseline.is_some() {
        if commands(&base, "%check") > 0 && commands(&fin, "%check") == 0 {
            out.push(FixFlag::CheckRemoved);
        }
        if let Some(wheel) = new_wheel(&base, &fin, &added) {
            out.push(FixFlag::WheelSwitch { wheel });
        }
    }
    let check = section_span(&fin, "%check");
    let prep = section_span(&fin, "%prep");
    let mut items: Vec<String> = Vec::new();
    for &i in &added {
        let place = Place {
            check: check.is_some_and(|(s, e)| (s..e).contains(&i)),
            prep: prep.is_some_and(|(s, e)| (s..e).contains(&i)),
        };
        for item in skipped_in(fin[i], place, &removed) {
            if !items.contains(&item) {
                items.push(item);
            }
        }
    }
    if !items.is_empty() {
        out.push(FixFlag::TestsSkipped { items });
    }
    out
}

/// Indices of the lines of `a` that `b` does not have, counting repeats.
pub(crate) fn extra(a: &[&str], b: &[&str]) -> Vec<usize> {
    let mut counts: HashMap<&str, usize> = HashMap::new();
    for l in b {
        *counts.entry(l.trim()).or_default() += 1;
    }
    a.iter()
        .enumerate()
        .filter_map(|(i, l)| match counts.get_mut(l.trim()) {
            Some(c) if *c > 0 => {
                *c -= 1;
                None
            }
            _ => Some(i),
        })
        .collect()
}

/// The body `[start, end)` of the section `name` (its header is `name`).
pub(super) fn section_span(lines: &[&str], name: &str) -> Option<(usize, usize)> {
    let header = lines
        .iter()
        .position(|l| l.split_whitespace().next() == Some(name))?;
    let end = lines[header + 1..]
        .iter()
        .position(|l| is_section(l))
        .map_or(lines.len(), |p| header + 1 + p);
    Some((header + 1, end))
}

/// Lines of `name`'s body that run something: not blank, comment, `%{nil}`,
/// conditional directives, a command whose failure is ignored (see
/// [`masked`]), or a line inside an `%if 0` block.
pub(super) fn commands(lines: &[&str], name: &str) -> usize {
    let Some((start, end)) = section_span(lines, name) else {
        return 0;
    };
    // One entry per open conditional. `dead`: the branch being read is known
    // not to run (its condition is `0`/`false`, or an earlier branch is known
    // to run). `done`: an earlier branch is known to run, so the later
    // `%elif`/`%else` branches do not. A condition that is neither is read as
    // live, as before.
    let mut open: Vec<Branch> = Vec::new();
    let mut live = 0;
    for line in &lines[start..end] {
        let t = line.trim();
        let word = t.split_whitespace().next().unwrap_or("");
        let cond = t.split_whitespace().skip(1).collect::<Vec<_>>().join(" ");
        let zero = matches!(cond.as_str(), "0" | "false");
        let known_true = matches!(cond.as_str(), "1" | "true");
        match word {
            "%if" | "%ifarch" | "%ifnarch" | "%ifos" | "%ifnos" => open.push(Branch {
                dead: zero,
                done: known_true,
            }),
            "%elif" | "%elifarch" | "%elifnarch" | "%elifos" | "%elifnos" => {
                if let Some(top) = open.last_mut() {
                    top.dead = top.done || zero;
                    top.done |= known_true;
                }
            }
            "%else" => {
                if let Some(top) = open.last_mut() {
                    top.dead = top.done;
                }
            }
            "%endif" => {
                open.pop();
            }
            _ => {
                let dead = open.iter().any(|b| b.dead);
                let runs =
                    !t.is_empty() && !t.starts_with('#') && t != "%{nil}" && t != ":" && !masked(t);
                if runs && !dead {
                    live += 1;
                }
            }
        }
    }
    live
}

/// One open conditional of [`commands`] (see there).
pub(super) struct Branch {
    dead: bool,
    done: bool,
}

/// A command whose failure is ignored: `... || true`, `|| :`, `|| exit 0`.
pub(super) fn masked(line: &str) -> bool {
    let squashed: String = line
        .trim()
        .trim_end_matches(';')
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect();
    ["||true", "||:", "||exit0", "||/bin/true", "||/usr/bin/true"]
        .iter()
        .any(|m| squashed.ends_with(m))
}

/// The wheel file a new `Source` line of `fin` names: the first, when `base`
/// names none, or the first one beyond those `base` names.
pub(super) fn new_wheel(base: &[&str], fin: &[&str], added: &[usize]) -> Option<String> {
    let had = base.iter().filter(|l| is_source_wheel(l)).count();
    let has = fin.iter().filter(|l| is_source_wheel(l)).count();
    if had > 0 && has <= had {
        return None;
    }
    added
        .iter()
        .map(|&i| fin[i])
        .find(|l| is_source_wheel(l))
        .and_then(|l| l.split_whitespace().last())
        .map(|f| f.rsplit('/').next().unwrap_or(f).to_string())
}

pub(super) fn is_source_wheel(line: &str) -> bool {
    crate::spec::parse_source_line(line, &["source"], str::to_string).is_some()
        && line.contains(".whl")
}

/// Where a line of the spec is: inside `%check`, inside `%prep`.
#[derive(Clone, Copy)]
pub(super) struct Place {
    check: bool,
    prep: bool,
}

/// The test-skipping items one added line of the spec carries.
pub(super) fn skipped_in(line: &str, place: Place, removed: &[&str]) -> Vec<String> {
    let t = line.trim();
    if t.is_empty() || t.starts_with('#') {
        return Vec::new();
    }
    let testy = place.check || t.to_ascii_lowercase().contains("test");
    let mut out = Vec::new();
    for opt in ["--deselect", "--ignore", "--ignore-glob"] {
        for value in option_values(t, opt) {
            out.push(format!("{opt} {value}"));
        }
    }
    if testy {
        // Any keyword or marker selection is flagged, positive or negative: a
        // selection that picks fewer tests drops the others just the same.
        for opt in ["-k", "-m"] {
            for value in option_values(t, opt) {
                out.push(format!("{opt} \"{value}\""));
            }
        }
        if let Some(file) = single_test_file(t) {
            out.push(format!("only {file}"));
        }
    }
    if t.contains("ctest") {
        for opt in ["-E", "--exclude-regex"] {
            for value in option_values(t, opt) {
                out.push(format!("ctest {opt} {value}"));
            }
        }
    }
    if testy && masked(t) {
        out.push(one_line(t));
    }
    if let Some((name, false)) = bcond(t)
        && is_testish(name)
        && removed.iter().any(|r| bcond(r) == Some((name, true)))
    {
        out.push(one_line(t));
    }
    if let Some((name, value)) = global_def(t)
        && is_testish(name)
        && ((name.starts_with("_without_") && value == "1")
            || (name.starts_with("_with_") && value == "0"))
    {
        out.push(one_line(t));
    }
    const GUARDS: [&str; 6] = [
        "%{?_with",
        "%{!?_with",
        "%{?with_",
        "%{!?with_",
        "%{?_without",
        "%{!?_without",
    ];
    if place.check && (GUARDS.iter().any(|g| t.contains(g)) || conditional_skip(t)) {
        out.push(one_line(t));
    }
    if place.prep && removes_tests(t) {
        out.push(one_line(t));
    }
    out
}

pub(super) fn is_testish(name: &str) -> bool {
    name.contains("test") || name.contains("check")
}

/// The name a `%bcond_with NAME` (or `%bcond NAME 0`) line turns off, with
/// `false`; a `%bcond_without NAME` (or `%bcond NAME 1`) turns on, with `true`.
pub(super) fn bcond(line: &str) -> Option<(&str, bool)> {
    let words: Vec<&str> = line.split_whitespace().collect();
    match words.as_slice() {
        ["%bcond_with", name, ..] => Some((name, false)),
        ["%bcond_without", name, ..] => Some((name, true)),
        ["%bcond", name, "0"] => Some((name, false)),
        ["%bcond", name, "1"] => Some((name, true)),
        _ => None,
    }
}

/// The name and value of a `%global` or `%define` line.
pub(super) fn global_def(line: &str) -> Option<(&str, &str)> {
    let words: Vec<&str> = line.split_whitespace().collect();
    match words.as_slice() {
        ["%global" | "%define", name, value] => Some((name, value)),
        _ => None,
    }
}

/// A conditional that skips what it guards: `%if 0`, `%if false`, or one on a
/// `with`/`without` switch (`%if %{with tests}`).
pub(super) fn conditional_skip(line: &str) -> bool {
    let words: Vec<&str> = line.split_whitespace().collect();
    match words.split_first() {
        Some((first, rest)) if matches!(*first, "%if" | "%elif") => {
            let cond = rest.join(" ");
            matches!(cond.as_str(), "0" | "false") || cond.contains("with")
        }
        _ => false,
    }
}

/// Whether a line removes a test file or directory: `rm -rf tests`,
/// `rm tests/test_x.py`.
pub(super) fn removes_tests(line: &str) -> bool {
    let words: Vec<&str> = line.split_whitespace().collect();
    let rm = words.iter().any(|w| {
        let w = w.trim_matches(|c| matches!(c, '%' | '{' | '}'));
        w == "rm" || w == "__rm"
    });
    rm && words.iter().any(|w| {
        let p = w
            .trim_matches(|c| matches!(c, '"' | '\''))
            .trim_end_matches('/');
        p.split('/').any(|seg| seg == "test" || seg == "tests")
            || (p.rsplit('/').next().is_some_and(|b| b.starts_with("test_")) && p.ends_with(".py"))
    })
}

/// The one test file or node id a pytest line runs by itself, as a positional
/// argument (not the value of an option such as `--deselect`).
pub(super) fn single_test_file(line: &str) -> Option<String> {
    const VALUED: &[&str] = &[
        "-k",
        "-m",
        "-p",
        "-c",
        "-W",
        "-E",
        "--deselect",
        "--ignore",
        "--ignore-glob",
        "--rootdir",
        "--maxfail",
        "--junitxml",
    ];
    let words: Vec<&str> = line.split_whitespace().collect();
    let at = words.iter().position(|w| w.contains("pytest"))?;
    let mut found = Vec::new();
    let mut skip = false;
    for w in &words[at + 1..] {
        if std::mem::take(&mut skip) {
            continue;
        }
        if matches!(*w, "&&" | "||" | ";" | "|") {
            break;
        }
        if VALUED.contains(w) {
            skip = true;
        } else if !w.starts_with('-') && (w.ends_with(".py") || w.contains("::")) {
            found.push(w.to_string());
        }
    }
    (found.len() == 1).then(|| found.remove(0))
}

/// The values the option `opt` takes in `t`, as `opt value`, `opt=value` or
/// `opt 'quoted value'`, wherever it is a word of its own.
pub(super) fn option_values(t: &str, opt: &str) -> Vec<String> {
    let is_edge = |c: char| c.is_whitespace() || c == '"' || c == '\'';
    let mut out = Vec::new();
    for (pos, _) in t.match_indices(opt) {
        if !t[..pos].chars().last().is_none_or(is_edge) {
            continue;
        }
        let after = &t[pos + opt.len()..];
        let rest = match after.strip_prefix('=') {
            Some(r) => r,
            None if after.starts_with(is_edge) => after,
            None => continue,
        }
        .trim_start();
        let value = match rest.chars().next() {
            Some(q @ ('"' | '\'')) => rest[1..].split(q).next().unwrap_or(""),
            _ => rest.split(is_edge).next().unwrap_or(""),
        };
        let value = value.trim();
        if !value.is_empty() {
            out.push(value.to_string());
        }
    }
    out
}
