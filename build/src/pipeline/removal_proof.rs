//! The proof a patch removal needs. A patch an edit removes is kept unless its
//! change is already in the new sources: quilt reports it `can be
//! reverse-applied`, or quilt reports it does not apply and each hunk's added
//! lines are in the new archive, or every file it patches is gone from the new
//! archive (not for a security patch). Every other removal is refused, one quilt
//! did not check included.
//!
//! A patch the edit keeps declared but stops applying (see
//! [`disabled_patches`](crate::patch_check::disabled_patches)) is judged the
//! same way: disabling a patch is a removal that leaves the file behind.
//!
//! The gate and the backstop of the patch rules each ask quilt on the same
//! inputs; nothing is cached, so each ask runs the check again.

use super::*;
use crate::patch_check::{CheckReport, DropProof, PatchStatus, REVERSE_APPLIED};
use crate::pipeline::archive::*;
use crate::pipeline::rejection::*;
use crate::pipeline::snapshot::*;
use crate::pipeline::spec_edits::*;
use std::collections::BTreeMap;

/// How an edit took a patch out of the build.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum RemovalKind {
    /// The declaration or the file is gone.
    Removed,
    /// Still declared, but no longer applied as it was.
    Disabled(crate::patch_check::Disabled),
}

/// A patch an edit removed or disabled without a proof (see [`proof_of`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Refusal {
    pub(super) name: String,
    pub(super) kind: RemovalKind,
    /// The patch is a security fix (see [`is_security_patch`]).
    pub(super) security: bool,
    /// The patch the edit declares in its place with the same bytes: a rename,
    /// which is not judged.
    pub(super) renamed_to: Option<String>,
    /// Why no proof was made.
    pub(super) reason: String,
}

/// What the gate decided about the patches an edit removed.
#[derive(Debug, Default, Clone)]
pub(super) struct RemovalJudgement {
    /// Removed with a proof, and the proof the `.changes` bullet states.
    pub(super) proven: Vec<(String, DropProof)>,
    /// Removed without one: refused.
    pub(super) refused: Vec<Refusal>,
    /// The names in `proven` that the edit disabled and did not remove: the
    /// patch is still declared and its file is still there.
    pub(super) disabled: Vec<String>,
}

/// Whether a declared patch name is a file the gate judges: a relative path
/// inside the checkout. `sub/x.patch` is one (quilt does not check it, so its
/// removal is refused); an empty name, an absolute path and `..` are not.
pub(super) fn is_checkout_file(name: &str) -> bool {
    let path = Path::new(name);
    !name.is_empty()
        && path.is_relative()
        && path
            .components()
            .all(|c| matches!(c, std::path::Component::Normal(_)))
}

/// The `-pN` level the spec applies each declared patch with, by name.
fn patch_levels(spec: &str) -> BTreeMap<String, u32> {
    crate::patch_check::declared_patches(spec)
        .into_iter()
        .map(|(number, name)| (name, spec_patch_level(spec, &number)))
        .collect()
}

/// The archive a proof is made against: the one named for `version`, else the
/// only archive the checkout holds. With several archives and none named for
/// the version, `find_tarball` could pick the old version's, so there is none.
pub(super) fn proof_tarball(checkout_dir: &Path, version: &str) -> Option<PathBuf> {
    let found = find_tarball(checkout_dir, version)?;
    let name = found.file_name()?.to_string_lossy().into_owned();
    (names_version(&name, version) || source_archives(checkout_dir).len() == 1).then_some(found)
}

/// The patch files of `spec` as the gate reads them: by name, the content as the
/// snapshot kept it, else on disk.
pub(super) fn patch_files_of(
    checkout_dir: &Path,
    before: &str,
    snapshot: Option<&CheckoutSnapshot>,
) -> BTreeMap<String, Option<Vec<u8>>> {
    crate::patch_check::declared_patches(before)
        .into_iter()
        .filter(|(_, n)| is_checkout_file(n))
        .map(|(_, n)| {
            let content = snapshot
                .and_then(|s| s.content_of(&n))
                .or_else(|| std::fs::read(checkout_dir.join(&n)).ok());
            (n, content)
        })
        .collect()
}

