//! The NVIDIA mode of a project: the mode file, reading and writing it, and
//! the mode a run applies.

use super::*;

/// Refuse a project directory that is a symlink: `create_dir_all` and the
/// marker write would follow it and put state (and `--cleanup`'s deletes)
/// wherever it points, e.g. `~/Documents`.
/// How the NVIDIA CUDA libraries are packaged (`--nvidia`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub(crate) enum NvidiaMode {
    /// The `nvidia-*` PyPI wheels, repackaged as `python-nvidia-*`.
    Wheel,
    /// NVIDIA's redistributable archives, packaged as `cuda-<name>`.
    Redist,
}

impl NvidiaMode {
    fn as_str(self) -> &'static str {
        match self {
            NvidiaMode::Wheel => "wheel",
            NvidiaMode::Redist => "redist",
        }
    }
}

/// Where a work directory remembers its `--nvidia` choice.
pub(super) const NVIDIA_MODE_FILE: &str = "nvidia-mode";

pub(super) fn read_nvidia_mode(project_dir: &Path) -> Option<NvidiaMode> {
    match std::fs::read_to_string(project_dir.join(NVIDIA_MODE_FILE))
        .ok()?
        .trim()
    {
        "wheel" => Some(NvidiaMode::Wheel),
        "redist" => Some(NvidiaMode::Redist),
        _ => None,
    }
}

/// The mode to run in, and a warning when it changes what the work directory
/// last used. An explicit choice wins, then the remembered one, then `wheel`.
/// The two modes name their packages differently (`cuda-cublas` against
/// `python-nvidia-cublas`) and never share state, so flipping on a restart
/// without being asked would build a second copy of every CUDA package.
pub(crate) fn resolve_nvidia_mode(
    explicit: Option<NvidiaMode>,
    remembered: Option<NvidiaMode>,
) -> (NvidiaMode, Option<String>) {
    match (explicit, remembered) {
        (Some(chosen), Some(before)) if chosen != before => (
            chosen,
            Some(format!(
                "this work directory was run with --nvidia {}; switching to {}. Packages \
                 already built the other way stay in the project: the modes name them \
                 differently (cuda-<name> against python-nvidia-<name>)",
                before.as_str(),
                chosen.as_str()
            )),
        ),
        (Some(chosen), _) => (chosen, None),
        (None, Some(before)) => (before, None),
        (None, None) => (NvidiaMode::Wheel, None),
    }
}

/// Remember `mode` in the work directory. Called once `--cleanup` has had its
/// chance to empty it. A plain run in the default mode leaves no file behind.
pub(crate) fn write_nvidia_mode(
    project_dir: &Path,
    mode: NvidiaMode,
    explicit: bool,
) -> color_eyre::Result<()> {
    if mode != NvidiaMode::Wheel || explicit || project_dir.join(NVIDIA_MODE_FILE).exists() {
        ap_core::fsx::write_atomic(
            &project_dir.join(NVIDIA_MODE_FILE),
            format!("{}\n", mode.as_str()),
        )?;
    }
    Ok(())
}

/// Decide the mode for this run and switch the pipeline to it.
pub(crate) fn apply_nvidia_mode(
    project_dir: &Path,
    explicit: Option<NvidiaMode>,
    cleanup: bool,
) -> color_eyre::Result<NvidiaMode> {
    let remembered = if cleanup {
        None
    } else {
        read_nvidia_mode(project_dir)
    };
    let (mode, warning) = resolve_nvidia_mode(explicit, remembered);
    if let Some(w) = warning {
        eprintln!("warning: {w}");
    }
    ap_build::redist::set_enabled(mode == NvidiaMode::Redist);
    if mode == NvidiaMode::Redist {
        eprintln!(
            "info: NVIDIA CUDA libraries are packaged from NVIDIA's archives (--nvidia redist)"
        );
    }
    Ok(mode)
}
