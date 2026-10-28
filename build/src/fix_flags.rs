//! What a package's fix loop changed that its readers must be told: tests
//! switched off, `%check` removed, an upstream wheel put in for a source build,
//! and a turn that answered `blocked`.
//!
//! The spec changes are read from the diff of the OBS baseline against the
//! final spec, so they do not depend on what the agent said it did. A `blocked`
//! answer is recorded by the pipeline when the turn gives it. The flags live in
//! the package's state (`PkgState::fix_flags`) and are rendered by the caller:
//! the `.changes` entry, the submit request and the end-of-run summary.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::deselect::is_section;
use crate::pipeline::PkgState;
use crate::workspace::Workspace;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum FixFlag {
    /// Tests the spec skips now, each one as the spec names it (`--deselect
    /// tests/a.py::t`, `-k "not slow"`, a `%{?with_...}` guard in `%check`).
    TestsSkipped { items: Vec<String> },
    /// `%check` was removed, or no test command is left in it.
    CheckRemoved,
    /// The package is built from the upstream wheel `wheel`, not from source.
    WheelSwitch { wheel: String },
    /// A fix turn answered `blocked`; `why` is the reason it gave.
    Blocked { why: String },
}

/// Longest a flag's text is kept on its one line.
const LINE_CAP: usize = 240;

impl FixFlag {
    /// The one line that says it. The changelog, the request and the summary
    /// all print this line, so the wording is the same everywhere.
    pub fn line(&self) -> String {
        let text = match self {
            FixFlag::TestsSkipped { items } => format!("FLAG: tests skipped: {}", items.join("; ")),
            FixFlag::CheckRemoved => {
                "FLAG: %check removed or left without a test: the tests do not run".to_string()
            }
            FixFlag::WheelSwitch { wheel } => {
                format!("FLAG: built from the upstream wheel {wheel}, not from source")
            }
            FixFlag::Blocked { why } => format!("FLAG: a fix turn answered blocked: {why}"),
        };
        one_line(&text)
    }
}

/// `text` on one line, its words single-spaced, cut to [`LINE_CAP`] characters.
pub(crate) fn one_line(text: &str) -> String {
    let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= LINE_CAP {
        return flat;
    }
    let cut: String = flat.chars().take(LINE_CAP).collect();
    format!("{}...", cut.trim_end())
}

/// Records a `blocked` answer of `run_id`.
pub fn add_blocked(st: &mut PkgState, run_id: &str, why: String) {
    add(st, FixFlag::Blocked { why });
    st.fix_flags_run_id = Some(run_id.to_string());
}

/// Adds `flag` unless it is already recorded. Tests skipped by an earlier
/// commit and by this one are one flag listing both.
pub fn add(st: &mut PkgState, flag: FixFlag) {
    if let FixFlag::TestsSkipped { items } = &flag
        && let Some(FixFlag::TestsSkipped { items: have }) = st
            .fix_flags
            .iter_mut()
            .find(|f| matches!(f, FixFlag::TestsSkipped { .. }))
    {
        for item in items {
            if !have.contains(item) {
                have.push(item.clone());
            }
        }
        return;
    }
    if !st.fix_flags.contains(&flag) {
        st.fix_flags.push(flag);
    }
}

/// Adds the spec-derived flags `found` of a commit of `run_id`. Earlier flags
/// stay: a commit is compared with the OBS head, which already holds the
/// earlier commits' changes, so their flags would otherwise be lost. They are
/// dropped once the package is submitted (see [`clear_submitted`]).
pub fn settle(st: &mut PkgState, run_id: &str, found: Vec<FixFlag>) {
    for flag in found {
        add(st, flag);
    }
    st.fix_flags_run_id = Some(run_id.to_string());
}

/// The flags the package's last run recorded, for a reader outside the run.
/// Empty when they cannot be read; a reader that must not act on that uses
/// [`try_of_package`].
pub fn of_package(state_root: &std::path::Path, pkg: &str) -> Vec<FixFlag> {
    try_of_package(state_root, pkg).unwrap_or_default()
}

/// The flags of the package OBS calls `pkg`, or why they cannot be read: no
/// workspace for it, or a state file that is missing or corrupt.
pub fn try_of_package(state_root: &std::path::Path, pkg: &str) -> std::io::Result<Vec<FixFlag>> {
    let ws = Workspace::find_by_obs_name(state_root, pkg).ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!("no build state for {pkg} in {}", state_root.display()),
        )
    })?;
    Ok(PkgState::read_strict(&ws)?.fix_flags)
}

