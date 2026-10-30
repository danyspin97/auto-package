//! The hosts the service sandbox and the source lookups may contact. One
//! [`AllowedHosts`] is read by every check, the proxy included, on each
//! request, so a host allowed while a run is going applies at once.
//!
//! The list is merged from four origins, in this order; the first one that
//! lists a host gives its provenance:
//! - built-in: the lists in [`builtin`] and the fixed forges;
//! - env: `AUTO_PACKAGE_SOURCE_HOSTS` (comma separated);
//! - file: the user's `allowed-hosts` file;
//! - run: `--allow-host` and the TUI, for this process only.
//!
//! Allowing a host is a security decision of the user. Only the CLI, the
//! config file and the TUI add to the list, and every host is checked by
//! [`normalize_host`] first. An allowed host is a name, not a port or an
//! address: the proxy still refuses other ports and private addresses.

use std::collections::BTreeSet;
use std::sync::{Arc, OnceLock, RwLock};

use crate::discovery::host_is_under;

mod builtin;
pub mod file;
#[cfg(test)]
mod proxy_tests;
#[cfg(test)]
mod tests;
mod validate;

pub use validate::{normalize_host, normalize_user_host};

/// What a host is checked for: a service's network access, or a source fetch
/// (an agent's URL, a lookup). Built-in lists differ between the two.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Use {
    Service,
    Source,
}

impl Use {
    /// The use as the refusal text names it.
    pub fn label(self) -> &'static str {
        match self {
            Use::Service => "a service's network access",
            Use::Source => "a source fetch",
        }
    }
}

/// Who asked for a host: the package being built and what asked for it, such
/// as `service cargo_vendor` or `source lookup`. A refusal keeps it, so the
/// prompt offers a host only to the package that was refused it.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Asker {
    pub package: String,
    pub what: String,
}

impl Asker {
    /// A service of `package` (its osc name, `service`) asking for a host.
    pub fn service(package: &str, service: &str) -> Self {
        Self {
            package: package.to_string(),
            what: format!("service {service}"),
        }
    }

    /// A source lookup for `package`.
    pub fn source(package: &str) -> Self {
        Self {
            package: package.to_string(),
            what: "source lookup".to_string(),
        }
    }
}

/// Where a run-scoped entry came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Via {
    /// `--allow-host` on the command line.
    Flag,
    /// The `a` or `A` key of the TUI.
    Tui,
}

/// The provenance of an entry.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Origin {
    Builtin,
    Env,
    File,
    Run(Via),
}

impl Origin {
    /// How the origin is named to the user.
    pub fn label(&self) -> &'static str {
        match self {
            Origin::Builtin => "built-in",
            Origin::Env => "env AUTO_PACKAGE_SOURCE_HOSTS",
            Origin::File => "file",
            Origin::Run(Via::Flag) => "--allow-host (this run)",
            Origin::Run(Via::Tui) => "TUI (this run)",
        }
    }
}

/// One allowed host. A user entry covers the host and its subdomains.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub host: String,
    pub origin: Origin,
    pub reason: Option<String>,
}

/// What [`AllowedHosts::allow`] did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AllowOutcome {
    Added,
    /// Already allowed, by this origin.
    AlreadyAllowed(Origin),
}

#[derive(Default)]
struct State {
    /// User-side entries (env, file, run), in merge order.
    entries: Vec<Entry>,
    /// Lines or values that were refused, for the user to see.
    problems: Vec<String>,
    /// Hosts refused during this process, for each use and asker; sorted, no
    /// repeats.
    refused: BTreeSet<(String, Use, Asker)>,
}

/// The allowed hosts. Shared behind an `Arc`; the entries and the refusal
/// record are behind a lock so the TUI can add to them during a run.
pub struct AllowedHosts {
    state: RwLock<State>,
}

impl Default for AllowedHosts {
    fn default() -> Self {
        Self::empty()
    }
}

impl AllowedHosts {
    /// Only the built-in hosts.
    pub fn empty() -> Self {
        Self {
            state: RwLock::new(State::default()),
        }
    }

    /// The built-in hosts plus the hosts of an `AUTO_PACKAGE_SOURCE_HOSTS`
    /// value. A bad name is reported in [`Self::problems`] and skipped.
    pub fn with_env(raw: &str) -> Self {
        let hosts = Self::empty();
        for part in raw.split(',').map(str::trim).filter(|p| !p.is_empty()) {
            match normalize_user_host(part) {
                Ok(host) => {
                    let _ = hosts.allow(&host, Origin::Env, None);
                }
                Err(e) => hosts.add_problem(format!("AUTO_PACKAGE_SOURCE_HOSTS: {e}")),
            }
        }
        hosts
    }

    /// Whether `host` (any spelling) is allowed for `use_`. Invalid names are
    /// not allowed.
    pub fn allows(&self, host: &str, use_: Use) -> bool {
        normalize_host(host).is_ok_and(|h| self.allows_normalized(&h, use_))
    }

    fn allows_normalized(&self, host: &str, use_: Use) -> bool {
        builtin::allows(host, use_)
            || self
                .state
                .read()
                .unwrap_or_else(|p| p.into_inner())
                .entries
                .iter()
                .any(|e| host_is_under(host, &e.host))
    }

    /// The host as a name to use, or the refusal. A refused but valid host is
    /// recorded with `asker`, so the run summary and the prompt can name it.
    /// The refusal says how to allow it (see [`refusal_hint`]).
    pub fn check_host(&self, raw: &str, use_: Use, asker: &Asker) -> Result<String, String> {
        let host = normalize_host(raw)?;
        if self.allows_normalized(&host, use_) {
            return Ok(host);
        }
        self.note_refused(&host, use_, asker);
        Err(refusal_hint(&host, use_))
    }

