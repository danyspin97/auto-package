//! Startup self-check: what the work directory says that its parts disagree
//! about, found before the run builds on it. Every finding is a warning, never
//! an error: the run goes on, and the finding says what it will do about it.

use crate::osc::BuildStatus;
use crate::pipeline::PkgState;
use crate::workspace::Workspace;
use std::collections::HashMap;
use std::path::Path;

/// Check every package with a `state.json` under `state_root` against its
/// checkout under `checkout_root` and against `statuses`, which is called at
/// most once and says what OBS shows for each package (`None`: unknown, the
/// OBS comparison is skipped). Returns one finding per problem.
pub fn run(
    state_root: &Path,
    checkout_root: &Path,
    statuses: impl FnOnce() -> Option<HashMap<String, BuildStatus>>,
) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(state_root)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.path().join("state.json").is_file())
        .filter_map(|e| e.file_name().into_string().ok())
        .collect();
    names.sort();
    let statuses = statuses().map(|m| {
        m.into_iter()
            .map(|(k, v)| (k.to_lowercase(), v))
            .collect::<HashMap<_, _>>()
    });
    let mut findings = Vec::new();
    for name in names {
        let Some(ws) = Workspace::find_existing(state_root, &[&name]) else {
            continue;
        };
        if let Ok(text) = std::fs::read_to_string(ws.state_json())
            && let Err(e) = serde_json::from_str::<PkgState>(&text)
        {
            findings.push(format!(
                "{name}: state.json cannot be read ({e}); it is treated as empty"
            ));
        }
        let state = PkgState::load(&ws);
        // The checkout is named by the OBS name the state records, as the
        // pipeline and `reconcile_with_obs` name it; `rpm_name` otherwise.
        let checkout = checkout_root.join(state.obs_name.as_deref().unwrap_or(&name));
        findings.extend(check_checkout(&name, &state, &ws, &checkout));
        if let Some(statuses) = &statuses
            && let Some(f) = check_obs(&name, &state, statuses.get(&name.to_lowercase()))
        {
            findings.push(f);
        }
    }
    findings
}

/// What the checkout and the state directory of one package say about each other.
fn check_checkout(name: &str, state: &PkgState, ws: &Workspace, checkout: &Path) -> Vec<String> {
    let mut out = Vec::new();
    let obs = state.obs_name.as_deref().unwrap_or(name);
    let spec = std::fs::read_to_string(checkout.join(format!("{obs}.spec"))).ok();
    // A spec that builds from a wheel got there by a switch that wrote its
    // provenance into the state directory (see the check below).
    let wheel_spec = spec.as_deref().is_some_and(|spec| {
        spec.lines()
            .any(|l| l.trim_start().starts_with("Source") && l.contains(".whl"))
    });
    if checkout.is_dir() {
        let archives = source_archives(checkout);
        let skipped: Vec<String> = crate::osc::publish_plan(checkout)
            .skipped
            .into_iter()
            // The switch's own files have a finding of their own below, and
            // the source archive it replaced is an expected leftover.
            .filter(|f| {
                !(f == "wheel-origin.txt"
                    || f.ends_with(".spec.pre-wheel-switch")
                    || (wheel_spec && archives.contains(f)))
            })
            .collect();
        if !skipped.is_empty() {
            out.push(format!(
                "{name}: the checkout has file(s) its spec does not reference, which are not \
                 committed: {}",
                skipped.join(", ")
            ));
        }
    }
    let Some(spec) = spec else {
        return out;
    };
    let spec_version = crate::spec::parse_header(&spec).map(|h| h.version);
    if state.last_outcome.as_deref() == Some("success")
        && let (Some(built), Some(spec_version)) = (&state.last_built_version, &spec_version)
        && built != spec_version
    {
        out.push(format!(
            "{name}: state.json records a successful build of {built} but the spec says \
             {spec_version}; the package is built again"
        ));
    }
    // A source archive for another release than the spec names: the spec was
    // not bumped with it, or the archive is left from an older release.
    if let Some(spec_version) = &spec_version {
        let archives = source_archives(checkout);
        if !archives.is_empty() && !archives.iter().any(|a| names_version(a, spec_version)) {
            out.push(format!(
                "{name}: the spec says {spec_version} but the checkout's source archive(s) are {}",
                archives.join(", ")
            ));
        }
    }
    if let (Some(reviewed), Some(version)) = (&state.reviewed_version, &spec_version)
        && reviewed != version
    {
        out.push(format!(
            "{name}: state.json records a review of {reviewed} but the spec says {version}"
        ));
    }
    // None there, or one left in the checkout, means the switch was made by an
    // older version of the tool; the pipeline copies it into the state
    // directory the next time it runs the package.
    let state_has = |f: &str| ws.dir().join(f).is_file();
    let switched = state_has(&format!("{name}.spec.pre-wheel-switch"))
        || checkout
            .join(format!("{name}.spec.pre-wheel-switch"))
            .is_file()
        || checkout.join("wheel-origin.txt").is_file();
    if wheel_spec && switched && !state_has("wheel-origin.txt") {
        out.push(format!(
            "{name}: the spec was switched to an upstream wheel but its wheel-origin.txt is \
             missing from the state directory"
        ));
    }
    out
}

