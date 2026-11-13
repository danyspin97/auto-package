//! The edit that needs a `.changes` bullet (convention 40). An edit that changes
//! only dependencies (and comments) leaves `changelog` empty and gets no bullet;
//! so does housekeeping: a source archive or `%setup -n` name that only tracks
//! the release, and a workaround removed from `%prep`, `%build` or `%check` that
//! skips no test. Any other change with an empty `changelog` is rolled back, so
//! the agent writes the bullet.
//!
//! The whole edit is judged: the spec, and the files of the checkout the turn
//! changed (an rpmlintrc, `_service`, `_multibuild`, `_constraints`, a patch
//! file, `series`). The check runs on the agent's own `changelog`: the bullet
//! the pipeline adds for a build system switch or a proven patch removal does
//! not stand for anything else the edit changed.
//!
//! It runs on every path that keeps an agent edit with the agent's changelog
//! (`reject_unchecked_edit`, `gate_turn_edit_without_accept`). The patch stage
//! (`patch_apply`) does not call it: there the pipeline writes the bullets of the
//! patches it refreshed or removed, and the turn's edit is limited to patches.

use super::*;
use crate::pipeline::render::*;

/// Section headers of a spec; a line that starts one changes the section key.
const SECTIONS: &[&str] = &[
    "%description",
    "%package",
    "%prep",
    "%conf",
    "%build",
    "%install",
    "%check",
    "%files",
    "%pre",
    "%post",
    "%preun",
    "%postun",
    "%pretrans",
    "%posttrans",
    "%preuntrans",
    "%postuntrans",
    "%trigger",
    "%triggerin",
    "%triggerun",
    "%triggerpostun",
    "%filetriggerin",
    "%filetriggerun",
    "%generate_buildrequires",
    "%changelog",
];

const CONDITIONALS: &[&str] = &["%if", "%ifarch", "%ifnarch", "%ifos", "%ifnos"];
const BRANCHES: &[&str] = &["%else", "%elif", "%elifarch", "%elifnarch", "%elifos"];

/// Whether `line` is one the pipeline or a dependency edit accounts for: a
/// comment, a blank line, a dependency tag, a patch declaration or application,
/// or a conditional line (what it wraps is judged line by line through the key).
fn needs_no_bullet(line: &str) -> bool {
    let lower = line.trim().to_ascii_lowercase();
    if lower.is_empty() || lower.starts_with('#') || lower.starts_with("%patch") {
        return true;
    }
    let word = lower.split_whitespace().next().unwrap_or("");
    if CONDITIONALS.contains(&word) || BRANCHES.contains(&word) || word == "%endif" {
        return true;
    }
    let tag = lower.split(':').next().unwrap_or("");
    let tag = tag.split('(').next().unwrap_or("").trim();
    const DEPS: &[&str] = &[
        "buildrequires",
        "requires",
        "provides",
        "obsoletes",
        "conflicts",
        "buildconflicts",
        "recommends",
        "suggests",
        "supplements",
        "enhances",
    ];
    DEPS.contains(&tag)
        || tag
            .strip_prefix("patch")
            .is_some_and(|d| d.chars().all(|c| c.is_ascii_digit()))
}

/// The lines of a spec as `(section, enclosing conditionals, line)`: the same
/// line in another section, or under another condition, is another entry.
pub(super) fn keyed_lines(spec: &str) -> Vec<(String, String, String)> {
    let mut section = String::from("preamble");
    let mut conds: Vec<String> = Vec::new();
    let mut out = Vec::new();
    for line in spec.lines() {
        let t = line.trim();
        let word = t.split_whitespace().next().unwrap_or("");
        if CONDITIONALS.contains(&word) {
            conds.push(t.to_string());
            out.push((
                section.clone(),
                conds[..conds.len() - 1].join(" / "),
                t.to_string(),
            ));
            continue;
        }
        if BRANCHES.contains(&word) {
            if let Some(last) = conds.last_mut() {
                *last = format!("{last} // {t}");
            }
            out.push((section.clone(), conds.join(" / "), t.to_string()));
            continue;
        }
        if word == "%endif" {
            conds.pop();
            out.push((section.clone(), conds.join(" / "), t.to_string()));
            continue;
        }
        if SECTIONS.contains(&word) {
            section = t.to_string();
        }
        out.push((section.clone(), conds.join(" / "), t.to_string()));
    }
    out
}

/// A spec line as the gate keys it: its section, its enclosing conditionals and
/// the line itself.
type Key = (String, String, String);

