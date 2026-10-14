//! OBS/osc integration: package creation, checkout, source sync, and build.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use ap_core::event::Event;

use crate::obs_status;
use crate::proc::{self, Captured, Emit};

/// Where `osc` runs when the command is not scoped to a checkout.
fn anywhere() -> PathBuf {
    std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/"))
}

/// Fired at most once per process the first time `rpmspec --parse` is
/// invoked, surfacing the known host-vs-target-repo macro-mismatch gap the
/// 2026-10-06 external review flagged. Behavior is unchanged — this just
/// tells the user to treat a passing `rpmspec` as a cheap syntax check, not
/// as a reproduction of what the OBS worker will do.
fn emit_host_macro_warning(emit: Emit<'_>) {
    use std::sync::Once;
    static WARNED: Once = Once::new();
    WARNED.call_once(|| {
        emit.reporter.report(&Event::Warning {
            step: emit.step,
            message: "`rpmspec --parse` validates specs using host RPM macros, not \
                 the target OBS build repo's macros — a clean parse here does \
                 not guarantee the OBS worker will see the same expansions \
                 (e.g. `%python3_sitelib`, `%python_module`, `%suse_version`). \
                 Treat it as a syntax check, not a reproduction."
                .to_string(),
        });
    });
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("could not run osc: {0}")]
    Launch(std::io::Error),

    #[error(transparent)]
    Process(#[from] proc::ProcessError),

    /// A launch failure or timeout, named after the osc subcommand it was for.
    #[error("{source} (during osc {cmd})")]
    Command {
        cmd: String,
        #[source]
        source: proc::ProcessError,
    },

    #[error("osc {cmd} failed (exit {code}):\n{stderr}")]
    OscFailed {
        cmd: String,
        code: i32,
        stderr: String,
    },

    #[error("rpmspec --parse {spec} failed (exit {code}):\n{stderr}")]
    RpmspecFailed {
        spec: String,
        code: i32,
        stderr: String,
    },

    #[error("osc build failed for {spec} (exit {code}); log at {log}")]
    BuildFailed {
        spec: String,
        code: i32,
        log: PathBuf,
    },

    /// Build failed for a reason that no agent fix can address.
    /// The caller should stop immediately and report the message.
    #[error("not actionable: {0}")]
    NotActionable(String),

    /// One or more RPM dependencies could not be resolved. `names` holds the
    /// raw RPM package names (e.g. `python314-puccinialin`) the caller
    /// should queue for building and retry this one against; `detail` holds
    /// the verbatim `nothing provides ...`/`(got version ...)` lines behind
    /// them, when OBS gave any — the only way to tell a genuinely missing
    /// package apart from a version constraint this spec itself wrote that
    /// the repo's one available version can never satisfy (e.g.
    /// `setuptools < 70.0.0` when only 80.9.0 is shipped).
    #[error("unresolvable RPM dependencies: {}", .names.join(", "))]
    UnresolvableDeps {
        names: Vec<String>,
        detail: Vec<String>,
    },

    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

/// OBS remote build status for a single arch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BuildStatus {
    Succeeded,
    Failed,
    Unresolvable,
    Building,
    Scheduled,
    Blocked,
    Broken,
    Disabled,
    Excluded,
    Unknown(String),
}

impl BuildStatus {
    pub fn is_terminal(&self) -> bool {
        matches!(
            self,
            BuildStatus::Succeeded
                | BuildStatus::Failed
                | BuildStatus::Unresolvable
                | BuildStatus::Broken
                | BuildStatus::Disabled
                | BuildStatus::Excluded
        )
    }

    /// Decode one of `osc results <project>`'s single-letter per-package
    /// status codes (see its own "Legend:" footer).
    fn from_code(c: char) -> Self {
        match c {
            '.' => BuildStatus::Succeeded,
            ' ' => BuildStatus::Disabled,
            'U' => BuildStatus::Unresolvable,
            'F' => BuildStatus::Failed,
            'B' => BuildStatus::Broken,
            'b' => BuildStatus::Blocked,
            '%' | 'd' | 'S' => BuildStatus::Building,
            's' => BuildStatus::Scheduled,
            'x' => BuildStatus::Excluded,
            other => BuildStatus::Unknown(other.to_string()),
        }
    }

    pub fn is_success(&self) -> bool {
        matches!(self, BuildStatus::Succeeded)
    }
}

impl std::fmt::Display for BuildStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            BuildStatus::Succeeded => "succeeded",
            BuildStatus::Failed => "failed",
            BuildStatus::Unresolvable => "unresolvable",
            BuildStatus::Building => "building",
            BuildStatus::Scheduled => "scheduled",
            BuildStatus::Blocked => "blocked",
            BuildStatus::Broken => "broken",
            BuildStatus::Disabled => "disabled",
            BuildStatus::Excluded => "excluded",
            BuildStatus::Unknown(s) => s.as_str(),
        };
        write!(f, "{s}")
    }
}

impl From<&str> for BuildStatus {
    fn from(s: &str) -> Self {
        match s {
            "succeeded" => BuildStatus::Succeeded,
            "failed" => BuildStatus::Failed,
            "unresolvable" => BuildStatus::Unresolvable,
            "building" => BuildStatus::Building,
            "scheduled" => BuildStatus::Scheduled,
            "blocked" => BuildStatus::Blocked,
            "broken" => BuildStatus::Broken,
            "disabled" => BuildStatus::Disabled,
            "excluded" => BuildStatus::Excluded,
            other => BuildStatus::Unknown(other.to_string()),
        }
    }
}

/// Structured classification of why a build failed.
#[derive(Debug)]
pub enum FailureKind {
    NoRepositories,
    NotWorkingCopy,
    UnresolvableDeps {
        names: Vec<String>,
        detail: Vec<String>,
    },
    BuildFailed,
}

/// Classify a failed build log into a structured failure kind.
fn classify_build_failure(log: &str) -> FailureKind {
    if log.contains("no repositories defined") {
        return FailureKind::NoRepositories;
    }
    if log.contains("is not a working copy") {
        return FailureKind::NotWorkingCopy;
    }
    let names = extract_unresolvable(log);
    if !names.is_empty() {
        let detail = extract_unresolvable_detail(log);
        return FailureKind::UnresolvableDeps { names, detail };
    }
    FailureKind::BuildFailed
}

/// Extract RPM package names from "nothing provides <pkg>" lines in a build log.
///
/// Handles both bare `nothing provides <pkg>` and `unresolvable: nothing provides <pkg>`,
/// strips any trailing `needed by ...` clause, and deduplicates.
fn extract_unresolvable(log: &str) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    let mut result = Vec::new();
    for raw_line in log.lines() {
        let line = raw_line.trim();
        let pkg_str = if let Some(rest) = line.strip_prefix("unresolvable: nothing provides ") {
            rest.trim()
        } else if let Some(rest) = line.strip_prefix("nothing provides ") {
            rest.trim()
        } else {
            continue;
        };
        // Take just the package name; drop " needed by ..." suffix.
        let pkg = pkg_str.split_whitespace().next().unwrap_or(pkg_str);
        let pkg = clean_unresolvable_name(pkg);
        if !pkg.is_empty() && seen.insert(pkg.to_string()) {
            result.push(pkg.to_string());
        }
    }
    result
}

/// Strip decoration OBS leaves on a "nothing provides <name>" token so the
/// result is an actual RPM name: a `(`/`)` from a rich `(A or B)` requires
/// group (e.g. `python313-duckdb)`), and a trailing `[flavor]` multibuild
/// annotation (e.g. `python314-pybind11[global]`). Left in place, these
/// aren't valid RPM or PyPI names, so `strip_rpm_prefix` and PyPI lookup
/// both miss and the dependency silently never gets queued to build.
fn clean_unresolvable_name(pkg: &str) -> &str {
    let pkg = pkg.trim_start_matches('(').trim_end_matches(')');
    match pkg.rfind('[') {
        Some(idx) if pkg.ends_with(']') => &pkg[..idx],
        _ => pkg,
    }
}

/// Extract the verbatim `nothing provides ...` lines from a build log or
/// `osc results --verbose` output, together with any `(got version ...)`
/// continuation line immediately following one — unlike [`extract_unresolvable`],
/// which throws away everything but the bare package name, this keeps the
/// version constraint and the version OBS actually has, e.g.:
///   `nothing provides python314-setuptools < 70.0.0`
///   `(got version 80.9.0-3.3)`
/// That pairing is the only signal that distinguishes a genuinely missing
/// package from a version constraint the spec itself wrote that the repo's
/// one available version can never satisfy — `extract_unresolvable`'s bare
/// name alone looks identical in both cases.
fn extract_unresolvable_detail(log: &str) -> Vec<String> {
    let mut result = Vec::new();
    let lines: Vec<&str> = log.lines().collect();
    for (i, raw_line) in lines.iter().enumerate() {
        let line = raw_line.trim();
        if !line.starts_with("nothing provides")
            && !line.starts_with("unresolvable: nothing provides")
        {
            continue;
        }
        result.push(line.to_string());
        if let Some(next) = lines.get(i + 1) {
            let next = next.trim();
            if next.starts_with('(') {
                result.push(next.to_string());
            }
        }
    }
    result
}

#[cfg(test)]
mod extract_unresolvable_tests {
    use super::*;

    /// Regression test: a rich `(A or B)` requires group leaves a stray `)`
    /// on the last alternative's "nothing provides" line, and a multibuild
    /// flavor annotation leaves a trailing `[flavor]`. Either one turns the
    /// extracted name into something neither `strip_rpm_prefix` nor a PyPI
    /// lookup recognizes, so the dependency silently drops out of the queue
    /// instead of being built.
    #[test]
    fn strips_boolean_group_parens_and_flavor_brackets() {
        let log = "\
unresolvable: nothing provides python314-pytest-rerunfailures
unresolvable: nothing provides python313-duckdb)
nothing provides python314-pybind11[global] needed by python314-fastar
nothing provides (python313-foo
";
        let names = extract_unresolvable(log);
        assert_eq!(
            names,
            vec![
                "python314-pytest-rerunfailures",
                "python313-duckdb",
                "python314-pybind11",
                "python313-foo",
            ]
        );
    }

    #[test]
    fn leaves_ordinary_names_untouched() {
        assert_eq!(
            clean_unresolvable_name("python314-duckdb"),
            "python314-duckdb"
        );
    }
}

/// RPM package name for a Python package: `python-<name>` (lowercased).
///
/// This is only a guess at the real name for a package that does not exist
/// yet — openSUSE's actual Python packaging has no single deterministic rule
/// for how a PyPI name's `-`/`_`/`.` characters map to the RPM name: some
/// real packages convert underscores to hyphens (PyPI's `pydantic_core` is
/// `python-pydantic-core`, `mistral_common` is `python-mistral-common`),
/// while just as many keep the underscore as-is (`typing_extensions`,
/// `setuptools_scm`, `mypy_extensions`, `prompt_toolkit`, `async_timeout` are
/// all packaged with the literal underscore), and dotted names always keep
/// the dot (`ruamel.yaml`, `jaraco.classes`, `backports.zstd`) — confirmed by
/// checking real openSUSE package listings, not derivable from the PyPI name
/// alone. Never compare this against a real package list with `==`/
/// `.contains()` for an existence check — use [`fuzzy_key`] on both sides, and
/// when it matches, use the real listing's spelling, not this guess.
pub fn rpm_name(pypi_name: &str) -> String {
    format!("python-{}", pypi_name.to_lowercase())
}

/// A comparison-only key for matching two RPM/PyPI package names that might
/// spell the same project with a different `-`/`_`/`.` choice (see
/// [`rpm_name`] for why no single guess is reliable): lowercase with every
/// `-`, `_`, and `.` removed entirely, so `pydantic_core`, `pydantic-core`,
/// and `pydantic.core` all collapse to the same key. Two distinct real PyPI
/// projects differing only in separator choice are not a realistic
/// collision to worry about here.
///
/// Use this to test "does a package matching this name already exist",
/// never to construct a name to actually build or commit under — when a
/// match is found, use the real listing's own spelling instead.
pub fn fuzzy_key(name: &str) -> String {
    name.chars()
        .filter(|c| !matches!(c, '-' | '_' | '.'))
        .flat_map(|c| c.to_lowercase())
        .collect()
}

#[cfg(test)]
mod fuzzy_key_tests {
    use super::*;

    #[test]
    fn treats_hyphen_underscore_and_dot_as_equivalent() {
        assert_eq!(
            fuzzy_key("python-pydantic-core"),
            fuzzy_key("python-pydantic_core")
        );
        assert_eq!(
            fuzzy_key("python-ruamel.yaml"),
            fuzzy_key("python-ruamel-yaml")
        );
        assert_eq!(
            fuzzy_key("Python-Backports.Zstd"),
            fuzzy_key("python-backports_zstd")
        );
    }

