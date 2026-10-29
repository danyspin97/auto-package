//! Tests for the environment the agent process gets: scrubbing and pass-through.

use super::*;

// ── scrub_env_for_agent ───────────────────────────────────────────────────

pub(super) fn pair(k: &str, v: &str) -> (String, String) {
    (k.to_string(), v.to_string())
}

#[test]
fn scrub_env_keeps_anthropic_api_key() {
    let scrubbed = scrub_env_for_agent(vec![pair("ANTHROPIC_API_KEY", "sk-ant-x")], &[]);
    assert_eq!(
        scrubbed.get("ANTHROPIC_API_KEY").map(String::as_str),
        Some("sk-ant-x")
    );
}

#[test]
fn scrub_env_drops_osc_password() {
    // OSC_PASSWORD is a publishing credential — the agent must never see
    // it. OSC pushes happen in the pipeline's own subprocesses, not the
    // agent's turn.
    let scrubbed = scrub_env_for_agent(vec![pair("OSC_PASSWORD", "hunter2")], &[]);
    assert!(
        !scrubbed.contains_key("OSC_PASSWORD"),
        "OSC_PASSWORD must not pass through"
    );
}

#[test]
fn scrub_env_drops_aws_secret_access_key() {
    // Cloud credentials — not in the allow-list and matches *_SECRET too.
    let scrubbed = scrub_env_for_agent(vec![pair("AWS_SECRET_ACCESS_KEY", "AKIA...")], &[]);
    assert!(
        !scrubbed.contains_key("AWS_SECRET_ACCESS_KEY"),
        "AWS_SECRET_ACCESS_KEY must not pass through"
    );
}

#[test]
fn scrub_env_drops_arbitrary_token_pattern() {
    // The suffix matcher catches unknown credential-shaped names that no
    // one thought to list explicitly — e.g. some new forge's auth token.
    let scrubbed = scrub_env_for_agent(vec![pair("FOO_TOKEN", "ghp_abc")], &[]);
    assert!(
        !scrubbed.contains_key("FOO_TOKEN"),
        "FOO_TOKEN must be dropped by the suffix guard"
    );
}

#[test]
fn scrub_env_is_case_insensitive_and_covers_more_credential_shapes() {
    let vars = vec![
        pair("github_token", "x"),
        pair("DB_PASSWD", "x"),
        pair("DEPLOY_PAT", "x"),
        pair("STRIPE_SECRET_KEY", "x"),
        pair("SSH_AUTH_SOCK", "/tmp/agent.1"),
        pair("DATABASE_URL", "postgres://u:p@h/db"),
    ];
    assert!(scrub_env_for_agent(vars, &[]).is_empty());
    // ...but a proxy URL keeps its credentials: the CLI needs them.
    let proxied = scrub_env_for_agent(
        vec![pair("HTTPS_PROXY", "http://user:hunter2@proxy:3128")],
        &[],
    );
    assert!(proxied.contains_key("HTTPS_PROXY"));
}

#[test]
fn scrub_env_keeps_credential_free_urls_and_anthropic_keys_in_any_case() {
    let scrubbed = scrub_env_for_agent(
        vec![
            pair("HTTPS_PROXY", "http://proxy:3128"),
            pair("anthropic_api_key", "sk-ant-x"),
        ],
        &[],
    );
    assert!(scrubbed.contains_key("HTTPS_PROXY"));
    assert!(scrubbed.contains_key("anthropic_api_key"));
}

#[test]
fn scrub_env_keeps_path() {
    let scrubbed = scrub_env_for_agent(vec![pair("PATH", "/usr/bin:/bin")], &[]);
    assert_eq!(
        scrubbed.get("PATH").map(String::as_str),
        Some("/usr/bin:/bin")
    );
}

#[test]
fn scrub_env_keeps_home() {
    let scrubbed = scrub_env_for_agent(vec![pair("HOME", "/home/u")], &[]);
    assert_eq!(scrubbed.get("HOME").map(String::as_str), Some("/home/u"));
}

#[test]
fn scrub_env_drops_unknown_var_and_keeps_locale_and_xdg() {
    let scrubbed = scrub_env_for_agent(
        vec![
            pair("SOMETHING_IRRELEVANT", "value"),
            pair("DBUS_SESSION_BUS_ADDRESS", "unix:path=/run/x"),
            pair("LC_ALL", "C"),
            pair("XDG_CONFIG_HOME", "/home/u/.config"),
        ],
        &[],
    );
    assert!(!scrubbed.contains_key("SOMETHING_IRRELEVANT"));
    assert!(!scrubbed.contains_key("DBUS_SESSION_BUS_ADDRESS"));
    assert!(scrubbed.contains_key("LC_ALL"));
    assert!(scrubbed.contains_key("XDG_CONFIG_HOME"));
}

#[test]
fn scrub_env_keeps_node_and_ssl_runtime_vars() {
    // The exact regression that killed the previous strict-allowlist
    // attempt: claude is a Node.js CLI and silently exits 1 when its
    // Node runtime vars / SSL cert paths get stripped. These are
    // benign (no credential value) and must pass through.
    for var in &[
        "NODE_PATH",
        "NODE_EXTRA_CA_CERTS",
        "SSL_CERT_FILE",
        "SSL_CERT_DIR",
        "NIX_SSL_CERT_FILE",
        "SHELL",
        "COLORTERM",
    ] {
        let scrubbed = scrub_env_for_agent(vec![pair(var, "x")], &[]);
        assert!(
            scrubbed.contains_key(*var),
            "{var} must survive scrubbing — stripping it was the \
                 regression that made `claude exited 1` with empty stderr"
        );
    }
}