/// The proof for removing `name`, given quilt's `report` on the patch files and
/// the tarball. `Err` is why no proof was made.
pub(super) fn proof_of(
    report: &CheckReport,
    tarball: Option<&Path>,
    diff: Option<&[u8]>,
    level: u32,
    name: &str,
) -> Result<DropProof, String> {
    match report
        .patches
        .iter()
        .find(|p| p.name == name)
        .map(|p| &p.status)
    {
        Some(PatchStatus::AlreadyApplied { .. }) => Ok(DropProof::ReverseApplies),
        // Only a patch quilt says does not apply may be proven by the archive:
        // one that applies is not upstream. A patch quilt reverse-applies only
        // with fuzz is one such patch, and its hunks are judged the same way.
        Some(PatchStatus::Fails { output, .. }) => match (tarball, diff) {
            (Some(t), Some(d)) if crate::patch_check::already_in_tarball(t, d, level) => {
                Ok(DropProof::InTarball)
            }
            // A security patch is never dropped for its files being gone: the
            // proof above is the only one such a patch gets.
            (Some(t), Some(d))
                if !is_security_patch(name, d)
                    && crate::patch_check::targets_absent_in_tarball(t, d, level) =>
            {
                Ok(DropProof::TargetsAbsent)
            }
            _ if output.contains(REVERSE_APPLIED) => Err(
                "quilt says it `can be reverse-applied` only with fuzz, and not every hunk \
                 is in the new tarball with its added and removed lines"
                    .to_string(),
            ),
            _ => Err(
                "quilt checked it, it cannot be reverse-applied on the new sources, \
                 and its hunks are not all in the new tarball"
                    .to_string(),
            ),
        },
        Some(PatchStatus::NotChecked { reason }) => Err(format!(
            "quilt did not check it ({})",
            report.not_run.clone().unwrap_or_else(|| reason.clone())
        )),
        Some(_) => Err(
            "quilt checked it and it applies to the new sources, so its change is not in them"
                .to_string(),
        ),
        None => Err("it is not in quilt's series".to_string()),
    }
}

/// Judges the patches an edit removed or disabled (the spec before it is
/// `before`, the patch files as in `patch_files_of`). Empty when it did
/// neither.
pub(super) fn judge_removals(
    version: &str,
    checkout_dir: &Path,
    spec_path: &Path,
    before: &str,
    snapshot: Option<&CheckoutSnapshot>,
) -> RemovalJudgement {
    let before_files = patch_files_of(checkout_dir, before, snapshot);
    if before_files.is_empty() {
        return RemovalJudgement::default();
    }
    let after = std::fs::read_to_string(spec_path).unwrap_or_default();
    let after_names: Vec<String> = crate::patch_check::declared_patches(&after)
        .into_iter()
        .map(|(_, n)| n)
        .collect();
    let after_files = before_files
        .keys()
        .map(|n| (n.clone(), std::fs::read(checkout_dir.join(n)).ok()))
        .collect();
    let (_, removed) = crate::patch_check::diff_patches(&before_files, &after_names, &after_files);
    let disabled: Vec<(String, crate::patch_check::Disabled)> =
        crate::patch_check::disabled_patches(before, &after)
            .into_iter()
            .filter(|(n, _)| before_files.contains_key(n) && !removed.contains(n))
            .collect();
    let mut judged = RemovalJudgement::default();
    if removed.is_empty() && disabled.is_empty() {
        return judged;
    }
    let tarball = proof_tarball(checkout_dir, version);
    let report = crate::patch_check::check_files(before, &before_files, tarball.as_deref());
    let levels = patch_levels(before);
    let narrows = crate::patch_check::narrows_applications(before, &after);
    let kinds = removed
        .into_iter()
        .map(|n| (n, RemovalKind::Removed))
        .chain(
            disabled
                .into_iter()
                .map(|(n, how)| (n, RemovalKind::Disabled(how))),
        );
    for (name, kind) in kinds {
        let diff = before_files.get(&name).and_then(|c| c.as_deref());
        let level = levels.get(&name).copied().unwrap_or(1);
        let mut proof = proof_of(&report, tarball.as_deref(), diff, level, &name);
        // A disable by narrowing %autosetup or %autopatch is kept a refusal even
        // when proven: it would leave every later patch unapplied. The patch
        // is to be removed instead.
        if proof.is_ok() && narrows && matches!(kind, RemovalKind::Disabled(_)) {
            proof = Err(
                "quilt proved it already applied upstream, but narrowing %autosetup \
                         or %autopatch would leave every later patch unapplied: remove its \
                         `PatchN:` line and its file instead"
                    .to_string(),
            );
        }
        match proof {
            Ok(proof) => {
                if matches!(kind, RemovalKind::Disabled(_)) {
                    judged.disabled.push(name.clone());
                }
                judged.proven.push((name, proof));
            }
            Err(reason) => {
                let security = is_security_patch(&name, diff.unwrap_or_default());
                let renamed_to = diff
                    .filter(|d| !d.is_empty() && kind == RemovalKind::Removed)
                    .and_then(|d| {
                        after_names
                            .iter()
                            .filter(|n| **n != name && is_checkout_file(n))
                            .find(|n| {
                                std::fs::read(checkout_dir.join(n)).ok().as_deref() == Some(d)
                            })
                            .cloned()
                    });
                judged.refused.push(Refusal {
                    name,
                    kind,
                    security,
                    renamed_to,
                    reason,
                });
            }
        }
    }
    judged
}

