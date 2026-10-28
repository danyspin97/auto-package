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
fn one_line(text: &str) -> String {
    let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= LINE_CAP {
        return flat;
    }
    let cut: String = flat.chars().take(LINE_CAP).collect();
    format!("{}...", cut.trim_end())
}

/// Keeps `st`'s flags for `run_id` only: flags of an earlier run are dropped
/// the first time this run looks at the package.
pub fn claim(st: &mut PkgState, run_id: &str) {
    if st.fix_flags_run_id.as_deref() != Some(run_id) {
        st.fix_flags.clear();
        st.fix_flags_run_id = Some(run_id.to_string());
    }
}

/// Adds `flag` unless it is already recorded.
pub fn add(st: &mut PkgState, flag: FixFlag) {
    if !st.fix_flags.contains(&flag) {
        st.fix_flags.push(flag);
    }
}

/// Replaces the spec-derived flags of `run_id` with `found` (the final spec's
/// own), keeping the `blocked` answers the run recorded.
pub fn settle(st: &mut PkgState, run_id: &str, found: Vec<FixFlag>) {
    claim(st, run_id);
    st.fix_flags
        .retain(|f| matches!(f, FixFlag::Blocked { .. }));
    for flag in found {
        add(st, flag);
    }
}

/// The flags the package's last run recorded, for a reader outside the run.
pub fn of_package(state_root: &std::path::Path, pkg: &str) -> Vec<FixFlag> {
    crate::workspace::Workspace::find_existing(state_root, &[pkg])
        .map(|ws| PkgState::load(&ws).fix_flags)
        .unwrap_or_default()
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
    let mut items: Vec<String> = Vec::new();
    for &i in &added {
        let in_check = check.is_some_and(|(s, e)| (s..e).contains(&i));
        for item in skipped_in(fin[i], in_check, &removed) {
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
fn extra(a: &[&str], b: &[&str]) -> Vec<usize> {
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

/// Lines of `name`'s body that run something: not blank, comment or `%{nil}`.
fn commands(lines: &[&str], name: &str) -> usize {
    let Some((start, end)) = section_span(lines, name) else {
        return 0;
    };
    lines[start..end]
        .iter()
        .map(|l| l.trim())
        .filter(|t| !t.is_empty() && !t.starts_with('#') && *t != "%{nil}" && *t != ":")
        .count()
}

/// The wheel file a new `Source` line of `fin` names, when `base` names none.
fn new_wheel(base: &[&str], fin: &[&str], added: &[usize]) -> Option<String> {
    if base.iter().any(|l| is_source_wheel(l)) {
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
    let t = line.trim();
    t.len() >= 6
        && t.is_char_boundary(6)
        && t[..6].eq_ignore_ascii_case("source")
        && t.contains(':')
        && t.contains(".whl")
}

/// The test-skipping items one added line of the spec carries.
fn skipped_in(line: &str, in_check: bool, removed: &[&str]) -> Vec<String> {
    let t = line.trim();
    if t.is_empty() || t.starts_with('#') {
        return Vec::new();
    }
    let mut out = Vec::new();
    for opt in ["--deselect", "--ignore", "--ignore-glob"] {
        for value in option_values(t, opt) {
            out.push(format!("{opt} {value}"));
        }
    }
    for opt in ["-k", "-m"] {
        for value in option_values(t, opt) {
            if value.split_whitespace().any(|w| w == "not") {
                out.push(format!("{opt} \"{value}\""));
            }
        }
    }
    if t.contains("ctest") {
        for value in option_values(t, "-E") {
            out.push(format!("ctest -E {value}"));
        }
    }
    if let Some(rest) = t.strip_prefix("%bcond_with ") {
        let name = rest.split_whitespace().next().unwrap_or("");
        let testish = name.contains("test") || name.contains("check");
        if testish
            && removed
                .iter()
                .any(|r| r.trim() == format!("%bcond_without {name}"))
        {
            out.push(one_line(t));
        }
    }
    const GUARDS: [&str; 6] = [
        "%{?_with",
        "%{!?_with",
        "%{?with_",
        "%{!?with_",
        "%{?_without",
        "%{!?_without",
    ];
    if in_check && GUARDS.iter().any(|g| t.contains(g)) {
        out.push(one_line(t));
    }
    out
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
    fn settle_keeps_blocked_answers_and_replaces_spec_flags_of_the_run() {
        let mut st = PkgState::default();
        claim(&mut st, "r1");
        add(
            &mut st,
            FixFlag::Blocked {
                why: "no gcc".into(),
            },
        );
        settle(&mut st, "r1", vec![FixFlag::CheckRemoved]);
        settle(&mut st, "r1", vec![]);
        assert_eq!(
            st.fix_flags,
            vec![FixFlag::Blocked {
                why: "no gcc".into()
            }]
        );
        // A later run starts from nothing.
        settle(&mut st, "r2", vec![FixFlag::CheckRemoved]);
        assert_eq!(st.fix_flags, vec![FixFlag::CheckRemoved]);
        assert_eq!(st.fix_flags_run_id.as_deref(), Some("r2"));
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
