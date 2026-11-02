//! `keyring accept`: take the signing key of an upstream release into the
//! package's keyring, once the user has checked its full fingerprint.

use super::*;

#[derive(clap::Subcommand)]
pub enum KeyringCommand {
    /// Add the signing key with this full fingerprint to the package's keyring.
    ///
    /// The key is fetched from the keyserver, and only the key whose full
    /// fingerprint equals `--fingerprint` is kept. It must have made the
    /// signature of the checkout's archive. The keyring is written and a
    /// `.changes` entry names the fingerprint. Both reach OBS with an update that
    /// builds a newer version of the package; `update` does not commit a change
    /// to a package already at the upstream version. Check the fingerprint on a
    /// second channel (the upstream's site or announcement) before running this.
    Accept(KeyringAcceptArgs),
}

#[derive(clap::Args)]
pub struct KeyringAcceptArgs {
    /// OBS package name (its checkout directory name).
    pub package: String,

    /// The full 40-digit fingerprint of the signing key. Spaces and case do not
    /// matter; a short key ID is refused.
    #[arg(long)]
    pub fingerprint: String,

    /// The OBS project the package is updated from. Names the branch project
    /// when `--branch-project` is not given (as for `update`).
    #[arg(long)]
    pub project: Option<String>,

    /// The branch project the package is checked out in.
    #[arg(long)]
    pub branch_project: Option<String>,

    /// The package lives in `--project` itself (`update --in-place`).
    #[arg(long, default_value_t = false)]
    pub in_place: bool,

    #[arg(long, default_value = "ap-work")]
    pub work_dir: PathBuf,

    #[arg(long, default_value = "x86_64")]
    pub arch: String,

    /// Keyserver the key is fetched from.
    #[arg(long, default_value = "hkps://keys.openpgp.org")]
    pub keyserver: String,
}

pub fn accept_command(args: &KeyringAcceptArgs) -> color_eyre::Result<()> {
    let branch = match (&args.branch_project, &args.project, args.in_place) {
        (_, Some(source), true) => source.clone(),
        (Some(branch), _, _) => branch.clone(),
        (None, Some(source), false) => {
            let user = ap_build::osc::current_user(ap_build::proc::Emit::quiet())
                .map_err(|e| color_eyre::eyre::eyre!("{e}"))?;
            ap_build::osc::default_branch_project(&user, source)
        }
        (None, None, _) => {
            return Err(color_eyre::eyre::eyre!(
                "name the package's project: --project (its source) or --branch-project"
            ));
        }
    };
    for (what, name) in [("branch", &branch), ("package", &args.package)] {
        if !ap_build::osc::is_valid_obs_name(name) {
            return Err(color_eyre::eyre::eyre!(
                "{name:?} is not a valid {what} name"
            ));
        }
    }
    let project_dir = args.work_dir.join(format!(
        "{}-{}",
        ap_core::run::sanitize_name(&branch),
        ap_core::run::sanitize_name(&args.arch),
    ));
    let checkout = project_dir.join(&branch).join(&args.package);
    if !checkout.is_dir() {
        return Err(color_eyre::eyre::eyre!(
            "no checkout of {branch}/{} at {}: run the update of this package first, so it is checked out",
            args.package,
            checkout.display()
        ));
    }
    let _lock = ProjectLock::acquire(&project_dir)?;
    let checkout = std::path::absolute(&checkout)?;
    let accepted = ap_build::pipeline::keyring_accept::fetch_and_accept(
        &checkout,
        &args.fingerprint,
        &args.keyserver,
        ap_build::proc::Emit::quiet(),
    )
    .map_err(|e| color_eyre::eyre::eyre!("{e}"))?;
    println!(
        "accepted key {} into {} in {}, with a .changes entry naming it",
        accepted.fingerprint,
        accepted.keyring,
        checkout.display()
    );
    println!(
        "next: `auto-package update --project <SOURCE> --only {} --branch-project {branch}` publishes the keyring and the .changes entry only in a run that builds a newer version; a package already at the upstream version is skipped",
        args.package
    );
    Ok(())
}
