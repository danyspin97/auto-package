//! The run-wide limits every pipeline command takes, flattened into its args.

use clap::Args;

#[derive(Args, Clone, Debug)]
pub struct RunLimits {
    /// Most USD the agent may spend per UTC day, across every run and process
    /// that uses this work directory (recorded in `<work-dir>/ledger.json`). A
    /// turn that would pass it is refused; an unreadable ledger refuses too.
    #[arg(long, default_value_t = ap_core::limits::DEFAULT_MAX_TOTAL_USD, value_name = "USD")]
    pub max_total_usd: f64,

    /// Most packages one run starts, counting the dependency closure (0 =
    /// unlimited). Past it nothing more is queued and the run says so.
    #[arg(long, default_value_t = ap_core::limits::DEFAULT_MAX_PACKAGES, value_name = "N")]
    pub max_packages: usize,
}

impl RunLimits {
    /// Make the limits effective for this process. `work_dir` is the work
    /// directory root, where the spend ledger lives.
    pub fn apply(&self, work_dir: &std::path::Path) -> color_eyre::Result<()> {
        if !(self.max_total_usd.is_finite() && self.max_total_usd >= 0.0) {
            return Err(color_eyre::eyre::eyre!(
                "--max-total-usd must be a number of USD, 0 or more"
            ));
        }
        // 0 would read as "unset" in the limits module; a tiny cap refuses everything.
        ap_core::limits::set_max_total_usd(self.max_total_usd.max(f64::MIN_POSITIVE));
        ap_core::limits::set_ledger_dir(std::path::absolute(work_dir)?);
        set_max_packages(self.max_packages);
        Ok(())
    }
}

static MAX_PACKAGES: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(ap_core::limits::DEFAULT_MAX_PACKAGES);

fn set_max_packages(n: usize) {
    MAX_PACKAGES.store(n, std::sync::atomic::Ordering::SeqCst);
}

/// The `--max-packages` of this process.
pub fn max_packages() -> usize {
    MAX_PACKAGES.load(std::sync::atomic::Ordering::SeqCst)
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[derive(Parser)]
    struct P {
        #[command(flatten)]
        l: RunLimits,
    }

    #[test]
    fn defaults_and_overrides() {
        let p = P::parse_from(["x"]);
        assert_eq!(p.l.max_total_usd, 50.0);
        assert_eq!(p.l.max_packages, 25);
        let p = P::parse_from(["x", "--max-total-usd", "3.5", "--max-packages", "0"]);
        assert_eq!((p.l.max_total_usd, p.l.max_packages), (3.5, 0));
    }

    #[test]
    fn a_negative_cap_is_refused() {
        let p = P::parse_from(["x", "--max-total-usd=-1"]);
        assert!(p.l.apply(std::path::Path::new(".")).is_err());
    }
}
