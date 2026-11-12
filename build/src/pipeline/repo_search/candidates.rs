/// Source repository candidates for a package with no (or no usable) PyPI
/// entry.
#[derive(Default)]
pub struct RepoCandidates {
    /// Confirmed to exist via `verify_repo_exists` — safe to build from
    /// automatically without a human/agent confirming it first.
    pub verified: Vec<String>,
    /// Unverified guesses, surfaced only for a human/agent to check manually.
    pub guessed: Vec<String>,
    /// Name search hits that exist but that nothing vouches for (not used automatically).
    pub unvouched_search_hits: Vec<String>,
    /// The repository the agent fallback picked when nothing vouches for it.
    pub agent_pick: Option<String>,
}

/// Curated mappings for names with a repository the project itself names:
/// `(name prefix, repository)`. A package whose lowercased name starts with a
/// prefix is looked up at that repository first (see `search_repo_candidates`).
// NOTE: nvidia-cuda-nvrtc, nvidia-cuda-runtime, nvidia-cuda-cupti and
// nvidia-cuda-nvcc are not mapped here: NVIDIA/cuda-python
// is the *bindings* monorepo (cuda.bindings/cuda.core/cuda.pathfinder) —
// it does not contain the closed-source nvcc compiler, cupti profiler,
// nvrtc or cudart binaries those wheels actually redistribute. There is
// no public source for them at all; they fall through to the
// wheel-binary-repackaging path in `generate_spec` instead.
pub(super) const KNOWN_MAPPINGS: &[(&str, &str)] = &[
    // NVIDIA CUDA packages
    ("cuda-python", "https://github.com/NVIDIA/cuda-python"),
    ("nvmath-python", "https://github.com/NVIDIA/nvmath-python"),
    ("nvidia-ml-py", "https://github.com/NVIDIA/nvidia-ml-py"),
    ("nvidia-nvshmem-cu", "https://github.com/NVIDIA/nvshmem"),
    ("nvshmem4py", "https://github.com/NVIDIA/nvshmem"),
    ("nvidia-cuda-cccl", "https://github.com/NVIDIA/cccl"),
    (
        "nvidia-nvjitlink",
        "https://github.com/NVIDIA/nvjitlink-python",
    ),
];

/// Whether a lowercased package name starts with a curated mapping's name, so
/// it has a repository that is not found by a name search.
pub fn is_known_mapped_name(lower: &str) -> bool {
    KNOWN_MAPPINGS.iter().any(|(p, _)| lower.starts_with(p))
}
