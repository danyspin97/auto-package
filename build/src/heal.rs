//! Start-up healing of what `state.json`, a package's checkout and OBS say
//! about each other. A fix the tool can make safely is made here and recorded
//! in the package's `reconciled.log`; what only a person can settle is left to
//! the self-check in `doctor`, which says the one thing to do. The decisions
//! are pure: the callers read the files and ask OBS, then act on the answer.

use crate::osc::BuildStatus;

/// What to do with a file the publish plan leaves out of a checkout.
#[derive(Debug, PartialEq, Eq)]
pub enum ArchiveAction {
    /// Delete the local copy: the tool's own scratch of another release.
    Delete,
    /// Leave it alone.
    Keep,
}

/// Whether a file the publish plan skipped is a leftover archive of another
/// release that the tool may delete from the checkout. That is an archive the
/// spec does not name, that OBS does not track, and whose name does not carry
/// the spec's version. An archive OBS tracks is never deleted here: the next
/// commit removes it from OBS on purpose, as a version bump does.
pub fn stale_archive_action(
    file: &str,
    is_archive: bool,
    tracked: bool,
    named: bool,
    spec_version: &str,
) -> ArchiveAction {
    if is_archive && !tracked && !named && !crate::doctor::names_version(file, spec_version) {
        ArchiveAction::Delete
    } else {
        ArchiveAction::Keep
    }
}

/// Whether a recorded review is of a version other than the spec's. It does
/// not apply to the spec as it is, so it is dropped; the next run reviews the
/// spec again, which costs one review at most.
pub fn is_stale_review(reviewed: Option<&str>, spec_version: Option<&str>) -> bool {
    matches!((reviewed, spec_version), (Some(r), Some(v)) if r != v)
}

/// Whether the output of `osc status` shows a change the user made. The tool's
/// own quilt `series` scratch is not one while OBS does not track it: the
/// publish plan leaves it out of every commit anyway.
pub fn has_user_changes(status: &str, tracked: &[String]) -> bool {
    status
        .lines()
        .filter_map(|line| line.split_whitespace().last())
        .any(|name| !(name == "series" && !tracked.iter().any(|t| t == name)))
}

/// What the OBS comparison of one package decides.
#[derive(Debug, PartialEq, Eq)]
pub enum ObsDecision {
    /// Not a package OBS built while the state says it did not: nothing to do.
    NotApplicable,
    /// OBS built the version the spec says from a clean checkout: the RPMs are
    /// fetched and the state recorded as built.
    Fetch,
    /// OBS built it, but the tool cannot take the RPMs safely. The text says
    /// the one thing to do.
    Decline(String),
}

/// Decide what the start-up reconcile does for a package whose state says it
/// did not build and whose OBS status is `obs`. Only a state of `failed`,
/// `interrupted` or `missing_deps` with OBS `succeeded` applies. The version
/// the state last built must be the spec's, and the checkout must have no local
/// changes (`dirty` is asked last, as it runs `osc status`).
pub fn obs_built_decision(
    outcome: Option<&str>,
    obs: Option<&BuildStatus>,
    spec_version: Option<&str>,
    last_built: Option<&str>,
    dirty: impl FnOnce() -> bool,
) -> ObsDecision {
    let applies = matches!(outcome, Some("failed" | "interrupted" | "missing_deps"))
        && obs == Some(&BuildStatus::Succeeded);
    if !applies {
        return ObsDecision::NotApplicable;
    }
    let Some(spec) = spec_version else {
        return ObsDecision::Decline(
            "its spec cannot be read in the checkout; restore the checkout, then run update for it"
                .to_string(),
        );
    };
    if last_built != Some(spec) {
        return ObsDecision::Decline(format!(
            "state.json says the last build was {} and the spec says {spec}; run update for it \
             to build {spec}",
            last_built.unwrap_or("nothing")
        ));
    }
    if dirty() {
        return ObsDecision::Decline(
            "the checkout has local changes; discard or commit them, and the next run takes \
             the RPMs"
                .to_string(),
        );
    }
    ObsDecision::Fetch
}

