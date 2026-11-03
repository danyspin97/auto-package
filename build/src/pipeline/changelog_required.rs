//! The edit that needs a `.changes` bullet (convention 40). An edit that changes
//! only dependencies (and comments) leaves `changelog` empty and gets no bullet;
//! any other change with an empty `changelog` is rolled back, so the agent
//! writes the bullet.
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
fn keyed_lines(spec: &str) -> Vec<(String, String, String)> {
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

/// The lines the edit added or removed that are not dependency lines, comments,
/// conditional lines or patch lines. A line is keyed by its section and the
/// conditions around it, so a line moved to another section, or newly wrapped in
/// (or freed from) a conditional, counts. With `switch`, the lines of `%prep`,
/// `%build` and `%install` are the build system switch's, which has its own
/// bullet.
pub(super) fn changes_needing_a_bullet(before: &str, after: &str, switch: bool) -> Vec<String> {
    let mut counts: std::collections::BTreeMap<(String, String, String), i64> = Default::default();
    for k in keyed_lines(before) {
        *counts.entry(k).or_default() -= 1;
    }
    for k in keyed_lines(after) {
        *counts.entry(k).or_default() += 1;
    }
    counts
        .into_iter()
        .filter(|((section, _, line), n)| {
            *n != 0
                && !needs_no_bullet(line)
                && !(switch
                    && ["%prep", "%build", "%install"]
                        .iter()
                        .any(|s| section.split_whitespace().next() == Some(s)))
        })
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
