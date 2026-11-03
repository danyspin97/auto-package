//! A build system switch in an agent's edit (autotools to meson or CMake, and
//! the other pairs). The switch is accepted only with evidence that upstream
//! dropped the old system, and then the `.changes` entry gets a fixed bullet that
//! names both systems (D-20261102-32). The turn's own words cannot change it.

use super::*;
use crate::pipeline::patch_rules::reject_unnamed_patches;
use crate::pipeline::snapshot::CheckoutSnapshot;

/// The build systems a spec can drive from its build section.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum BuildSystem {
    Autotools,
    Meson,
    CMake,
}

impl BuildSystem {
    /// The name the `.changes` bullet uses.
    fn name(self) -> &'static str {
        match self {
            Self::Autotools => "autotools",
            Self::Meson => "meson",
            Self::CMake => "CMake",
        }
    }

    /// Words that name this system's files or tool in an evidence clause.
    fn markers(self) -> &'static [&'static str] {
        match self {
            Self::Autotools => &["autotools", "autoconf", "automake", "configure.ac"],
            Self::Meson => &["meson"],
            Self::CMake => &["cmake"],
        }
    }
}

/// A spec whose build moves from one system to another.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Switch {
    pub(super) from: BuildSystem,
    pub(super) to: BuildSystem,
}

impl Switch {
    /// The fixed `.changes` bullet of this switch.
    pub(super) fn bullet(self) -> String {
        format!(
            "Switch to the {} build system: upstream removed {}",
            self.to.name(),
            self.from.name()
        )
    }
}

/// The system a build-section macro calls, when it is one of the macros of a
/// system (`%configure`, `%make_build`, `%meson*`, `%cmake*`).
fn macro_system(word: &str) -> Option<BuildSystem> {
    match word {
        "%configure" | "%make_build" => Some(BuildSystem::Autotools),
        w if w == "%meson" || w.starts_with("%meson_") => Some(BuildSystem::Meson),
        w if w == "%cmake" || w.starts_with("%cmake_") => Some(BuildSystem::CMake),
        _ => None,
    }
}

/// The system a BuildRequires name belongs to. A version after the name does
/// not match, so `meson >= 1.0` names meson.
fn required_system(word: &str) -> Option<BuildSystem> {
    match word {
        "meson" => Some(BuildSystem::Meson),
        "cmake" => Some(BuildSystem::CMake),
        "autoconf" | "automake" => Some(BuildSystem::Autotools),
        _ => None,
    }
}

/// The one system all of `systems` name, or `None` when they name none or two.
fn only(systems: &[BuildSystem]) -> Option<BuildSystem> {
    let first = *systems.first()?;
    systems.iter().all(|s| *s == first).then_some(first)
}

/// The build system `spec` uses: the one its build-section macros call, or,
/// when no macro calls one, the one its BuildRequires name. `None` when the
/// spec calls two systems or none.
pub(super) fn build_system(spec: &str) -> Option<BuildSystem> {
    let (mut macros, mut required) = (Vec::new(), Vec::new());
    for line in spec.lines() {
        let line = line.trim_start();
        if line.starts_with('#') {
            continue;
        }
        let is_requires = line
            .get(..14)
            .is_some_and(|p| p.eq_ignore_ascii_case("BuildRequires:"));
        if is_requires {
            required.extend(
                line[14..]
                    .split(|c: char| c.is_whitespace() || c == ',')
                    .filter_map(required_system),
            );
        } else {
            macros.extend(line.split_whitespace().filter_map(macro_system));
        }
    }
    if macros.is_empty() {
        only(&required)
    } else {
        only(&macros)
    }
}

/// The switch from `before` to `after`, when both name a system and they differ.
pub(super) fn switch_between(before: &str, after: &str) -> Option<Switch> {
    let (from, to) = (build_system(before)?, build_system(after)?);
    (from != to).then_some(Switch { from, to })
}

