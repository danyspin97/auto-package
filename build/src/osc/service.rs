//! The working copy and the source services: local changes and repair, the
//! service XML validation, and the run of the services.

use super::*;

/// Return true if `checkout_dir`'s working copy has uncommitted local
/// changes (e.g. a spec version bump from a run that crashed before `osc
/// commit`). `osc status` prints one line per changed file and nothing when
/// the working copy is clean.
pub fn has_local_changes(checkout_dir: &Path, emit: Emit<'_>) -> bool {
    status_text(checkout_dir, emit).is_none_or(|text| !text.trim().is_empty())
}

/// [`has_local_changes`] that leaves out the tool's own `series` scratch when
/// OBS does not track it (see [`crate::heal::has_user_changes`]): what the
/// start-up reconcile asks before it takes OBS's RPMs.
pub fn has_user_changes(checkout_dir: &Path, emit: Emit<'_>) -> bool {
    status_text(checkout_dir, emit).is_none_or(|text| {
        crate::heal::has_user_changes(&text, &publish::tracked_files(checkout_dir))
    })
}

/// The output of `osc status` in `checkout_dir`, or `None` when it could not be
/// read (the answer is then "has changes").
fn status_text(checkout_dir: &Path, emit: Emit<'_>) -> Option<String> {
    let Ok(out) = proc::capture("osc", &["status"], checkout_dir, emit) else {
        return None;
    };
    if out.success() {
        return Some(out.stdout);
    }
    if is_inconsistent_wc_error(&out.stderr) && repair_wc(checkout_dir, emit).is_ok() {
        return proc::capture("osc", &["status"], checkout_dir, emit)
            .ok()
            .map(|out| out.stdout);
    }
    // `osc status` failed for a reason other than the one recognized,
    // repairable case (a network/server hiccup, say), or `repair_wc` itself
    // failed: "couldn't tell", which the callers read as "has changes". Silently
    // skipping a needed rebuild is far worse than running an unnecessary one.
    None
}

