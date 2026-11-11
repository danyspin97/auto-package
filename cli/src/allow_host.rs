//! `--allow-host` for the commands that run services or fetch sources, and
//! the `hosts` subcommand that lists and changes the persistent list. Only
//! what the user types, or the user's own file, can allow a host.

use std::path::Path;

use ap_build::allowed_hosts::file::{self, Added};
use ap_build::allowed_hosts::{
    AllowOutcome, AllowedHosts, Origin, Use, Via, normalize_host, normalize_user_host, printable,
    refusal_hint, shared,
};
use color_eyre::eyre::eyre;

/// `--allow-host`, for this run only.
#[derive(clap::Args, Clone, Default)]
pub struct AllowHostArgs {
    /// Allow this host for this run only: services and source fetches may
    /// contact it over https, and so may its subdomains. Repeatable; a plain
    /// host name under a domain, not a wildcard, port, address, URL or public
    /// suffix such as `co.uk`.
    #[arg(long = "allow-host", value_name = "HOST")]
    pub allow_host: Vec<String>,
    /// The hosts [`Self::apply`] added, recorded once the run has started.
    #[arg(skip)]
    pub added: Vec<String>,
    /// The problems found in the environment and the file, once applied.
    #[arg(skip)]
    pub problems: Vec<String>,
}

impl AllowHostArgs {
    /// Add the flags to the run's allowlist, before anything runs. A bad
    /// name stops the command.
    pub fn apply(&mut self) -> color_eyre::Result<()> {
        let hosts = shared();
        for raw in &self.allow_host {
            let host = normalize_user_host(raw).map_err(|e| eyre!("--allow-host {raw}: {e}"))?;
            match hosts.allow(&host, Origin::Run(Via::Flag), None) {
                Ok(AllowOutcome::Added) => {
                    ap_core::sink::eprint_coloured(&format!(
                        "note: {host} is allowed for this run (--allow-host)"
                    ));
                    self.added.push(host.clone());
                }
                Ok(AllowOutcome::AlreadyAllowed(origin)) => ap_core::sink::eprint_coloured(
                    &format!("note: {host} is already allowed ({})", origin.label()),
                ),
                Err(e) => return Err(eyre!("--allow-host {raw}: {e}")),
            }
        }
        // A bad name in the environment or the user's file is said here, once,
        // so the run does not silently drop a host it was meant to allow.
        self.problems = hosts.problems();
        for p in &self.problems {
            ap_core::sink::eprint_coloured(&format!("warning: {p}"));
        }
        Ok(())
    }

    /// Record the hosts added by [`Self::apply`] in the run's log. Called once
    /// the run has its reporter.
    pub fn record(&self, reporter: &dyn ap_core::sink::Reporter) {
        for host in &self.added {
            reporter.report(&ap_core::event::Event::HostAllowed {
                host: host.clone(),
                scope: ap_core::event::HostScope::Run,
                source: ap_core::event::HostSource::Flag,
            });
        }
        for message in &self.problems {
            reporter.report(&ap_core::event::Event::Warning {
                step: None,
                message: message.clone(),
            });
        }
    }
}

/// `auto-package hosts ...`.
#[derive(clap::Subcommand, Clone)]
pub enum HostsCommand {
    /// List the hosts services and source fetches may contact, where each
    /// comes from, and the reason written beside a user's entry. Reads only.
    List,

    /// Allow a host permanently, for this tool's services and source fetches,
    /// over https (its subdomains too). This is a security decision: the host
    /// receives the requests the tool makes for sources. A host name under a
    /// domain only: wildcards (`*.example.org`), ports, addresses, URLs, a
    /// single label (`com`) and a public suffix (`co.uk`) are refused.
    Allow {
        /// The host name, e.g. mirror.example.org.
        host: String,

        /// Why it is allowed; kept in the file as a comment.
        #[arg(long)]
        reason: Option<String>,
    },

    /// Remove a host that was allowed with `hosts allow`. Built-in hosts cannot
    /// be removed, and a host allowed by AUTO_PACKAGE_SOURCE_HOSTS needs that
    /// variable unset. Takes effect for later runs.
    Remove {
        /// The host name to remove.
        host: String,
    },
}

