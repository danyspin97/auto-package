//! Names and strings of OBS: the rpm and fuzzy keys, the valid OBS names, the
//! RPM prefixes and the small text helpers.

use super::*;

/// Where `osc` runs when the command is not scoped to a checkout.
pub(super) fn anywhere() -> PathBuf {
    std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/"))
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
/// `.contains()` for an existence check — use [`find_listed`] (or
/// [`project_key`]) on both sides, and when it matches, use the real listing's
/// spelling, not this guess.
///
/// With `--nvidia-redist` a name of one of NVIDIA's CUDA components (the
/// `nvidia-cublas` wheel, or the `cuda-cublas` package itself) is `cuda-<short>`
/// instead: see [`crate::redist`].
pub fn rpm_name(pypi_name: &str) -> String {
    if let Some(cuda) = crate::redist::package_name(pypi_name) {
        return cuda;
    }
    format!("python-{}", pypi_name.to_lowercase())
}

/// A loose comparison key: lowercase with every `-`, `_` and `.` removed, so
/// `pydantic_core`, `pydantic-core` and `pydantic.core` share a key.
///
/// It is NOT an identity key. Separators are dropped, so two different PyPI
/// projects share a key when they differ only in where a separator sits:
/// `python-whois` and `pythonwhois`, `junit-xml` and `junitxml`,
/// `retry-requests` and `retryrequests`, `uri-template` and `uritemplate`
/// (and their OBS names `python-python-whois` and `python-pythonwhois`, and
/// so on). Do not use it to decide that a listed package is the one a name
/// asks for. Use [`find_listed`] for that, which keys on [`project_key`]
/// (PEP 503), and keep this key for the callers that deliberately want the
/// loose match.
///
/// Never use it to construct a name to build or commit under: when a match
/// is found, use the real listing's own spelling.
pub fn fuzzy_key(name: &str) -> String {
    name.chars()
        .filter(|c| !matches!(c, '-' | '_' | '.'))
        .flat_map(|c| c.to_lowercase())
        .collect()
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
pub(super) fn during(cmd: impl Into<String>) -> impl FnOnce(proc::ProcessError) -> Error {
    move |source| Error::Command {
        cmd: cmd.into(),
        source,
    }
}

/// First non-empty line of a command's output, for one-line diagnostics.
pub(super) fn first_line(s: &str) -> &str {
    s.lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("")
}

/// Whether `osc` reported a missing project, package or file on the server.
/// A bare `404` would also match unrelated digits, so the HTTP wording is
/// required; `osc` goes through urllib, which prints `HTTP Error 404: Not Found`.
pub(super) fn is_not_found(text: &str) -> bool {
    // Not a bare `Not Found`: that phrase turns up in a 500 body or a message
    // about a missing file, and a package would be created over it.
    text.contains("HTTP Error 404: Not Found")
}

pub(super) fn check_obs_name(what: &str, name: &str) -> Result<(), Error> {
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
    // `lib-devel` names nothing once `lib` is dropped: keep the name as written.
    match stripped.strip_prefix("lib").unwrap_or(stripped) {
        "" => name,
        upstream => upstream,
    }
}

/// The name OBS lists for the package `guess` refers to, or `guess` itself
/// when nothing listed matches. `Err` when the listing holds several packages
/// that are the same project up to separators and none is `guess` up to
/// case: the caller reports that instead of choosing one.
///
/// `guess` is an [`rpm_name`] guess, which is lowercase and may spell the
/// separators differently from the real package. A listed name matches as
/// [`find_listed`] describes, so `python-apscheduler` finds `python-APScheduler`
/// and `python-pydantic-core` finds `python-pydantic_core`, but not
/// `pythonwhois` for `python-whois`. Anything that checks out, builds or names
/// a package in `project` must use this spelling, not the guess: osc, the
/// checkout directory and the spec file all follow the real name.
pub fn obs_name(
    listing: &std::collections::HashSet<String>,
    guess: &str,
) -> Result<String, String> {
    match find_listed(guess, listing.iter().map(String::as_str)) {
        Listed::Found(name) => Ok(name.to_string()),
        Listed::Absent => Ok(guess.to_string()),
        Listed::Ambiguous(names) => Err(ambiguity_message(guess, &names)),
    }
}
