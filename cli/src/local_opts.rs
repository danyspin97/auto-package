//! The options of `--local`, flattened into `build` and `update`.

use clap::Args;
use std::path::PathBuf;

/// What goes with `--local`. Each of them is refused without it, so a typo does
/// not quietly build on OBS.
#[derive(Args, Clone, Debug)]
pub struct LocalOpts {
    /// Build root of the local build (`osc build --root`; osc's default is
    /// /var/tmp/build-root). It needs many GB.
    #[arg(long, value_name = "DIR", requires = "local")]
    pub local_root: Option<PathBuf>,

    /// Refuse a local build when the filesystem of the build root has less than
    /// this many GiB free.
    #[arg(
        long,
        default_value_t = ap_build::osc::DEFAULT_MIN_FREE_GIB,
        value_name = "GIB",
        requires = "local"
    )]
    pub local_min_free_gib: u64,
}

impl LocalOpts {
    /// Turn local builds on for this process when `--local` was given.
    pub fn apply(&self, local: bool) -> color_eyre::Result<()> {
        if !local {
            return Ok(());
        }
        let root = match &self.local_root {
            Some(r) => Some(std::path::absolute(r)?),
            None => None,
        };
        ap_build::osc::enable_local_build(ap_build::osc::LocalBuild {
            root,
            min_free_gib: self.local_min_free_gib,
        });
        Ok(())
    }
}