/// Finish the patches an edit disabled with a proof (`judged.disabled`) as
/// removals: the `PatchN:` line, the `%patch` lines, the file and the `series`
/// line go. Done before the edit is accepted, so the acceptance checks see the
/// patch removed; a rejected edit restores them with the rest of the turn.
///
/// Only with a `snapshot`: the later checks read the removed file from it, and
/// without one they would find it gone and refuse. The spec is written first and
/// the files go only once it is written. `Ok(true)` when the patches were
/// finished, `Ok(false)` when there was nothing to do or no snapshot.
pub(super) fn finish_proven_disables(
    checkout_dir: &Path,
    spec_path: &Path,
    before: &str,
    judged: &RemovalJudgement,
    snapshot: Option<&CheckoutSnapshot>,
) -> Result<bool, String> {
    let names = &judged.disabled;
    if names.is_empty() || snapshot.is_none() {
        return Ok(false);
    }
    let nums: Vec<String> = crate::patch_check::declared_patches(before)
        .into_iter()
        .filter(|(_, n)| names.contains(n))
        .map(|(num, _)| num)
        .collect();
    let spec = std::fs::read_to_string(spec_path).map_err(|e| e.to_string())?;
    let fixed = crate::pipeline::patch_stage::without_patch_applications(
        &without_patches(&spec, names),
        &nums,
    );
    if fixed != spec {
        std::fs::write(spec_path, &fixed)
            .map_err(|e| format!("cannot write {}: {e}", spec_path.display()))?;
    }
    for name in names {
        let _ = std::fs::remove_file(checkout_dir.join(name));
    }
    crate::pipeline::patch_stage::drop_series_lines(checkout_dir, names);
    Ok(true)
}

/// The patches whose removal is refused (see [`judge_removals`]).
pub(super) fn unproven_patch_removals(
    version: &str,
    checkout_dir: &Path,
    spec_path: &Path,
    before: &str,
    snapshot: Option<&CheckoutSnapshot>,
) -> Vec<Refusal> {
    judge_removals(version, checkout_dir, spec_path, before, snapshot).refused
}

/// `bullet` with the `Drop` bullet of each patch removed with a proof added, one per
/// line: the pipeline writes those, the agent does not. A patch the agent's own
/// bullet already drops (a line starting `Drop <name>`) gets no second one.
pub(super) fn with_drop_bullets(bullet: String, proven: &[(String, DropProof)]) -> String {
    proven.iter().fold(bullet, |text, (name, proof)| {
        let head = format!("Drop {name}");
        if text.lines().any(|l| {
            l.trim()
                .trim_start_matches(['-', '*', ' '])
                .starts_with(&head)
        }) {
            return text;
        }
        let drop = crate::patch_check::proven_drop_bullet(name, *proof);
        if text.trim().is_empty() {
            drop
        } else {
            format!("{text}\n{drop}")
        }
    })
}