    #[test]
    fn distinct_names_still_produce_distinct_keys() {
        assert_ne!(
            fuzzy_key("python-pydantic-core"),
            fuzzy_key("python-pydantic")
        );
    }
}

/// Whether `s` is safe to hand to `osc` as a project or package name and to
/// join into a path: no leading `-` (would be parsed as an option), no path
/// separators or `..`, nothing but the characters OBS itself allows.
pub fn is_valid_obs_name(s: &str) -> bool {
    let mut chars = s.chars();
    chars
        .next()
        .is_some_and(|c| c.is_ascii_alphanumeric() || c == '_')
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '+' | '-' | ':'))
        && !s.contains("..")
}

/// Attach the osc subcommand to a launch failure or timeout, which
/// `ProcessError` reports without saying which `osc` call it was.
fn during(cmd: impl Into<String>) -> impl FnOnce(proc::ProcessError) -> Error {
    move |source| Error::Command {
        cmd: cmd.into(),
        source,
    }
}

/// First non-empty line of a command's output, for one-line diagnostics.
fn first_line(s: &str) -> &str {
    s.lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("")
}

/// Whether `osc` reported a missing project, package or file on the server.
/// A bare `404` would also match unrelated digits, so the HTTP wording is
/// required; `osc` goes through urllib, which prints `HTTP Error 404: Not Found`.
fn is_not_found(text: &str) -> bool {
    text.contains("HTTP Error 404")
        || text.contains("404 Client Error")
        || text.contains("Not Found")
}

fn check_obs_name(what: &str, name: &str) -> Result<(), Error> {
    if is_valid_obs_name(name) {
        Ok(())
    } else {
        Err(Error::NotActionable(format!(
            "refusing to use {what} {name:?}: not a valid OBS name"
        )))
    }
}

/// Strip the `python<digits>-` or `python-` prefix from an RPM name to get
/// the underlying package name (PyPI name for Python packages, or the bare
/// name for system packages).
pub fn strip_rpm_prefix(rpm: &str) -> &str {
    // Match `python` + optional digits + `-`, e.g. `python313-`, `python3-`, `python-`.
    if let Some(rest) = rpm.strip_prefix("python") {
        let rest = rest.trim_start_matches(|c: char| c.is_ascii_digit());
        if let Some(name) = rest.strip_prefix('-') {
            return name;
        }
    }
    rpm
}

/// Strip the openSUSE devel-package wrapping from a BuildRequires name so it
/// names the upstream project, not the distro packaging convention around it
/// — `libtrantor-devel` -> `trantor`, `libfoo-dev` -> `foo` (convention 27's
/// examples). The agent is told to do this itself before reporting a C/C++
/// dependency as missing, but doesn't always (the pyarrow case: a `NoChange`
/// turn left `libarrow-devel` to fall back to OBS's raw name, which then
/// canonicalized to the `arrow` monorepo and looped back onto pyarrow
/// itself). Only strips when the `-devel`/`-dev` suffix is present — a bare
/// `lib...` name with no such suffix is frequently the real upstream/runtime
/// library name and must not be touched.
pub fn strip_devel_wrapping(name: &str) -> &str {
    let Some(stripped) = name
        .strip_suffix("-devel")
        .or_else(|| name.strip_suffix("-dev"))
    else {
        return name;
    };
    stripped.strip_prefix("lib").unwrap_or(stripped)
}

#[cfg(test)]
mod strip_devel_wrapping_tests {
    use super::*;

    #[test]
    fn strips_lib_prefix_and_devel_suffix() {
        assert_eq!(strip_devel_wrapping("libtrantor-devel"), "trantor");
    }

    #[test]
    fn strips_lib_prefix_and_dev_suffix() {
        assert_eq!(strip_devel_wrapping("libfoo-dev"), "foo");
    }

    #[test]
    fn strips_devel_suffix_without_lib_prefix() {
        assert_eq!(strip_devel_wrapping("apache-arrow-devel"), "apache-arrow");
    }

    #[test]
    fn leaves_a_bare_lib_name_without_devel_suffix_untouched() {
        assert_eq!(strip_devel_wrapping("libyaml"), "libyaml");
    }

    #[test]
    fn leaves_unrelated_names_untouched() {
        assert_eq!(strip_devel_wrapping("numpy"), "numpy");
    }
}

/// Timeout for [`distro_provides`]: a local `zypper search`, not a network
/// round trip to OBS, but `--no-refresh` still has to read each configured
/// repo's on-disk solv cache, which is slow on a cold page cache.
const ZYPPER_QUERY_TIMEOUT: Duration = Duration::from_secs(30);

/// Parse `/etc/zypp/repos.d/*.repo` (plain INI) and return the `baseurl` of
/// every repo whose `[alias]` section does not set `enabled=0`.
///
/// Reads the config files directly instead of `zypper lr`'s table output —
/// that output wraps and column-packs under narrow terminal widths (see
/// `get_project_build_statuses`'s doc comment for a case where exactly this
/// bit a different `osc`/`zypper` table parser), and it's one less process
/// to spawn.
fn configured_repo_urls() -> Vec<(String, String)> {
    let Ok(entries) = fs::read_dir("/etc/zypp/repos.d") else {
        return Vec::new();
    };
    let mut repos = Vec::new();
    for entry in entries.flatten() {
        if entry.path().extension().and_then(|e| e.to_str()) != Some("repo") {
            continue;
        }
        let Ok(contents) = fs::read_to_string(entry.path()) else {
            continue;
        };
        let mut alias: Option<String> = None;
        let mut baseurl: Option<String> = None;
        let mut enabled = true;
        for line in contents.lines() {
            let line = line.trim();
            if let Some(name) = line.strip_prefix('[').and_then(|s| s.strip_suffix(']')) {
                if let (Some(a), Some(u)) = (alias.take(), baseurl.take())
                    && enabled
                {
                    repos.push((a, u));
                }
                alias = Some(name.to_string());
                baseurl = None;
                enabled = true;
                continue;
            }
            if let Some(v) = line.strip_prefix("baseurl=") {
                baseurl = Some(v.trim().to_string());
            } else if let Some(v) = line.strip_prefix("enabled=") {
                enabled = v.trim() != "0";
            }
        }
        if let (Some(a), Some(u)) = (alias, baseurl)
            && enabled
        {
            repos.push((a, u));
        }
    }
    repos
}

/// Whether `baseurl` is a genuine openSUSE base-distribution tree rather
/// than a third-party vendor repo (`packman`, `google-cloud-cli`, …) or an
/// arbitrary OBS sub-project (`home:*`, `openSUSE:Tools`, …) that happens to
/// publish a package under a name that collides with what's being searched
/// for. Trusts two shapes:
///
///  - the plain released binary tree, no `/repositories/` segment at all
///    (`download.opensuse.org/tumbleweed/repo/oss/`,
///    `.../update/tumbleweed/`, `.../debug/tumbleweed/repo/oss/`);
///  - the base distro project published *as* an OBS project
///    (`download.opensuse.org/repositories/openSUSE:/Tumbleweed/...` or
///    `.../openSUSE:/Factory/...` or `.../openSUSE:/Leap:...`).
///
/// Any other `/repositories/<namespace>/...` is someone's personal or devel
/// project — never "the distro already has this", no matter how official
/// its packages look.
fn is_base_distro_repo(baseurl: &str) -> bool {
    let Some(path) = baseurl
        .strip_prefix("http://download.opensuse.org/")
        .or_else(|| baseurl.strip_prefix("https://download.opensuse.org/"))
    else {
        return false;
    };
    let Some(rest) = path.strip_prefix("repositories/") else {
        // No `/repositories/` segment at all: the plain released tree.
        return true;
    };
    rest.starts_with("openSUSE:/Tumbleweed/")
        || rest.starts_with("openSUSE:/Factory/")
        || rest.starts_with("openSUSE:/Leap:")
}

/// Cache of `(capability -> provided)` for the lifetime of this process.
/// `distro_provides` is called once per reported dependency, repeatedly
/// across rounds for a package stuck reporting the same name, and
/// potentially for the same system capability (`openssl`, `gcc`, …) from
/// several unrelated packages in one run — without this, each repeat spawns
/// its own `zypper` subprocess, up to [`ZYPPER_QUERY_TIMEOUT`] apiece.
static DISTRO_PROVIDES_CACHE: Mutex<Option<HashMap<String, bool>>> = Mutex::new(None);

/// Whether the local system's configured base-distro zypper repositories
/// already provide `capability` — checked with `zypper search --provides
/// --match-exact`, i.e. the exact capability name, not a name/summary
/// substring search, restricted via `--repo` to repos [`is_base_distro_repo`]
/// trusts.
///
/// This exists because the queue-filter checks elsewhere in this crate
/// (`try_list_project_packages` / `get_project_repo_paths`, matched by
/// [`fuzzy_key`]) only ever compare against *source package names* in OBS
/// projects — and a C library's RPM package name routinely embeds its
/// SONAME version (`libpng16`, `libopenssl3`) where the bare upstream
/// project name (`libpng`, `openssl`) does not. No separator/case
/// normalization bridges that gap, so a dependency the distro has shipped
/// for years still reads as "not found anywhere" and gets queued for a
/// redundant from-source build (observed: `libpng`, already `libpng16-16`
/// / `libpng16-devel` in openSUSE Tumbleweed, got probed, cloned from
/// upstream, and queued as a brand-new from-source C package).
///
/// `zypper search --provides` matches the actual RPM `Provides:` capability
/// list, not just the package's own name, so it catches exactly this case:
/// `libpng16-16` carries a compatibility `Provides: libpng` even though its
/// package name doesn't contain the string. `--match-exact` avoids the
/// opposite failure mode — a plain substring search would also match
/// unrelated packages that merely contain `capability` somewhere in a
/// longer provides string.
///
/// Restricting to [`is_base_distro_repo`] repos matters as much as
/// `--match-exact` does: an unrestricted search also matches whatever else
/// happens to be enabled locally (a personal `home:*` project, Packman, a
/// cloud vendor's own repo, …), none of which the actual OBS build target
/// resolves against — treating one of *those* as "the distro already has
/// it" would silently and permanently drop a dependency the build can never
/// actually satisfy, which is worse than today's "queue a redundant
/// from-source build". If no local repo is recognized as a base-distro repo
/// at all, this returns `false` rather than guessing from the full,
/// untrusted repo set.
///
/// This still assumes the local machine tracks the same distribution the
/// build target does (true for this tool's own use: it runs on the same
/// openSUSE Tumbleweed the target project builds against). A stale or
/// missing local repo cache only produces a false negative — capability
/// reported as not provided — which just falls through to the existing
/// from-source-build path, so it fails open to today's behavior rather than
/// silently skipping a dependency that's actually missing.
pub fn distro_provides(capability: &str) -> bool {
    let capability = capability.trim();
    if capability.is_empty() {
        return false;
    }

    if let Some(hit) = DISTRO_PROVIDES_CACHE
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_ref()
        .and_then(|cache| cache.get(capability).copied())
    {
        return hit;
    }

    let trusted_aliases: Vec<String> = configured_repo_urls()
        .into_iter()
        .filter(|(_, url)| is_base_distro_repo(url))
        .map(|(alias, _)| alias)
        .collect();
    // `None` means the query itself failed (timeout, launch, ...): that is
    // not an answer, so it is neither cached nor allowed to stick for the run.
    let provided = if trusted_aliases.is_empty() {
        Some(false)
    } else {
        // `--repo` is a `search` subcommand option, not a global one — it
        // must come after `search`, not before it (zypper rejects it
        // outright, "The flag --repo is not known", otherwise).
        let mut args: Vec<String> = vec![
            "--non-interactive".into(),
            "--no-refresh".into(),
            "search".into(),
        ];
        for alias in &trusted_aliases {
            args.push("--repo".into());
            args.push(alias.clone());
        }
        args.push("--provides".into());
        args.push("--match-exact".into());
        args.push(capability.to_string());
        match proc::capture_with_timeout(
            "zypper",
            &args,
            &anywhere(),
            Emit::quiet(),
            ZYPPER_QUERY_TIMEOUT,
        ) {
            Ok(out) if out.success() => Some(true),
            // zypper's "capability not found" exit: a real answer, cacheable.
            Ok(out) if out.code == ZYPPER_CAPABILITY_NOT_FOUND => Some(false),
            Ok(out) => {
                ap_core::diag!(
                    "  zypper search for {capability}: exit {}: {}",
                    out.code,
                    first_line(&out.stderr)
                );
                None
            }
            Err(e) => {
                ap_core::diag!("  zypper search for {capability}: {e}");
                None
            }
        }
    };

    let Some(provided) = provided else {
        return false;
    };
    DISTRO_PROVIDES_CACHE
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get_or_insert_with(HashMap::new)
        .insert(capability.to_string(), provided);
    provided
}

