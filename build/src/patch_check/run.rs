use super::*;

/// Run quilt over the spec's patches on a scratch copy of `tarball`. The patch
/// files are read from `checkout` and copied: the checkout is never written.
pub fn check(spec: &str, checkout: &Path, tarball: Option<&Path>, refresh: Refresh) -> CheckReport {
    check_with(
        spec,
        &|name| std::fs::read(checkout.join(name)).ok(),
        tarball,
        refresh,
    )
}

/// [`check`] with the patch files given by name (`None`: not there), for a
/// state of the patches that is no longer on disk (the one before a turn).
pub fn check_files(
    spec: &str,
    files: &std::collections::BTreeMap<String, Option<Vec<u8>>>,
    tarball: Option<&Path>,
) -> CheckReport {
    check_with(
        spec,
        &|name| files.get(name).cloned().flatten(),
        tarball,
        Refresh::No,
    )
}

pub(super) fn check_with(
    spec: &str,
    read: &dyn Fn(&str) -> Option<Vec<u8>>,
    tarball: Option<&Path>,
    refresh: Refresh,
) -> CheckReport {
    let plan = plan_series(spec);
    if !plan.iter().any(|p| matches!(p, Planned::Check(_))) {
        return not_run_or_skips(&plan);
    }
    let Some(tarball) = tarball else {
        return not_run(
            &plan,
            "no source archive of this version in the checkout".into(),
        );
    };
    if !quilt_available() {
        return not_run(&plan, "quilt is not installed".into());
    }
    match run_check(&plan, read, tarball, refresh) {
        Ok(report) => report,
        Err(why) => not_run(&plan, why),
    }
}

/// The report for a plan with nothing to check: every patch skipped.
pub(super) fn not_run_or_skips(plan: &[Planned]) -> CheckReport {
    if plan.is_empty() {
        return CheckReport::default();
    }
    let mut report = not_run(plan, String::new());
    report.not_run = None;
    report
}