/// The sections whose workaround lines a fix may remove without a bullet.
const WORKAROUND_SECTIONS: &[&str] = &["%prep", "%build", "%check"];

/// Characters that make a line run more than one command: a chain, a pipe, a
/// redirect or a substitution.
const COMPOUND: &[&str] = &["&&", ";", "|", ">", "`", "$("];

/// Words of a line that runs tests: a workaround line that names one is no
/// workaround, its removal may drop a test.
const TEST_WORDS: &[&str] = &[
    "pytest", "unittest", "nose", "test", "make", "tox", "ctest", "meson", "-m ", "setup.py",
];

/// Variables that tools read without a reference in the spec (a `PYTHON*`
/// setting, a compiler or a locale): removing their `export` changes a build.
const IMPLICIT_ENV: &[&str] = &[
    "PYTHON",
    "PATH",
    "LD_",
    "LC_",
    "LANG",
    "HOME",
    "TMP",
    "CC",
    "CXX",
    "CFLAGS",
    "CXXFLAGS",
    "CPPFLAGS",
    "LDFLAGS",
    "RUST",
    "CARGO",
    "GO",
    "PKG_CONFIG",
    "CMAKE",
    "PIP",
    "SETUPTOOLS",
    "MAKE",
    "TEST",
    "PYTEST",
    "OMP_",
    "MKL_",
    "OPENBLAS_",
    "NUMBA_",
    "NPY_",
    "SOURCE_DATE_EPOCH",
];

/// The archive extensions a `Source` name may end in.
const ARCHIVE_EXTS: &[&str] = &[
    ".tar.gz", ".tgz", ".tar.bz2", ".tbz2", ".tar.xz", ".txz", ".tar.zst", ".tar", ".zip",
];

/// The first word of a spec line, lowercased.
fn first_word(line: &str) -> String {
    line.split_whitespace()
        .next()
        .unwrap_or("")
        .to_ascii_lowercase()
}

/// Whether a section key names one of `WORKAROUND_SECTIONS`.
fn in_workaround_section(section: &str) -> bool {
    let word = section.split_whitespace().next().unwrap_or("");
    WORKAROUND_SECTIONS.contains(&word)
}