/// The proof the `.changes` bullet of a removal states, `None` when no proof can
/// be made: the caller then states none.
pub(super) fn drop_proof(
    checkout_dir: &Path,
    version: &str,
    before: &str,
    before_files: &BTreeMap<String, Option<Vec<u8>>>,
    name: &str,
) -> Option<DropProof> {
    let tarball = proof_tarball(checkout_dir, version);
    let report = crate::patch_check::check_files(before, before_files, tarball.as_deref());
    let level = patch_levels(before).get(name).copied().unwrap_or(1);
    proof_of(
        &report,
        tarball.as_deref(),
        before_files.get(name).and_then(|c| c.as_deref()),
        level,
        name,
    )
    .ok()
}

/// Whether a patch is a security fix: its name carries a CVE id (`CVE-2024-1234`,
/// also glued as `cve2024-1234`), a `bsc` or `boo` reference as a name token
/// (`bsc#1234`, `boo1234`), or its header (the first 40 lines) cites one.
/// `boost-*` and `foo-boo-2d` are not.
pub(super) fn is_security_patch(name: &str, content: &[u8]) -> bool {
    let name_lower = name.to_ascii_lowercase();
    let tokens: Vec<&str> = name_lower
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|t| !t.is_empty())
        .collect();
    let all_digits = |t: &str| !t.is_empty() && t.chars().all(|c| c.is_ascii_digit());
    let name_hit = tokens.iter().enumerate().any(|(i, t)| {
        let next_is_digits = tokens.get(i + 1).is_some_and(|n| all_digits(n));
        if matches!(*t, "cve" | "bsc" | "boo") {
            // `CVE-2024-1234`, `bsc#1234`, `boo-1234`: the id is the next token.
            next_is_digits
        } else if let Some(year) = t.strip_prefix("cve") {
            // Glued: `cve2024-1234`.
            all_digits(year) && next_is_digits
        } else if let Some(n) = t.strip_prefix("bsc").or_else(|| t.strip_prefix("boo")) {
            // Glued: `bsc1234`, `boo1234`.
            all_digits(n) && n.len() > 1
        } else {
            false
        }
    });
    if name_hit {
        return true;
    }
    String::from_utf8_lossy(content)
        .lines()
        .take(40)
        .any(|line| cites_id(&line.to_ascii_lowercase()))
}

/// Whether `line` cites `CVE-\d+-\d+`, `bsc#\d+` or `boo#\d+`.
fn cites_id(line: &str) -> bool {
    let after = |prefix: &str| {
        line.match_indices(prefix)
            .any(|(i, _)| line[i + prefix.len()..].starts_with(|c: char| c.is_ascii_digit()))
    };
    after("bsc#") || after("boo#") || {
        line.match_indices("cve-").any(|(i, _)| {
            let rest = &line[i + 4..];
            let digits = rest.chars().take_while(|c| c.is_ascii_digit()).count();
            digits > 0
                && rest[digits..].starts_with('-')
                && rest[digits + 1..].starts_with(|c: char| c.is_ascii_digit())
        })
    }
}

/// Why disabling a patch is refused: what the edit did to it.
fn disabling_cause(how: &crate::patch_check::Disabled) -> String {
    use crate::patch_check::Disabled;
    match how {
        Disabled::NotApplied => {
            "the edit keeps its `PatchN:` line but no line of %prep applies it \
             any more (a deleted or commented-out `%patch`, `%autosetup -N`, an `%autopatch` \
             range or list that leaves it out, `-R`, or the application moved out of %prep)"
                .to_string()
        }
        Disabled::NotDeclared { guard } => {
            format!("the edit puts its `PatchN:` line under `{guard}`, which is always false")
        }
        Disabled::Conditional { guard } => format!(
            "the edit applies it only when `{guard}` holds, a condition the gate cannot \
             evaluate (build flags and the project config decide it)"
        ),
    }
}