/// zypper's exit code for "capability not found" (`ZYPPER_EXIT_INF_CAP_NOT_FOUND`).
const ZYPPER_CAPABILITY_NOT_FOUND: i32 = 104;

#[cfg(test)]
mod distro_provides_tests {
    use super::*;

    #[test]
    fn an_empty_capability_is_never_provided() {
        // Guards against `zypper search --provides --match-exact ""`,
        // which would otherwise shell out and list unrelated packages
        // instead of cleanly reporting "no capability to check".
        assert!(!distro_provides(""));
        assert!(!distro_provides("   "));
    }

    #[test]
    fn trusts_the_plain_released_tree_and_the_base_distro_obs_project() {
        assert!(is_base_distro_repo(
            "http://download.opensuse.org/tumbleweed/repo/oss/"
        ));
        assert!(is_base_distro_repo(
            "http://download.opensuse.org/update/tumbleweed/"
        ));
        assert!(is_base_distro_repo(
            "https://download.opensuse.org/repositories/openSUSE:/Tumbleweed/standard/"
        ));
        assert!(is_base_distro_repo(
            "https://download.opensuse.org/repositories/openSUSE:/Factory/standard/"
        ));
    }

    #[test]
    fn distrusts_vendor_repos_and_arbitrary_obs_sub_projects() {
        assert!(!is_base_distro_repo(
            "https://ftp.gwdg.de/pub/linux/misc/packman/suse/openSUSE_Tumbleweed/"
        ));
        assert!(!is_base_distro_repo(
            "https://packages.cloud.google.com/yum/repos/cloud-sdk-el10-x86_64"
        ));
        assert!(!is_base_distro_repo(
            "https://download.opensuse.org/repositories/home:/someone:/project/openSUSE_Tumbleweed/"
        ));
        assert!(!is_base_distro_repo(
            "https://download.opensuse.org/repositories/openSUSE:/Tools/openSUSE_Tumbleweed"
        ));
    }
}

/// Return the `(project, repository)` pairs the OBS project's repositories
/// build against.
///
/// Parses `osc meta prj <project>` XML and extracts every
/// `<path project="..." repository="..."/>` element. These are the upstream
/// projects and repositories (e.g. `openSUSE:Factory`, repository
/// `snapshot`) whose packages are considered "already available" and do not
/// need to be built.
///
/// The repository half of each pair is deliberately not used by either
/// caller to filter further ([`select_target`]'s resolvability check and
/// the CLI's queue filter both just list every package name in
/// `path_project`, regardless of that package's build status *in* the named
/// repository) — a package whose build in the linked repo is disabled or
/// broken there is currently still treated as available here. Narrowing that
/// would need a per-repo build-status query in addition to this one; nothing
/// does that today.
///
/// A failed query is an `Err`, never an empty list: empty reads as "no build
/// paths", which widens the set of packages reported missing.
pub fn get_project_repo_paths(
    project: &str,
    emit: Emit<'_>,
) -> Result<Vec<(String, String)>, Error> {
    let cmd = format!("meta prj {project}");
    let out = proc::capture_with_timeout(
        "osc",
        &["meta", "prj", project],
        &anywhere(),
        emit,
        OSC_QUERY_TIMEOUT,
    )
    .map_err(during(cmd.clone()))?;
    if !out.success() {
        return Err(Error::OscFailed {
            cmd,
            code: out.code,
            stderr: out.stderr,
        });
    }

    Ok(parse_project_repo_paths(&out.stdout))
}

/// Parsing half of [`get_project_repo_paths`], split out so it is testable
/// without shelling out to `osc`.
fn parse_project_repo_paths(xml: &str) -> Vec<(String, String)> {
    let mut paths = Vec::new();
    for chunk in xml.split("<path ").skip(1) {
        let tag = &chunk[..chunk.find("/>").unwrap_or(chunk.len())];
        let project_name = extract_xml_attr(tag, "project");
        let repo_name = extract_xml_attr(tag, "repository");
        if let (Some(p), Some(r)) = (project_name, repo_name) {
            let pair = (p, r);
            if !paths.contains(&pair) {
                paths.push(pair);
            }
        }
    }
    paths
}

/// Does the OBS project meta list at least one `<repository>` element?
///
/// A project with no repositories cannot build anything — every `osc build`
/// against it fails immediately with `no repositories defined` (see
/// [`FailureKind::NoRepositories`]). Catching that at the top of a run gives
/// the user a clear, actionable error up front instead of a per-package
/// `NotActionable` after each queued package has already been checked out.
///
/// Returns `Ok(true)` when at least one `<repository name="...">` is present,
/// `Ok(false)` when the project meta loaded but has none, and `Err` when the
/// `osc meta prj` call itself failed (project doesn't exist, auth broken, ...).
pub fn project_has_repositories(project: &str, emit: Emit<'_>) -> Result<bool, Error> {
    let out = proc::capture_with_timeout(
        "osc",
        &["meta", "prj", project],
        &anywhere(),
        emit,
        OSC_QUERY_TIMEOUT,
    )
    .map_err(during(format!("meta prj {project}")))?;
    if !out.success() {
        return Err(Error::OscFailed {
            cmd: format!("meta prj {project}"),
            code: out.code,
            stderr: out.stderr,
        });
    }
    Ok(parse_has_repository(&out.stdout))
}

/// Parsing half of [`project_has_repositories`], split out so it is testable
/// without shelling out to `osc`.
fn parse_has_repository(xml: &str) -> bool {
    xml.contains("<repository ") || xml.contains("<repository\n") || xml.contains("<repository\t")
}

/// Extract `attr="value"` from a fragment of XML attribute text.
fn extract_xml_attr(tag: &str, attr: &str) -> Option<String> {
    let needle = format!("{attr}=\"");
    let start = tag.find(&needle)? + needle.len();
    let rest = &tag[start..];
    let end = rest.find('"')?;
    Some(rest[..end].to_owned())
}

static PROJECT_PACKAGES_CACHE: OnceLock<
    Mutex<HashMap<String, Arc<std::collections::HashSet<String>>>>,
> = OnceLock::new();

/// Per-project count of invalidations. A fetch that started before an
/// invalidation holds a listing that may predate the package just committed;
/// the reader compares the generation it saw before fetching and drops the
/// result if it moved, instead of caching the stale set for the whole run.
static PROJECT_PACKAGES_GENERATION: OnceLock<Mutex<HashMap<String, u64>>> = OnceLock::new();

fn project_generation(project: &str) -> u64 {
    PROJECT_PACKAGES_GENERATION
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(project)
        .copied()
        .unwrap_or(0)
}

/// Insert `pkgs` for `project` unless it was invalidated since `generation`
/// was read. The generation is checked under the cache lock, and
/// invalidation bumps it before taking that lock, so no interleaving lets a
/// stale set land after the removal.
fn insert_if_current(
    project: &str,
    generation: u64,
    pkgs: &Arc<std::collections::HashSet<String>>,
) {
    let cache = PROJECT_PACKAGES_CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    let mut guard = cache.lock().unwrap_or_else(|e| e.into_inner());
    if project_generation(project) == generation {
        guard.insert(project.to_owned(), Arc::clone(pkgs));
    }
}

/// Return the set of package names that exist in `project`.
///
/// The result is cached per project name so repeated calls within a process
/// skip the network round-trip — callers look names up once per dependency,
/// so without caching a single batch can mean dozens of `osc ls` round-trips.
/// Callers that successfully commit a *new* package into `project` MUST call
/// [`invalidate_project_packages_cache`] afterward: a long `auto-package
/// build` run adds packages to the same project continuously, and this
/// cache has no TTL and no other invalidation path. Without that call, the
/// very first lookup permanently freezes the answer for the rest of the
/// process — every package built after it would look like it still doesn't
/// exist to every dependent package's availability check, for the entire
/// remainder of the run, no matter how many more succeed.
///
/// Errors when the listing cannot be fetched, so a caller cannot mistake
/// "OBS did not answer" for "the package is absent".
pub fn try_list_project_packages(
    project: &str,
    emit: Emit<'_>,
) -> Result<Arc<std::collections::HashSet<String>>, String> {
    let cache = PROJECT_PACKAGES_CACHE.get_or_init(|| Mutex::new(HashMap::new()));

    {
        let guard = cache.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(pkgs) = guard.get(project) {
            return Ok(Arc::clone(pkgs));
        }
    }

    let generation = project_generation(project);
    // A failed listing must not be cached: an empty set would make every
    // package look absent from OBS for the rest of the run. Retry once, then
    // report the error so the next call tries again.
    let pkgs = fetch_project_packages(project, emit)
        .or_else(|_| fetch_project_packages(project, Emit::quiet()))
        .map(Arc::new)
        .map_err(|e| format!("could not list packages of {project}: {e}"))?;
    insert_if_current(project, generation, &pkgs);
    Ok(pkgs)
}

/// Drop `project`'s cached package listing so the next [`try_list_project_packages`]
/// call re-fetches it. Call this right after successfully building/committing
/// a package into `project` — see [`try_list_project_packages`]'s doc comment for
/// why skipping this silently breaks every later availability check for the
/// rest of the run, not just the one for the package just built.
pub fn invalidate_project_packages_cache(project: &str) {
    // Bump first: a fetch in flight must see the change even if it reaches
    // `insert_if_current` before the removal below.
    *PROJECT_PACKAGES_GENERATION
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .entry(project.to_owned())
        .or_insert(0) += 1;
    if let Some(cache) = PROJECT_PACKAGES_CACHE.get() {
        cache
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(project);
    }
}

#[cfg(test)]
mod project_packages_cache_tests {
    use super::*;

    /// Regression test for the bug where a long run permanently froze every
    /// package's availability at whatever the cache first saw: builds z3
    /// -solver, pydantic-core, etc. later in the same run kept looking
    /// missing to every dependent package no matter how many succeeded,
    /// because nothing ever dropped the stale entry.
    #[test]
    fn invalidate_drops_the_cached_entry() {
        let project = "ap-test-cache-invalidation-project";
        let cache = PROJECT_PACKAGES_CACHE.get_or_init(|| Mutex::new(HashMap::new()));
        cache
            .lock()
            .unwrap()
            .insert(project.to_string(), Arc::new(std::collections::HashSet::new()));
        assert!(cache.lock().unwrap().contains_key(project));

        invalidate_project_packages_cache(project);

        assert!(!cache.lock().unwrap().contains_key(project));
    }

    /// A worker that read the listing before an invalidation must not put its
    /// stale set back afterwards.
    #[test]
    fn invalidation_between_fetch_and_insert_is_not_overwritten() {
        let project = "ap-test-cache-stale-insert-project";
        let generation = project_generation(project);
        // ...fetch happens here, then another worker commits and invalidates...
        invalidate_project_packages_cache(project);
        insert_if_current(project, generation, &Arc::default());
        let cache = PROJECT_PACKAGES_CACHE.get_or_init(|| Mutex::new(HashMap::new()));
        assert!(!cache.lock().unwrap().contains_key(project));

        // A fetch that started after the invalidation is cached normally.
        insert_if_current(project, project_generation(project), &Arc::default());
        assert!(cache.lock().unwrap().contains_key(project));
    }

    /// A project this call never touched must not be affected — invalidation
    /// is per-project, not a blanket cache flush of every project this
    /// process has ever looked at.
    #[test]
    fn invalidate_does_not_touch_the_cache_before_anything_is_in_it() {
        // Just needs to not panic when the cache is empty or lacks the key.
        invalidate_project_packages_cache("ap-test-project-never-cached");
    }
}

/// Enumerate the files committed to an OBS package.
///
/// Returns `None` when `osc ls` fails (project/package doesn't exist, auth
/// broken) rather than panicking, same as [`cat_file`]: the update sweep
/// uses that `None` to skip the package and record a SpecFetchFailed instead
/// of killing the whole scan.
///
/// Used so the update sweep can discover the real `.spec` filename rather
/// than guess `<pkg>.spec` — AppCo's versioned-library convention gives one
/// OBS package per upstream version (`pytorch+2.4`, `pytorch+2.5`), each
/// holding a plain `<base-name>.spec` (`pytorch.spec`) that has no relation
/// to the OBS package name. The guess silently missed every one of these
/// before (Oct 6 review finding #3).
pub fn list_package_files(project: &str, pkg: &str, emit: Emit<'_>) -> Option<Vec<String>> {
    let out = osc_query(&["ls", project, pkg], emit, &format!("ls {project}/{pkg}"))?;
    if !out.success() {
        return None;
    }
    Some(
        out.stdout
            .lines()
            .map(|l| l.trim().to_string())
            .filter(|l| !l.is_empty())
            .collect(),
    )
}

