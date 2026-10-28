use super::*;

/// How pip is run for a resolution.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResolveMode {
    /// Inside bubblewrap, sdists allowed. The default when bwrap works.
    Sandboxed,
    /// On the host, wheels only: no sandbox is available and the unsafe
    /// opt-in was not given, so no build backend may run.
    WheelsOnly,
    /// On the host, sdists allowed, because the user asked for it.
    Unsandboxed,
}

impl ResolveMode {
    /// Stable label for this mode, folded into the resolution cache key.
    pub(super) fn cache_tag(self) -> &'static str {
        match self {
            ResolveMode::Sandboxed => "sandboxed",
            ResolveMode::WheelsOnly => "wheels-only",
            ResolveMode::Unsandboxed => "unsandboxed",
        }
    }

    /// What the user should be told about this mode, when it is not the
    /// ordinary sandboxed one.
    pub fn notice(self) -> Option<String> {
        match self {
            ResolveMode::Sandboxed => None,
            ResolveMode::WheelsOnly => Some(
                "bubblewrap is not usable here (bwrap missing, or unprivileged user \
                 namespaces are disabled), so pip resolves wheels only and releases that \
                 ship only an sdist will fail to resolve. Building an sdist runs its \
                 build backend on your machine; to allow that anyway, pass \
                 --unsafe-resolve (or set AUTO_PACKAGE_UNSAFE_RESOLVE=1)."
                    .to_string(),
            ),
            ResolveMode::Unsandboxed => Some(
                "--unsafe-resolve: pip runs without a sandbox and may build sdists. \
                 Their build backends run as you, with access to your files and network."
                    .to_string(),
            ),
        }
    }
}

static UNSAFE_RESOLVE: AtomicBool = AtomicBool::new(false);

/// Environment variable equivalent to `--unsafe-resolve`.
pub const UNSAFE_RESOLVE_ENV: &str = "AUTO_PACKAGE_UNSAFE_RESOLVE";

/// Allow (or stop allowing) resolution to run pip without the sandbox.
pub fn set_unsafe_resolve(unsafe_resolve: bool) {
    UNSAFE_RESOLVE.store(unsafe_resolve, Ordering::Relaxed);
}

fn unsafe_resolve() -> bool {
    UNSAFE_RESOLVE.load(Ordering::Relaxed) || ap_core::env::env_flag(UNSAFE_RESOLVE_ENV)
}

/// How resolution runs now: unsandboxed on request, sandboxed when bwrap
/// works here (probed once per process), wheels only otherwise.
pub fn resolve_mode() -> ResolveMode {
    if unsafe_resolve() {
        ResolveMode::Unsandboxed
    } else if ap_core::sandbox::bwrap_works() {
        ResolveMode::Sandboxed
    } else {
        ResolveMode::WheelsOnly
    }
}