/// The switch the turn's edit made to the spec at `spec_path`, against `before`.
pub(super) fn switch_of_edit(spec_path: &Path, before: &str) -> Option<Switch> {
    let after = std::fs::read_to_string(spec_path).unwrap_or_default();
    switch_between(before, &after)
}

/// `bullet` with the switch's fixed bullet on its own line.
pub(super) fn with_switch_bullet(bullet: String, switch: Option<Switch>) -> String {
    match switch {
        Some(s) => format!("{bullet}\n{}", s.bullet()),
        None => bullet,
    }
}

/// Whether the turn says upstream dropped the old system: a clause of its
/// evidence or reasoning names the old system's files or tool and says it is
/// removed, gone or missing.
pub(super) fn evidence_shows_removal(switch: Switch, evidence: &[String], reasoning: &str) -> bool {
    const REMOVED: &[&str] = &["remov", "no longer", "gone", "dropped", "missing"];
    evidence
        .iter()
        .map(String::as_str)
        .chain(std::iter::once(reasoning))
        .flat_map(|text| text.split(['\n', ';']))
        .flat_map(|clause| clause.split(". "))
        .map(str::to_lowercase)
        .any(|clause| {
            switch.from.markers().iter().any(|m| clause.contains(m))
                && REMOVED.iter().any(|r| clause.contains(r))
        })
}

/// The `.changes` checks of an edit: a build system switch the evidence does not
/// back, then a new patch the bullet does not name. Returns the reason when the
/// edit was rolled back.
#[allow(clippy::too_many_arguments)]
pub(super) fn reject_unchecked_edit(
    switch: Option<Switch>,
    outcome: &crate::agent::TurnOutcome,
    spec_path: &Path,
    before: &str,
    snapshot: Option<&CheckoutSnapshot>,
    step: &StepDir,
    reporter: &dyn Reporter,
    notes_path: PathBuf,
    keep: &[String],
    bullet_source: &str,
    label: &str,
) -> Option<String> {
    reject_unevidenced_switch(
        switch,
        outcome,
        spec_path,
        before,
        snapshot,
        step,
        reporter,
        notes_path.clone(),
        keep,
        label,
    )
    .or_else(|| {
        reject_unnamed_patches(
            spec_path,
            before,
            snapshot,
            step,
            reporter,
            notes_path.clone(),
            keep,
            bullet_source,
            label,
        )
    })
    .or_else(|| {
        reject_missing_changelog(
            spec_path,
            before,
            snapshot,
            step,
            reporter,
            notes_path,
            keep,
            &turn_bullet_source(outcome),
            switch.is_some(),
            label,
        )
    })
}

/// The rejection of a switch the turn's evidence does not back: the edit is
/// rolled back like a rejected patch and the reason is returned. `None` when
/// there is no switch or the evidence backs it.
#[allow(clippy::too_many_arguments)]
pub(super) fn reject_unevidenced_switch(
    switch: Option<Switch>,
    outcome: &crate::agent::TurnOutcome,
    spec_path: &Path,
    before: &str,
    snapshot: Option<&CheckoutSnapshot>,
    step: &StepDir,
    reporter: &dyn Reporter,
    notes_path: PathBuf,
    keep: &[String],
    label: &str,
) -> Option<String> {
    let switch = switch?;
    if evidence_shows_removal(switch, &outcome.evidence, &outcome.reasoning) {
        return None;
    }
    let problem = format!(
        "needs a maintainer: build system switch without evidence: the spec moves from {} to {}, \
         and the evidence does not say upstream removed {} (name its files, such as configure.ac \
         or CMakeLists.txt gone from the tarball, or quote NEWS)",
        switch.from.name(),
        switch.to.name(),
        switch.from.name()
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
            "\n## {label} — edit rejected: build system switch without evidence\n{problem}\n\
             The edit was rolled back.\n"
        ),
    );
    Some(problem)
}