/// Fetch a single file's contents from an OBS package without checking out.
///
/// Returns `None` when the package doesn't exist, the file isn't in it, or
/// `osc cat` otherwise fails — the update sweep uses that `None` to skip the
/// package instead of blowing up the whole run on a stray missing spec.
pub fn cat_file(project: &str, pkg: &str, filename: &str, emit: Emit<'_>) -> Option<String> {
    let out = osc_query(
        &["cat", project, pkg, filename],
        emit,
        &format!("cat {project}/{pkg} {filename}"),
    )?;
    if !out.success() {
        return None;
    }
    Some(out.stdout)
}

fn fetch_project_packages(
    project: &str,
    emit: Emit<'_>,
) -> Result<std::collections::HashSet<String>, String> {
    let out = proc::capture_with_timeout(
        "osc",
        &["ls", project],
        &anywhere(),
        emit,
        // A whole distro project (openSUSE:Factory) is a long listing.
        Duration::from_secs(180),
    )
    .map_err(|e| e.to_string())?;
    if !out.success() {
        return Err(format!("osc ls exited {}: {}", out.code, out.stderr.trim()));
    }
    Ok(parse_package_listing(&out.stdout))
}

/// Package names from `osc ls <project>`, in the spelling OBS stores them
/// (OBS names are case-sensitive: `python-APScheduler`). A multibuild package
/// is listed as `<source>:<flavor>` (`apache-arrow:python-pyarrow`) and its
/// flavor is itself a package that gets built and published (python313-pyarrow),
/// so the flavor name counts as available too — otherwise a dependency Factory
/// already ships looks absent and gets queued to be built again.
fn parse_package_listing(stdout: &str) -> std::collections::HashSet<String> {
    let mut out = std::collections::HashSet::new();
    for name in stdout.lines().map(str::trim).filter(|s| !s.is_empty()) {
        if let Some((_, flavor)) = name.split_once(':')
            && flavor.starts_with("python")
        {
            out.insert(flavor.to_string());
        }
        out.insert(name.to_string());
    }
    out
}

/// The name OBS lists for the package `guess` refers to, or `guess` itself
/// when nothing listed matches.
///
/// `guess` is an [`rpm_name`] guess, which is lowercase and may spell the
/// separators differently from the real package. A listed name matches when
/// its [`fuzzy_key`] equals the guess's, so `python-apscheduler` finds
/// `python-APScheduler`. Anything that checks out, builds or names a package
/// in `project` must use this spelling, not the guess: osc, the checkout
/// directory and the spec file all follow the real name.
pub fn obs_name(listing: &std::collections::HashSet<String>, guess: &str) -> String {
    if listing.contains(guess) {
        return guess.to_string();
    }
    let key = fuzzy_key(guess);
    // Several listed spellings can share a key; take the first in sort order
    // so the answer does not depend on hash iteration order.
    listing
        .iter()
        .filter(|p| fuzzy_key(p) == key)
        .min()
        .cloned()
        .unwrap_or_else(|| guess.to_string())
}

/// Create an initial `.changes` entry for a new package using `osc vc`.
///
/// Only runs when the `.changes` file does not yet exist. `osc vc` reads the
/// committer identity from `~/.config/osc/oscrc` and writes a datestamped
/// entry at the top of the file.
pub fn create_initial_changes(
    checkout_dir: &Path,
    pkg_name: &str,
    emit: Emit<'_>,
) -> Result<(), Error> {
    let changes_name = format!("{pkg_name}.changes");
    if checkout_dir.join(&changes_name).exists() {
        return Ok(());
    }
    let out = proc::capture(
        "osc",
        &["vc", "-m", "Initial packaging"],
        checkout_dir,
        emit,
    )
    .map_err(during(format!("vc in {}", checkout_dir.display())))?;
    if !out.success() {
        return Err(Error::OscFailed {
            cmd: format!("vc -m \"Initial packaging\" in {}", checkout_dir.display()),
            code: out.code,
            stderr: out.stderr,
        });
    }
    Ok(())
}

/// Ensure the package exists in `project`, creating it if not.
///
/// Creates it by pushing package meta directly (`osc meta pkg ... -F -`,
/// which the server creates implicitly if it doesn't exist yet) rather than
/// `osc mkpac`. `mkpac` infers "already registered" from local working-copy
/// state — a stray directory left over from a previous, incomplete attempt,
/// or a project checkout that still remembers the package from one — and
/// happily reports success without the package ever having been created on
/// the server. That false success surfaces much later as a 404 on `osc ci`.
/// Pushing meta directly has no such local state to get confused by: the
/// server is the only thing that decides whether the package exists.
pub fn ensure_package(
    project: &str,
    pkg_name: &str,
    work_dir: &Path,
    emit: Emit<'_>,
) -> Result<(), Error> {
    check_obs_name("project", project)?;
    check_obs_name("package", pkg_name)?;
    // `osc ls project/pkg` exits non-zero when the package does not exist.
    let check = proc::capture_with_timeout(
        "osc",
        &["ls", &format!("{project}/{pkg_name}")],
        work_dir,
        emit,
        OSC_QUERY_TIMEOUT,
    )
    .map_err(during(format!("ls {project}/{pkg_name}")))?;

    if check.success() {
        return Ok(());
    }
    // OBS names are case-sensitive, so the failed lookup above does not rule
    // out a package that differs only in case or separators
    // (`python-apscheduler` vs. `python-APScheduler`). Creating one would
    // leave the project with two packages for one upstream, so refuse and let
    // the caller use the existing name.
    let listing = try_list_project_packages(project, emit).map_err(Error::NotActionable)?;
    if let Some(existing) = listing
        .iter()
        .filter(|p| fuzzy_key(p) == fuzzy_key(pkg_name))
        .min()
    {
        return Err(Error::NotActionable(format!(
            "{project} already has {existing}; refusing to create {pkg_name} next to it"
        )));
    }

    let meta = format!(
        "<package name=\"{}\" project=\"{}\">\n  \
         <title/>\n  <description/>\n</package>\n",
        xml_escape(pkg_name),
        xml_escape(project)
    );
    let meta_file = work_dir.join(format!(".{pkg_name}.meta.xml"));
    fs::write(&meta_file, &meta).map_err(|e| Error::OscFailed {
        cmd: format!("meta pkg {project} {pkg_name} (writing temp meta file)"),
        code: -1,
        stderr: e.to_string(),
    })?;

    let out = proc::capture_with_timeout(
        "osc",
        &[
            "meta",
            "pkg",
            project,
            pkg_name,
            "-F",
            &meta_file.to_string_lossy(),
        ],
        work_dir,
        emit,
        OSC_QUERY_TIMEOUT,
    );
    let _ = fs::remove_file(&meta_file);
    let out = out.map_err(during(format!("meta pkg {project} {pkg_name}")))?;

    if !out.success() {
        return Err(Error::OscFailed {
            cmd: format!("meta pkg {project} {pkg_name} -F -"),
            code: out.code,
            stderr: out.stderr,
        });
    }

    Ok(())
}

/// Check out `project/pkg_name` into `into_dir`.
///
/// Returns the path of the checkout directory, which is
/// `into_dir/<project>/<pkg_name>/`.
pub fn checkout(
    project: &str,
    pkg_name: &str,
    into_dir: &Path,
    emit: Emit<'_>,
) -> Result<PathBuf, Error> {
    check_obs_name("project", project)?;
    check_obs_name("package", pkg_name)?;
    let checkout_dir = into_dir.join(project).join(pkg_name);

    // Re-use an existing checkout rather than re-cloning every run.
    if checkout_dir.exists() {
        let out = osc_up_recovering(&checkout_dir, emit)?;

        if !out.success() {
            // 404 means `ensure_package`'s meta push hasn't propagated to
            // this checkout's view of the server yet — use the directory
            // as-is rather than failing on a lag that resolves itself.
            if is_not_found(&out.stderr) {
                return Ok(checkout_dir);
            }
            return Err(Error::OscFailed {
                cmd: format!("up {project}/{pkg_name} in {}", checkout_dir.display()),
                code: out.code,
                stderr: out.stderr,
            });
        }

        return Ok(checkout_dir);
    }

    // Every `osc co` into the same directory rewrites the shared
    // `<project>/.osc/_packages` file (write `.new`, rename). With several
    // packages checking out in parallel (`--jobs`, or roots added from the
    // TUI) the renames race and one fails with FileNotFoundError, so
    // serialise checkouts process-wide.
    static CHECKOUT_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _guard = CHECKOUT_LOCK.lock().unwrap_or_else(|e| e.into_inner());

    let out = proc::capture_with_timeout(
        "osc",
        &["co", &format!("{project}/{pkg_name}")],
        into_dir,
        emit,
        OSC_TRANSFER_TIMEOUT,
    )
    .map_err(during(format!("co {project}/{pkg_name}")))?;

    if !out.success() {
        return Err(Error::OscFailed {
            cmd: format!("co {project}/{pkg_name}"),
            code: out.code,
            stderr: out.stderr,
        });
    }

    Ok(checkout_dir)
}

/// Run `osc up` from `checkout_dir` to update the working copy.
///
/// Non-fatal: an already-up-to-date or brand-new checkout exits non-zero; we
/// ignore those cleanly.
pub fn pull(checkout_dir: &Path, emit: Emit<'_>) -> Result<(), Error> {
    let out = osc_up_recovering(checkout_dir, emit)?;
    if !out.success() {
        // 404 means `ensure_package`'s meta push hasn't propagated to this
        // checkout's view of the server yet — the directory is valid,
        // nothing to pull.
        if !is_benign_up_output(&out.stdout)
            && !is_benign_up_output(&out.stderr)
            && !is_not_found(&out.stdout)
            && !is_not_found(&out.stderr)
        {
            return Err(Error::OscFailed {
                cmd: format!("up in {}", checkout_dir.display()),
                code: out.code,
                stderr: out.stderr,
            });
        }
    }
    Ok(())
}

/// Whether `text` is osc's own "nothing to do" report from `osc up`: a whole
/// line that is `nothing to update` or `At revision <n>.`. Matching a line
/// rather than a substring keeps a real failure that merely quotes those
/// words (a path, a package name, a traceback) an error.
fn is_benign_up_output(text: &str) -> bool {
    let mut lines = text
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .peekable();
    lines.peek().is_some()
        && lines.all(|l| {
            l == "nothing to update"
                || l.strip_prefix("At revision ")
                    .and_then(|r| r.strip_suffix('.'))
                    .is_some_and(|r| !r.is_empty() && r.bytes().all(|b| b.is_ascii_digit()))
        })
}

/// Run `osc up` in `checkout_dir`, recovering in-place from the two known
/// self-inflicted working-copy failure modes before handing the (possibly
/// still-failing) result back to the caller: a prior run that died
/// mid-`up`/`ci` leaving `.osc/` inconsistent ([`is_inconsistent_wc_error`],
/// fixed by [`repair_wc`]), and an untracked local file colliding with a
/// same-named file the server gained in the meantime
/// ([`changes_conflict_file`], fixed by moving the local one aside).
///
/// Shared by [`checkout`]'s reuse path and [`pull`] so both get the same
/// recovery — `checkout`'s "osc up" on an existing directory is exactly the
/// same command `pull` runs, and the same interrupted-run history that leaves
/// `.osc/` inconsistent or a stray untracked `.changes` file behind can be hit
/// from either entry point, not just the one a fix happened to land in first.
/// Returns the final `osc up`'s output either way — success or not — so each
/// caller can keep interpreting failure (which stderr patterns are benign,
/// whether 404 is a non-error) in its own, different way.
fn osc_up_recovering(checkout_dir: &Path, emit: Emit<'_>) -> Result<Captured, Error> {
    let up = || {
        proc::capture_with_timeout("osc", &["up"], checkout_dir, emit, OSC_TRANSFER_TIMEOUT)
            .map_err(during(format!("up in {}", checkout_dir.display())))
    };
    let out = up()?;
    if out.success() {
        return Ok(out);
    }

    if is_inconsistent_wc_error(&out.stderr) {
        repair_wc(checkout_dir, emit)?;
        return up();
    }

    if let Some(name) = changes_conflict_file(&out.stderr)
        .filter(|n| !n.contains('/') && !n.contains("..") && !n.starts_with('-'))
    {
        // osc only raises this when `name` is untracked in this working
        // copy's `.osc/_files` (see `Package.__update`'s check against
        // `filenamelist_unvers`), so it is always safe to move aside — it
        // was never osc's file to begin with. The usual cause here is our
        // own `create_initial_changes` writing `<pkg>.changes` directly
        // before that revision's commit reached the server, and the
        // server later gaining its own file of the same name in the
        // meantime (e.g. a prior interrupted run that got further).
        let name = name.to_string();
        let conflicting = checkout_dir.join(&name);
        // This checkout can legitimately go through this recovery more than
        // once across retried pipeline attempts — `fs::rename` overwrites
        // silently on Unix, so without the existence check a second recovery
        // would clobber whatever the first one had moved aside with no trace
        // either backup ever existed.
        let mut backup = checkout_dir.join(format!("{name}.preexisting"));
        let mut n = 1;
        while backup.exists() {
            n += 1;
            backup = checkout_dir.join(format!("{name}.preexisting.{n}"));
        }
        if let Err(e) = std::fs::rename(&conflicting, &backup) {
            ap_core::diag!(
                "  could not move '{name}' aside in {}: {e}",
                checkout_dir.display()
            );
        }
        let backup_name = backup.file_name().map(|n| n.to_string_lossy().into_owned());
        emit.reporter.report(&Event::Warning {
            step: emit.step,
            message: format!(
                "osc up: untracked local '{name}' conflicted with a newer revision's \
                 file of the same name; moved it aside as '{}' and retrying",
                backup_name.as_deref().unwrap_or("<unknown>")
            ),
        });
        return up();
    }

    Ok(out)
}

