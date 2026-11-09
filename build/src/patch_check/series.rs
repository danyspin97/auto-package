use super::*;

/// The strip levels the check runs (the ones rpm's macros use in practice).
pub(super) const SUPPORTED_LEVELS: [u32; 2] = [0, 1];

/// What one patch's quilt output and its rejected hunks keep for the report.
pub(super) const OUTPUT_LIMIT: usize = 2000;

pub(super) const REJECTS_LIMIT: usize = 4000;

/// Rounds of agent turns the pipeline gives the failing patches.
pub const MAX_ROUNDS: usize = 4;

/// What quilt says when a patch that does not apply is already in the tree.
pub(crate) const REVERSE_APPLIED: &str = "can be reverse-applied";

/// One patch of the series, as the spec applies it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SeriesEntry {
    /// The `PatchN:` number (`"0"` for a bare `Patch:`).
    pub number: String,
    /// The file name, with `%{name}` and `%{version}` expanded.
    pub name: String,
    /// The `-pN` level the spec applies it with.
    pub level: u32,
}

/// A patch of the spec, either checked or with the reason it cannot be.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Planned {
    Check(SeriesEntry),
    Skip { name: String, reason: String },
}

/// What quilt said about one patch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PatchStatus {
    /// `quilt push` applied it with no offset and no fuzz.
    Applies,
    /// It applied with an offset, or with fuzz. An offset alone is left as it
    /// is: rpm's `%autosetup` applies it too. Fuzz fails rpm's `--fuzz=0`, so
    /// `quilt refresh` makes that patch exact.
    AppliesWithOffsetOrFuzz { fuzz: bool, output: String },
    /// It does not apply: quilt's output and the rejected hunks. Never a
    /// reason to remove the patch: it is rebased.
    Fails { output: String, rejects: String },
    /// It does not apply because its change is already in the sources: quilt
    /// said `can be reverse-applied`, also with `--fuzz=0`. The proof that the
    /// patch may be removed.
    AlreadyApplied { output: String },
    /// It was not tried, and why. Never a success.
    NotChecked { reason: String },
}

/// One patch's verdict.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PatchResult {
    pub name: String,
    pub level: u32,
    pub status: PatchStatus,
    /// The patch as `quilt refresh` rewrote it, when the check was asked to
    /// refresh fuzzy patches (see [`Refresh`]).
    pub refreshed: Option<Vec<u8>>,
}

/// Counts that decide whether another round is worth it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Tally {
    pub failing: usize,
    pub refresh: usize,
    /// Patches proven already applied upstream (to be removed).
    pub upstream: usize,
}

/// The verdict on every patch of the spec.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CheckReport {
    /// Every `PatchN:` of the spec, in series order.
    pub patches: Vec<PatchResult>,
    /// Why quilt did not run at all (no quilt, no archive, an archive that
    /// would not unpack). The patches are then all `NotChecked`.
    pub not_run: Option<String>,
}

impl CheckReport {
    pub fn failing(&self) -> impl Iterator<Item = &PatchResult> {
        self.patches
            .iter()
            .filter(|p| matches!(p.status, PatchStatus::Fails { .. }))
    }

    /// The patches that applied with fuzz: the ones a refresh makes exact.
    pub fn to_refresh(&self) -> impl Iterator<Item = &PatchResult> {
        self.patches.iter().filter(|p| {
            matches!(
                p.status,
                PatchStatus::AppliesWithOffsetOrFuzz { fuzz: true, .. }
            )
        })
    }

    pub fn already_applied(&self) -> impl Iterator<Item = &PatchResult> {
        self.patches
            .iter()
            .filter(|p| matches!(p.status, PatchStatus::AlreadyApplied { .. }))
    }

    pub fn tally(&self) -> Tally {
        Tally {
            failing: self.failing().count(),
            refresh: self.to_refresh().count(),
            upstream: self.already_applied().count(),
        }
    }