/// Heal the checkouts and the state files of every package that has one,
/// without asking OBS: a leftover archive of another release is deleted from
/// its checkout, and a recorded review of another version is dropped. Returns
/// `(package, what was done)` for each change, which is also written to the
/// package's `reconciled.log`.
pub fn checkouts(
    state_root: &std::path::Path,
    checkout_root: &std::path::Path,
) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for name in package_names(state_root) {
        let Some(ws) = crate::workspace::Workspace::find_existing(state_root, &[&name]) else {
            continue;
        };
        let state = crate::pipeline::PkgState::load(&ws);
        let obs = state.obs_name.clone().unwrap_or_else(|| name.clone());
        let checkout = checkout_root.join(&obs);
        let Ok(spec) = std::fs::read_to_string(checkout.join(format!("{obs}.spec"))) else {
            continue;
        };
        let Some(version) = crate::spec::parse_header(&spec).map(|h| h.version) else {
            continue;
        };
        // A wheel switch keeps its own leftovers (see `doctor`): not touched here.
        let wheel = is_wheel_spec(&spec);
        let mut whats = Vec::new();
        if !wheel && checkout.is_dir() {
            for file in drop_stale_archives(&checkout, &spec, &version) {
                whats.push(format!("removed the stale archive {file}"));
            }
        }
        if is_stale_review(state.reviewed_version.as_deref(), Some(&version)) {
            let saved = crate::pipeline::PkgState::update(&ws, |st| {
                st.reviewed_version = None;
                st.reviewed_spec_hash = None;
            });
            if saved.is_ok() {
                whats.push(format!(
                    "dropped the review of {} (the spec says {version})",
                    state.reviewed_version.as_deref().unwrap_or_default()
                ));
            }
        }
        for what in whats {
            record(&ws, &what);
            out.push((name.clone(), what));
        }
    }
    out
}

/// The files of `checkout` the publish plan leaves out that
/// [`stale_archive_action`] deletes. Deleted through `osc::discard_untracked_archives`,
/// which also refuses anything named or tracked. Returns the names deleted.
fn drop_stale_archives(checkout: &std::path::Path, spec: &str, version: &str) -> Vec<String> {
    let named = crate::osc::spec_source_names(spec);
    let tracked = crate::osc::tracked_files(checkout);
    let mut gone = Vec::new();
    for file in crate::osc::publish_plan(checkout).skipped {
        let is_archive = crate::osc::ARCHIVE_SUFFIXES
            .iter()
            .any(|s| file.ends_with(s));
        let action = stale_archive_action(
            &file,
            is_archive,
            tracked.contains(&file),
            crate::doctor::spec_names_archive(spec, &file),
            version,
        );
        if action == ArchiveAction::Delete {
            gone.extend(crate::osc::discard_untracked_archives(
                checkout,
                std::slice::from_ref(&file),
                &named,
            ));
        }
    }
    gone
}

/// Whether the spec builds from a wheel (one of its `Source:` lines names a
/// `.whl`), as `doctor` reads it.
fn is_wheel_spec(spec: &str) -> bool {
    spec.lines().any(|l| {
        crate::spec::parse_source_line(l.trim_start(), &["source"], str::to_string).is_some()
            && l.contains(".whl")
    })
}

/// The one line that says what the start-up reconcile changed, one entry per
/// package with what was done to it (`changes` is `(package, what)`), or `None`
/// when nothing changed. Names are sorted and each appears once.
pub fn summary_line(changes: &[(String, String)]) -> Option<String> {
    let mut names: Vec<&str> = changes.iter().map(|(n, _)| n.as_str()).collect();
    names.sort_unstable();
    names.dedup();
    if names.is_empty() {
        return None;
    }
    let items: Vec<String> = names
        .iter()
        .map(|name| {
            let whats: Vec<&str> = changes
                .iter()
                .filter(|(n, _)| n == name)
                .map(|(_, w)| w.as_str())
                .collect();
            format!("{name} ({})", whats.join("; "))
        })
        .collect();
    let noun = if names.len() == 1 {
        "package"
    } else {
        "packages"
    };
    Some(format!(
        "note: reconciled {} {noun} with OBS: {}",
        names.len(),
        items.join(", ")
    ))
}

/// Append one line saying what was reconciled in a package's state directory.
pub(crate) fn record(ws: &crate::workspace::Workspace, what: &str) {
    use std::io::Write;
    let path = ws.dir().join("reconciled.log");
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        let _ = writeln!(file, "{what}");
    }
}