pub(super) fn run_check(
    plan: &[Planned],
    read: &dyn Fn(&str) -> Option<Vec<u8>>,
    tarball: &Path,
    refresh: Refresh,
) -> Result<CheckReport, String> {
    let scratch = tempfile::tempdir().map_err(|e| format!("no scratch directory: {e}"))?;
    let src = scratch.path().join("src");
    let patches_dir = scratch.path().join("patches");
    std::fs::create_dir_all(&patches_dir).map_err(|e| e.to_string())?;
    // Unpack only what the patches touch: quilt reads nothing else.
    let mut targets: Vec<String> = Vec::new();
    for p in plan {
        if let Planned::Check(e) = p
            && let Some(diff) = read(&e.name)
        {
            targets.extend(crate::pipeline::patch_targets(&String::from_utf8_lossy(
                &diff,
            )));
        }
    }
    let wanted = |p: &Path| targets.iter().any(|t| p.ends_with(t));
    ap_ecosystem::sdist::extract_where(tarball, &src, wanted)
        .map_err(|e| format!("could not unpack {}: {e}", tarball.display()))?;
    let entries: Vec<_> = std::fs::read_dir(&src)
        .map_err(|e| e.to_string())?
        .filter_map(|e| e.ok())
        .collect();
    let root = match entries.as_slice() {
        [only] if only.path().is_dir() => only.path(),
        _ => src.clone(),
    };
    let mut env = crate::proc::minimal_env(&[]);
    env.insert("QUILT_PATCHES".into(), patches_dir.display().to_string());
    env.insert("QUILT_PC".into(), ".pc".into());
    env.insert("QUILTRC".into(), "/dev/null".into());
    env.insert("LC_ALL".into(), "C".into());
    env.insert("LANG".into(), "C".into());
    let quilt = Quilt {
        root: root.clone(),
        patches: patches_dir.clone(),
        env,
        scratch_text: format!("{}/", scratch.path().display()),
    };

    let mut report = CheckReport::default();
    // The patches applied so far: the series quilt sees. A failing patch is
    // taken out of it, so the patches after it are tried without it.
    let mut applied: Vec<SeriesEntry> = Vec::new();
    for p in plan {
        let e = match p {
            Planned::Skip { name, reason } => {
                report.patches.push(PatchResult {
                    name: name.clone(),
                    level: 1,
                    status: PatchStatus::NotChecked {
                        reason: reason.clone(),
                    },
                    refreshed: None,
                });
                continue;
            }
            Planned::Check(e) => e,
        };
        let result = |status| PatchResult {
            name: e.name.clone(),
            level: e.level,
            status,
            refreshed: None,
        };
        let original = read(&e.name);
        let copied = original
            .as_deref()
            .is_some_and(|b| std::fs::write(quilt.patches.join(&e.name), b).is_ok());
        if !copied {
            report.patches.push(result(PatchStatus::NotChecked {
                reason: "the file is not in the checkout".into(),
            }));
            continue;
        }
        let mut series = applied.clone();
        series.push(e.clone());
        std::fs::write(quilt.patches.join("series"), series_text(&series))
            .map_err(|e| e.to_string())?;
        let (ok, output) = match quilt.run(&["push"]) {
            Ok(v) => v,
            Err(why) => {
                report
                    .patches
                    .push(result(PatchStatus::NotChecked { reason: why }));
                continue;
            }
        };
        match classify_push(ok, &output) {
            PushVerdict::Clean => {
                report.patches.push(result(PatchStatus::Applies));
                applied.push(e.clone());
            }
            PushVerdict::OffsetOrFuzz { fuzz } => {
                let mut r = result(PatchStatus::AppliesWithOffsetOrFuzz {
                    fuzz,
                    output: truncate(&output, OUTPUT_LIMIT),
                });
                if refresh == Refresh::Offset && fuzz {
                    let mut args = vec!["refresh"];
                    args.extend(refresh_args(
                        original.as_deref().unwrap_or_default(),
                        e.level,
                    ));
                    if quilt.run(&args).is_ok_and(|(ok, _)| ok) {
                        r.refreshed = std::fs::read(quilt.patches.join(&e.name)).ok();
                    }
                }
                report.patches.push(r);
                applied.push(e.clone());
            }
            PushVerdict::ReverseApplied => {
                // Confirmed without fuzz: a proof made with fuzz is no proof.
                let strict = quilt
                    .run(&["push", "--fuzz=0"])
                    .map(|(ok, t)| (classify_push(ok, &t), t));
                if quilt.top().as_deref() == Some(e.name.as_str()) {
                    let _ = quilt.run(&["pop", "-f"]);
                }
                std::fs::write(quilt.patches.join("series"), series_text(&applied))
                    .map_err(|e| e.to_string())?;
                let status = match strict {
                    Ok((PushVerdict::ReverseApplied, t)) => PatchStatus::AlreadyApplied {
                        output: truncate(&t, OUTPUT_LIMIT),
                    },
                    _ => PatchStatus::Fails {
                        output: truncate(&output, OUTPUT_LIMIT),
                        rejects: String::new(),
                    },
                };
                report.patches.push(result(status));
            }
            PushVerdict::Failed => {
                // Forced, to get the rejected hunks quilt would show the agent;
                // then taken back off so the next patch sees a tree without it.
                let forced = quilt
                    .run(&["push", "-f"])
                    .map(|(_, t)| t)
                    .unwrap_or_default();
                let mut rejects = String::new();
                for file in reject_files(&forced)
                    .into_iter()
                    .chain(reject_files(&output))
                {
                    let path = root.join(&file);
                    if let Ok(body) = std::fs::read_to_string(&path)
                        && !rejects.contains(&format!("--- {file}\n"))
                    {
                        rejects.push_str(&format!("--- {file}\n{body}"));
                    }
                    let _ = std::fs::remove_file(&path);
                }
                if quilt.top().as_deref() == Some(e.name.as_str()) {
                    let _ = quilt.run(&["pop", "-f"]);
                }
                std::fs::write(quilt.patches.join("series"), series_text(&applied))
                    .map_err(|e| e.to_string())?;
                report.patches.push(result(PatchStatus::Fails {
                    output: truncate(&output, OUTPUT_LIMIT),
                    rejects: truncate(&rejects, REJECTS_LIMIT),
                }));
            }
        }
    }
    Ok(report)
}
