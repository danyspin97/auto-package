use super::*;

#[derive(clap::Args, Clone)]
pub struct UpdateProjectArgs {
    #[command(flatten)]
    pub allow_host: crate::allow_host::AllowHostArgs,

    /// OBS project to browse (e.g. Base:System): for packages with a newer
    /// upstream release, or with `lint` for quality problems. It is
    /// only read, unless `--in-place` is given.
    pub source: String,

    /// Project to branch the packages into and build them in. Defaults to
    /// `home:<osc user>:branches:<SOURCE>`, what `osc branch` uses. Created by
    /// the first branch when it does not exist.
    #[arg(long)]
    pub branch_project: Option<String>,

    /// Build the new versions (`lint`: commit the fixes) straight in SOURCE
    /// instead of in a branch project. Nothing is submitted, since nothing is
    /// branched; use it on a project you own.
    #[arg(long, default_value_t = false)]
    pub in_place: bool,

    /// Set by `lint`: take the packages from the quality audit of
    /// SOURCE instead of from the upstream release sweep.
    #[arg(skip)]
    pub improve: bool,

    /// Only update these packages (comma separated).
    #[arg(long, value_delimiter = ',')]
    pub only: Vec<String>,

    /// Leave these packages out (comma separated).
    #[arg(long, value_delimiter = ',')]
    pub exclude: Vec<String>,

    /// Create a submit request to the package's origin project for every
    /// package that built successfully in the branch project. Asks for
    /// confirmation first; see `--yes`.
    #[arg(long, default_value_t = false)]
    pub submit: bool,

    /// Create the submit requests without asking. Refuses, and submits
    /// nothing, when a change needs a review (its "Needs review" lines name
    /// the packages): run without it to review them.
    #[arg(short, long, default_value_t = false)]
    pub yes: bool,

    /// Print the plan, the branch project, the build paths that would be added
    /// and the requests that would be created, and stop. Changes nothing on
    /// OBS.
    #[arg(long, alias = "check", default_value_t = false)]
    pub dry_run: bool,

    /// Also branch and update a dependency that lives in another project and
    /// has a newer upstream release itself; its request goes to its own
    /// origin project. Without this, such a dependency is only made reachable
    /// through a build path of the branch project, and never touched.
    #[arg(long, default_value_t = false)]
    pub update_deps: bool,

    /// Branch from a package's devel project, when it has one, instead of
    /// from the project named by SOURCE; its submit request then goes to the
    /// devel project. Without this the package is branched from SOURCE as
    /// asked, which skips the devel project's own review, and a warning says
    /// so for each package that has one.
    #[arg(long, default_value_t = false)]
    pub follow_devel: bool,

    #[arg(long, default_value = "ap-work")]
    pub work_dir: PathBuf,