/// The names of the packages with a `state.json` under `state_root`, sorted.
pub(crate) fn package_names(state_root: &std::path::Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(state_root)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.path().join("state.json").is_file())
        .filter_map(|e| e.file_name().into_string().ok())
        .collect();
    names.sort();
    names
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_leftover_archive_of_another_release_is_deleted_only_when_nothing_owns_it() {
        let stale = |tracked, named| {
            stale_archive_action("pyenv-2.8.4.tar.gz", true, tracked, named, "2.8.14")
        };
        assert_eq!(stale(false, false), ArchiveAction::Delete);
        // OBS holds it: the next commit removes it from OBS, not the reconcile.
        assert_eq!(stale(true, false), ArchiveAction::Keep);
        // The spec names it.
        assert_eq!(stale(false, true), ArchiveAction::Keep);
        // Not an archive: the user's file, left alone.
        assert_eq!(
            stale_archive_action("notes.txt", false, false, false, "2.8.14"),
            ArchiveAction::Keep
        );
        // The spec's own version is not an old release.
        assert_eq!(
            stale_archive_action("pyenv-2.8.14.tar.gz", true, false, false, "2.8.14"),
            ArchiveAction::Keep
        );
    }

    #[test]
    fn a_review_of_another_version_is_stale_and_one_of_the_spec_is_not() {
        assert!(is_stale_review(Some("0.7.12"), Some("0.7.11")));
        assert!(!is_stale_review(Some("0.7.11"), Some("0.7.11")));
        assert!(!is_stale_review(None, Some("0.7.11")));
        assert!(!is_stale_review(Some("0.7.12"), None));
    }

    #[test]
    fn only_a_failed_state_that_obs_built_is_decided() {
        let dirty = || panic!("not asked for this outcome");
        for outcome in [None, Some("success"), Some("skipped"), Some("running")] {
            assert_eq!(
                obs_built_decision(
                    outcome,
                    Some(&BuildStatus::Succeeded),
                    Some("1.0"),
                    Some("1.0"),
                    dirty
                ),
                ObsDecision::NotApplicable,
                "{outcome:?}"
            );
        }
        assert_eq!(
            obs_built_decision(
                Some("failed"),
                Some(&BuildStatus::Failed),
                Some("1.0"),
                Some("1.0"),
                dirty
            ),
            ObsDecision::NotApplicable
        );
    }

    #[test]
    fn a_clean_checkout_at_the_recorded_version_is_fetched() {
        for outcome in ["failed", "interrupted", "missing_deps"] {
            assert_eq!(
                obs_built_decision(
                    Some(outcome),
                    Some(&BuildStatus::Succeeded),
                    Some("2.7.1"),
                    Some("2.7.1"),
                    || false
                ),
                ObsDecision::Fetch,
                "{outcome}"
            );
        }
    }

    #[test]
    fn a_checkout_with_changes_or_another_version_is_declined_with_the_one_thing_to_do() {
        let built = Some(&BuildStatus::Succeeded);
        let dirty = obs_built_decision(Some("failed"), built, Some("2.0"), Some("2.0"), || true);
        assert!(
            matches!(&dirty, ObsDecision::Decline(r) if r.contains("discard or commit")),
            "{dirty:?}"
        );
        let other = obs_built_decision(Some("failed"), built, Some("2.0"), Some("1.0"), || false);
        assert!(
            matches!(&other, ObsDecision::Decline(r) if r.contains("the spec says 2.0") && r.contains("run update")),
            "{other:?}"
        );
        let unread = obs_built_decision(Some("failed"), built, None, Some("1.0"), || false);
        assert!(matches!(unread, ObsDecision::Decline(_)), "{unread:?}");
        // A state that never built says so rather than `None`.
        let never = obs_built_decision(Some("failed"), built, Some("2.0"), None, || false);
        assert!(
            matches!(&never, ObsDecision::Decline(r) if r.contains("nothing")),
            "{never:?}"
        );
    }

    #[test]
    fn the_tools_series_scratch_is_not_a_user_change_unless_obs_holds_it() {
        let none: Vec<String> = Vec::new();
        assert!(!has_user_changes("", &none));
        assert!(!has_user_changes("A    series\n", &none));
        assert!(has_user_changes("A    series\n", &["series".to_string()]));
        assert!(has_user_changes("M    pyenv.spec\n", &none));
        assert!(has_user_changes(
            "?    pyenv-2.8.14.tar.gz\nA    series\n",
            &none
        ));
    }

    #[test]
    fn the_summary_names_each_package_once_with_what_was_done() {
        assert_eq!(summary_line(&[]), None);
        let changes = [
            (
                "pyenv".to_string(),
                "removed the stale archive pyenv-2.8.4.tar.gz".to_string(),
            ),
            ("cookiecutter".to_string(), "took OBS's RPMs".to_string()),
            (
                "pyenv".to_string(),
                "dropped the review of 2.8.4 (the spec says 2.8.14)".to_string(),
            ),
        ];
        assert_eq!(
            summary_line(&changes).as_deref(),
            Some(
                "note: reconciled 2 packages with OBS: cookiecutter (took OBS's RPMs), pyenv \
                 (removed the stale archive pyenv-2.8.4.tar.gz; dropped the review of 2.8.4 \
                 (the spec says 2.8.14))"
            )
        );
        let one = [("eric".to_string(), "took OBS's RPMs".to_string())];
        assert!(
            summary_line(&one)
                .unwrap()
                .starts_with("note: reconciled 1 package with OBS: eric")
        );
    }

    #[test]
    fn a_leftover_archive_of_another_release_is_removed_and_one_obs_holds_is_kept() {
        use crate::pipeline::PkgState;
        use crate::workspace::Workspace;
        let root = tempfile::tempdir().unwrap();
        let state_root = root.path().join("state");
        let proj = root.path().join("proj");
        let dir = proj.join("pyenv");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("pyenv.spec"),
            "Name: pyenv\nVersion: 2.8.14\nSource0: pyenv-%{version}.tar.gz\n",
        )
        .unwrap();
        std::fs::write(dir.join("pyenv-2.8.14.tar.gz"), "x").unwrap();
        std::fs::write(dir.join("pyenv-2.8.4.tar.gz"), "x").unwrap();
        let ws = Workspace::open(&state_root, "pyenv").unwrap();
        PkgState::update(&ws, |s| {
            s.reviewed_version = Some("2.8.4".into());
            s.reviewed_spec_hash = Some(1);
        })
        .unwrap();
        // The same leftover, but OBS holds it: the next commit removes it there.
        let tracked = proj.join("tracked");
        std::fs::create_dir_all(tracked.join(".osc")).unwrap();
        std::fs::write(
            tracked.join("tracked.spec"),
            "Name: tracked\nVersion: 2.0\nSource0: tracked-%{version}.tar.gz\n",
        )
        .unwrap();
        std::fs::write(tracked.join("tracked-2.0.tar.gz"), "x").unwrap();
        std::fs::write(tracked.join("tracked-1.0.tar.gz"), "x").unwrap();
        std::fs::write(
            tracked.join(".osc/_files"),
            "<directory name=\"tracked\"><entry name=\"tracked-1.0.tar.gz\" md5=\"x\" size=\"1\" mtime=\"1\" /></directory>",
        )
        .unwrap();
        Workspace::open(&state_root, "tracked").unwrap();

        let done = checkouts(&state_root, &proj);

        assert!(!dir.join("pyenv-2.8.4.tar.gz").exists());
        assert!(dir.join("pyenv-2.8.14.tar.gz").exists());
        assert!(tracked.join("tracked-1.0.tar.gz").exists());
        let st = PkgState::load(&ws);
        assert_eq!((st.reviewed_version, st.reviewed_spec_hash), (None, None));
        assert!(
            done.iter()
                .any(|(n, w)| n == "pyenv" && w.contains("stale archive pyenv-2.8.4.tar.gz")),
            "{done:?}"
        );
        assert!(
            done.iter()
                .any(|(n, w)| n == "pyenv" && w.contains("review of 2.8.4")),
            "{done:?}"
        );
        assert!(!done.iter().any(|(n, _)| n == "tracked"), "{done:?}");
        let log = std::fs::read_to_string(ws.dir().join("reconciled.log")).unwrap();
        assert!(log.contains("stale archive pyenv-2.8.4.tar.gz"), "{log}");
        // A second run has nothing left to do.
        assert!(checkouts(&state_root, &proj).is_empty());
    }
}