/// Whether an `osc` command failed because the working copy's `.osc/`
/// metadata is inconsistent — the message `osc` itself prints when a prior
/// run died mid-`up`/`ci` and left it half-written, naming `osc repairwc .`
/// as the fix.
pub(super) fn is_inconsistent_wc_error(stderr: &str) -> bool {
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

/// The services a checkout's `_service` may name, and the parameters each one
/// takes. The agent can edit the file, and a service runs arbitrary code on
/// this machine, so a service or a parameter not listed here is refused. A
/// parameter is accepted only for the services listed with it: the helper of
/// another service would reject it, or read it as another option (argparse
/// takes an unambiguous prefix, so `--file` is `--filename` for tar_scm).
///
/// The lists are the helpers' own options (`--help`, their argument parsers,
/// their `.service` files), less the parameters refused below. `node_modules`
/// and `go_modules` have no helper installed here, so their lists are not
/// checked; `run_services` refuses such a service before it runs. Every mode
/// runs: `osc service ra` runs `buildtime` and `disabled` services too (osc's
/// `serviceinfo.py` skips modes only for the other run commands), so a
/// `_service` is checked and run in full.
///
/// Refused on purpose:
/// - `outdir`, `changesoutfile` (cargo_vendor): the sandbox picks the output
///   directory, and `changesoutfile` writes into the checkout itself, past
///   the move of the outputs.
/// - `set-name`, `set-version`, `tag` (cargo_vendor), `extension`,
///   `extract-rename` (tar_scm, obs_scm): they feed output names or archive
///   paths, which are not checked as paths (`tag` gives `vendor-<tag>.tar`).
/// - `fromfile`, `maintainers-asc`, `import-cargo-config`: read a file whose
///   path is not checked, so they could read outside the checkout.
/// - `user`, `keyring-passphrase`: SCM credentials, read from `/etc/obs`.
/// - `sslverify`, `sslvalidation`, `enforceipv4`, `locale`, `encoding`: change
///   TLS checks, networking or the helper's environment; not needed here.
/// - `i-accept-the-risk` (cargo_vendor): the RustSec ids cargo_vendor's audit
///   (against the local advisory database) should ignore. Accepting a risk is
///   the maintainer's call, not the agent's.
/// - `update-crate`, `custom-root`: not needed here. `lockfile` and `strategy`
///   are not options of cargo_vendor (clap rejects them).
/// - services `cargo_audit`, `refresh_patches`, `roast_scm`: `cargo_audit` is a
///   stripped binary with no usage text, and nothing shows it only reads the
///   checkout. The other two are not installed here, so their parameters
///   cannot be checked.
///
/// The regex parameters (`exclude-re`, `include-re`, `regex`) are not bounded
/// in time, as `versionrewriteregex` is not: the 20 minute capture timeout
/// bounds them.
const SERVICE_PARAMS: &[(&str, &[&str])] = &[
    ("tar_scm", TAR_SCM_PARAMS),
    ("obs_scm", TAR_SCM_PARAMS),
    // tar is tar_scm run as `tar` (argv[0]): it packs the checkout's .obsinfo
    // into a tarball. Its obsinfo and extension params are refused.
    (
        "tar",
        &["version", "versionprefix", "filename", "package-meta"],
    ),
    ("cargo_vendor", CARGO_VENDOR_PARAMS),
    (
        "set_version",
        &["revision", "file", "version", "basename", "regex"],
    ),
    ("recompress", &["compression", "file"]),
    (
        "download_files",
        &["enforceupstream", "changesgenerate", "changesauthor"],
    ),
    ("format_spec_file", &["specfile"]),
    (
        "node_modules",
        &[
            "input",
            "node-dir",
            "omit-optional",
            "legacy-container",
            "cpio",
            "output",
            "source-offset",
        ],
    ),
    ("go_modules", &[]),
];

/// tar_scm's options; obs_scm is the same program.
const TAR_SCM_PARAMS: &[&str] = &[
    "url",
    "scm",
    "revision",
    "versionformat",
    "versionprefix",
    "match-tag",
    "versionrewrite-pattern",
    "versionrewrite-replacement",
    "changesgenerate",
    "changesauthor",
    "subdir",
    "filename",
    "version",
    "without-version",
    "exclude",
    "exclude-re",
    "include",
    "include-re",
    "extract",
    "submodules",
    // argparse takes this unambiguous prefix of --submodules
    "submodule",
    "history-depth",
    "parent-tag",
    "package-meta",
];

/// cargo_vendor's options.
pub(super) const CARGO_VENDOR_PARAMS: &[&str] = &[
    "url",
    "src",
    // aliases of --src in clap
    "srcdir",
    "srctar",
    "revision",
    "changesgenerate",
    "changesauthor",
    "changesemail",
    "compression",
    "method",
    "cargotoml",
    "no-root-manifest",
    "update",
    "exclude",
    "filter",
    "versioned-dirs",
    "respect-lockfile",
    "versionrewriteregex",
    "versionrewritepattern",
];

/// Params whose value is a path relative to the checkout.
const SERVICE_PATH_PARAMS: &[&str] = &[
    "subdir",
    "filename",
    "file",
    "src",
    "srcdir",
    "srctar",
    "specfile",
    "cargotoml",
    "exclude",
    "include",
    "extract",
    "input",
    "node-dir",
    "cpio",
    "output",
];

/// Params that name a source: a path in the checkout, or a URL. cargo_vendor
/// treats a value that matches no path as a git URL and clones it, a
/// `file://` URL included, so a value with a `:` is checked as a URL.
pub(crate) const SOURCE_PARAMS: &[&str] = &["src", "srcdir", "srctar"];

/// The parameters `service` takes; `None` for a service that is not allowed.
fn service_params(service: &str) -> Option<&'static [&'static str]> {
    SERVICE_PARAMS
        .iter()
        .find(|(s, _)| *s == service)
        .map(|(_, params)| *params)
}

