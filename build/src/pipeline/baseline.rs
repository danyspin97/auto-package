use super::*;

/// Where a package keeps what OBS held before its first commit (see
/// [`restore_obs_baseline`]). It lives in the package's workspace, not in the
/// run's directory, so a baseline that could not be put back outlives the run
/// and is restored before the next one writes.
pub(super) const OBS_BASELINE_DIR: &str = "obs-baseline";

/// The baseline directory of `config`'s package (see [`OBS_BASELINE_DIR`]).
pub(super) fn obs_baseline_dir(config: &PipelineConfig) -> std::io::Result<PathBuf> {
    Workspace::open(&config.state_root, &config.rpm_name).map(|ws| ws.dir().join(OBS_BASELINE_DIR))
}

/// The workspace directory of `config`'s package, where a run keeps what it
/// wrote or accepted (see `rpmlint_sources`).
pub(super) fn run_dir(config: &PipelineConfig) -> Option<PathBuf> {
    Workspace::open(&config.state_root, &config.rpm_name)
        .ok()
        .map(|ws| ws.dir().to_path_buf())
}

/// The checkout the run of `config`'s package writes: the OBS name its state
/// records (the RPM name before one is recorded). A baseline saved from any
/// other path is not restored into it (see `osc::restore_baseline`).
pub(super) fn expected_checkout(config: &PipelineConfig) -> PathBuf {
    let obs_name = Workspace::open(&config.state_root, &config.rpm_name)
        .ok()
        .and_then(|ws| PkgState::load(&ws).obs_name)
        .unwrap_or_else(|| config.rpm_name.clone());
    checkout_paths(&config.work_dir, &config.project, &obs_name).0
}

/// Puts back the OBS revision a failed run found, when the run committed
/// anything: the attempts stay in OBS history, but the project must not keep a
/// revision that does not build (`--in-place` writes the project itself). A
/// run that wrote nothing has nothing to put back. When the baseline cannot go
/// back, the failure says so, in its reason, or in a warning for a result that
/// has none.
pub(super) fn restore_obs_baseline(
    dir: &Path,
    reporter: &dyn Reporter,
    result: PipelineResult,
    restore: impl FnOnce(&Path) -> Result<osc::Restored, String>,
) -> PipelineResult {
    let built = matches!(
        result,
        PipelineResult::Built { .. } | PipelineResult::AlreadyBuilt { .. }
    );
    if built || osc::attempted_files(dir).is_empty() {
        let _ = std::fs::remove_dir_all(dir);
        return result;
    }
    match restore(dir) {
        Ok(restored) => {
            let _ = std::fs::remove_dir_all(dir);
            let message = match restored {
                osc::Restored::Undone => {
                    "restored the OBS baseline: the failed attempt is undone on OBS"
                }
                osc::Restored::NothingWritten => {
                    "nothing was written to OBS by the failed attempt: nothing to restore"
                }
            };
            reporter.report(&Event::Warning {
                step: None,
                message: message.into(),
            });
            result
        }
        Err(why) => {
            let message = format!(
                "the OBS baseline was NOT restored ({why}): OBS keeps the failed attempt; \
                 the saved baseline is in {}",
                dir.display()
            );
            reporter.report(&Event::Warning {
                step: None,
                message: message.clone(),
            });
            match result {
                PipelineResult::Failed {
                    rpm_name,
                    reason,
                    kind,
                } => PipelineResult::Failed {
                    rpm_name,
                    reason: format!("{reason}; {message}"),
                    kind,
                },
                other => other,
            }
        }
    }
}

/// What OBS held for `name` when the baseline was kept. `osc::save_baseline`
/// keeps each tracked file under `files/`; `None` when OBS did not hold it.
pub(crate) fn baseline_text(baseline: &Path, name: impl AsRef<Path>) -> Option<String> {
    std::fs::read_to_string(baseline.join("files").join(name)).ok()
}