/// Extract the filename osc names in its uncaught `PackageFileConflict`
/// message from `Package.__update()` (`osc/obs_scm/package.py`): `failed to
/// add file '<name>' file/dir with the same name already exists`. Raised by
/// `osc up` when a file of that name sits on disk but untracked in this
/// working copy while the server's newer revision also adds a file with the
/// same name — an unhandled exception in osc itself, not one of its usual
/// "inconsistent working copy" messages, so `repairwc` does not apply here.
fn changes_conflict_file(stderr: &str) -> Option<&str> {
    stderr
        .split("failed to add file '")
        .nth(1)?
        .split('\'')
        .next()
}

#[cfg(test)]
mod changes_conflict_tests {
    use super::*;

    #[test]
    fn extracts_the_filename_from_oscs_package_file_conflict_message() {
        let stderr = "osc: failed to add file 'python-foo.changes' \
                       file/dir with the same name already exists\n";
        assert_eq!(changes_conflict_file(stderr), Some("python-foo.changes"));
    }

    #[test]
    fn returns_none_for_unrelated_errors() {
        assert_eq!(changes_conflict_file("404 Not Found"), None);
    }
}

#[cfg(test)]
mod not_found_tests {
    use super::*;

    #[test]
    fn recognizes_osc_http_not_found_wording() {
        assert!(is_not_found("urllib: HTTP Error 404: Not Found\n"));
        assert!(is_not_found("404 Client Error: Not Found for url"));
    }

    #[test]
    fn a_bare_number_containing_404_is_not_a_missing_resource() {
        assert!(!is_not_found("request id 1404 timed out"));
        assert!(!is_not_found("Skipping foo: conflict"));
    }
}

/// Return true if `checkout_dir`'s working copy has uncommitted local
/// changes (e.g. a spec version bump from a run that crashed before `osc
/// commit`). `osc status` prints one line per changed file and nothing when
/// the working copy is clean.
pub fn has_local_changes(checkout_dir: &Path, emit: Emit<'_>) -> bool {
    let Ok(out) = proc::capture("osc", &["status"], checkout_dir, emit) else {
        return true;
    };
    if out.success() {
        return !out.stdout.trim().is_empty();
    }
    if is_inconsistent_wc_error(&out.stderr) && repair_wc(checkout_dir, emit).is_ok() {
        return proc::capture("osc", &["status"], checkout_dir, emit)
            .map(|out| !out.stdout.trim().is_empty())
            .unwrap_or(true);
    }
    // `osc status` failed for a reason other than the one recognized,
    // repairable case (a network/server hiccup, say), or `repair_wc` itself
    // failed — either way `out.stdout` is from a failed invocation and
    // almost certainly empty, which used to fall through to the same
    // "no local changes" read as an actually-clean checkout. The caller
    // uses this to gate skipping a rebuild (see the `AlreadyBuilt` fast path
    // in pipeline.rs), so treat "couldn't tell" as "has changes" — silently
    // skipping a needed rebuild is far worse than running an unnecessary one.
    true
}

/// Whether an `osc` command failed because the working copy's `.osc/`
/// metadata is inconsistent — the message `osc` itself prints when a prior
/// run died mid-`up`/`ci` and left it half-written, naming `osc repairwc .`
/// as the fix.
fn is_inconsistent_wc_error(stderr: &str) -> bool {
    stderr.contains("inconsistent state") || stderr.contains("repairwc")
}

/// Run `osc repairwc .` to fix a working copy `osc` itself reports as
/// inconsistent (see [`is_inconsistent_wc_error`]). Note this can remove
/// files from `.osc/` — exactly what `osc`'s own error message warns about —
/// so it is only ever called after that specific error, not speculatively.
pub fn repair_wc(checkout_dir: &Path, emit: Emit<'_>) -> Result<(), Error> {
    let out = proc::capture("osc", &["repairwc", "."], checkout_dir, emit)
        .map_err(during(format!("repairwc in {}", checkout_dir.display())))?;
    if !out.success() {
        return Err(Error::OscFailed {
            cmd: format!("repairwc in {}", checkout_dir.display()),
            code: out.code,
            stderr: out.stderr,
        });
    }
    Ok(())
}

/// Run `osc service ra` from `checkout_dir` to fetch sources and vendor Cargo deps.
pub fn run_service(checkout_dir: &Path, emit: Emit<'_>) -> Result<(), Error> {
    let out = proc::capture("osc", &["service", "ra"], checkout_dir, emit)
        .map_err(during(format!("service ra in {}", checkout_dir.display())))?;
    if !out.success() {
        return Err(Error::OscFailed {
            cmd: format!("service ra in {}", checkout_dir.display()),
            code: out.code,
            stderr: out.stderr,
        });
    }
    Ok(())
}

/// Run `rpmspec --parse` on a spec file and check for syntax errors.
///
/// Judges on exit status only (warnings in stderr are not treated as failures).
/// Returns the expanded spec on success (stdout from `rpmspec --parse`).
///
/// **Caveat (flagged by the 2026-10-06 external review):** `rpmspec` expands
/// macros using the **host** RPM configuration, not the target build repo's
/// configuration. A spec that `rpmspec --parse`s cleanly on this machine can
/// still fail to parse (or expand to different Requires / BuildRequires) on
/// the OBS build worker if the host's macro set diverges from the target
/// distro's — `%python3_sitelib`, `%python_module`, `%suse_version` and the
/// like are all host-local here. The agent still relies on this as a cheap
/// pre-flight syntax check; the one-shot [`emit_host_macro_warning`] call
/// below surfaces the gap to the user. A durable fix would require
/// containerized validation against the target repo's rpm-config.
pub fn rpmspec_check(spec_path: &Path, emit: Emit<'_>) -> Result<String, Error> {
    emit_host_macro_warning(emit);

    let out = proc::capture(
        "rpmspec",
        &["--parse", &spec_path.to_string_lossy()],
        &anywhere(),
        emit,
    )?;

    if !out.success() {
        return Err(Error::RpmspecFailed {
            spec: spec_path.display().to_string(),
            code: out.code,
            stderr: out.stderr,
        });
    }

    Ok(out.stdout)
}

/// Commit the package to OBS, triggering a remote build.
///
/// Runs `osc addremove` first to track any new or deleted files, then
/// `osc ci` to push the changes to OBS.
pub fn commit(checkout_dir: &Path, message: &str, emit: Emit<'_>) -> Result<(), Error> {
    // Sync tracked files with directory contents. A failure is not fatal (a
    // new package may have nothing to add), but `ci` then commits without
    // them, so say so.
    match proc::capture("osc", &["addremove"], checkout_dir, emit) {
        Ok(out) if !out.success() => ap_core::diag!(
            "  osc addremove in {}: exit {}: {}",
            checkout_dir.display(),
            out.code,
            first_line(&out.stderr)
        ),
        Err(e) => ap_core::diag!("  osc addremove in {}: {e}", checkout_dir.display()),
        Ok(_) => {}
    }

    let out = proc::capture_with_timeout(
        "osc",
        &["ci", "-m", message],
        checkout_dir,
        emit,
        OSC_TRANSFER_TIMEOUT,
    )
    .map_err(during(format!("ci in {}", checkout_dir.display())))?;

    if !out.success() {
        // "nothing to do" means files are already committed — not an error.
        if out.stdout.contains("nothing to do") || out.stderr.contains("nothing to do") {
            return Ok(());
        }
        return Err(Error::OscFailed {
            cmd: format!("ci -m {message:?} in {}", checkout_dir.display()),
            code: out.code,
            stderr: out.stderr,
        });
    }
    Ok(())
}

/// Query the OBS build status for one repo/arch combination.
///
/// Reads `/build/<project>/_result` (XML) and interprets it with
/// [`obs_status::resolve`]: a bare `finished` is resolved through its
/// `<details>` verdict, and a terminal result in a `dirty` repository is only
/// trusted once the job history confirms it was built from the source
/// revision currently on the server (otherwise `Unknown("stale: ...")`).
///
/// If the API call or its parse fails, falls back to scraping `osc results`
/// so a surprising response shape degrades to the old behaviour instead of
/// breaking the build loop.
pub fn get_build_status(
    project: &str,
    pkg_name: &str,
    repo: &str,
    arch: &str,
    emit: Emit<'_>,
) -> Result<BuildStatus, Error> {
    if let Some(status) = get_build_status_xml(project, pkg_name, repo, arch) {
        return Ok(status);
    }
    get_build_status_from_results_table(project, pkg_name, repo, arch, emit)
}

/// The `osc results` human-table path only — no XML, no staleness
/// resolution via job history. Used by [`get_build_status`] as its fallback
/// when the XML `_result` document is unreadable, and by [`obs_build`] as an
/// independent second opinion when the XML path's answer has stopped
/// changing for a long time: the two paths hit different OBS code (an XML
/// API document vs. a human-table-rendering endpoint), so if one of them is
/// silently serving a stale/cached snapshot, the other is the cheapest way
/// to notice without guessing at why.
fn get_build_status_from_results_table(
    project: &str,
    pkg_name: &str,
    repo: &str,
    arch: &str,
    emit: Emit<'_>,
) -> Result<BuildStatus, Error> {
    let cmd = format!("results --verbose -r {repo} -a {arch} {project} {pkg_name}");
    // `--verbose` is what carries a `finished` result's verdict.
    let out = proc::capture_with_timeout(
        "osc",
        &[
            "results",
            "--verbose",
            "-r",
            repo,
            "-a",
            arch,
            project,
            pkg_name,
        ],
        &anywhere(),
        emit,
        OSC_QUERY_TIMEOUT,
    )
    .map_err(during(cmd.clone()))?;

    if !out.success() {
        return Err(Error::OscFailed {
            cmd,
            code: out.code,
            stderr: out.stderr,
        });
    }

    Ok(parse_single_build_status(
        &out.stdout,
        pkg_name,
        repo,
        arch,
    ))
}

/// Per-call limit for the short, network-bound `osc` queries used while
/// polling. Generous for a healthy OBS, but finite: see
/// [`proc::capture_with_timeout`].
const OSC_QUERY_TIMEOUT: Duration = Duration::from_secs(60);

/// Bound for `osc co`, `up` and `ci`, which move a package's sources.
const OSC_TRANSFER_TIMEOUT: Duration = Duration::from_secs(15 * 60);

/// Bound for `osc buildlog` and `getbinaries`, which move whole logs and RPM
/// sets and so run longer than a source transfer.
const OSC_ARTIFACT_TIMEOUT: Duration = Duration::from_secs(30 * 60);

/// Run a short network-bound `osc` query in the current directory. A launch
/// failure or timeout is reported and yields `None`, for callers that treat a
/// failed query as "no answer" rather than as an error.
fn osc_query(args: &[&str], emit: Emit<'_>, what: &str) -> Option<Captured> {
    proc::capture_with_timeout("osc", args, &anywhere(), emit, OSC_QUERY_TIMEOUT)
        .map_err(|e| ap_core::diag!("  osc {what}: {e}"))
        .ok()
}

/// `osc api <path>` stdout, or `None` on any failure or timeout.
fn osc_api(path: &str) -> Option<String> {
    let out = proc::capture_with_timeout(
        "osc",
        &["api", path],
        &anywhere(),
        Emit::quiet(),
        OSC_QUERY_TIMEOUT,
    )
    .map_err(|e| ap_core::diag!("  osc api {path}: {e}"))
    .ok()?;
    if !out.success() {
        ap_core::diag!("  osc api {path}: exit {}: {}", out.code, out.stderr.trim());
        return None;
    }
    Some(out.stdout)
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// Percent-encode one URL component. Package names like `libsigc++` contain
/// `+`, which a query string would otherwise read as a space, and `&` or `#`
/// would end the value outright.
fn enc(s: &str) -> std::borrow::Cow<'_, str> {
    urlencoding::encode(s)
}

