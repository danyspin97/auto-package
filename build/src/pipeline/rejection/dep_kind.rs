//! The kind of an inherited dependency: an update changes only what the release
//! forces (convention 86), so a `Requires:` the origin has as a `Requires:` stays
//! one. An edit that moves such a dependency to another kind (`Requires:` to
//! `Recommends:`) is rolled back unless the release forces it, or the scope is
//! `--fix`, which may change inherited problems. A dependency the origin does not
//! declare is new and is not judged here.

use super::*;
use crate::pipeline::dep_edit::dependency_key;
use std::collections::BTreeSet;

/// The dependency tags the gate judges. `Requires(post)` and the like are not
/// runtime dependencies of the package and are left out.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(super) enum DepKind {
    Requires,
    Recommends,
    BuildRequires,
}

impl DepKind {
    pub(super) fn tag(self) -> &'static str {
        match self {
            DepKind::Requires => "Requires",
            DepKind::Recommends => "Recommends",
            DepKind::BuildRequires => "BuildRequires",
        }
    }
}

/// A dependency the edit moved from the origin's kind to another one.
#[derive(Debug, PartialEq, Eq)]
pub(super) struct KindChange {
    pub(super) name: String,
    pub(super) from: DepKind,
    pub(super) to: DepKind,
}

impl KindChange {
    /// `Requires: python-Babel -> Recommends: python-Babel`.
    pub(super) fn describe(&self) -> String {
        format!(
            "{}: {} -> {}: {}",
            self.from.tag(),
            self.name,
            self.to.tag(),
            self.name
        )
    }
}

/// The kind, the dependency key and the written name of each judged dependency
/// line of `spec`. The name is the first token of the value (a bound, a comma or
/// a `%{...}` expression is not part of it).
fn dependency_lines(spec: &str) -> Vec<(DepKind, String, String)> {
    spec.lines()
        .filter_map(|line| {
            let (tag, value) = line.trim_start().split_once(':')?;
            let kind = match tag.trim_end().to_ascii_lowercase().as_str() {
                "requires" => DepKind::Requires,
                "recommends" => DepKind::Recommends,
                "buildrequires" => DepKind::BuildRequires,
                _ => return None,
            };
            let name = value
                .split(|c: char| c.is_whitespace() || c == ',')
                .find(|t| !t.is_empty())?;
            if name.starts_with(['(', '%']) {
                return None;
            }
            Some((kind, dependency_key(name), name.to_string()))
        })
        .collect()
}

/// The kinds `key` is declared with in `lines`.
fn kinds_of(lines: &[(DepKind, String, String)], key: &str) -> BTreeSet<DepKind> {
    lines
        .iter()
        .filter(|(_, k, _)| k == key)
        .map(|(kind, _, _)| *kind)
        .collect()
}

/// The inherited dependencies `after` moves away from the kind `origin` declares
/// them with. A dependency counts only when this edit did the move: `before`
/// (the spec the turn started from) still has the origin's kinds, the edited
/// `after` has a kind the origin lacks and has lost one the origin had. A kind
/// added next to the origin's (both `Requires:` and `Recommends:`) is not a move.
pub(super) fn kind_changes(origin: &str, before: &str, after: &str) -> Vec<KindChange> {
    let origin_lines = dependency_lines(origin);
    let before_lines = dependency_lines(before);
    let after_lines = dependency_lines(after);
    let mut seen = BTreeSet::new();
    let mut changes = Vec::new();
    for (_, key, name) in &origin_lines {
        if !seen.insert(key.clone()) {
            continue;
        }
        let o = kinds_of(&origin_lines, key);
        let b = kinds_of(&before_lines, key);
        let a = kinds_of(&after_lines, key);
        let lost = o.difference(&a).next().copied();
        let gained = a.difference(&o).next().copied();
        if let (Some(from), Some(to)) = (lost, gained)
            && b == o
        {
            changes.push(KindChange {
                name: name.clone(),
                from,
                to,
            });
        }
    }
    changes
}

/// The kind changes an edit is judged on: [`kind_changes`] less the dependencies
/// the release forces (`forced`, names), and nothing in the `fix` scope, which
/// may change inherited problems.
pub(super) fn judged_kind_changes(
    origin: &str,
    before: &str,
    after: &str,
    forced: &[String],
    fix: bool,
) -> Vec<KindChange> {
    if fix {
        return Vec::new();
    }
    let forced: BTreeSet<String> = forced.iter().map(|n| dependency_key(n)).collect();
    kind_changes(origin, before, after)
        .into_iter()
        .filter(|c| !forced.contains(&dependency_key(&c.name)))
        .collect()
}

