//! The OBS API calls of the branch module and the user and default project they
//! read.

use super::*;
use crate::osc::error::*;
use crate::osc::names::*;
use crate::osc::query::*;

/// Run `osc api` with an optional HTTP method and body.
pub(super) fn api_call(
    method: Option<&str>,
    path: &str,
    data: Option<&str>,
    emit: Emit<'_>,
) -> Result<Captured, Error> {
    let mut args: Vec<String> = vec!["api".into()];
    if let Some(m) = method {
        args.extend(["-X".into(), m.into()]);
    }
    if let Some(d) = data {
        args.extend(["-d".into(), d.into()]);
    }
    args.push(path.into());
    proc::capture_with_timeout("osc", &args, &anywhere(), emit, Duration::from_secs(120))
        .map_err(during(format!("api {path}")))
}

/// `GET path` through `osc api`, with a failure reported as an error.
pub(super) fn api_get(path: &str, emit: Emit<'_>) -> Result<String, Error> {
    let out = api_call(None, path, None, emit)?;
    if !out.success() {
        return Err(Error::OscFailed {
            cmd: format!("api {path}"),
            code: out.code,
            stderr: out.stderr,
        });
    }
    Ok(out.stdout)
}

/// The text of `<data name="NAME">value</data>` in an OBS status document.
pub(super) fn xml_data_value(xml: &str, name: &str) -> Option<String> {
    let open = format!("<data name=\"{name}\">");
    let (_, rest) = xml.split_once(&open)?;
    let (value, _) = rest.split_once("</data>")?;
    Some(value.trim().to_string())
}

/// The OBS user `osc` is configured for. Newer `osc` has `whoami`; older
/// versions only have `whois`, which prints `name: "Full Name" <email>`.
pub fn current_user(emit: Emit<'_>) -> Result<String, Error> {
    let mut last = None;
    for cmd in ["whoami", "whois"] {
        let out = proc::capture_with_timeout("osc", &[cmd], &anywhere(), emit, OSC_QUERY_TIMEOUT)
            .map_err(during(cmd))?;
        if !out.success() {
            last = Some(Error::OscFailed {
                cmd: cmd.into(),
                code: out.code,
                stderr: out.stderr,
            });
            continue;
        }
        return parse_whoami(&out.stdout).ok_or_else(|| {
            Error::NotActionable(format!(
                "could not read the OBS user from `osc {cmd}` (got {:?}); pass --branch-project",
                first_line(&out.stdout)
            ))
        });
    }
    Err(last.expect("two commands were tried"))
}

/// The user name in `osc whoami` or `osc whois` output: the bare name, a
/// `Username: name` line, or `name: "Full Name" <email>`.
pub(super) fn parse_whoami(stdout: &str) -> Option<String> {
    let line = first_line(stdout);
    let name = line.strip_prefix("Username:").unwrap_or(line).trim();
    let name = name.split_whitespace().next()?.trim_end_matches(':');
    is_valid_obs_name(name).then(|| name.to_string())
}

/// The project `osc branch` puts a branch of `source` in when no target is
/// named.
pub fn default_branch_project(user: &str, source: &str) -> String {
    format!("home:{user}:branches:{source}")
}