/// Check one `<param>` of `service` against the allowlists. `value` is the
/// decoded text, as osc will see it.
fn validate_service_param(
    package: &str,
    service: &str,
    name: &str,
    value: &str,
) -> Result<(), String> {
    let value = value.trim();
    if !service_params(service).unwrap_or_default().contains(&name) {
        return Err(format!(
            "param `{name}` is not allowed for service `{service}`"
        ));
    }
    if name == "url" || (SOURCE_PARAMS.contains(&name) && value.contains(':')) {
        let asker = crate::allowed_hosts::Asker::service(package, service);
        crate::service_sandbox::check_service_url(value, &asker)?;
    }
    if name == "revision" && value.starts_with('-') {
        return Err(format!("revision `{value}` starts with `-`"));
    }
    if SERVICE_PATH_PARAMS.contains(&name)
        && (value.starts_with('/') || value.split('/').any(|c| c == ".."))
    {
        return Err(format!("{name} `{value}` leaves the checkout"));
    }
    Ok(())
}

/// Check a `_service` file against the allowlists. `Err` names the problem.
///
/// The file is parsed as XML and the checks run on decoded values, since osc
/// decodes character references (`&#46;&#46;/`) before using them. Anything
/// the parser cannot read, any DOCTYPE, processing instruction or entity
/// other than the five predefined ones is refused.
#[cfg(test)]
pub(crate) fn validate_service_xml(xml: &[u8]) -> Result<(), String> {
    parse_service_xml(xml, "test-pkg").map(|_| ())
}

/// Why osc would stop on the checkout's `_service` before it commits: the file
/// is not well-formed XML. osc prints the position on stdout and exits 1, so
/// the commit is refused here with the line. `None` without a `_service`, or
/// when it reads.
pub fn service_problem(checkout_dir: &std::path::Path) -> Option<String> {
    let xml = std::fs::read_to_string(checkout_dir.join("_service")).ok()?;
    service_xml_problem(&xml)
}

/// [`service_problem`] for the text of a `_service`.
pub(crate) fn service_xml_problem(xml: &str) -> Option<String> {
    use quick_xml::Reader;
    use quick_xml::events::Event;

    let mut reader = Reader::from_str(xml);
    loop {
        match reader.read_event() {
            Ok(Event::Eof) => return None,
            Ok(_) => {}
            Err(e) => {
                let at = (reader.buffer_position() as usize).min(xml.len());
                let line = xml.get(..at).map_or(1, |s| s.matches('\n').count() + 1);
                return Some(format!(
                    "the _service is not well-formed XML (around line {line}): {e}"
                ));
            }
        }
    }
}