/// Drops the flags of `pkg` once its change is submitted: the request text
/// carries them from then on. The workspace is the one [`try_of_package`]
/// reads.
pub fn clear_submitted(state_root: &std::path::Path, pkg: &str) -> std::io::Result<()> {
    let Some(ws) = Workspace::find_by_obs_name(state_root, pkg) else {
        return Ok(());
    };
    PkgState::update(&ws, |st| st.fix_flags.clear()).map(|_| ())
}

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
fn section_span(lines: &[&str], name: &str) -> Option<(usize, usize)> {
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
fn commands(lines: &[&str], name: &str) -> usize {
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
struct Branch {
    dead: bool,
    done: bool,
}

/// A command whose failure is ignored: `... || true`, `|| :`, `|| exit 0`.
fn masked(line: &str) -> bool {
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
fn new_wheel(base: &[&str], fin: &[&str], added: &[usize]) -> Option<String> {
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

fn is_source_wheel(line: &str) -> bool {
    crate::spec::parse_source_line(line, &["source"], str::to_string).is_some()
        && line.contains(".whl")
}

/// Where a line of the spec is: inside `%check`, inside `%prep`.
#[derive(Clone, Copy)]
struct Place {
    check: bool,
    prep: bool,
}

/// The test-skipping items one added line of the spec carries.
fn skipped_in(line: &str, place: Place, removed: &[&str]) -> Vec<String> {
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

fn is_testish(name: &str) -> bool {
    name.contains("test") || name.contains("check")
}

/// The name a `%bcond_with NAME` (or `%bcond NAME 0`) line turns off, with
/// `false`; a `%bcond_without NAME` (or `%bcond NAME 1`) turns on, with `true`.
fn bcond(line: &str) -> Option<(&str, bool)> {
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
fn global_def(line: &str) -> Option<(&str, &str)> {
    let words: Vec<&str> = line.split_whitespace().collect();
    match words.as_slice() {
        ["%global" | "%define", name, value] => Some((name, value)),
        _ => None,
    }
}

/// A conditional that skips what it guards: `%if 0`, `%if false`, or one on a
/// `with`/`without` switch (`%if %{with tests}`).
fn conditional_skip(line: &str) -> bool {
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
fn removes_tests(line: &str) -> bool {
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
fn single_test_file(line: &str) -> Option<String> {
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
fn option_values(t: &str, opt: &str) -> Vec<String> {
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

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn flags_accumulate_until_the_package_is_submitted() {
        let mut st = PkgState::default();
        settle(&mut st, "r1", vec![FixFlag::CheckRemoved]);
        // A run that makes no commit (AlreadyBuilt, missing_deps) and a later
        // commit that finds nothing new keep what the earlier commit found.
        settle(&mut st, "r2", vec![]);
        assert_eq!(st.fix_flags, vec![FixFlag::CheckRemoved]);
        settle(
            &mut st,
            "r3",
            vec![FixFlag::TestsSkipped {
                items: vec!["a".into()],
            }],
        );
        settle(
            &mut st,
            "r4",
            vec![FixFlag::TestsSkipped {
                items: vec!["a".into(), "b".into()],
            }],
        );
        assert_eq!(
            st.fix_flags,
            vec![
                FixFlag::CheckRemoved,
                FixFlag::TestsSkipped {
                    items: vec!["a".into(), "b".into()]
                }
            ]
        );
    }

    #[test]
    fn a_blocked_answer_survives_later_commits() {
        let mut st = PkgState::default();
        settle(
            &mut st,
            "r1",
            vec![FixFlag::WheelSwitch { wheel: "w".into() }],
        );
        add_blocked(&mut st, "r2", "no tests".into());
        settle(&mut st, "r2", vec![FixFlag::CheckRemoved]);
        settle(&mut st, "r3", vec![]);
        assert_eq!(st.fix_flags.len(), 3, "{:?}", st.fix_flags);
        assert!(st.fix_flags.contains(&FixFlag::Blocked {
            why: "no tests".into()
        }));
    }

    #[test]
    fn submitting_a_package_clears_its_flags() {
        let root = tempfile::tempdir().unwrap();
        let ws = Workspace::open(root.path(), "python-foo").unwrap();
        PkgState::update(&ws, |st| settle(st, "r1", vec![FixFlag::CheckRemoved])).unwrap();
        clear_submitted(root.path(), "python-foo").unwrap();
        assert!(of_package(root.path(), "python-foo").is_empty());
    }

    #[test]
    fn submitting_clears_the_workspace_the_gate_reads() {
        // The workspace is named for the RPM name; the OBS spelling is found
        // through the state's `obs_name`, as the gate finds it.
        let root = tempfile::tempdir().unwrap();
        let ws = Workspace::open(root.path(), "python-apscheduler").unwrap();
        PkgState::update(&ws, |st| {
            st.obs_name = Some("python-APScheduler".into());
            settle(st, "r1", vec![FixFlag::CheckRemoved]);
        })
        .unwrap();
        assert_eq!(
            try_of_package(root.path(), "python-APScheduler")
                .unwrap()
                .len(),
            1
        );
        clear_submitted(root.path(), "python-APScheduler").unwrap();
        assert!(
            try_of_package(root.path(), "python-APScheduler")
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn commands_follow_elif_and_else_chains() {
        let count = |body: &str| {
            let spec = format!("%check\n{body}%files\n");
            let lines: Vec<&str> = spec.lines().collect();
            commands(&lines, "%check")
        };
        // The `%elif 1` branch runs; the `%if 0` one does not.
        assert_eq!(count("%if 0\n%pytest\n%elif 1\npytest\n%endif\n"), 1);
        // The `%if 1` branch runs, so the `%elif` and `%else` ones do not.
        assert_eq!(count("%if 1\npytest\n%elif 0\nx\n%else\ny\n%endif\n"), 1);
        // An unknown `%elif` may run, and so may the `%else`.
        assert_eq!(count("%if 0\na\n%elif foo\nb\n%else\nc\n%endif\n"), 2);
    }

    const BASE: &str = "Name: python-foo\nVersion: 1.0\nSource0: foo-1.0.tar.gz\n\
        %prep\n%setup -q\n%build\n%py3_build\n\
        %check\n%pytest\n\
        %install\n%py3_install\n%files\n%{python3_sitelib}/foo\n";

    fn with(replace: &str, with: &str) -> String {
        BASE.replace(replace, with)
    }

    #[test]
    fn an_unchanged_spec_has_no_flags() {
        assert!(from_spec_diff(Some(BASE), BASE).is_empty());
    }

    #[test]
    fn a_deselected_test_in_check_is_flagged_by_its_id() {
        let fin = with(
            "%check\n%pytest\n",
            "%check\n# evidence: FAILED tests/test_a.py::test_x\n\
             %pytest --deselect tests/test_a.py::test_x\n",
        );
        assert_eq!(
            from_spec_diff(Some(BASE), &fin),
            vec![FixFlag::TestsSkipped {
                items: vec!["--deselect tests/test_a.py::test_x".into()]
            }]
        );
    }

    #[test]
    fn a_comment_quoting_a_log_is_not_a_skipped_test() {
        let fin = with(
            "%check\n%pytest\n",
            "%check\n# evidence: FAILED t.py -k not --deselect t.py::x\n%pytest\n",
        );
        assert!(from_spec_diff(Some(BASE), &fin).is_empty());
    }

    #[test]
    fn a_keyword_expression_with_not_is_flagged_quoted() {
        let fin = with(
            "%check\n%pytest\n",
            "%check\nPYTEST_ADDOPTS=\"-k 'not test_slow and not test_gpu'\" %pytest\n",
        );
        assert_eq!(
            from_spec_diff(Some(BASE), &fin),
            vec![FixFlag::TestsSkipped {
                items: vec!["-k \"not test_slow and not test_gpu\"".into()]
            }]
        );
    }

    #[test]
    fn an_ignored_file_is_named() {
        let fin = with(
            "%pytest\n%install",
            "%pytest --ignore=tests/test_net.py\n%install",
        );
        assert_eq!(
            from_spec_diff(Some(BASE), &fin),
            vec![FixFlag::TestsSkipped {
                items: vec!["--ignore tests/test_net.py".into()]
            }]
        );
    }

    #[test]
    fn a_test_skip_is_flagged_without_a_baseline_too() {
        let fin = "Name: python-foo\n%check\n%pytest --deselect t.py::a\n";
        assert_eq!(
            from_spec_diff(None, fin),
            vec![FixFlag::TestsSkipped {
                items: vec!["--deselect t.py::a".into()]
            }]
        );
    }

    #[test]
    fn a_removed_check_is_flagged() {
        let fin = BASE.replace("%check\n%pytest\n", "");
        assert_eq!(
            from_spec_diff(Some(BASE), &fin),
            vec![FixFlag::CheckRemoved]
        );
    }

    #[test]
    fn an_emptied_check_is_flagged() {
        let fin = BASE.replace("%check\n%pytest\n", "%check\n# nothing to run\n");
        assert_eq!(
            from_spec_diff(Some(BASE), &fin),
            vec![FixFlag::CheckRemoved]
        );
    }

    #[test]
    fn a_test_command_with_its_failure_ignored_is_flagged_and_is_no_command() {
        let fin = with("%check\n%pytest\n", "%check\n%pytest || true\n");
        assert_eq!(
            from_spec_diff(Some(BASE), &fin),
            vec![
                FixFlag::CheckRemoved,
                FixFlag::TestsSkipped {
                    items: vec!["%pytest || true".into()]
                }
            ]
        );
        let fin = with("%check\n%pytest\n", "%check\n%pytest ||:;\n");
        assert!(from_spec_diff(Some(BASE), &fin).contains(&FixFlag::CheckRemoved));
    }

    #[test]
    fn a_check_behind_a_false_conditional_is_removed() {
        let fin = with("%check\n%pytest\n", "%check\n%if 0\n%pytest\n%endif\n");
        assert_eq!(
            from_spec_diff(Some(BASE), &fin),
            vec![
                FixFlag::CheckRemoved,
                FixFlag::TestsSkipped {
                    items: vec!["%if 0".into()]
                }
            ]
        );
    }

    #[test]
    fn a_check_behind_a_with_switch_is_flagged_by_its_condition() {
        let fin = with(
            "%check\n%pytest\n",
            "%check\n%if %{with tests}\n%pytest\n%endif\n",
        );
        assert_eq!(
            from_spec_diff(Some(BASE), &fin),
            vec![FixFlag::TestsSkipped {
                items: vec!["%if %{with tests}".into()]
            }]
        );
    }

    #[test]
    fn a_new_syntax_bcond_switched_off_is_flagged() {
        let base = format!("%bcond_without tests\n{BASE}");
        let fin = format!("%bcond tests 0\n{BASE}");
        assert_eq!(
            from_spec_diff(Some(&base), &fin),
            vec![FixFlag::TestsSkipped {
                items: vec!["%bcond tests 0".into()]
            }]
        );
    }

    #[test]
    fn a_global_that_switches_the_tests_off_is_flagged() {
        let fin = format!("%global _without_tests 1\n{BASE}");
        assert_eq!(
            from_spec_diff(Some(BASE), &fin),
            vec![FixFlag::TestsSkipped {
                items: vec!["%global _without_tests 1".into()]
            }]
        );
    }

    #[test]
    fn a_keyword_or_marker_selection_is_flagged_positive_or_negative() {
        for (line, item) in [
            ("%pytest -k \"(not slow)\"", "-k \"(not slow)\""),
            ("%pytest -k test_fast", "-k \"test_fast\""),
            ("%pytest -m smoke", "-m \"smoke\""),
            ("%pytest -k\"not slow\"", "-k \"not slow\""),
        ] {
            let fin = with("%check\n%pytest\n", &format!("%check\n{line}\n"));
            assert_eq!(
                from_spec_diff(Some(BASE), &fin),
                vec![FixFlag::TestsSkipped {
                    items: vec![item.into()]
                }],
                "{line}"
            );
        }
    }

    #[test]
    fn a_pytest_line_running_one_file_is_flagged_and_its_option_values_are_not() {
        let fin = with("%check\n%pytest\n", "%check\n%pytest tests/test_a.py\n");
        assert_eq!(
            from_spec_diff(Some(BASE), &fin),
            vec![FixFlag::TestsSkipped {
                items: vec!["only tests/test_a.py".into()]
            }]
        );
        // Two files, or a file as an option's value: not a single-file run.
        let two = with(
            "%check\n%pytest\n",
            "%check\n%pytest tests/a.py tests/b.py\n",
        );
        assert!(from_spec_diff(Some(BASE), &two).is_empty());
    }

    #[test]
    fn a_ctest_exclusion_is_flagged_under_either_spelling() {
        let fin = with("%check\n%pytest\n", "%check\nctest --exclude-regex slow_\n");
        assert_eq!(
            from_spec_diff(Some(BASE), &fin),
            vec![FixFlag::TestsSkipped {
                items: vec!["ctest --exclude-regex slow_".into()]
            }]
        );
    }

    #[test]
    fn a_test_directory_removed_in_prep_is_flagged() {
        let fin = with("%setup -q\n", "%setup -q\nrm -rf tests\n");
        assert_eq!(
            from_spec_diff(Some(BASE), &fin),
            vec![FixFlag::TestsSkipped {
                items: vec!["rm -rf tests".into()]
            }]
        );
    }

    #[test]
    fn a_second_wheel_beside_the_baseline_one_is_a_switch() {
        let base = format!("Source1: a-1.0-py3-none-any.whl\n{BASE}");
        let fin =
            format!("Source1: a-1.0-py3-none-any.whl\nSource2: b-1.0-py3-none-any.whl\n{BASE}");
        assert_eq!(
            from_spec_diff(Some(&base), &fin),
            vec![FixFlag::WheelSwitch {
                wheel: "b-1.0-py3-none-any.whl".into()
            }]
        );
    }

    #[test]
    fn a_check_that_was_already_empty_is_not_flagged() {
        let base = BASE.replace("%check\n%pytest\n", "%check\n");
        assert!(from_spec_diff(Some(&base), &base).is_empty());
    }

    #[test]
    fn a_test_toggle_switched_off_by_bcond_is_flagged() {
        let base = format!("%bcond_without tests\n{BASE}");
        let fin = format!("%bcond_with tests\n{BASE}");
        assert_eq!(
            from_spec_diff(Some(&base), &fin),
            vec![FixFlag::TestsSkipped {
                items: vec!["%bcond_with tests".into()]
            }]
        );
    }

    #[test]
    fn a_macro_guard_in_check_is_flagged_by_its_line() {
        let fin = with(
            "%check\n%pytest\n",
            "%check\n%{!?with_slowtests:%pytest -k 'not slow'}\n",
        );
        let flags = from_spec_diff(Some(BASE), &fin);
        assert!(
            matches!(&flags[0], FixFlag::TestsSkipped { items } if items.iter().any(|i| i.contains("with_slowtests"))),
            "{flags:?}"
        );
    }

    #[test]
    fn a_wheel_put_in_for_a_source_build_is_flagged_by_its_file() {
        let fin = with(
            "Source0: foo-1.0.tar.gz",
            "Source0:        foo-1.0-py3-none-any.whl",
        );
        assert_eq!(
            from_spec_diff(Some(BASE), &fin),
            vec![FixFlag::WheelSwitch {
                wheel: "foo-1.0-py3-none-any.whl".into()
            }]
        );
    }

    #[test]
    fn a_new_package_built_from_a_wheel_is_not_a_switch() {
        let fin = "Name: python-foo\nSource0:        foo-1.0-py3-none-any.whl\n";
        assert!(from_spec_diff(None, fin).is_empty());
    }

    #[test]
    fn a_line_is_one_line_and_capped() {
        let why = "word ".repeat(200);
        let line = FixFlag::Blocked { why }.line();
        assert!(!line.contains('\n'));
        assert!(line.chars().count() <= LINE_CAP + "FLAG: a fix turn answered blocked: ...".len());
        assert!(line.ends_with("..."));
    }

    #[test]
    fn settle_keeps_blocked_answers_and_the_spec_flags_of_earlier_commits() {
        let mut st = PkgState::default();
        add_blocked(&mut st, "r1", "no gcc".into());
        settle(&mut st, "r1", vec![FixFlag::CheckRemoved]);
        settle(&mut st, "r1", vec![]);
        assert_eq!(
            st.fix_flags,
            vec![
                FixFlag::Blocked {
                    why: "no gcc".into()
                },
                FixFlag::CheckRemoved
            ]
        );
        // A later run adds to them, and owns the run id from then on.
        settle(&mut st, "r2", vec![FixFlag::CheckRemoved]);
        assert_eq!(st.fix_flags.len(), 2);
        assert_eq!(st.fix_flags_run_id.as_deref(), Some("r2"));
    }

    #[test]
    fn a_reader_outside_the_run_sees_the_flags_the_package_recorded() {
        let root = tempfile::tempdir().unwrap();
        assert!(of_package(root.path(), "python-foo").is_empty());
        let ws = Workspace::open(root.path(), "python-foo").unwrap();
        PkgState::update(&ws, |st| {
            settle(st, "r1", vec![FixFlag::CheckRemoved]);
        })
        .unwrap();
        assert_eq!(
            of_package(root.path(), "python-foo"),
            vec![FixFlag::CheckRemoved]
        );
    }

    #[test]
    fn the_state_round_trips_the_flags() {
        let mut st = PkgState::default();
        settle(
            &mut st,
            "r1",
            vec![FixFlag::WheelSwitch {
                wheel: "a.whl".into(),
            }],
        );
        let text = serde_json::to_string(&st).unwrap();
        let back: PkgState = serde_json::from_str(&text).unwrap();
        assert_eq!(back.fix_flags, st.fix_flags);
    }
}
