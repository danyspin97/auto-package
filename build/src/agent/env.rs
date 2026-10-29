//! The environment the agent process gets: the scrub rules, the pass list
//! and the settings env. The turn scrubs it and checks the credentials.

use super::*;

// ── subprocess environment scrubbing ──────────────────────────────────────────

/// Names (exact, upper-case) the `claude` subprocess may inherit.
const AGENT_ENV_ALLOW: &[&str] = &[
    "PATH",
    "HOME",
    "USER",
    "LOGNAME",
    "SHELL",
    "LANG",
    "LANGUAGE",
    "TERM",
    "COLORTERM",
    "NO_COLOR",
    "TZ",
    "TMPDIR",
    // Proxies and CA bundles the CLI and Node need to reach the API.
    "HTTP_PROXY",
    "HTTPS_PROXY",
    "ALL_PROXY",
    "NO_PROXY",
    "SSL_CERT_FILE",
    "SSL_CERT_DIR",
    "NIX_SSL_CERT_FILE",
    "NODE_EXTRA_CA_CERTS",
    "NODE_PATH",
    "REQUESTS_CA_BUNDLE",
    // The Vertex region: a setting, not a credential, and the settings `env`
    // block usually carries it next to CLAUDE_CODE_USE_VERTEX.
    "CLOUD_ML_REGION",
];

/// Name prefixes (upper-case) the subprocess may inherit.
const AGENT_ENV_ALLOW_PREFIXES: &[&str] = &["LC_", "XDG_", "ANTHROPIC_", "CLAUDE_"];

/// Env vars this process explicitly STRIPS from the Claude subprocess
/// environment (publishing credentials, cloud / forge push tokens).
///
/// The agent subprocess does not inherit the full caller environment: the
/// environment is an allowlist ([`AGENT_ENV_ALLOW`]); this denylist and the
/// credential-shape checks are a
/// second layer over the names that pass it.
///
/// Publishing (osc commit, GitHub pushes, cloud deploys) all happens in
/// the pipeline's OWN subprocess calls, not the agent's — the agent never
/// needs any of these.
pub(super) const AGENT_ENV_DENYLIST: &[&str] = &[
    // OBS publishing — osc reads these for the submit path the pipeline
    // runs itself; the agent never commits.
    "OSC_USER",
    "OSC_PASS",
    "OSC_PASSWORD",
    "OSC_CONFIG",
    "OSC_APIURL",
    "OSCRC",
    // Cloud credentials — any of these flowing through a tool-grant like
    // `Bash(curl ...)` is a straight exfiltration path.
    "AWS_ACCESS_KEY_ID",
    "AWS_SECRET_ACCESS_KEY",
    "AWS_SESSION_TOKEN",
    "AWS_PROFILE",
    "GOOGLE_APPLICATION_CREDENTIALS",
    "GCP_SERVICE_ACCOUNT",
    "AZURE_CLIENT_ID",
    "AZURE_CLIENT_SECRET",
    "AZURE_TENANT_ID",
    // Generic credential plumbing.
    "NETRC",
    "GIT_ASKPASS",
    "GIT_SSH_COMMAND",
    "SSH_AUTH_SOCK",
    "KUBECONFIG",
    "DOCKER_AUTH_CONFIG",
    "DATABASE_URL",
    "PGPASSWORD",
];

/// Credential-shaped suffixes dropped even when not explicitly listed.
///
/// Caught before the explicit denylist above would miss anything — e.g. a
/// user-set `MYSERVICE_TOKEN` or a `FOO_API_KEY` from a CI runner. The
/// Anthropic / Claude keys the agent legitimately needs match this
/// pattern too (`ANTHROPIC_API_KEY`), so they are kept via explicit
/// exceptions.
const CREDENTIAL_SUFFIX_PATTERNS: &[&str] = &[
    "_TOKEN",
    "_AUTH",
    "_KEY",
    "_PASSPHRASE",
    "_SECRET",
    "_SECRET_KEY",
    "_PASSWORD",
    "_PASSWD",
    "_PASS",
    "_PAT",
    "_API_KEY",
    "_APIKEY",
    "_CREDENTIAL",
    "_CREDENTIALS",
    "_PRIVATE_KEY",
];

/// Credential-shaped names the agent IS allowed to keep, even though the
/// suffix guard would otherwise strip them.
const CREDENTIAL_EXCEPTIONS: &[&str] = &[
    "ANTHROPIC_API_KEY",
    "ANTHROPIC_AUTH_TOKEN",
    "CLAUDE_API_KEY",
    // The token the CLI logs in with when it is not given an API key.
    "CLAUDE_CODE_OAUTH_TOKEN",
];