/// Whether the archive file name `archive` names `version` as a whole version:
/// it starts the name or follows a `-`/`_` (a `v` may precede it, as in
/// `v1.0.zip`), and it is followed by the end, an archive or wheel extension,
/// or a `-py`/`-cp` tag, not by more of a version. `1.0` is in `a-1.0.tar.gz`,
/// not in `a-1.0.1.tar.gz` or `a-11.0.tar.gz`, and `foo-1.0-docs.tar.gz` names
/// another archive. The NVIDIA main archives (`…-13.8.0.4-archive.tar.xz`,
/// `…-9.27.0.42_cuda13-archive.tar.xz`) are also accepted.
fn names_version(archive: &str, version: &str) -> bool {
    let bare = version.replace('~', "");
    [version, bare.as_str()].iter().any(|v| {
        !v.is_empty()
            && archive.match_indices(*v).any(|(at, _)| {
                let before = &archive[..at];
                let before = before.strip_suffix(['v', 'V']).unwrap_or(before);
                let starts_here = before.is_empty() || before.ends_with(['-', '_']);
                let rest = &archive[at + v.len()..];
                starts_here
                    && (rest.is_empty()
                        || [
                            ".tar", ".zip", ".whl", ".tgz", "-py", "-cp", "_", "-archive",
                        ]
                        .iter()
                        .any(|ext| rest.starts_with(ext)))
            })
    })
}

/// File names of the source archives in a checkout.
fn source_archives(checkout: &Path) -> Vec<String> {
    let mut found: Vec<String> = std::fs::read_dir(checkout)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| e.file_name().into_string().ok())
        .filter(|n| {
            [".tar.gz", ".tar.xz", ".tar.bz2", ".tar.zst", ".tgz", ".zip"]
                .iter()
                .any(|ext| n.ends_with(ext))
        })
        .collect();
    found.sort();
    found
}