    /// Directory for per-package notes, shared across projects. Kept outside
    /// work_dir and outside this repository; created on first write.
    #[arg(
        long = "notes",
        alias = "notes-dir",
        default_value = "../auto-package-notes"
    )]
    pub notes_dir: PathBuf,

    /// Repository to build for (default: the branch project's own, Tumbleweed
    /// first; it is the one the source project builds for).
    #[arg(long)]
    pub distro: Option<String>,

    #[arg(long, default_value = "x86_64")]
    pub arch: String,

    /// Log level: error, warn, info, debug, trace.
    #[arg(long, default_value = "info")]
    pub log: String,

    /// Enable interactive TUI mode for the per-package builds. Cannot be
    /// combined with `--submit` unless `--yes` is also given: the TUI owns the
    /// terminal, so there is nowhere to ask. Without `--submit` the TUI stays
    /// open once everything is built: `d` shows a package's diff, `S` submits
    /// the selected one and `A` every built one, each after a confirmation.
    #[arg(long, default_value_t = false)]
    pub tui: bool,

    /// `update`: branch this revision of the package (an OBS revision number or
    /// a srcmd5) instead of the newest one, and update from the version that
    /// revision has. Needs exactly one package in `--only`. Fails when the
    /// branch project already has the package, since it would keep another
    /// revision; delete that branch package first to start over.
    #[arg(long, value_name = "REV")]
    pub revision: Option<String>,

    /// `update`: update to this version instead of the newest release found
    /// upstream. It is checked to exist where the package's source lives (the
    /// `Source:` URL with the version swapped in, a forge tag, a PyPI release)
    /// before anything is branched, and may be older than the packaged one (a
    /// downgrade, noted in the plan). Needs exactly one package in `--only`.
    #[arg(long, value_name = "VERSION")]
    pub to_version: Option<String>,

    /// Maximum agent fix attempts per package.
    #[arg(long, default_value_t = 10)]
    pub max_attempts: usize,

    /// USD budget cap for agent calls (per package build).
    #[arg(long, default_value_t = 10.0)]
    pub budget_usd: f64,

    /// Build only; never call the model.
    #[arg(long, default_value_t = false)]
    pub no_agent: bool,

    /// Ask PyPI and the forges for every package's newest version, instead of
    /// reusing an answer from the last hour (kept in the work directory).
    #[arg(long, default_value_t = false)]
    pub refresh: bool,

    /// Resolve dependencies without the sandbox: pip runs on the host and
    /// may build sdists, running their build backends as you. Without
    /// this, pip runs in bubblewrap, or resolves wheels only when
    /// bubblewrap is unusable. Same as AUTO_PACKAGE_UNSAFE_RESOLVE=1.
    #[arg(long, default_value_t = false)]
    pub unsafe_resolve: bool,

    /// Build with `osc build` on this machine, against the checkout, instead of
    /// committing to OBS and waiting for its build. Nothing is committed from
    /// the build step, so it cannot be combined with `--submit`. Needs osc,
    /// and root without a password (`sudo -n`) or run as root.
    #[arg(long, default_value_t = false, conflicts_with = "submit")]
    pub local: bool,

    /// Do not check the built package on the other architectures its branch
    /// builds it for (after the primary build). `--local` skips the check anyway.
    #[arg(long, default_value_t = false, conflicts_with = "arches")]
    pub no_arch_check: bool,

    /// Check the other architectures given here (comma separated), and add the
    /// ones the branch lacks: OBS then schedules every package of the branch on
    /// them. Names OBS knows, except armv7l, which cannot be checked: x86_64,
    /// aarch64, armv6l, i586, ppc64le, s390x, riscv64, ppc64, ppc. The ports
    /// (s390x, ppc64le, riscv64) come from the Factory ports, see `--ports`.
    #[arg(long, value_delimiter = ',', value_name = "ARCHES")]
    pub arches: Vec<String>,

    /// Check the updated package on the Factory ports: s390x, ppc64le and
    /// riscv64, which Factory itself does not build. Their repositories are
    /// added to the branch once, and the package is enabled for them, so OBS
    /// builds it there. A port's failure blocks the submit only when Factory
    /// builds the package there. Results can take hours on s390x and riscv64;
    /// a result still pending is shown as not final, and re-running `update`
    /// later refreshes it.
    #[arg(long, default_value_t = false, conflicts_with = "no_arch_check")]
    pub ports: bool,

    /// The total time to wait for the other architectures' builds, in minutes.
    /// Builds still running after it are reported as still building.
    #[arg(long, default_value_t = 30, value_name = "MINUTES")]
    pub arch_wait_minutes: u64,

    /// `--submit`: submit a package even when it fails on another architecture
    /// that its origin builds. Without it such a package is not submitted.
    #[arg(long, default_value_t = false)]
    pub allow_arch_failures: bool,

    #[command(flatten)]
    pub local_opts: crate::local_opts::LocalOpts,

    /// Maximum number of packages to build concurrently (0 = unlimited). Each
    /// build still runs its own dependency closure, so this bounds the
    /// top-level loop, not every spawned sub-build.
    #[arg(long, default_value_t = ap_core::limits::DEFAULT_JOBS)]
    pub jobs: usize,

    #[command(flatten)]
    pub limits: crate::run_limits::RunLimits,

    /// Wait for this many GiB free on the work directory's disk before a
    /// package starts its heavy stages (0 = never wait).
    #[arg(long, default_value_t = 3)]
    pub min_free_gb: u64,

    /// How the NVIDIA CUDA libraries are packaged: `wheel` (the default) or
    /// `redist` (see `build --nvidia`). Remembered in the work directory.
    #[arg(long, value_enum, value_name = "MODE")]
    pub nvidia: Option<crate::NvidiaMode>,

    /// Same as `--nvidia redist`.
    #[arg(long, default_value_t = false, conflicts_with = "nvidia")]
    pub nvidia_redist: bool,

    /// Regenerate each package's spec from scratch instead of carrying the
    /// existing recipe forward (see `update --regen`).
    #[arg(long, default_value_t = false)]
    pub regen: bool,

    /// Send the packages of `--only` to the maintenance of these code streams
    /// (update projects such as SUSE:SLE-15-SP7:Update, comma separated, or
    /// `all`) instead of a submit request: each code stream's update project is
    /// browsed instead of PROJECT, the change is built in
    /// `home:<user>:branches:<update project>`, and the packages that built get
    /// one maintenance request to the maintenance project, after confirmation
    /// (see `--yes`). Without a value, asks which of the code streams that
    /// maintain the package. Needs `--bug` (or `--no-bug-ref`). By default a
    /// backport (see `--maintenance-mode`), which needs `--backport`.
    #[arg(long, value_name = "CODESTREAM", value_delimiter = ',', num_args = 0..)]
    pub maintenance: Option<Vec<String>>,

    /// `update --maintenance`: `backport` (the default: each code stream keeps
    /// its version and gets the fix of `--backport` as a patch, what
    /// maintenance normally accepts) or `update` (each code stream is updated
    /// to the newest release, or `--to-version`).
    #[arg(long, value_name = "MODE")]
    pub maintenance_mode: Option<String>,

    /// The upstream fix of a maintenance backport (repeatable): a patch file,
    /// an https URL of a `.patch`/`.diff`, a GitHub/GitLab/Gitea commit URL or
    /// GitHub pull request (its `.patch` is fetched), or a commit id of
    /// `--backport-repo`. Stored in each code stream's package as
    /// `CVE-YYYY-NNNN.patch` (or `fix-<commit>.patch`) and added to its spec.
    #[arg(long = "backport", value_name = "SRC")]
    pub backport: Vec<String>,

    /// The https repository (GitHub, GitLab, Gitea) of a `--backport` commit id.
    #[arg(long, value_name = "URL")]
    pub backport_repo: Option<String>,

    /// A command that tells which code streams a maintenance fix affects: run
    /// without a shell as `COMMAND PACKAGE CODESTREAM REFS [BACKPORT...]` for
    /// each code stream; stdout `affected`, `not-affected <reason>` or
    /// `unknown` (anything else, a failure or a timeout is `unknown`). A code
    /// stream it says is not affected is left out, with the reason.
    #[arg(long, value_name = "COMMAND")]
    pub affected_by: Option<String>,

    /// Print the code streams each package of `--only` is maintained in, and
    /// stop. Reads only.
    #[arg(long, default_value_t = false)]
    pub maintenance_list: bool,

    /// A bug reference for the maintenance update (`bsc#1234567`,
    /// `jsc#SLE-12345`, `CVE-2025-1234`); repeatable or comma separated. Added
    /// to the newest changelog entry and the request description.
    #[arg(long = "bug", value_name = "REF", value_delimiter = ',')]
    pub bug: Vec<String>,

    /// Send a maintenance request without a bug reference.
    #[arg(long, default_value_t = false)]
    pub no_bug_ref: bool,

    /// Set for the runs of `--maintenance`: PROJECT is an update project,
    /// which is branched from.
    #[arg(skip)]
    pub maintenance_source: bool,

    /// Set for the backport runs of `--maintenance`: the package is built at
    /// the version it has, with these patches staged for the agent to add.
    #[arg(skip)]
    pub backport_job: Option<Arc<ap_build::backport::Job>>,
}

/// What `update` changes about how one package is built.
pub struct BranchBuild {
    /// The package's name in the branch project, for a repository package whose
    /// name the pipeline would otherwise guess.
    pub rpm_name: Option<String>,
    /// Looks a missing dependency up on OBS before the pipeline builds it.
    pub resolver: Option<Arc<DepResolver>>,
    /// `lint`: what the quality checks found, handed to the agent as
    /// the problem to fix in the existing recipe.
    pub improve: Option<String>,
    /// `update`: the version the package is at, shown beside the new one.
    pub from_version: Option<String>,
    /// The package is brought to a new upstream version: its recipe is kept
    /// and the update is checked against its origin. Not for `lint`, a
    /// maintenance run or a backport.
    pub update: bool,
}