fn get_build_status_xml(
    project: &str,
    pkg_name: &str,
    repo: &str,
    arch: &str,
) -> Option<BuildStatus> {
    let xml = osc_api(&format!(
        "/build/{project}/_result?package={}&repository={}&arch={}",
        enc(pkg_name),
        enc(repo),
        enc(arch)
    ))?;
    let entry = obs_status::find_entry(&xml, repo, arch, pkg_name)?;
    let evidence = if obs_status::needs_evidence(&entry) {
        obs_status::Evidence {
            source: osc_api(&format!("/source/{project}/{}?view=info", enc(pkg_name)))
                .and_then(|x| obs_status::parse_source_info(&x)),
            job: osc_api(&format!(
                "/build/{project}/{}/{}/_jobhistory?package={}&limit=1",
                enc(repo),
                enc(arch),
                enc(pkg_name)
            ))
            .and_then(|x| obs_status::parse_latest_job(&x)),
        }
    } else {
        obs_status::Evidence::default()
    };
    Some(obs_status::resolve(&entry, &evidence))
}

/// Parsing half of the `osc results` fallback in [`get_build_status`], split
/// out so it is testable without shelling out to `osc`.
///
/// `osc results --verbose -r <repo> -a <arch> <project> <pkg>` prints one line
/// per result as `<repo> <arch> <pkg> <status>` (osc's `get_results` format,
/// columns in that order). The status is the rest of the line:
/// - a plain code (`succeeded`, `failed`, `scheduled`, ...), optionally with
///   `: <details>` after it, and `(unpublished)` for a build that succeeded
///   but is not published yet;
/// - `finished: <verdict>`: OBS hides the verdict behind the bare `finished`
///   code, and `--verbose` prints it as the details;
/// - `outdated (was: ...)` when the repository is dirty, or a trailing `*` in
///   the non-verbose form: the result may be for an older source revision.
///
/// Multi-line `unresolvable:` details only add lines that do not start with
/// the repo, so reading the first matching line is enough.
fn parse_single_build_status(stdout: &str, pkg_name: &str, repo: &str, arch: &str) -> BuildStatus {
    for line in stdout.lines() {
        let mut rest = line.trim_start();
        let mut columns = [""; 3];
        for col in &mut columns {
            let end = rest.find(char::is_whitespace).unwrap_or(rest.len());
            *col = &rest[..end];
            rest = rest[end..].trim_start();
        }
        if columns != [repo, arch, pkg_name] {
            continue;
        }
        return status_from_osc_text(rest.trim());
    }
    BuildStatus::Unknown("no results".to_string())
}

/// The status part of one `osc results` line (see [`parse_single_build_status`]).
fn status_from_osc_text(text: &str) -> BuildStatus {
    // OBS marks a result as possibly older than the sources on the server:
    // the dirty-repository form, which `--verbose` spells `outdated (was: ..)`
    // and the plain form spells with a trailing `*`. Never trust those as-is:
    // reporting the stripped word as plain `Succeeded` would make the
    // `AlreadyBuilt` fast path in pipeline.rs believe a since-superseded
    // result still verifies the current revision. `Unknown` makes that path
    // fall through and resync instead.
    if text.starts_with("outdated") {
        return BuildStatus::Unknown(format!("stale: {text}"));
    }
    let (head, details) = match text.split_once(':') {
        Some((head, details)) => (head.trim(), Some(details.trim())),
        None => (text, None),
    };
    let starred = head.ends_with('*');
    let word = head
        .trim_end_matches('*')
        .trim_end_matches("(unpublished)");
    if starred {
        return BuildStatus::Unknown(format!("stale: {word}"));
    }
    if word == "finished" {
        // The verdict is the first word of the details, if osc printed it.
        return match details.and_then(|d| d.split_whitespace().next()) {
            Some(verdict) => BuildStatus::from(verdict),
            None => BuildStatus::Unknown("finished".to_string()),
        };
    }
    BuildStatus::from(word)
}

#[cfg(test)]
mod single_build_status_tests {
    use super::*;

    const REPO: &str = "openSUSE_Tumbleweed";
    const ARCH: &str = "x86_64";

    fn parse(stdout: &str) -> BuildStatus {
        parse_single_build_status(stdout, "python-foo", REPO, ARCH)
    }

    #[test]
    fn a_plain_status_is_read_from_the_last_column() {
        // Real `osc results -r .. -a .. prj pkg` line: repo, arch, package, status.
        let stdout = "openSUSE_Tumbleweed  x86_64  python-foo  succeeded\n";
        assert_eq!(parse(stdout), BuildStatus::Succeeded);
    }

    #[test]
    fn a_starred_status_is_unknown_not_its_unstarred_meaning() {
        // The real incident this guards against: committing a new revision
        // and immediately watching build status used to report `Succeeded`
        // from the *previous* revision's still-displayed, now-stale result,
        // before OBS had even started rebuilding the new one.
        let stdout = "openSUSE_Tumbleweed  x86_64  python-foo  succeeded*\n";
        assert_eq!(
            parse(stdout),
            BuildStatus::Unknown("stale: succeeded".to_string())
        );
    }

    #[test]
    fn a_verbose_outdated_result_is_stale() {
        let stdout = "openSUSE_Tumbleweed x86_64 python-foo outdated (was: finished: succeeded)\n";
        assert!(matches!(parse(stdout), BuildStatus::Unknown(s) if s.starts_with("stale: ")));
    }

    #[test]
    fn a_bare_finished_takes_its_verdict_from_the_details() {
        let stdout = "openSUSE_Tumbleweed x86_64 python-foo finished: failed\n";
        assert_eq!(parse(stdout), BuildStatus::Failed);
        let stdout = "openSUSE_Tumbleweed x86_64 python-foo finished: succeeded\n";
        assert_eq!(parse(stdout), BuildStatus::Succeeded);
    }

    #[test]
    fn a_finished_without_a_verdict_is_not_terminal() {
        let stdout = "openSUSE_Tumbleweed x86_64 python-foo finished\n";
        let status = parse(stdout);
        assert_eq!(status, BuildStatus::Unknown("finished".to_string()));
        assert!(!status.is_terminal());
    }

    #[test]
    fn verbose_details_after_a_code_are_ignored() {
        let stdout = "openSUSE_Tumbleweed x86_64 python-foo failed: some details\n";
        assert_eq!(parse(stdout), BuildStatus::Failed);
    }

    #[test]
    fn an_unpublished_success_counts_as_succeeded() {
        let stdout = "openSUSE_Tumbleweed x86_64 python-foo succeeded(unpublished)\n";
        assert_eq!(parse(stdout), BuildStatus::Succeeded);
    }

    #[test]
    fn lines_for_other_repos_arches_and_packages_are_skipped() {
        let stdout = "\
openSUSE_Leap x86_64 python-foo failed
openSUSE_Tumbleweed aarch64 python-foo failed
openSUSE_Tumbleweed x86_64 python-foo:flavor failed
openSUSE_Tumbleweed x86_64 python-foo building
";
        assert_eq!(parse(stdout), BuildStatus::Building);
        assert_eq!(
            parse_single_build_status("", "python-foo", REPO, ARCH),
            BuildStatus::Unknown("no results".to_string())
        );
    }
}

/// Query the OBS build status of every package in `project` for one
/// repo/arch in a single call.
///
/// Runs `osc results -r <repo> -a <arch> <project>` (no package name lists
/// every package). Unlike the single-package form, this layout is NOT
/// `<pkg> ... <status-word>` per line: `osc` prints the package names as an
/// indented tree, one per line, wrapping into as many same-order blocks as
/// fit the terminal width, and after each block a single status line packs
/// one letter code per package into fixed two-column slots (`<code><sep>`),
/// e.g. `. B U U . U ...  openSUSE_Tumbleweed x86_64 (published)`. A blank
/// (space) code means "disabled" — indistinguishable from the column
/// separator under naive whitespace splitting — hence the positional
/// indexing here instead of `split_whitespace`.
pub fn get_project_build_statuses(
    project: &str,
    repo: &str,
    arch: &str,
    emit: Emit<'_>,
) -> Result<HashMap<String, BuildStatus>, Error> {
    // XML first: one entry per package with a named code, instead of the
    // positional letter table below (which depends on terminal width and
    // `osc`'s padding). Fall back to the table if the API path yields nothing.
    if let Some(xml) = osc_api(&format!(
        "/build/{}/_result?repository={}&arch={}",
        enc(project),
        enc(repo),
        enc(arch)
    )) {
        let statuses: HashMap<String, BuildStatus> = match obs_status::try_parse_results(&xml) {
            Ok(entries) => entries
                .into_iter()
                .filter(|e| e.repo == repo && e.arch == arch)
                .map(|e| (e.pkg.clone(), obs_status::resolve_lenient(&e)))
                .collect(),
            Err(e) => {
                // A cut-off listing must not read as a partial set of statuses.
                ap_core::diag!("  cannot read the OBS results listing: {e}");
                HashMap::new()
            }
        };
        if !statuses.is_empty() {
            return Ok(statuses);
        }
    }

    // An empty map reads as "no package has a status", so a failed listing
    // is an error rather than an empty answer.
    let cmd = format!("results -r {repo} -a {arch} {project}");
    let out = proc::capture_with_timeout(
        "osc",
        &["results", "-r", repo, "-a", arch, project],
        &anywhere(),
        emit,
        OSC_ARTIFACT_TIMEOUT,
    )
    .map_err(during(cmd.clone()))?;
    if !out.success() {
        return Err(Error::OscFailed {
            cmd,
            code: out.code,
            stderr: out.stderr,
        });
    }

    Ok(parse_project_build_statuses(&out.stdout, repo, arch))
}

/// Parsing half of [`get_project_build_statuses`], split out so it is
/// testable without shelling out to `osc`.
fn parse_project_build_statuses(
    stdout: &str,
    repo: &str,
    arch: &str,
) -> HashMap<String, BuildStatus> {
    let marker = format!(" {repo} {arch} (");
    let mut statuses = HashMap::new();
    let mut pending_pkgs: Vec<String> = Vec::new();

    for line in stdout.lines() {
        if line.trim() == "Legend:" {
            break;
        }
        if let Some(marker_idx) = line.find(&marker) {
            let chars: Vec<char> = line.chars().collect();
            let n = pending_pkgs.len();
            // The code table is dense two-char slots (`<code><sep>`), but
            // real `osc` builds disagree on whether there's a leading sep
            // before the first code and a trailing one after the last: some
            // print it flush (`code sep code ... code`, length `2n-1`, codes
            // at `2*i`), others pad both ends (`sep code sep ... code sep`,
            // length `2n+1`, codes at `1 + 2*i`). Derive the offset from the
            // actual prefix length instead of hardcoding one — hardcoding
            // `2*i` silently read every code as its own separator space
            // against the padded form, making every package look
            // "disabled" and so falsely resolvable.
            let prefix_len = line[..marker_idx].chars().count() as isize;
            let offset = if n > 0 {
                ((prefix_len - 2 * n as isize + 1) / 2).max(0) as usize
            } else {
                0
            };
            for (i, pkg) in pending_pkgs.iter().enumerate() {
                if let Some(&c) = chars.get(offset + 2 * i)
                    && c != ' '
                {
                    statuses.insert(pkg.clone(), BuildStatus::from_code(c));
                }
            }
            pending_pkgs.clear();
            continue;
        }
        let name = line.trim_start_matches([' ', '|']).trim();
        if !name.is_empty() {
            pending_pkgs.push(name.to_string());
        }
    }

    statuses
}

/// Fetch the OBS build log into `log_path`.
///
/// Runs `osc buildlog <project> <package> <repo> <arch>` and writes
/// stdout to `log_path`. Returns `Ok` even when the exit code is non-zero
/// if some output was captured (partial logs are still useful).
pub fn fetch_buildlog(
    project: &str,
    pkg_name: &str,
    repo: &str,
    arch: &str,
    log_path: &Path,
    emit: Emit<'_>,
) -> Result<(), Error> {
    let out = proc::capture_with_timeout(
        "osc",
        &["buildlog", project, pkg_name, repo, arch],
        &anywhere(),
        emit,
        OSC_ARTIFACT_TIMEOUT,
    )
    .map_err(during(format!(
        "buildlog {project} {pkg_name} {repo} {arch}"
    )))?;

    std::fs::write(log_path, &out.stdout)?;

    if !out.success() && out.stdout.is_empty() {
        return Err(Error::OscFailed {
            cmd: format!("buildlog {project} {pkg_name} {repo} {arch}"),
            code: out.code,
            stderr: out.stderr,
        });
    }
    Ok(())
}