/// The refusal text for the removals and disablings `names` (the edit is not
/// kept).
pub(super) fn unproven_removal_problem(names: &[Refusal]) -> String {
    names
        .iter()
        .map(|r| {
            let security = if r.security {
                " It is a security patch (CVE, bsc or boo): it is never removed without \
                 the proof."
            } else {
                ""
            };
            if let RemovalKind::Disabled(how) = &r.kind {
                return format!(
                    "disabling {n} is refused: {cause}. A patch stops being applied only when \
                     its change is already in the new sources, and {reason}. Apply it as before \
                     and refresh it with quilt (`quilt push -f`, fix the rejected hunks, `quilt \
                     refresh`).{security} If it cannot be refreshed, stop and say so: a human \
                     must decide.",
                    n = r.name,
                    cause = disabling_cause(how),
                    reason = r.reason
                );
            }
            if let Some(new) = &r.renamed_to {
                return format!(
                    "removal of {n} is refused: {new} has the same content, so it looks \
                     renamed, and a rename is not judged. Keep the name {n}.{security} If it \
                     must be renamed, stop and say so: a human must decide.",
                    n = r.name
                );
            }
            format!(
                "removal of {n} is refused: {reason}. Keep the patch and refresh it with \
                 quilt (`quilt push -f`, fix the rejected hunks, `quilt refresh`).{security} If \
                 it cannot be refreshed, stop and say so: a human must decide.",
                n = r.name,
                reason = r.reason
            )
        })
        .collect::<Vec<_>>()
        .join("; ")
}

/// Roll back a turn whose edit removed patches without proof, and journal why.
pub(super) fn reject_unproven_removal(
    spec_path: &Path,
    before: &str,
    snapshot: Option<&CheckoutSnapshot>,
    step: &StepDir,
    reporter: &dyn Reporter,
    notes_path: PathBuf,
    keep: &[String],
    label: &str,
    unproven: &[Refusal],
) -> String {
    let problem = unproven_removal_problem(unproven);
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
            "\n## {label} — edit rejected: a patch removal or disable is not proven\n{problem}\n\
             A patch is never removed or disabled because it does not apply. The edit was rolled back.\n"
        ),
    );
    problem
}

/// The security patches a wheel switch of the spec `before` would drop: a wheel
/// carries no source patches, so every patch the spec declares goes with it, and
/// those that [`is_security_patch`] are not the switch's to drop. Read from the
/// snapshot in preference to the disk, like the removal gate.
pub(super) fn wheel_switch_security_drops(
    checkout_dir: &Path,
    before: &str,
    snapshot: Option<&CheckoutSnapshot>,
) -> Vec<String> {
    patch_files_of(checkout_dir, before, snapshot)
        .into_iter()
        .filter(|(n, content)| is_security_patch(n, content.as_deref().unwrap_or_default()))
        .map(|(n, _)| n)
        .collect()
}

/// Why a wheel switch that would drop the security patches `dropped` is
/// refused: the package needs a maintainer.
pub(super) fn wheel_drop_problem(dropped: &[String]) -> String {
    let plural = if dropped.len() > 1 { "es" } else { "" };
    format!(
        "needs a maintainer: a wheel switch would drop the security patch{plural} {}; a wheel \
         carries no source patches: a maintainer must decide",
        dropped.join(", ")
    )
}

/// Roll back a refused wheel switch (`problem`, see [`wheel_drop_problem`]): the
/// turn's own edits go (keeping the staged files) and the refusal is journaled.
/// The switch wrote nothing. The error is the refusal.
pub(super) fn reject_wheel_security_drop(
    spec_path: &Path,
    before: &str,
    snapshot: Option<&CheckoutSnapshot>,
    step: &StepDir,
    reporter: &dyn Reporter,
    notes_path: PathBuf,
    keep: &[String],
    problem: String,
) -> TurnError {
    if let Some(snap) = snapshot {
        snap.restore_keeping(reporter, Some(step), keep);
    }
    restore_spec(spec_path, before, Some(step), reporter);
    reporter.report(&Event::Warning {
        step: Some(step.id),
        message: format!("wheel switch refused: {problem}"),
    });
    append_notes(
        notes_path,
        &format!(
            "\n## Wheel switch refused — drops a security patch\n{problem}\n\
             Do NOT return `use_upstream_wheel` again for this package: the answer stays the \
             same. Fix the build from source, or request what it needs.\n"
        ),
    );
    TurnError::needs_maintainer(problem)
}