    /// The origin that allows `host` (normalized) for `use_`, in merge order.
    /// A host built in for services only has no built-in origin for sources.
    pub fn origin_of(&self, host: &str, use_: Use) -> Option<Origin> {
        if builtin::allows(host, use_) {
            return Some(Origin::Builtin);
        }
        self.state
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .entries
            .iter()
            .find(|e| host_is_under(host, &e.host))
            .map(|e| e.origin.clone())
    }

    /// Every entry in merge order: built-in hosts first, then the rest.
    pub fn entries(&self) -> Vec<Entry> {
        let mut out: Vec<Entry> = builtin::all()
            .map(|host| Entry {
                host: host.to_string(),
                origin: Origin::Builtin,
                reason: None,
            })
            .collect();
        out.extend(
            self.state
                .read()
                .unwrap_or_else(|p| p.into_inner())
                .entries
                .iter()
                .cloned(),
        );
        out
    }

    /// Allow `raw` from `origin`. A host already allowed is not added again.
    /// Callers that take a name from the user check it with
    /// [`normalize_user_host`] first.
    pub fn allow(
        &self,
        raw: &str,
        origin: Origin,
        reason: Option<String>,
    ) -> Result<AllowOutcome, String> {
        let host = normalize_host(raw)?;
        // Nothing to write when both uses are already allowed. A host built in
        // for services only still gets an entry, which covers source fetches.
        if let (Some(existing), Some(_)) = (
            self.origin_of(&host, Use::Service),
            self.origin_of(&host, Use::Source),
        ) {
            return Ok(AllowOutcome::AlreadyAllowed(existing));
        }
        self.state
            .write()
            .unwrap_or_else(|p| p.into_inner())
            .entries
            .push(Entry {
                host,
                origin,
                reason,
            });
        Ok(AllowOutcome::Added)
    }

    /// Add an entry read from a source (the file), without the check that
    /// [`Self::allow`] makes: a file may name a host the env already did.
    pub(crate) fn push_loaded(&self, entry: Entry) {
        self.state
            .write()
            .unwrap_or_else(|p| p.into_inner())
            .entries
            .push(entry);
    }

    /// Record a host refused during this process, for `use_` and `asker`.
    pub fn note_refused(&self, host: &str, use_: Use, asker: &Asker) {
        self.state
            .write()
            .unwrap_or_else(|p| p.into_inner())
            .refused
            .insert((host.to_string(), use_, asker.clone()));
    }

    /// The refusals so far: host, use and asker; sorted, without repeats.
    pub fn refusals(&self) -> Vec<(String, Use, Asker)> {
        self.state
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .refused
            .iter()
            .cloned()
            .collect()
    }

    /// The hosts refused so far, for each use; sorted, without repeats.
    pub fn refused_hosts(&self) -> Vec<(String, Use)> {
        let set: BTreeSet<(String, Use)> = self
            .refusals()
            .into_iter()
            .map(|(host, use_, _)| (host, use_))
            .collect();
        set.into_iter().collect()
    }

    /// Problems found while loading (bad env names, bad file lines).
    pub fn problems(&self) -> Vec<String> {
        self.state
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .problems
            .clone()
    }

    pub(crate) fn add_problem(&self, problem: String) {
        self.state
            .write()
            .unwrap_or_else(|p| p.into_inner())
            .problems
            .push(problem);
    }
}

/// `text` with control characters (an ANSI escape, a line break) removed, and
/// tabs and line breaks turned into spaces, so it cannot change how a line is
/// shown.
pub fn printable(text: &str) -> String {
    text.chars()
        .filter_map(|c| match c {
            '\n' | '\r' | '\t' => Some(' '),
            c if c.is_control() => None,
            c => Some(c),
        })
        .collect()
}

/// The message for a valid host that is not allowed. The wording is matched
/// by [`refused_hosts_in`]; change both together.
pub fn refusal_hint(host: &str, use_: Use) -> String {
    let scope = match use_ {
        Use::Service => "",
        Use::Source => " (for source fetches)",
    };
    format!(
        "host {host} is not allowed; allow it for this run with --allow-host {host} \
         or permanently with `auto-package hosts allow {host}`{scope}"
    )
}

const REFUSAL_MARK: &str = "is not allowed; allow it for this run with --allow-host ";

/// The hosts named by refusals in `text` (a failure reason, a log, a summary),
/// in order, without repeats.
pub fn refused_hosts_in(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for (at, _) in text.match_indices(REFUSAL_MARK) {
        let host: String = text[at + REFUSAL_MARK.len()..]
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '.')
            .collect();
        if !host.is_empty() && !out.contains(&host) {
            out.push(host);
        }
    }
    out
}

/// The built-in hosts, then the env, then the user's file, with their
/// problems. A missing file is no problem.
pub fn load_default() -> AllowedHosts {
    let hosts =
        AllowedHosts::with_env(&std::env::var("AUTO_PACKAGE_SOURCE_HOSTS").unwrap_or_default());
    if let Some(path) = file::config_file() {
        let loaded = file::load(&path);
        for problem in loaded.problems {
            hosts.add_problem(format!("{}: {problem}", path.display()));
        }
        for entry in loaded.entries {
            hosts.push_loaded(entry);
        }
    }
    hosts
}

static SHARED: OnceLock<Arc<AllowedHosts>> = OnceLock::new();

/// The process-wide list, built on first use by [`load_default`]. Every
/// consumer reads this one.
pub fn shared() -> Arc<AllowedHosts> {
    Arc::clone(SHARED.get_or_init(|| Arc::new(load_default())))
}