/// Download built binaries from OBS into `dest_dir`.
///
/// How many of the binary RPMs OBS lists for `pkg_name` are now in `rpms_dir`.
///
/// Used after `osc getbinaries` to confirm it actually produced artifacts
/// rather than silently exiting 0 with an empty binary set (a misconfigured
/// publishing setup for the repo/arch pair). The RPMs are matched by the
/// names OBS lists, NOT by the source package name: a multi-flavor Python
/// package `python-events` ships `python313-Events-…rpm`, which no
/// `python-events-` prefix would ever match. If the listing cannot be read,
/// fall back to counting RPMs written in the last few minutes.
fn downloaded_rpm_count(
    project: &str,
    pkg_name: &str,
    repo: &str,
    arch: &str,
    rpms_dir: &std::path::Path,
) -> usize {
    let listing = osc_api(&format!(
        "/build/{project}/{}/{}/{}",
        enc(repo),
        enc(arch),
        enc(pkg_name)
    ))
    .map(|xml| obs_status::parse_binary_rpms(&xml));
    match listing {
        Some(names) => names.iter().filter(|n| rpms_dir.join(n).is_file()).count(),
        None => count_recent_rpms(rpms_dir, Duration::from_secs(600)),
    }
}

/// `.rpm` files (not `.src.rpm`) in `rpms_dir` modified within `window`.
/// Returns 0 when the directory doesn't exist or can't be read.
fn count_recent_rpms(rpms_dir: &std::path::Path, window: Duration) -> usize {
    let Ok(iter) = std::fs::read_dir(rpms_dir) else {
        return 0;
    };
    iter.filter_map(|e| e.ok())
        .filter(|e| {
            let n = e.file_name();
            let n = n.to_string_lossy();
            n.ends_with(".rpm") && !n.ends_with(".src.rpm")
        })
        .filter(|e| {
            e.metadata()
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| t.elapsed().ok())
                .is_some_and(|age| age <= window)
        })
        .count()
}

/// Runs `osc getbinaries --destdir=<dest> <project> <package> <repo> <arch>`.
/// Errors are non-fatal: the caller logs a warning and continues.
pub fn get_binaries(
    project: &str,
    pkg_name: &str,
    repo: &str,
    arch: &str,
    dest_dir: &Path,
    emit: Emit<'_>,
) -> Result<(), Error> {
    fs::create_dir_all(dest_dir)?;
    let dest = dest_dir.to_string_lossy();
    let out = proc::capture_with_timeout(
        "osc",
        &[
            "getbinaries",
            &format!("--destdir={dest}"),
            project,
            pkg_name,
            repo,
            arch,
        ],
        &anywhere(),
        emit,
        OSC_ARTIFACT_TIMEOUT,
    )
    .map_err(during(format!(
        "getbinaries {project} {pkg_name} {repo} {arch}"
    )))?;

    if !out.success() {
        return Err(Error::OscFailed {
            cmd: format!("getbinaries {project} {pkg_name} {repo} {arch}"),
            code: out.code,
            stderr: out.stderr,
        });
    }
    Ok(())
}

/// Try to extract unresolvable package names (and their raw version-detail
/// lines, see [`extract_unresolvable_detail`]) from `osc results --verbose`.
///
/// Used as a fallback when the build log doesn't contain "nothing provides" lines.
fn get_unresolvable_from_results(
    project: &str,
    pkg_name: &str,
    repo: &str,
    arch: &str,
    emit: Emit<'_>,
) -> (Vec<String>, Vec<String>) {
    let out = match proc::capture(
        "osc",
        &[
            "results",
            "--verbose",
            "-r",
            repo,
            "-a",
            arch,
            project,
            pkg_name,
        ],
        &anywhere(),
        emit,
    ) {
        Ok(out) => out,
        Err(e) => {
            ap_core::diag!("  osc results --verbose {project}/{pkg_name}: {e}");
            return (vec![], vec![]);
        }
    };
    (
        extract_unresolvable(&out.stdout),
        extract_unresolvable_detail(&out.stdout),
    )
}

/// Get the unresolvable package names (and raw detail lines) OBS already
/// knows about for a package, without triggering a new build: reads whatever
/// build log already sits at `log_path`, falling back to `osc results
/// --verbose` when that log has no "nothing provides" lines (the same
/// fallback [`obs_build`] uses for a build it just ran). Exposed so a caller
/// that finds a package already `Unresolvable` on restart can inform the fix
/// agent immediately instead of spending an attempt rediscovering it through
/// a fresh build.
pub fn current_unresolvable(
    project: &str,
    pkg_name: &str,
    repo: &str,
    arch: &str,
    log_path: &Path,
    emit: Emit<'_>,
) -> (Vec<String>, Vec<String>) {
    let log_text = fs::read_to_string(log_path).unwrap_or_default();
    let mut names = extract_unresolvable(&log_text);
    let mut detail = extract_unresolvable_detail(&log_text);
    if names.is_empty() {
        (names, detail) = get_unresolvable_from_results(project, pkg_name, repo, arch, emit);
    }
    (names, detail)
}

/// Wait for the OBS remote build to complete, fetch the log, and download
/// built RPMs on success.
///
/// The caller must commit the package to OBS before calling this (see `commit`).
/// Polls the OBS build status every 15 seconds, for at most two hours (the
/// `MAX_WAIT` below), then gives up with a timeout error. Emits a `Progress`
/// event each time the build status changes so the user can see progress.
pub fn obs_build(
    project: &str,
    pkg_name: &str,
    repo: &str,
    arch: &str,
    log_path: &Path,
    rpms_dir: &Path,
    emit: Emit<'_>,
) -> Result<(), Error> {
    let reporter = emit.reporter;
    let step = emit.step;
    const POLL_INTERVAL: Duration = Duration::from_secs(15);
    const MAX_WAIT: Duration = Duration::from_secs(7200);

    let start = Instant::now();
    let mut prev_status = String::new();

    // A single `osc` hiccup (network blip, OBS 5xx, rate limit) during a
    // build that can run for many minutes must not abort the wait and throw
    // the build away: tolerate a few consecutive poll failures, then give up.
    const MAX_CONSECUTIVE_POLL_ERRORS: u32 = 8;
    let mut poll_errors: u32 = 0;

    // The previous mechanism only ever broke silence for one specific case
    // (an `Unknown("stale: ...")` verdict). A plain non-terminal status
    // (`building`, `scheduled`, ...) that simply never changes again —
    // because the XML `_result` document this reads is serving a stale
    // snapshot while OBS has actually moved on — produced no `Progress`
    // event (nothing to report: the string didn't change) and no `Warning`
    // either, so the wait looked indistinguishable from a healthy long
    // build for up to the full `MAX_WAIT`. Reported live: the TUI sat on an
    // unchanging "building" for 1000+ seconds while OBS itself showed
    // nothing in progress.
    const NO_PROGRESS_WARNING: Duration = Duration::from_secs(120);
    const NO_PROGRESS_RECHECK: Duration = Duration::from_secs(300);
    let mut last_change = Instant::now();
    let mut last_warning = Instant::now();
    let mut rechecked_since_change = false;

    let status = loop {
        if start.elapsed() >= MAX_WAIT {
            return Err(Error::OscFailed {
                cmd: format!("wait {project}/{pkg_name}"),
                code: -1,
                stderr: format!(
                    "timed out after {}s waiting for OBS build (last status: {})",
                    MAX_WAIT.as_secs(),
                    if prev_status.is_empty() {
                        "none"
                    } else {
                        &prev_status
                    }
                ),
            });
        }

        let status = match get_build_status(project, pkg_name, repo, arch, Emit::quiet()) {
            Ok(s) => {
                poll_errors = 0;
                s
            }
            Err(e) => {
                poll_errors += 1;
                if poll_errors >= MAX_CONSECUTIVE_POLL_ERRORS {
                    return Err(Error::OscFailed {
                        cmd: format!("wait {project}/{pkg_name} ({repo}/{arch})"),
                        code: -1,
                        stderr: format!(
                            "gave up after {poll_errors} consecutive failed status polls; last error: {e}"
                        ),
                    });
                }
                reporter.report(&Event::Warning {
                    step,
                    message: format!(
                        "OBS status poll failed ({poll_errors}/{MAX_CONSECUTIVE_POLL_ERRORS}), retrying: {e}"
                    ),
                });
                std::thread::sleep(POLL_INTERVAL);
                continue;
            }
        };
        let status_str = status.to_string();

        let mut status = status;
        if status_str != prev_status {
            reporter.report(&Event::Progress {
                step,
                message: format!("OBS build status: {status_str}"),
            });
            prev_status = status_str;
            last_change = Instant::now();
            last_warning = Instant::now();
            rechecked_since_change = false;
        } else if !status.is_terminal() {
            // Non-terminal and unchanged: make sure the user can always
            // tell this is still alive and polling, not silently wedged.
            if last_warning.elapsed() >= NO_PROGRESS_WARNING {
                reporter.report(&Event::Warning {
                    step,
                    message: format!(
                        "OBS build status still {status_str} after {}s with no change",
                        last_change.elapsed().as_secs()
                    ),
                });
                last_warning = Instant::now();
            }
            // One independent cross-check per stuck period, via a
            // different OBS endpoint than whichever one produced `status`
            // (see `get_build_status_from_results_table`'s doc comment) —
            // catches one of them silently serving a stale snapshot while
            // the other has already moved on.
            if !rechecked_since_change && last_change.elapsed() >= NO_PROGRESS_RECHECK {
                rechecked_since_change = true;
                if let Ok(table_status) = get_build_status_from_results_table(
                    project,
                    pkg_name,
                    repo,
                    arch,
                    Emit::quiet(),
                ) {
                    let table_str = table_status.to_string();
                    if table_str != status_str {
                        reporter.report(&Event::Warning {
                            step,
                            message: format!(
                                "OBS status disagreement after {}s of no change: the API \
                                 document says {status_str}, but `osc results` says \
                                 {table_str} — trusting the independent read",
                                last_change.elapsed().as_secs()
                            ),
                        });
                        reporter.report(&Event::Progress {
                            step,
                            message: format!("OBS build status: {table_str}"),
                        });
                        prev_status = table_str;
                        last_change = Instant::now();
                        last_warning = Instant::now();
                        rechecked_since_change = false;
                        status = table_status;
                    }
                }
            }
        }

        if status.is_terminal() {
            break status;
        }

        // `stale: <verdict>` is not a final answer: that terminal result is
        // from a source revision the server may already have superseded, so
        // keep polling. Only a verdict confirmed for the current sources
        // (see `obs_status::resolve`) ends the wait; if none arrives, the
        // MAX_WAIT check above reports a timeout error.
        std::thread::sleep(POLL_INTERVAL);
    };

    // Fetch the log regardless of outcome — partial logs help diagnose failures.
    // A missing or stale log leaves the failure without its cause, so say so.
    // Announce the command that really writes `log_path`; the polling above
    // goes through the API and runs no `osc results --watch`.
    reporter.report(&Event::Command {
        step,
        program: "osc".to_string(),
        argv: vec![
            "buildlog".to_string(),
            project.to_string(),
            pkg_name.to_string(),
            repo.to_string(),
            arch.to_string(),
        ],
        cwd: anywhere(),
        log: Some(log_path.to_path_buf()),
    });
    if let Err(e) = fetch_buildlog(project, pkg_name, repo, arch, log_path, Emit::quiet()) {
        reporter.report(&Event::Warning {
            step,
            message: format!("could not fetch the OBS build log for {pkg_name}: {e}"),
        });
    }
    let log_bytes = log_path.metadata().map(|m| m.len()).unwrap_or(0);
    let elapsed = start.elapsed();

    if status.is_success() {
        // Download the built RPMs. The Oct 6 review's reproduction of
        // 'succeeded status followed by a failing getbinaries still
        // returns success, leaving the RPM directory empty' turned on
        // this call being non-fatal: a succeeded OBS build without any
        // locally-downloaded artifacts is NOT a success a dependent
        // build can rely on — `rpm_satisfies_dep` can't find anything,
        // the dep is reported unresolvable even though OBS says the
        // source is green, and the dependent spends attempts chasing a
        // nonexistent problem. Promote this to fatal: if OBS says
        // succeeded but we cannot pull the binaries, surface that as a
        // build failure.
        //
        // We also verify that at least one `.rpm` landed under
        // `rpms_dir` after the fetch — `osc getbinaries` can exit 0
        // with no files written when the project is misconfigured
        // (empty binary set for this repo/arch pair), which the warning
        // version silently papered over.
        let fetch_result = get_binaries(project, pkg_name, repo, arch, rpms_dir, emit);
        let downloaded_count = downloaded_rpm_count(project, pkg_name, repo, arch, rpms_dir);
        if let Err(e) = fetch_result {
            reporter.report(&Event::CommandDone {
                step,
                code: 1,
                elapsed,
                stdout_bytes: log_bytes,
                stderr_bytes: 0,
            });
            return Err(Error::NotActionable(format!(
                "OBS reported {pkg_name} succeeded but downloading the built RPMs \
                 failed: {e}. A green source without downloadable artifacts is not \
                 a usable build — a dependent package would see every \
                 BuildRequires as unresolvable. Re-check the OBS project's binary \
                 publishing config for repo={repo}/arch={arch}."
            )));
        }
        if downloaded_count == 0 {
            reporter.report(&Event::CommandDone {
                step,
                code: 1,
                elapsed,
                stdout_bytes: log_bytes,
                stderr_bytes: 0,
            });
            return Err(Error::NotActionable(format!(
                "OBS reported {pkg_name} succeeded and `osc getbinaries` exited 0, \
                 but no .rpm files appeared under {}. The project's binary \
                 publishing for repo={repo}/arch={arch} is probably misconfigured \
                 (empty binary set) — check the OBS <publish>/<binaries> settings.",
                rpms_dir.display()
            )));
        }
        reporter.report(&Event::CommandDone {
            step,
            code: 0,
            elapsed,
            stdout_bytes: log_bytes,
            stderr_bytes: 0,
        });
        return Ok(());
    }

    reporter.report(&Event::CommandDone {
        step,
        code: 1,
        elapsed,
        stdout_bytes: log_bytes,
        stderr_bytes: 0,
    });

    let log_text = fs::read_to_string(log_path).unwrap_or_default();

    Err(match status {
        BuildStatus::Unresolvable => {
            let (names, detail) =
                current_unresolvable(project, pkg_name, repo, arch, log_path, emit);
            Error::UnresolvableDeps { names, detail }
        }
        BuildStatus::Broken => Error::NotActionable("OBS: package source is broken".to_string()),
        BuildStatus::Disabled => {
            Error::NotActionable("OBS: build is disabled in project config".to_string())
        }
        BuildStatus::Excluded => {
            Error::NotActionable("OBS: build is excluded in project config".to_string())
        }
        _ => match classify_build_failure(&log_text) {
            FailureKind::NoRepositories => Error::NotActionable(
                "OBS project has no build repositories — configure them in OBS first".to_string(),
            ),
            FailureKind::NotWorkingCopy => {
                Error::NotActionable("osc checkout is not server-backed".to_string())
            }
            FailureKind::UnresolvableDeps { names, detail } => {
                Error::UnresolvableDeps { names, detail }
            }
            FailureKind::BuildFailed => Error::BuildFailed {
                spec: format!("{pkg_name}.spec"),
                code: 1,
                log: log_path.to_owned(),
            },
        },
    })
}