/// Build the environment map handed to the `claude` subprocess.
///
/// Allowlist posture ([`AGENT_ENV_ALLOW`], [`AGENT_ENV_ALLOW_PREFIXES`]):
/// anything else is dropped, so a credential with an unexpected name never
/// reaches the agent. The denylist and credential-shape checks below still
/// apply to allowed names, so `ANTHROPIC_*` or `CLAUDE_*` credentials not
/// listed in [`CREDENTIAL_EXCEPTIONS`] are dropped too.
///
/// The pure-function shape is kept so tests can drive it with crafted
/// inputs. The production call sites use [`agent_env`].
///
/// `extra_deny` reserves an extension slot for a future `--deny-env`
/// flag the user could use to add project-specific sensitive names
/// (e.g. `MY_DEPLOY_HOOK=...`). Pass `&[]` when none is requested.
#[cfg(test)]
pub(crate) fn scrub_env_for_agent(
    vars: Vec<(String, String)>,
    extra_deny: &[String],
) -> BTreeMap<String, String> {
    scrub_env_for_agent_passing(vars, extra_deny, &[])
}

/// Env var naming extra variables (comma-separated) to hand to the agent on
/// top of the allowlist, e.g. `AWS_PROFILE,AWS_REGION` for Bedrock.
pub(crate) const AGENT_ENV_PASS_VAR: &str = "AUTO_PACKAGE_AGENT_ENV_PASS";

/// The names listed in [`AGENT_ENV_PASS_VAR`].
fn agent_env_pass_list() -> Vec<String> {
    parse_pass_list(&std::env::var(AGENT_ENV_PASS_VAR).unwrap_or_default())
}

pub(super) fn parse_pass_list(raw: &str) -> Vec<String> {
    raw.split(',')
        .map(str::trim)
        .filter(|n| !n.is_empty())
        .map(str::to_string)
        .collect()
}

/// The environment for the `claude` subprocess: this process's, scrubbed,
/// plus whatever [`AGENT_ENV_PASS_VAR`] asks for.
///
/// The agent runs with `--setting-sources project`, so the `env` block of the
/// user's `settings.json` is never applied by claude itself. That block is
/// where a Vertex / Bedrock / proxy setup usually lives (`CLAUDE_CODE_USE_VERTEX`,
/// `ANTHROPIC_VERTEX_PROJECT_ID`, ...), and without it the child falls back to
/// Anthropic OAuth and reports "not logged in". The entries are added here
/// instead, under the variables of this process, which take precedence.
pub(crate) fn agent_env() -> BTreeMap<String, String> {
    merge_agent_env(
        user_settings_env(),
        ap_core::env::utf8_vars(),
        &agent_env_pass_list(),
    )
}

/// Both sources go through the same policy: [`AGENT_ENV_ALLOW`] and its
/// prefixes, the publishing denylist, the credential-shape checks with
/// [`CREDENTIAL_EXCEPTIONS`], and the `pass` list. So a settings entry such as
/// `GITHUB_TOKEN` or `NODE_OPTIONS` is dropped as it would be from the process
/// environment. What the agent legitimately reads from settings survives:
/// `ANTHROPIC_*` / `CLAUDE_*` (base URL, model, Vertex or Bedrock switches,
/// `ANTHROPIC_API_KEY`, `ANTHROPIC_AUTH_TOKEN`, `CLAUDE_CODE_OAUTH_TOKEN`
/// through the exceptions), `CLOUD_ML_REGION` and the proxy and CA variables.
/// Cloud names such as `AWS_REGION` need the same `AUTO_PACKAGE_AGENT_ENV_PASS`
/// opt-in as in the process environment. Process variables take precedence.
pub(super) fn merge_agent_env(
    settings: BTreeMap<String, String>,
    process: Vec<(String, String)>,
    pass: &[String],
) -> BTreeMap<String, String> {
    let mut env = scrub_env_for_agent_passing(settings.into_iter().collect(), &[], pass);
    env.extend(scrub_env_for_agent_passing(process, &[], pass));
    env
}