/// Whether OBS and `state.json` disagree about one package.
fn check_obs(name: &str, state: &PkgState, obs: Option<&BuildStatus>) -> Option<String> {
    let obs = obs?;
    let outcome = state.last_outcome.as_deref()?;
    let bad = matches!(
        obs,
        BuildStatus::Failed | BuildStatus::Unresolvable | BuildStatus::Broken
    );
    if outcome == "success" && bad {
        return Some(format!(
            "{name}: state.json says it built, but OBS shows {obs:?}; it is checked again"
        ));
    }
    if matches!(outcome, "failed" | "interrupted" | "missing_deps")
        && *obs == BuildStatus::Succeeded
    {
        return Some(format!(
            "{name}: state.json says {outcome}, but OBS shows it built (the start-up \
             reconcile fetches its RPMs when the checkout is clean and at the recorded version)"
        ));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(state_root: &Path, name: &str, f: impl FnOnce(&mut PkgState)) {
        let ws = Workspace::open(state_root, name).unwrap();
        PkgState::update(&ws, f).unwrap();
    }

    #[test]
    fn the_doctor_finds_what_the_parts_disagree_about() {
        let root = tempfile::tempdir().unwrap();
        let state_root = root.path().join("state");
        let checkouts = root.path().join("proj");
        std::fs::create_dir_all(checkouts.join("python-a")).unwrap();
        std::fs::write(
            checkouts.join("python-a/python-a.spec"),
            "Name: python-a\nVersion: 2.0\nSource0: https://h/a-2.0-py3-none-any.whl\n",
        )
        .unwrap();
        std::fs::write(checkouts.join("python-a/stray.bin"), "x").unwrap();
        std::fs::write(checkouts.join("python-a/wheel-origin.txt"), "x").unwrap();
        write(&state_root, "python-a", |s| {
            s.last_outcome = Some("success".into());
            s.last_built_version = Some("1.0".into());
        });
        write(&state_root, "python-b", |s| {
            s.last_outcome = Some("failed".into())
        });
        let statuses = HashMap::from([
            ("python-a".to_string(), BuildStatus::Failed),
            ("Python-B".to_string(), BuildStatus::Succeeded),
        ]);
        let found = run(&state_root, &checkouts, || Some(statuses));
        let all = found.join("\n");
        assert!(
            all.contains("python-a: the checkout has file(s)") && all.contains("stray.bin"),
            "{all}"
        );
        assert!(
            all.contains("python-a: state.json records a successful build of 1.0"),
            "{all}"
        );
        assert!(
            all.contains("python-a: the spec was switched to an upstream wheel"),
            "{all}"
        );
        assert!(
            all.contains("python-a: state.json says it built, but OBS shows Failed"),
            "{all}"
        );
        assert!(
            all.contains("python-b: state.json says failed, but OBS shows it built"),
            "{all}"
        );
    }

    #[test]
    fn the_checkout_is_looked_up_under_the_obs_name_the_state_records() {
        let root = tempfile::tempdir().unwrap();
        let state_root = root.path().join("state");
        let checkouts = root.path().join("proj");
        // The workspace is keyed by the rpm name; the checkout by the OBS name.
        std::fs::create_dir_all(checkouts.join("python-APScheduler")).unwrap();
        std::fs::write(
            checkouts.join("python-APScheduler/python-APScheduler.spec"),
            "Name: python-APScheduler\nVersion: 2.0\n",
        )
        .unwrap();
        std::fs::write(checkouts.join("python-APScheduler/stray.bin"), "x").unwrap();
        write(&state_root, "python-apscheduler", |s| {
            s.obs_name = Some("python-APScheduler".into());
            s.last_outcome = Some("success".into());
            s.last_built_version = Some("1.0".into());
        });
        let all = run(&state_root, &checkouts, || None).join("\n");
        assert!(
            all.contains("python-apscheduler: state.json records a successful build of 1.0 but the spec says 2.0"),
            "{all}"
        );
        assert!(all.contains("not committed: stray.bin"), "{all}");
        // Without obs_name the rpm name is the checkout: nothing is there to find.
        write(&state_root, "python-apscheduler", |s| s.obs_name = None);
        let all = run(&state_root, &checkouts, || None).join("\n");
        assert!(!all.contains("python-apscheduler"), "{all}");
    }

    #[test]
    fn a_stale_archive_and_an_unreadable_state_file_are_found() {
        let root = tempfile::tempdir().unwrap();
        let state_root = root.path().join("state");
        let checkouts = root.path().join("proj");
        std::fs::create_dir_all(checkouts.join("python-a")).unwrap();
        std::fs::write(
            checkouts.join("python-a/python-a.spec"),
            "Name: python-a\nVersion: 2.0\nSource0: a-2.0.tar.gz\n",
        )
        .unwrap();
        std::fs::write(checkouts.join("python-a/a-1.0.tar.gz"), "x").unwrap();
        write(&state_root, "python-a", |s| {
            s.last_outcome = Some("failed".into())
        });
        std::fs::create_dir_all(state_root.join("python-b")).unwrap();
        std::fs::write(state_root.join("python-b/state.json"), "{not json").unwrap();
        let all = run(&state_root, &checkouts, || None).join("\n");
        assert!(
            all.contains(
                "python-a: the spec says 2.0 but the checkout's source archive(s) are a-1.0.tar.gz"
            ),
            "{all}"
        );
        assert!(all.contains("python-b: state.json cannot be read"), "{all}");
        // The matching archive is no finding.
        std::fs::write(checkouts.join("python-a/a-2.0.tar.gz"), "x").unwrap();
        assert!(
            !run(&state_root, &checkouts, || None)
                .join("\n")
                .contains("python-a: the spec says")
        );
    }

    #[test]
    fn names_version_needs_a_separator_before_and_an_archive_after() {
        assert!(names_version("foo-1.0.tar.gz", "1.0"));
        assert!(names_version("foo_1.0.tgz", "1.0"));
        assert!(names_version("foo-1.0-py3-none-any.whl", "1.0"));
        assert!(names_version("foo-1.0-cp311-cp311-linux_x86_64.whl", "1.0"));
        // A digit before the match means it is part of another version.
        assert!(!names_version("a-11.0.tar.gz", "1.0"));
        assert!(!names_version("a-21.0.tar.gz", "1.0"));
        // Another archive's suffix is not this version's main archive.
        assert!(!names_version("foo-1.0-docs.tar.gz", "1.0"));
        assert!(!names_version("foo-1.0.post1.tar.gz", "1.0"));
    }

    #[test]
    fn archive_names_match_the_spec_version_and_wheel_leftovers_are_not_noise() {
        assert!(names_version("a-1.0.tar.gz", "1.0"));
        assert!(names_version(
            "libcublas-linux-x86_64-13.8.0.4-archive.tar.xz",
            "13.8.0.4"
        ));
        assert!(names_version(
            "cudnn-linux-x86_64-9.27.0.42_cuda13-archive.tar.xz",
            "9.27.0.42"
        ));
        assert!(names_version("v1.0.zip", "1.0"));
        assert!(!names_version("a-1.0.1.tar.gz", "1.0"));
        assert!(!names_version("a-11.0.tar.gz", "1.0.1"));
        let root = tempfile::tempdir().unwrap();
        let state_root = root.path().join("state");
        let checkouts = root.path().join("proj");
        let dir = checkouts.join("python-w");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("python-w.spec"),
            "Name: python-w\nVersion: 1.0\nSource0: w-1.0-py3-none-any.whl\n",
        )
        .unwrap();
        for f in [
            "w-1.0.tar.gz",
            "wheel-origin.txt",
            "python-w.spec.pre-wheel-switch",
            "stray.bin",
        ] {
            std::fs::write(dir.join(f), "x").unwrap();
        }
        write(&state_root, "python-w", |s| {
            s.last_outcome = Some("failed".into())
        });
        let found = run(&state_root, &checkouts, || None).join("\n");
        // One finding for the leftovers a wheel switch makes, one for the stray file.
        assert!(found.contains("not committed: stray.bin"), "{found}");
        assert!(
            !found.contains("wheel-origin.txt") || found.contains("wheel-origin.txt is missing"),
            "{found}"
        );
        assert!(!found.contains("w-1.0.tar.gz"), "{found}");
    }

    #[test]
    fn an_unknown_obs_status_skips_the_obs_comparison_and_a_clean_package_has_no_findings() {
        let root = tempfile::tempdir().unwrap();
        let state_root = root.path().join("state");
        let checkouts = root.path().join("proj");
        std::fs::create_dir_all(checkouts.join("python-a")).unwrap();
        std::fs::write(
            checkouts.join("python-a/python-a.spec"),
            "Name: python-a\nVersion: 1.0\n",
        )
        .unwrap();
        write(&state_root, "python-a", |s| {
            s.last_outcome = Some("success".into());
            s.last_built_version = Some("1.0".into());
        });
        assert!(run(&state_root, &checkouts, || None).is_empty());
        let ok = HashMap::from([("python-a".to_string(), BuildStatus::Succeeded)]);
        assert!(run(&state_root, &checkouts, || Some(ok)).is_empty());
    }
}