#[cfg(test)]
mod package_listing_tests {
    use super::*;

    #[test]
    fn only_oscs_own_up_to_date_lines_are_benign() {
        assert!(is_benign_up_output("At revision 12.\n"));
        assert!(is_benign_up_output("nothing to update"));
        assert!(!is_benign_up_output(""));
        assert!(!is_benign_up_output("Skipping foo: conflict"));
        assert!(!is_benign_up_output("Error: At revision 3. failed\n"));
        assert!(!is_benign_up_output("At revision 12.\nTraceback: boom"));
    }

    #[test]
    fn get_binaries_reports_an_uncreatable_destination() {
        // A regular file in the path makes `create_dir_all` fail before any
        // `osc` call, which used to be ignored.
        let file = std::env::temp_dir().join(format!("ap-osc-notdir-{}", std::process::id()));
        std::fs::write(&file, b"x").unwrap();
        let r = get_binaries("p", "pkg", "r", "a", &file.join("rpms"), Emit::quiet());
        std::fs::remove_file(&file).ok();
        assert!(r.is_err());
    }

    #[test]
    fn a_python_multibuild_flavor_counts_as_an_available_package() {
        let pkgs =
            parse_package_listing("apache-arrow\napache-arrow:python-pyarrow\nPython-Foo\n\n");
        assert!(pkgs.contains("python-pyarrow"));
        assert!(pkgs.contains("apache-arrow:python-pyarrow"));
        assert!(pkgs.contains("apache-arrow"));
        assert!(pkgs.contains("Python-Foo"));
    }

    #[test]
    fn a_non_python_multibuild_flavor_is_not_promoted_to_a_package() {
        let pkgs = parse_package_listing("gcc15:bootstrap\nfoo:test\n");
        assert!(!pkgs.contains("bootstrap"));
        assert!(!pkgs.contains("test"));
    }

    /// OBS names are case-sensitive: the listing must keep the spelling it
    /// has, or the name passed back to osc points at a package that does not
    /// exist (`python-apscheduler` for `python-APScheduler`).
    #[test]
    fn the_listing_keeps_each_package_in_its_own_case() {
        let pkgs = parse_package_listing("python-APScheduler\n");
        assert!(pkgs.contains("python-APScheduler"));
        assert!(!pkgs.contains("python-apscheduler"));
    }

    #[test]
    fn obs_name_returns_the_listed_spelling_for_a_lowercase_guess() {
        let listing = parse_package_listing("python-APScheduler\npython-pydantic_core\n");
        assert_eq!(obs_name(&listing, "python-apscheduler"), "python-APScheduler");
        // A separator difference between the guess and the listing still finds it.
        assert_eq!(
            obs_name(&listing, "python-pydantic-core"),
            "python-pydantic_core"
        );
    }

    #[test]
    fn obs_name_keeps_the_guess_when_nothing_is_listed() {
        let listing = parse_package_listing("python-APScheduler\n");
        assert_eq!(obs_name(&listing, "python-croniter"), "python-croniter");
        assert_eq!(obs_name(&listing, "python-APScheduler"), "python-APScheduler");
    }

    #[test]
    fn obs_name_is_stable_when_several_spellings_match() {
        let listing = parse_package_listing("python-Foo\npython-foo\n");
        assert_eq!(obs_name(&listing, "python-FOO"), "python-Foo");
    }
}

#[cfg(test)]
mod obs_name_tests {
    use super::*;

    #[test]
    fn obs_names_reject_options_paths_and_junk() {
        for ok in [
            "python-Events",
            "libsigc++",
            "home:dspinella:python3",
            "a_b.c-1",
        ] {
            assert!(is_valid_obs_name(ok), "{ok}");
        }
        for bad in ["", "-rf", "../x", "a/b", "a b", "a\nb", "x..y", "/abs"] {
            assert!(!is_valid_obs_name(bad), "{bad:?}");
        }
    }

    #[test]
    fn xml_escape_covers_attribute_metacharacters() {
        assert_eq!(xml_escape("a&b<c>\"d"), "a&amp;b&lt;c&gt;&quot;d");
    }
}

#[cfg(test)]
mod count_rpms_tests {
    use super::*;

    #[test]
    fn recent_rpms_exclude_source_rpms_and_other_files() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("python313-Events-0.5-2.1.noarch.rpm"), b"").unwrap();
        std::fs::write(tmp.path().join("python-events-0.5-2.1.src.rpm"), b"").unwrap();
        std::fs::write(tmp.path().join("_buildenv"), b"").unwrap();
        assert_eq!(count_recent_rpms(tmp.path(), Duration::from_secs(60)), 1);
    }

    #[test]
    fn recent_rpms_tolerates_missing_dir() {
        assert_eq!(
            count_recent_rpms(
                std::path::Path::new("/does/not/exist"),
                Duration::from_secs(60)
            ),
            0
        );
    }
}

#[cfg(test)]
mod project_build_statuses_tests {
    use super::*;

    /// Regression test for the off-by-one that used to read the separator
    /// space between status codes instead of the code itself, silently
    /// returning no statuses at all and making every OBS-known package look
    /// "resolvable" regardless of its actual build state.
    #[test]
    fn reads_status_codes_at_even_columns_not_the_separators() {
        let stdout = "\
python-foo
python-bar
python-baz
. B U openSUSE_Tumbleweed x86_64 (published)
Legend:
 . succeeded
";
        let statuses = parse_project_build_statuses(stdout, "openSUSE_Tumbleweed", "x86_64");

        assert_eq!(statuses.get("python-foo"), Some(&BuildStatus::Succeeded));
        assert_eq!(statuses.get("python-bar"), Some(&BuildStatus::Broken));
        assert_eq!(statuses.get("python-baz"), Some(&BuildStatus::Unresolvable));
    }

    /// Regression test for the padded real-world format seen from actual
    /// `osc results` output on a project with many packages: a leading
    /// separator before the first code and a trailing one after the last
    /// (`" . . F ...  openSUSE_Tumbleweed x86_64 (published)"`, note the
    /// double space before the repo name), as opposed to the flush format
    /// the other tests here use. Both must parse to the same codes.
    #[test]
    fn reads_status_codes_in_the_padded_real_world_format() {
        let stdout = "\
python-foo
python-bar
python-baz
 . B U  openSUSE_Tumbleweed x86_64 (published)
Legend:
";
        let statuses = parse_project_build_statuses(stdout, "openSUSE_Tumbleweed", "x86_64");

        assert_eq!(statuses.get("python-foo"), Some(&BuildStatus::Succeeded));
        assert_eq!(statuses.get("python-bar"), Some(&BuildStatus::Broken));
        assert_eq!(statuses.get("python-baz"), Some(&BuildStatus::Unresolvable));
    }

    #[test]
    fn a_space_code_is_treated_as_no_status_rather_than_disabled() {
        // The blank/disabled code is indistinguishable from the column
        // separator, so it must be skipped rather than recorded — a caller
        // treating "no entry" as "not resolvable" is the safe default.
        let stdout = "\
python-foo
python-bar
.   openSUSE_Tumbleweed x86_64 (published)
Legend:
";
        let statuses = parse_project_build_statuses(stdout, "openSUSE_Tumbleweed", "x86_64");

        assert_eq!(statuses.get("python-foo"), Some(&BuildStatus::Succeeded));
        assert_eq!(statuses.get("python-bar"), None);
    }

    #[test]
    fn stops_parsing_at_the_legend_footer() {
        let stdout = "\
python-foo
F openSUSE_Tumbleweed x86_64 (published)
Legend:
F failed
";
        let statuses = parse_project_build_statuses(stdout, "openSUSE_Tumbleweed", "x86_64");
        assert_eq!(statuses.get("python-foo"), Some(&BuildStatus::Failed));
        assert_eq!(statuses.len(), 1);
    }
}

#[cfg(test)]
mod project_repo_paths_tests {
    use super::*;

    #[test]
    fn extracts_project_and_repository_from_each_path() {
        let xml = "\
<project name=\"home:dspinella:python\">
  <repository name=\"openSUSE_Tumbleweed\">
    <path project=\"openSUSE:Factory\" repository=\"snapshot\"/>
    <arch>x86_64</arch>
  </repository>
</project>
";
        let paths = parse_project_repo_paths(xml);
        assert_eq!(
            paths,
            vec![("openSUSE:Factory".to_string(), "snapshot".to_string())]
        );
    }

    #[test]
    fn deduplicates_the_same_pair_across_repositories() {
        let xml = "\
<project name=\"home:dspinella:python\">
  <repository name=\"openSUSE_Tumbleweed\">
    <path project=\"openSUSE:Factory\" repository=\"snapshot\"/>
  </repository>
  <repository name=\"openSUSE_Tumbleweed_i586\">
    <path project=\"openSUSE:Factory\" repository=\"snapshot\"/>
  </repository>
</project>
";
        let paths = parse_project_repo_paths(xml);
        assert_eq!(
            paths,
            vec![("openSUSE:Factory".to_string(), "snapshot".to_string())]
        );
    }

    #[test]
    fn has_repository_true_when_one_is_present() {
        let xml = "\
<project name=\"home:dspinella:python\">
  <repository name=\"openSUSE_Tumbleweed\">
    <path project=\"openSUSE:Factory\" repository=\"snapshot\"/>
  </repository>
</project>
";
        assert!(parse_has_repository(xml));
    }

    #[test]
    fn has_repository_false_on_empty_project() {
        let xml = "\
<project name=\"home:dspinella:python\">
  <title>empty</title>
  <description/>
</project>
";
        assert!(!parse_has_repository(xml));
    }

    #[test]
    fn has_repository_false_when_only_devel_project_present() {
        // A `<devel project="..."/>` element is not a build repository and
        // must not be mistaken for one.
        let xml = "\
<project name=\"home:dspinella:python\">
  <devel project=\"devel:languages:python\"/>
</project>
";
        assert!(!parse_has_repository(xml));
    }

    #[test]
    fn a_devel_project_attribute_elsewhere_is_not_mistaken_for_a_path() {
        // `<devel project="..."/>` uses the same `project="..."` attribute
        // shape but is not a build path and has no `repository` attribute.
        let xml = "\
<project name=\"python-foo\">
  <devel project=\"devel:languages:python\"/>
  <repository name=\"openSUSE_Tumbleweed\">
    <path project=\"openSUSE:Factory\" repository=\"snapshot\"/>
  </repository>
</project>
";
        let paths = parse_project_repo_paths(xml);
        assert_eq!(
            paths,
            vec![("openSUSE:Factory".to_string(), "snapshot".to_string())]
        );
    }
}