#[test]
fn scrub_env_denylist_entries_are_dropped() {
    for var in AGENT_ENV_DENYLIST {
        let scrubbed = scrub_env_for_agent(vec![pair(var, "secret")], &[]);
        assert!(
            !scrubbed.contains_key(*var),
            "denylist entry {var} must not pass through"
        );
    }
}

#[test]
fn scrub_env_credential_suffix_catches_random_token_names() {
    for var in &["MYSERVICE_TOKEN", "SOMETHING_SECRET", "DEPLOY_API_KEY"] {
        let scrubbed = scrub_env_for_agent(vec![pair(var, "secret")], &[]);
        assert!(
            !scrubbed.contains_key(*var),
            "{var} ends in a credential-suffix pattern and must be dropped"
        );
    }
}

#[test]
fn scrub_env_extra_deny_strips_project_specific_names() {
    // The extension point for a future `--deny-env` flag: a project-
    // specific name that doesn't match the suffix guard but should
    // still be stripped (e.g. a webhook URL that encodes a secret).
    let extra = vec!["PROJECT_DEPLOY_HOOK".to_string()];
    let scrubbed = scrub_env_for_agent(
        vec![pair("PROJECT_DEPLOY_HOOK", "https://hook/with-secret")],
        &extra,
    );
    assert!(!scrubbed.contains_key("PROJECT_DEPLOY_HOOK"));
}

#[test]
fn settings_env_is_scrubbed_like_process_env_but_keeps_claude_config() {
    let settings: BTreeMap<String, String> = [
        ("ANTHROPIC_API_KEY", "sk-ant-x"),
        ("ANTHROPIC_BASE_URL", "https://gw.example.com/v1"),
        ("ANTHROPIC_MODEL", "claude-x"),
        ("CLAUDE_CODE_USE_VERTEX", "1"),
        ("CLOUD_ML_REGION", "europe-west4"),
        ("HTTPS_PROXY", "http://user:pw@proxy:3128"),
        ("GITHUB_TOKEN", "ghp_x"),
        ("MY_SERVICE_TOKEN", "t"),
        ("NODE_OPTIONS", "--require /tmp/evil.js"),
        ("AWS_PROFILE", "prod"),
        ("OSC_PASS", "hunter2"),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_string(), v.to_string()))
    .collect();
    let process = vec![("ANTHROPIC_MODEL".to_string(), "claude-process".to_string())];
    let env = merge_agent_env(settings, process, &[]);
    assert_eq!(
        env.get("ANTHROPIC_API_KEY").map(String::as_str),
        Some("sk-ant-x")
    );
    assert_eq!(
        env.get("ANTHROPIC_BASE_URL").map(String::as_str),
        Some("https://gw.example.com/v1")
    );
    assert_eq!(
        env.get("CLAUDE_CODE_USE_VERTEX").map(String::as_str),
        Some("1")
    );
    assert_eq!(
        env.get("CLOUD_ML_REGION").map(String::as_str),
        Some("europe-west4")
    );
    assert!(env.contains_key("HTTPS_PROXY"), "proxy must survive");
    // Process variables win over settings.
    assert_eq!(
        env.get("ANTHROPIC_MODEL").map(String::as_str),
        Some("claude-process")
    );
    for dropped in [
        "GITHUB_TOKEN",
        "MY_SERVICE_TOKEN",
        "NODE_OPTIONS",
        "AWS_PROFILE",
        "OSC_PASS",
    ] {
        assert!(
            !env.contains_key(dropped),
            "{dropped} must not reach the agent"
        );
    }
}

#[test]
fn settings_env_cloud_names_need_the_pass_list() {
    let settings: BTreeMap<String, String> = [("AWS_REGION".to_string(), "us-east-1".to_string())]
        .into_iter()
        .collect();
    let dropped = merge_agent_env(settings.clone(), Vec::new(), &[]);
    assert!(!dropped.contains_key("AWS_REGION"));
    let kept = merge_agent_env(settings, Vec::new(), &["AWS_REGION".to_string()]);
    assert_eq!(
        kept.get("AWS_REGION").map(String::as_str),
        Some("us-east-1")
    );
}

#[test]
fn credential_shaped_variables_are_scrubbed_but_claude_auth_is_kept() {
    let vars = |names: &[&str]| -> Vec<(String, String)> {
        names
            .iter()
            .map(|n| (n.to_string(), "v".to_string()))
            .collect()
    };
    let kept = scrub_env_for_agent(
        vars(&[
            "GPG_PASSPHRASE",
            "MY_SIGNING_KEY",
            "OBS_AUTH",
            "GITHUB_TOKEN",
            "AWS_SECRET_ACCESS_KEY",
            "DB_PASSWORD",
            "KUBECONFIG",
            "DOCKER_AUTH_CONFIG",
            "SSH_AUTH_SOCK",
            "PATH",
            "HOME",
            "ANTHROPIC_API_KEY",
            "CLAUDE_CODE_OAUTH_TOKEN",
        ]),
        &[],
    );
    let names: Vec<&str> = kept.keys().map(String::as_str).collect();
    assert_eq!(
        names,
        [
            "ANTHROPIC_API_KEY",
            "CLAUDE_CODE_OAUTH_TOKEN",
            "HOME",
            "PATH"
        ]
    );
}