/// Run a `hosts` subcommand against the user's file.
pub fn hosts_command(command: &HostsCommand) -> color_eyre::Result<()> {
    let path = file::config_file().ok_or_else(|| {
        eyre!("no config directory: set HOME or XDG_CONFIG_HOME to an absolute path")
    })?;
    let hosts = ap_build::allowed_hosts::load_default();
    let out = match command {
        HostsCommand::List => list_text(&hosts, &path),
        HostsCommand::Allow { host, reason } => {
            allow_in(&path, host, reason.as_deref(), &hosts).map_err(|e| eyre!(e))?
        }
        HostsCommand::Remove { host } => remove_in(&path, host, &hosts).map_err(|e| eyre!(e))?,
    };
    print!("{out}");
    Ok(())
}

/// The list as text: each entry with its origin, what it covers and its
/// reason, then the problems found in the file or the environment. A user
/// entry covers services and source fetches, and the subdomains of its host;
/// a built-in entry covers what the built-in lists say.
pub fn list_text(hosts: &AllowedHosts, path: &Path) -> String {
    let mut out = String::new();
    out.push_str("host, origin, what it covers\n");
    for e in hosts.entries() {
        let reason = e
            .reason
            .as_deref()
            .map(|r| format!("  # {}", printable(r)))
            .unwrap_or_default();
        let covers = match e.origin {
            Origin::Builtin => "built-in list",
            _ => "services+sources, subdomains included",
        };
        out.push_str(&format!(
            "{:<40} {}  {covers}{reason}\n",
            printable(&e.host),
            e.origin.label()
        ));
    }
    out.push_str(&format!("\nuser file: {}\n", path.display()));
    for p in hosts.problems() {
        out.push_str(&format!("warning: {}\n", printable(&p)));
    }
    out
}

/// Allow `raw` in the file at `path`, unless the list already allows it.
pub fn allow_in(
    path: &Path,
    raw: &str,
    reason: Option<&str>,
    hosts: &AllowedHosts,
) -> Result<String, String> {
    let host = normalize_user_host(raw)?;
    // Nothing is written when both uses are already allowed by something other
    // than the file. A host built in for services only is still written.
    if let (Some(origin), Some(_)) = (
        hosts.origin_of(&host, Use::Service),
        hosts.origin_of(&host, Use::Source),
    ) && origin != Origin::File
    {
        return Ok(format!(
            "{host} is already allowed ({}); nothing was written\n",
            origin.label()
        ));
    }
    Ok(match file::add_host(path, &host, reason)? {
        Added::New => format!(
            "allowed {host} permanently in {}; services and source fetches may contact it over https, its subdomains too\n",
            path.display()
        ),
        Added::AlreadyPresent => {
            format!("{host} is already in {}; nothing changed\n", path.display())
        }
    })
}

/// The hosts refused during this run that are still not allowed for the use
/// they were refused for, each with how to allow it. Empty when nothing was
/// refused (or all were allowed since, from the TUI).
pub fn refused_section(hosts: &AllowedHosts) -> String {
    let still: Vec<(String, Use)> = hosts
        .refused_hosts()
        .into_iter()
        .filter(|(h, use_)| !hosts.allows(h, *use_))
        .collect();
    if still.is_empty() {
        return String::new();
    }
    let mut out = format!("\nHosts refused this run ({}):\n", still.len());
    for (h, use_) in still {
        out.push_str(&format!("  {}\n", refusal_hint(&h, use_)));
    }
    out
}

/// Remove `raw` from the file at `path`.
pub fn remove_in(path: &Path, raw: &str, hosts: &AllowedHosts) -> Result<String, String> {
    let host = normalize_host(raw)?;
    let origin = |h: &str| hosts.origin_of(h, Use::Service);
    if origin(&host) == Some(Origin::Builtin) {
        return Err(format!("{host} is built in and cannot be removed"));
    }
    if file::remove_host(path, &host)? {
        let mut out = format!("removed {host} from {}\n", path.display());
        if let Some(origin) = origin(&host).filter(|o| *o != Origin::File) {
            out.push_str(&format!("{host} is still allowed by {}\n", origin.label()));
        }
        return Ok(out);
    }
    match origin(&host) {
        Some(origin) => Err(format!(
            "{host} is allowed by {}, not by {}; unset that to remove it",
            origin.label(),
            path.display()
        )),
        None => Err(format!("{host} is not in {}", path.display())),
    }
}