/// The `env` block of the user's claude `settings.json`
/// (`$CLAUDE_CONFIG_DIR` or `~/.claude`), or nothing if absent or unreadable.
/// `OSC*` names are dropped here, and the values are scrubbed by
/// [`merge_agent_env`] like any other variable.
fn user_settings_env() -> BTreeMap<String, String> {
    let dir = match std::env::var_os("CLAUDE_CONFIG_DIR") {
        Some(d) if !d.is_empty() => std::path::PathBuf::from(d),
        _ => match std::env::var_os("HOME") {
            Some(h) => std::path::PathBuf::from(h).join(".claude"),
            None => return BTreeMap::new(),
        },
    };
    match std::fs::read_to_string(dir.join("settings.json")) {
        Ok(text) => parse_settings_env(&text),
        Err(_) => BTreeMap::new(),
    }
}

pub(super) fn parse_settings_env(text: &str) -> BTreeMap<String, String> {
    let Ok(serde_json::Value::Object(root)) = serde_json::from_str::<serde_json::Value>(text)
    else {
        return BTreeMap::new();
    };
    let Some(serde_json::Value::Object(env)) = root.get("env") else {
        return BTreeMap::new();
    };
    env.iter()
        .filter(|(name, _)| !name.to_ascii_uppercase().starts_with("OSC"))
        .filter_map(|(name, v)| match v {
            serde_json::Value::String(s) => Some((name.clone(), s.clone())),
            serde_json::Value::Number(n) => Some((name.clone(), n.to_string())),
            serde_json::Value::Bool(b) => Some((name.clone(), b.to_string())),
            _ => None,
        })
        .collect()
}

/// [`scrub_env_for_agent`] with `pass`: variables named there are kept even
/// if the allowlist, denylist or credential checks would drop them, since
/// the operator asked for them by name (Bedrock and Vertex need `AWS_*` and
/// `GOOGLE_*`, which are otherwise removed). `OSC*` names are never passed:
/// the agent does not publish.
pub(crate) fn scrub_env_for_agent_passing(
    vars: Vec<(String, String)>,
    extra_deny: &[String],
    pass: &[String],
) -> BTreeMap<String, String> {
    let mut kept: BTreeMap<String, String> = BTreeMap::new();
    for (name, value) in vars {
        let upper = name.to_ascii_uppercase();
        if pass.iter().any(|p| p.eq_ignore_ascii_case(&name)) {
            if upper.starts_with("OSC") {
                ap_core::diag!(
                    "ap-build: not passing {name} to the agent subprocess (OBS credential)"
                );
            } else if !extra_deny.iter().any(|a| a == &name) {
                kept.insert(name, value);
            }
            continue;
        }
        if !AGENT_ENV_ALLOW.contains(&upper.as_str())
            && !AGENT_ENV_ALLOW_PREFIXES
                .iter()
                .any(|p| upper.starts_with(p))
        {
            continue;
        }
        if AGENT_ENV_DENYLIST.contains(&upper.as_str()) {
            ap_core::diag!(
                "ap-build: scrubbing publishing credential {name} from agent subprocess \
                 (the agent never commits — see the Oct 2026 build/src/agent.rs review)"
            );
            continue;
        }
        if extra_deny.iter().any(|a| a == &name) {
            ap_core::diag!("ap-build: scrubbing user-denied env var {name} from agent subprocess");
            continue;
        }
        // Proxy URLs routinely embed credentials and the CLI itself needs
        // them to reach the API; dropping them would break every turn.
        if !upper.ends_with("_PROXY") && value_embeds_credentials(&value) {
            ap_core::diag!(
                "ap-build: scrubbing {name} from agent subprocess (its value is a URL \
                 with embedded credentials)"
            );
            continue;
        }
        if matches_credential_pattern(&upper) && !CREDENTIAL_EXCEPTIONS.contains(&upper.as_str()) {
            ap_core::diag!(
                "ap-build: scrubbing credential-shaped env var {name} from agent subprocess \
                 (name ends in _TOKEN/_SECRET/_PASSWORD/_API_KEY)"
            );
            continue;
        }
        kept.insert(name, value);
    }
    kept
}

/// A value shaped like `scheme://user:password@host` (proxy URLs, database
/// URLs, git remotes with tokens).
fn value_embeds_credentials(value: &str) -> bool {
    let Some((_, rest)) = value.split_once("://") else {
        return false;
    };
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    authority
        .rsplit_once('@')
        .is_some_and(|(userinfo, _)| userinfo.contains(':'))
}

/// `name` must already be upper-case.
fn matches_credential_pattern(name: &str) -> bool {
    CREDENTIAL_SUFFIX_PATTERNS
        .iter()
        .any(|pat| name.ends_with(pat))
}