/// [`validate_service_xml`], returning the services in order with their
/// decoded (trimmed) params: what is run is what was checked.
pub(crate) fn parse_service_xml(
    xml: &[u8],
    package: &str,
) -> Result<Vec<crate::service_sandbox::ServiceCall>, String> {
    use quick_xml::Reader;
    use quick_xml::events::Event;

    let xml = std::str::from_utf8(xml).map_err(|_| "not valid UTF-8".to_string())?;
    let mut reader = Reader::from_str(xml);
    // Open elements, and the name and text so far of the <param> being read.
    let mut depth: Vec<String> = Vec::new();
    let mut param: Option<(String, String)> = None;
    let mut seen_root = false;
    let mut services: Vec<crate::service_sandbox::ServiceCall> = Vec::new();
    loop {
        let event = reader
            .read_event()
            .map_err(|e| format!("malformed XML: {e}"))?;
        let (start, empty) = match &event {
            Event::Start(e) => (Some(e), false),
            Event::Empty(e) => (Some(e), true),
            _ => (None, false),
        };
        if let Some(e) = start {
            let tag = e.name().as_ref().to_string();
            let parent = depth.last().map(String::as_str);
            let ok = match (tag.as_str(), parent) {
                ("services", None) => !std::mem::replace(&mut seen_root, true),
                ("service", Some("services")) | ("param", Some("service")) => true,
                _ => false,
            };
            if !ok {
                return Err(format!("unexpected markup `<{tag}`"));
            }
            let mut name = None;
            for attr in e.attributes() {
                let attr = attr.map_err(|e| format!("malformed attribute: {e}"))?;
                if attr.key.as_ref() == "name" {
                    let v = attr
                        .normalized_value(quick_xml::XmlVersion::Implicit1_0)
                        .map_err(|e| format!("malformed attribute: {e}"))?;
                    name = Some(v.into_owned());
                }
            }
            match tag.as_str() {
                "service" => {
                    let name = name.ok_or("a <service> without a name")?;
                    if service_params(&name).is_none() {
                        return Err(format!("service `{name}` is not allowed"));
                    }
                    services.push(crate::service_sandbox::ServiceCall {
                        name,
                        params: Vec::new(),
                    });
                }
                "param" => {
                    let name = name.ok_or("a <param> without a name")?;
                    let service = services.last().map(|s| s.name.clone()).unwrap_or_default();
                    if empty {
                        validate_service_param(package, &service, &name, "")?;
                        if let Some(s) = services.last_mut() {
                            s.params.push((name, String::new()));
                        }
                    } else {
                        param = Some((name, String::new()));
                    }
                }
                _ => {}
            }
            if !empty {
                depth.push(tag);
            }
            continue;
        }
        match event {
            Event::End(_) => {
                if depth.pop().as_deref() == Some("param")
                    && let Some((name, value)) = param.take()
                {
                    let service = services.last().map(|s| s.name.clone()).unwrap_or_default();
                    validate_service_param(package, &service, &name, &value)?;
                    if let Some(s) = services.last_mut() {
                        s.params.push((name, value.trim().to_string()));
                    }
                }
            }
            Event::Text(t) => {
                if let Some((_, value)) = param.as_mut() {
                    value.push_str(&t);
                }
            }
            Event::CData(t) => {
                if let Some((_, value)) = param.as_mut() {
                    value.push_str(&t);
                }
            }
            Event::GeneralRef(r) => {
                let ch = match r.resolve_char_ref() {
                    Ok(Some(c)) => c,
                    Ok(None) => match &*r {
                        "lt" => '<',
                        "gt" => '>',
                        "amp" => '&',
                        "apos" => '\'',
                        "quot" => '"',
                        other => return Err(format!("entity `&{other};` is not allowed")),
                    },
                    Err(e) => return Err(format!("malformed reference: {e}")),
                };
                if let Some((_, value)) = param.as_mut() {
                    value.push(ch);
                }
            }
            Event::DocType(_) => return Err("a DOCTYPE is not allowed".to_string()),
            Event::PI(_) => return Err("processing instructions are not allowed".to_string()),
            Event::Eof => break,
            _ => {}
        }
    }
    if !depth.is_empty() {
        return Err("unclosed element".to_string());
    }
    Ok(services)
}

/// Whether a package that is blocked now, after `previous` was the status
/// label, has made progress worth restarting the stall timer for. A flip back
/// from `stale: ...` (the cross-check reading an older result) is not progress:
/// dtc sat for 1.5 hours flipping between `blocked` and `stale: outdated`, and
/// every flip restarted the timer so the stall limit never came.
pub(super) fn resets_block_timer(previous: &str) -> bool {
    !previous.starts_with("stale")
}

