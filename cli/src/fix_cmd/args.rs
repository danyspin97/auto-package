use std::path::PathBuf;

/// `auto-package fix`: the bug and where to build the fix.
#[derive(clap::Args, Clone)]
pub struct FixArgs {
    /// OBS project the package lives in. Only read: the fix is built in a
    /// branch project. A maintenance or update project is refused.
    #[arg(long, value_name = "PROJECT")]
    pub project: String,

    /// The package to fix (exactly one).
    #[arg(long, value_delimiter = ',', required = true, value_name = "PACKAGE")]
    pub only: Vec<String>,

    /// A bug reference: `bsc#1234567` (also `boo#`, `bnc#`), `jsc#SLE-12345` or
    /// a CVE. Repeatable or comma separated. A public bsc# bug is read from
    /// bugzilla.suse.com once the host is allowed.
    #[arg(long = "bug", value_name = "REF", value_delimiter = ',')]
    pub bug: Vec<String>,

    /// A CVE (`CVE-2025-1234`), repeatable. Read from api.osv.dev and
    /// cveawg.mitre.org once the user allows them.
    #[arg(long, value_name = "CVE", value_delimiter = ',')]
    pub cve: Vec<String>,

    /// The bug in words (at most 4 KiB), for a bug with no tracker entry or to
    /// add to one.
    #[arg(long, value_name = "TEXT")]
    pub description: Option<String>,

    /// A text file with the bug report (a paste of the bugzilla page, an
    /// advisory), at most 64 KiB. Read instead of fetched.
    #[arg(long, value_name = "FILE")]
    pub bug_file: Option<PathBuf>,

    /// The upstream fix: a commit, pull request or `.patch`/`.diff` URL, a
    /// patch file, or a commit id with `--commit-repo`. Repeatable for a fix in
    /// several commits. Without it the tool looks for one.
    #[arg(long, value_name = "URL")]
    pub commit_url: Vec<String>,

    /// The https repository of a bare commit id in `--commit-url`.
    #[arg(long, value_name = "URL")]
    pub commit_repo: Option<String>,

    /// Project to branch the package into and build the fix in. Defaults to
    /// `home:<osc user>:branches:<PROJECT>`. There is no in-place fix.
    #[arg(long)]
    pub branch_project: Option<String>,

    /// Branch from the package's devel project when it has one; a request goes
    /// to it.
    #[arg(long, default_value_t = false)]
    pub follow_devel: bool,

    #[arg(long, default_value = "ap-work")]
    pub work_dir: PathBuf,

    /// Directory for per-package notes.
    #[arg(
        long = "notes",
        alias = "notes-dir",
        default_value = "../auto-package-notes"
    )]
    pub notes_dir: PathBuf,

    /// Repository to build for.
    #[arg(long)]
    pub distro: Option<String>,

    #[arg(long, default_value = "x86_64")]
    pub arch: String,

    /// USD budget cap for the agent's turns on the package.
    #[arg(long, default_value_t = 10.0)]
    pub budget_usd: f64,

    /// Maximum agent fix attempts.
    #[arg(long, default_value_t = 10)]
    pub max_attempts: usize,

    /// Only add a given upstream patch that applies, without a model; anything
    /// that needs the agent ends with exit 4.
    #[arg(long, default_value_t = false)]
    pub no_agent: bool,

    /// After the build, create a submit request to the package's origin (the
    /// devel project with `--follow-devel`), after confirmation. A fix with a
    /// risk flag is never submitted by `--yes`.
    #[arg(long, default_value_t = false)]
    pub submit: bool,

    /// Create the request without asking (refused when a risk flag needs a
    /// review).
    #[arg(short, long, default_value_t = false)]
    pub yes: bool,

    /// Print what would be read, found and added, and stop. Reads the bug and
    /// the patch; branches, builds and submits nothing.
    #[arg(long, alias = "check", default_value_t = false)]
    pub dry_run: bool,

    /// Enable the interactive TUI for the build.
    #[arg(long, default_value_t = false)]
    pub tui: bool,

    /// Log level: error, warn, info, debug, trace.
    #[arg(long, default_value = "info")]
    pub log: String,

    #[command(flatten)]
    pub allow_host: crate::allow_host::AllowHostArgs,
}