    /// Whether quilt proved `name` already applied in the sources.
    pub fn proves_applied(&self, name: &str) -> bool {
        self.already_applied().any(|p| p.name == name)
    }

    /// Nothing fails and nothing needs a refresh. Unchecked patches are not
    /// problems quilt found, but they are not called clean either: see
    /// [`CheckReport::summary`].
    pub fn is_clean(&self) -> bool {
        self.tally() == Tally::default()
    }

    /// One line for the log.
    pub fn summary(&self) -> String {
        if let Some(why) = &self.not_run {
            return format!("the patches were not checked with quilt: {why}");
        }
        let count =
            |f: fn(&PatchStatus) -> bool| self.patches.iter().filter(|p| f(&p.status)).count();
        let clean = count(|s| {
            matches!(
                s,
                PatchStatus::Applies | PatchStatus::AppliesWithOffsetOrFuzz { fuzz: false, .. }
            )
        });
        let refresh =
            count(|s| matches!(s, PatchStatus::AppliesWithOffsetOrFuzz { fuzz: true, .. }));
        let failing = count(|s| matches!(s, PatchStatus::Fails { .. }));
        let upstream = count(|s| matches!(s, PatchStatus::AlreadyApplied { .. }));
        let skipped: Vec<String> = self
            .patches
            .iter()
            .filter_map(|p| match &p.status {
                PatchStatus::NotChecked { reason } => Some(format!("{} ({reason})", p.name)),
                _ => None,
            })
            .collect();
        let mut text = format!(
            "quilt: {clean} of {} patch(es) apply cleanly, {refresh} need a refresh, {failing} fail",
            self.patches.len()
        );
        if upstream > 0 {
            text.push_str(&format!(", {upstream} already applied upstream"));
        }
        if !skipped.is_empty() {
            text.push_str(&format!("; not checked: {}", skipped.join(", ")));
        }
        text
    }
}

/// Whether the check rewrites patches that applied with fuzz with `quilt
/// refresh`, and returns them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refresh {
    No,
    /// Refresh each patch that applied with fuzz: quilt makes it exact, and
    /// rpm's `%autosetup` (`--fuzz=0`) applies only exact patches. A patch
    /// that applied with an offset alone is left byte-identical.
    Fuzz,
}

/// The `PatchN:` lines of `spec`, as `(number, value)`; a bare `Patch:` is 0.
pub(super) fn patch_lines(spec: &str) -> Vec<(String, String)> {
    spec.lines()
        .filter_map(|l| crate::spec::parse_source_line(l, &["patch"], str::to_string))
        .map(|patch| (patch.index.to_string(), patch.url.trim().to_string()))
        .collect()
}

/// The `PatchN:` of `spec` as `(number, file name)`, the name expanded as the
/// check expands it.
pub fn declared_patches(spec: &str) -> Vec<(String, String)> {
    patch_lines(spec)
        .into_iter()
        .map(|(n, raw)| (n, crate::pipeline::expand_patch_name(&raw, spec)))
        .collect()
}

/// What a turn did to the patches: `(refreshed, removed)`. `before` holds each
/// patch the spec declared before the turn with its file's content (`None`
/// when not on disk); `after_names` are the patches the spec declares after it
/// and `after` their contents now. A patch is removed when the spec no longer
/// declares it or its file is gone, refreshed when its content changed.
pub fn diff_patches(
    before: &std::collections::BTreeMap<String, Option<Vec<u8>>>,
    after_names: &[String],
    after: &std::collections::BTreeMap<String, Option<Vec<u8>>>,
) -> (Vec<String>, Vec<String>) {
    let mut refreshed = Vec::new();
    let mut removed = Vec::new();
    for (name, old) in before {
        let now = after.get(name).cloned().flatten();
        if !after_names.contains(name) || (old.is_some() && now.is_none()) {
            removed.push(name.clone());
        } else if old.is_some() && now != *old {
            refreshed.push(name.clone());
        }
    }
    (refreshed, removed)
}