/// The origin's spec an edit of this package is judged against, read the way
/// the generate stage reads it: an update's origin, else a bumped dependency's
/// (`carried_recipe`). `None` for a new package, or when the origin cannot be
/// read (not knowing does not reject an edit).
/// The origin's spec a judged edit is measured against: an update's, else a
/// bumped dependency's (`carried_recipe`). Read only when the gate needs it.
pub(in crate::pipeline) type OriginSpec<'a> = &'a dyn Fn() -> Option<String>;

/// The origins this process has read, by checkout directory. A checkout belongs
/// to one run, so its origin is read once; a failed read is remembered too, so
/// its warning is said once.
static ORIGINS: std::sync::Mutex<Option<std::collections::HashMap<PathBuf, Option<String>>>> =
    std::sync::Mutex::new(None);

/// The origin's spec for `checkout_dir`, from [`ORIGINS`] or read now.
pub(in crate::pipeline) fn origin_for_edit(
    config: &PipelineConfig,
    checkout_dir: &Path,
    reporter: &dyn Reporter,
) -> Option<String> {
    if !config.update && !config.carried_recipe {
        return None;
    }
    let known = ORIGINS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_ref()
        .and_then(|m| m.get(checkout_dir).cloned());
    if let Some(spec) = known {
        return spec;
    }
    let spec = read_origin(config, checkout_dir, reporter);
    ORIGINS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get_or_insert_with(Default::default)
        .insert(checkout_dir.to_path_buf(), spec.clone());
    spec
}

fn read_origin(
    config: &PipelineConfig,
    checkout_dir: &Path,
    reporter: &dyn Reporter,
) -> Option<String> {
    let obs_name = Workspace::open(&config.state_root, &config.rpm_name)
        .ok()
        .and_then(|ws| PkgState::load(&ws).obs_name)
        .unwrap_or_else(|| config.rpm_name.clone());
    if config.update {
        update_mode::origin_base(&config.project, &obs_name, reporter).and_then(|o| o.spec)
    } else {
        carried_origin::origin_spec(&config.project, &obs_name, checkout_dir, reporter)
    }
}

/// The acceptance check for the dependency kinds of an edit: an inherited
/// dependency the edit moves to another kind is rolled back the way a rejected
/// spec is, unless the release forces it (`forced`, dependency names) or the
/// scope is `fix`. Returns the reason when the edit was rolled back.
#[allow(clippy::too_many_arguments)]
pub(super) fn reject_inherited_kind_changes(
    spec_path: &Path,
    before: &str,
    origin: OriginSpec<'_>,
    snapshot: Option<&CheckoutSnapshot>,
    step: &StepDir,
    reporter: &dyn Reporter,
    notes_path: PathBuf,
    keep: &[String],
    forced: &[String],
    fix: bool,
) -> Option<String> {
    if fix {
        return None;
    }
    let after = std::fs::read_to_string(spec_path).unwrap_or_else(|_| before.to_string());
    // The origin is read only for an edit that touches a dependency line.
    if dependency_lines(before) == dependency_lines(&after) {
        return None;
    }
    let origin = origin()?;
    let changes = judged_kind_changes(&origin, before, &after, forced, fix);
    if changes.is_empty() {
        return None;
    }
    let described: Vec<String> = changes.iter().map(KindChange::describe).collect();
    let problem = format!(
        "the edit changes the kind of an inherited dependency: {}. An update changes only \
         what the release forces (convention 86); the origin's kind stays. The gate allows \
         a kind change only for a dependency the origin does not declare: a change the \
         release forces is still rolled back, so report that dependency instead.",
        described.join(", ")
    );
    note_spec_rejection(spec_path, &problem);
    reporter.report(&Event::SpecRejected {
        step: step.id,
        reason: ap_core::event::SpecRejection::InheritedKindChange(described),
        restored_from: spec_path.to_path_buf(),
    });
    restore_spec(spec_path, before, Some(step), reporter);
    if let Some(snap) = snapshot {
        snap.restore_keeping(reporter, Some(step), keep);
    }
    append_notes(
        notes_path,
        &format!(
            "\n## Edit rejected — an inherited dependency changes kind\n{problem}\n\
             The edit was rolled back. Keep the origin's `Requires:`/`Recommends:`/\
             `BuildRequires:` for a dependency it already has.\n"
        ),
    );
    Some(problem)
}