/// Run the checkout's services, as `osc service ra` would, to fetch sources and
/// vendor Cargo deps.
///
/// The `_service` file is validated first (the agent may have edited it) and
/// each service runs in a sandbox without the home directory, the session bus
/// or OBS credentials, its network limited to the source hosts (see
/// [`crate::service_sandbox`]). Only `AUTO_PACKAGE_UNSAFE_SERVICE=1` runs the
/// unsandboxed `osc service ra` instead.
///
/// `baseline` is the OBS baseline the run kept, see [`run_start_copy`].
pub fn run_service(
    checkout_dir: &Path,
    package: &str,
    baseline: Option<&Path>,
    emit: Emit<'_>,
) -> Result<(), Error> {
    // Fail closed: only a missing file skips validation.
    let services = match fs::read(checkout_dir.join("_service")) {
        Ok(xml) => {
            let services = parse_service_xml(&xml, package)
                .map_err(|reason| Error::ServiceRejected { reason })?;
            warn_if_no_root(&xml, package, emit);
            services
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(e) => {
            return Err(Error::ServiceRejected {
                reason: format!("cannot read _service: {e}"),
            });
        }
    };
    let out = if !crate::service_sandbox::unsafe_mode() {
        crate::service_sandbox::run_services(checkout_dir, package, &services, emit)?
    } else {
        proc::capture_with_timeout_env(
            "osc",
            &["service", "ra"],
            checkout_dir,
            emit,
            proc::DEFAULT_CAPTURE_TIMEOUT,
            // osc reads its credentials from the desktop keyring at start-up,
            // which it reaches through the session bus: without the address it
            // dies with "Unable to initialize SecretService" before any
            // service runs.
            Some(&proc::minimal_env(&["DBUS_SESSION_BUS_ADDRESS"])),
        )
        .map_err(during(format!("service ra in {}", checkout_dir.display())))?
    };
    if !out.success() {
        // `osc service` reports a failing service on stdout and leaves stderr
        // empty, which made the failure read as "failed (exit 1):" and nothing.
        let stderr = if out.stderr.trim().is_empty() {
            out.stdout
        } else {
            out.stderr
        };
        // gpg prints one `aka` line per user id of the signing key, which
        // buries the line that says what failed.
        let stderr = stderr
            .lines()
            .filter(|l| !l.starts_with("gpg:") || !l.contains(" aka "))
            .collect::<Vec<_>>()
            .join("\n");
        return Err(service_run_failed(checkout_dir, out.code, stderr));
    }
    collapse_generated_changes(checkout_dir, baseline);
    Ok(())
}

/// The error for a `_service` run that exited with `code` and printed
/// `stderr`: an unset toolchain when the output says so, else the osc failure
/// with its text.
pub fn service_run_failed(checkout_dir: &Path, code: i32, stderr: String) -> Error {
    if let Some(err) = service_tool_error(&stderr) {
        return err;
    }
    Error::OscFailed {
        cmd: format!("service ra in {}", checkout_dir.display()),
        code,
        stderr,
    }
}

/// A `tar_scm` service with `changesgenerate` writes one entry listing every
/// upstream commit since the last release (a hundred bullets for a package
/// that was a few releases behind). That entry is cut down to `- Update to
/// <version>`, which is all a packaging changelog needs to say. What the entry
/// is compared with is what OBS held when the run started (`baseline`).
pub(super) fn collapse_generated_changes(checkout_dir: &Path, baseline: Option<&Path>) {
    let Ok(rd) = fs::read_dir(checkout_dir) else {
        return;
    };
    for file in rd
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "changes"))
    {
        let Some(name) = file.file_name() else {
            continue;
        };
        let pristine = run_start_copy(checkout_dir, baseline, name);
        if let Ok(working) = fs::read_to_string(&file)
            && let Some(collapsed) = collapsed_generated_entry(&working, &pristine)
        {
            let _ = fs::write(&file, collapsed);
        }
    }
}

pub(super) fn collapsed_generated_entry(working: &str, pristine: &str) -> Option<String> {
    let is_separator = |l: &str| l.len() >= 20 && l.bytes().all(|b| b == b'-');
    let lines: Vec<&str> = working.lines().collect();
    let first = lines.iter().position(|l| is_separator(l.trim()))?;
    let end = lines[first + 1..]
        .iter()
        .position(|l| is_separator(l.trim()))
        .map_or(lines.len(), |i| first + 1 + i);
    let entry = lines[first..end].join("\n");
    if pristine.contains(entry.trim_end()) {
        return None;
    }
    let body_from = (first + 2).min(end);
    let head = lines[body_from..end]
        .iter()
        .find(|l| l.trim_start().starts_with("- Update to version "))?;
    let version = head
        .trim_start()
        .trim_start_matches("- Update to version ")
        .trim_end_matches(':')
        .trim();
    let sub_bullets = lines[body_from..end]
        .iter()
        .filter(|l| l.starts_with("  * ") || l.starts_with("  - "))
        .count();
    if version.is_empty() || sub_bullets < 5 {
        return None;
    }
    let mut out: Vec<String> = lines[..body_from].iter().map(|l| l.to_string()).collect();
    out.push(format!("- Update to {version}"));
    out.push(String::new());
    out.extend(lines[end..].iter().map(|l| l.to_string()));
    let mut text = out.join("\n");
    text.push('\n');
    Some(text)
}