/// The spec's patches in the order rpm applies them, each with what the check
/// does with it. `%autosetup` and `%autopatch` apply by patch number; a spec
/// with only `%patch` lines applies in the order of those lines.
pub fn plan_series(spec: &str) -> Vec<Planned> {
    let lines: Vec<&str> = spec.lines().collect();
    let first = |l: &str| l.split_whitespace().next().unwrap_or("").to_string();
    let auto = lines.iter().any(|l| {
        let f = first(l);
        f == "%autopatch" || (f == "%autosetup" && !l.split_whitespace().any(|t| t == "-N"))
    });
    let patch_line_index = |number: &str| {
        lines.iter().position(|l| {
            first(l).starts_with("%patch")
                && crate::pipeline::applied_patch_number(l)
                    .is_some_and(|n| crate::pipeline::same_patch_number(&n, number))
        })
    };
    let mut planned: Vec<(usize, u64, Planned)> = Vec::new();
    for (number, raw) in patch_lines(spec) {
        let name = crate::pipeline::expand_patch_name(&raw, spec);
        let numeric: u64 = number.parse().unwrap_or(u64::MAX);
        let explicit = patch_line_index(&number);
        let skip = |reason: String| Planned::Skip {
            name: name.clone(),
            reason,
        };
        let plan = if name.is_empty()
            || name.contains('%')
            || Path::new(&name).file_name() != Some(name.as_ref())
        {
            skip(format!(
                "`{raw}` is not a plain file name once %{{name}} and %{{version}} are expanded"
            ))
        } else if !auto && explicit.is_none() {
            skip("the spec does not apply it (no %autosetup, %autopatch or %patch line)".into())
        } else {
            let level = crate::pipeline::spec_patch_level(spec, &number);
            if SUPPORTED_LEVELS.contains(&level) {
                Planned::Check(SeriesEntry {
                    number: number.clone(),
                    name: name.clone(),
                    level,
                })
            } else {
                skip(format!(
                    "it applies with -p{level}; only -p0 and -p1 are checked"
                ))
            }
        };
        let order = if auto {
            0
        } else {
            explicit.unwrap_or(usize::MAX)
        };
        planned.push((order, numeric, plan));
    }
    // A stable sort: equal keys keep the spec's order.
    planned.sort_by_key(|(order, numeric, _)| if auto { (0, *numeric) } else { (*order, 0) });
    planned.into_iter().map(|(_, _, p)| p).collect()
}

/// The quilt `series` file for `entries`: one name per line, with its `-pN`
/// when that is not quilt's default `-p1`.
pub fn series_text(entries: &[SeriesEntry]) -> String {
    entries
        .iter()
        .map(|e| {
            if e.level == 1 {
                format!("{}\n", e.name)
            } else {
                format!("{} -p{}\n", e.name, e.level)
            }
        })
        .collect()
}

/// The `.changes` bullet for the refreshed patches, in the order given: one
/// patch is `Refresh <patch>`; two or more are one `Refresh patches:` bullet
/// with an `  * <patch>` item each. The packaged version is not named: the
/// release's `Update to` entry already does.
pub fn refresh_bullet(names: &[&str]) -> String {
    match names {
        [] => String::new(),
        [one] => format!("Refresh {one}"),
        many => {
            let items: String = many.iter().map(|n| format!("\n  * {n}")).collect();
            format!("Refresh patches:{items}")
        }
    }
}

/// How a patch removal is proven: quilt's `can be reverse-applied`, or each
/// hunk's added lines already in the new source archive (see `present`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DropProof {
    ReverseApplies,
    InTarball,
}

/// The `.changes` bullet for a patch removed because its change is already
/// upstream: the bullet states the proof.
pub fn proven_drop_bullet(name: &str, proof: DropProof) -> String {
    let how = match proof {
        DropProof::ReverseApplies => "reverse-applies cleanly",
        DropProof::InTarball => "already in the new tarball",
    };
    format!("Drop {name}: already applied upstream ({how})")
}