/// Whether removing `line` (of `section`) only drops a workaround that the
/// sources no longer need, and skips no test: a `sed` edit, an `export` of a
/// variable the rest of the spec (`after`) never names, or a Python `import mock`
/// line that runs no test. A line that chains, pipes or redirects is no
/// workaround here, and neither is a `|| true` (see `relaxed_pair`).
fn is_workaround_removal(section: &str, line: &str, after: &str) -> bool {
    if !in_workaround_section(section) {
        return false;
    }
    let lower = line.trim().to_ascii_lowercase();
    if COMPOUND.iter().any(|c| lower.contains(c)) {
        return false;
    }
    match first_word(line).as_str() {
        "sed" | "%{__sed}" | "%__sed" => true,
        "export" => {
            let words: Vec<&str> = line.split_whitespace().collect();
            let Some(name) = words.get(1).and_then(|w| w.split('=').next()) else {
                return false;
            };
            let upper = name.to_ascii_uppercase();
            words.len() == 2
                && !name.is_empty()
                && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
                && !IMPLICIT_ENV.iter().any(|p| upper.starts_with(p))
                && !after
                    .split(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
                    .any(|w| w == name)
        }
        _ => lower.contains("import mock") && !TEST_WORDS.iter().any(|w| lower.contains(w)),
    }
}

/// `(tag, value)` of a `SourceN:` line, the tag in its written case.
fn source_tag_value(line: &str) -> Option<(&str, &str)> {
    let (tag, value) = line.trim().split_once(':')?;
    let tag = tag.trim();
    let digits = tag.to_ascii_lowercase();
    let digits = digits.strip_prefix("source")?;
    digits
        .chars()
        .all(|c| c.is_ascii_digit())
        .then_some((tag, value.trim()))
}

/// The form a name is compared in when only its case, its `_` or `.` for `-`,
/// or its `%{version}` differ (the PyPI normalised name is one of them).
fn name_form(name: &str) -> String {
    name.to_ascii_lowercase()
        .replace(['_', '.'], "-")
        .replace("%{version}", "")
}

/// Whether two archive names (or paths of them) differ only in their directory
/// and file name form, their extension, and `%{version}` usage: the same host
/// and directory, and the same base name in `name_form`. A changed host or
/// project is not the same.
fn same_archive(a: &str, b: &str) -> bool {
    let split = |v: &str| -> (String, String) {
        let (dir, file) = v.rsplit_once('/').unwrap_or(("", v));
        (name_form(dir), file.to_string())
    };
    let (dir_a, file_a) = split(a);
    let (dir_b, file_b) = split(b);
    let base = |file: &str| -> Option<String> {
        let lower = file.to_ascii_lowercase();
        let ext = ARCHIVE_EXTS.iter().find(|e| lower.ends_with(*e))?;
        Some(name_form(&file[..file.len() - ext.len()]))
    };
    match (base(&file_a), base(&file_b)) {
        (Some(x), Some(y)) => dir_a == dir_b && !x.is_empty() && x == y,
        _ => false,
    }
}

/// Whether two `SourceN:` lines differ only in the archive name of the release.
fn archive_rename(old: &str, new: &str) -> bool {
    match (source_tag_value(old), source_tag_value(new)) {
        (Some((ta, va)), Some((tb, vb))) => ta.eq_ignore_ascii_case(tb) && same_archive(va, vb),
        _ => false,
    }
}

/// Whether two `%setup` or `%autosetup` lines differ only in the value of their
/// `-n` directory name, which is the same name in `name_form`.
fn setup_dir_rename(old: &str, new: &str) -> bool {
    let a: Vec<&str> = old.split_whitespace().collect();
    let b: Vec<&str> = new.split_whitespace().collect();
    let is_setup = |w: &[&str]| {
        w.first().is_some_and(|f| {
            f.eq_ignore_ascii_case("%setup") || f.eq_ignore_ascii_case("%autosetup")
        })
    };
    if !is_setup(&a) || !is_setup(&b) || a.len() != b.len() {
        return false;
    }
    let mut renamed = false;
    for i in 0..a.len() {
        if a[i] == b[i] {
            continue;
        }
        let dir_pair = i > 0 && a[i - 1] == "-n" && b[i - 1] == "-n";
        if !dir_pair || name_form(a[i]) != name_form(b[i]) || name_form(a[i]).is_empty() {
            return false;
        }
        renamed = true;
    }
    renamed
}

/// Whether a removed `|| true` line and the added line are the same command,
/// the mask gone: the command now fails the build when it fails.
fn relaxed_pair(old: &str, new: &str) -> bool {
    old.trim()
        .strip_suffix("|| true")
        .is_some_and(|rest| rest.trim_end() == new.trim())
}

/// Whether a removed line and an added line of the same section and conditions
/// are one housekeeping change: an archive name or `-n` directory name that
/// only tracks the release, or a `|| true` mask removed from the command.
fn housekeeping_pair(old: &Key, new: &Key) -> bool {
    old.0 == new.0
        && old.1 == new.1
        && (archive_rename(&old.2, &new.2)
            || setup_dir_rename(&old.2, &new.2)
            || (in_workaround_section(&old.0) && relaxed_pair(&old.2, &new.2)))
}

/// The lines the edit added or removed that are not dependency lines, comments,
/// conditional lines, patch lines or housekeeping. A line is keyed by its section
/// and the conditions around it, so a line moved to another section, or newly
/// wrapped in (or freed from) a conditional, counts. With `switch`, the lines of
/// `%prep`, `%build` and `%install` are the build system switch's, which has its
/// own bullet. Housekeeping needs no bullet (convention 40): a source archive or
/// `%setup -n` directory name that only tracks the release, and a workaround
/// removed from `%prep`, `%build` or `%check` that skips no test.
pub(super) fn changes_needing_a_bullet(before: &str, after: &str, switch: bool) -> Vec<String> {
    let mut counts: std::collections::BTreeMap<Key, i64> = Default::default();
    for k in keyed_lines(before) {
        *counts.entry(k).or_default() -= 1;
    }
    for k in keyed_lines(after) {
        *counts.entry(k).or_default() += 1;
    }
    counts.retain(|(section, _, line), n| {
        *n != 0
            && !needs_no_bullet(line)
            && !super::diagnostic_edit::is_build_print(section, line)
            && !(switch
                && ["%prep", "%build", "%install"]
                    .iter()
                    .any(|s| section.split_whitespace().next() == Some(s)))
    });
    let removed: Vec<Key> = counts
        .iter()
        .filter(|(_, n)| **n < 0)
        .map(|(k, _)| k.clone())
        .collect();
    for r in &removed {
        while counts.get(r).is_some_and(|n| *n < 0) {
            let added = counts
                .iter()
                .find(|(a, n)| **n > 0 && housekeeping_pair(r, a))
                .map(|(a, _)| a.clone());
            let settled = match added {
                Some(a) => {
                    *counts.entry(a).or_default() -= 1;
                    true
                }
                None => is_workaround_removal(&r.0, &r.2, after),
            };
            if !settled {
                break;
            }
            *counts.entry(r.clone()).or_default() += 1;
        }
    }
    counts
        .into_iter()
        .filter(|(_, n)| *n != 0)
        .map(|((_, _, line), _)| line)
        .collect()
}

fn is_patch_file(name: &str) -> bool {
    name.ends_with(".patch") || name.ends_with(".diff")
}

/// The files of the checkout the turn changed that need a bullet: a patch file
/// that existed and changed (a new patch is named by `reject_unnamed_patches`, a
/// removed one has the pipeline's `Drop` bullet), and an rpmlintrc, `_service`,
/// `_multibuild` or `_constraints` the turn added or changed, and `series` when
/// no patch file changed with it. Files the turn staged (`keep`) are not listed.
pub(super) fn files_needing_a_bullet(
    dir: &Path,
    snapshot: Option<&CheckoutSnapshot>,
    keep: &[String],
) -> Vec<String> {
    let Some(snap) = snapshot else {
        return Vec::new();
    };
    let Ok(rd) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut names: Vec<String> = rd
        .flatten()
        .filter(|e| e.path().is_file())
        .filter_map(|e| e.file_name().into_string().ok())
        .filter(|n| !keep.contains(n))
        .collect();
    names.sort();
    // A new file is changed; one whose old bytes the snapshot did not keep is not
    // judged.
    let changed = |name: &str| match snap.entry_of(name) {
        None => true,
        Some(None) => false,
        Some(Some(old)) => std::fs::read(dir.join(name)).is_ok_and(|now| now != old),
    };
    let mut out = Vec::new();
    let mut patch_changed = false;
    for name in &names {
        let existed = snap.entry_of(name).is_some();
        if is_patch_file(name) {
            if existed && changed(name) {
                patch_changed = true;
                out.push(name.clone());
            }
        } else if (name.ends_with("-rpmlintrc")
            || name.ends_with(".rpmlintrc")
            || matches!(name.as_str(), "_service" | "_multibuild" | "_constraints"))
            && changed(name)
        {
            out.push(name.clone());
        }
    }
    if !patch_changed && names.iter().any(|n| n == "series") && changed("series") {
        out.push("series".to_string());
    }
    out
}

/// The acceptance check for an edit with no bullet of the agent's own: when
/// `changelog` is empty and the edit changed more than dependencies, comments and
/// the patches the pipeline writes bullets for, it is rolled back the way a
/// rejected edit is. `switch` marks a build system switch (its build lines have
/// their own bullet); the pipeline writes the `Drop` bullet of a proven patch
/// removal itself. Returns the reason when it was rolled back.
#[allow(clippy::too_many_arguments)]
pub(super) fn reject_missing_changelog(
    spec_path: &Path,
    before: &str,
    snapshot: Option<&CheckoutSnapshot>,
    step: &StepDir,
    reporter: &dyn Reporter,
    notes_path: PathBuf,
    keep: &[String],
    changelog: &str,
    switch: bool,
    label: &str,
) -> Option<String> {
    if !changelog.trim().is_empty() {
        return None;
    }
    let after = std::fs::read_to_string(spec_path).unwrap_or_else(|_| before.to_string());
    let mut changes = changes_needing_a_bullet(before, &after, switch);
    if let Some(dir) = spec_path.parent() {
        changes.extend(files_needing_a_bullet(dir, snapshot, keep));
    }
    let first = changes.first()?;
    let problem = format!(
        "`changelog` is empty, but the edit changes more than dependencies (for example `{first}`): \
         write one `.changes` bullet per change (convention 40); only an edit that changes nothing \
         but dependencies leaves it empty"
    );
    restore_spec(spec_path, before, Some(step), reporter);
    if let Some(snap) = snapshot {
        snap.restore_keeping(reporter, Some(step), keep);
    }
    note_spec_rejection(spec_path, &problem);
    reporter.report(&Event::Warning {
        step: Some(step.id),
        message: format!("{label}: edit rejected: {problem}"),
    });
    append_notes(
        notes_path,
        &format!(
            "\n## {label} — edit rejected: no changelog\n{problem}\nThe edit was rolled back.\n"
        ),
    );
    Some(problem)
}
