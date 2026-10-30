//! The working copy and the source services: local changes and repair, the
//! service XML validation, and the run of the services.

use super::*;

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
    // almost certainly empty, which must not read as an actually-clean
    // checkout ("no local changes"). The caller
    // uses this to gate skipping a rebuild (see the `AlreadyBuilt` fast path
    // in pipeline.rs), so treat "couldn't tell" as "has changes" — silently
    // skipping a needed rebuild is far worse than running an unnecessary one.
    true
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

/// Services a checkout's `_service` may name. The agent can edit the file, and
/// a service runs arbitrary code on this machine (`refresh_obs_scm` etc. would
/// too), so anything outside the set the pipeline and OBS packaging use is
/// refused.
const ALLOWED_SERVICES: &[&str] = &[
    "tar_scm",
    "obs_scm",
    "recompress",
    "set_version",
    "cargo_vendor",
    "node_modules",
    "download_files",
    "go_modules",
    "format_spec_file",
];

/// Parameters those services take. Notably absent are anything that names a
/// command or an output directory.
const ALLOWED_SERVICE_PARAMS: &[&str] = &[
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
    "file",
    "compression",
    "src",
    "update",
    "method",
    "cargotoml",
    "no-root-manifest",
    "version",
    "basename",
    "without-version",
    "protocol",
    "host",
    "path",
    "archive",
    "disablereview",
    "strategy",
    // tar_scm / obs_scm
    "exclude",
    "include",
    "extract",
    "submodules",
    "depth",
    "history-depth",
    "parent-tag",
    "package-meta",
    // cargo_vendor
    "versioned-dirs",
    "respect-lockfile",
    // node_modules
    "input",
    "node-dir",
    "omit-optional",
    "legacy-container",
    "cpio",
    "output",
    "source-offset",
    // download_files
    "enforceupstream",
    "enforcehash",
    // set_version
    "regex",
    "parse-version",
];

/// Params whose value is a path relative to the checkout.
const SERVICE_PATH_PARAMS: &[&str] = &[
    "subdir",
    "filename",
    "file",
    "src",
    "cargotoml",
    "archive",
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
const SOURCE_PARAMS: &[&str] = &["src", "srcdir", "srctar"];

/// Check one `<param>` against the allowlists. `value` is the decoded text,
/// as osc will see it.
fn validate_service_param(name: &str, value: &str) -> Result<(), String> {
    let value = value.trim();
    if !ALLOWED_SERVICE_PARAMS.contains(&name) {
        return Err(format!("param `{name}` is not allowed"));
    }
    if name == "url" || (SOURCE_PARAMS.contains(&name) && value.contains(':')) {
        crate::service_sandbox::check_service_url(value)?;
    }
    if name == "protocol" && value != "https" {
        return Err(format!("protocol `{value}` must be https"));
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
    parse_service_xml(xml).map(|_| ())
}

/// [`validate_service_xml`], returning the services in order with their
/// decoded (trimmed) params: what is run is what was checked.
pub(crate) fn parse_service_xml(
    xml: &[u8],
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
                    if !ALLOWED_SERVICES.contains(&name.as_str()) {
                        return Err(format!("service `{name}` is not allowed"));
                    }
                    services.push(crate::service_sandbox::ServiceCall {
                        name,
                        params: Vec::new(),
                    });
                }
                "param" => {
                    let name = name.ok_or("a <param> without a name")?;
                    if empty {
                        validate_service_param(&name, "")?;
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
                    validate_service_param(&name, &value)?;
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
pub fn run_service(checkout_dir: &Path, emit: Emit<'_>) -> Result<(), Error> {
    // Fail closed: only a missing file skips validation.
    let services = match fs::read(checkout_dir.join("_service")) {
        Ok(xml) => parse_service_xml(&xml).map_err(|reason| Error::ServiceRejected { reason })?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(e) => {
            return Err(Error::ServiceRejected {
                reason: format!("cannot read _service: {e}"),
            });
        }
    };
    let out = if !crate::service_sandbox::unsafe_mode() {
        crate::service_sandbox::run_services(checkout_dir, &services, emit)?
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
        return Err(Error::OscFailed {
            cmd: format!("service ra in {}", checkout_dir.display()),
            code: out.code,
            stderr,
        });
    }
    collapse_generated_changes(checkout_dir);
    Ok(())
}

/// A `tar_scm` service with `changesgenerate` writes one entry listing every
/// upstream commit since the last release (a hundred bullets for a package
/// that was a few releases behind). That entry is cut down to `- Update to
/// <version>`, which is all a packaging changelog needs to say.
fn collapse_generated_changes(checkout_dir: &Path) {
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
        let pristine = fs::read_to_string(checkout_dir.join(".osc").join("sources").join(name))
            .unwrap_or_default();
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
