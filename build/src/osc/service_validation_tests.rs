use super::validate_service_xml;

#[test]
fn the_rendered_services_pass() {
    let xml = r#"<services>
  <service mode="manual" name="tar_scm">
    <param name="url">https://github.com/o/r.git</param>
    <param name="revision">v1.0</param>
    <param name="changesauthor"> YOUR EMAIL HERE </param>
  </service>
  <service mode="manual" name="set_version"/>
  <service name="cargo_vendor" mode="manual">
    <param name="src">r</param>
  </service>
</services>"#;
    validate_service_xml(xml.as_bytes()).unwrap();
}

#[test]
fn common_tar_scm_and_cargo_vendor_params_pass() {
    let xml = r#"<services>
  <service name="tar_scm">
    <param name="url">https://github.com/o/r.git</param>
    <param name="exclude">.git</param>
    <param name="include">src</param>
    <param name="submodules">enable</param>
    <param name="history-depth">1</param>
  </service>
  <service name="cargo_vendor">
    <param name="src">r</param>
    <param name="versioned-dirs">true</param>
    <param name="respect-lockfile">true</param>
  </service>
</services>"#;
    validate_service_xml(xml.as_bytes()).unwrap();
    let bad = r#"<services><service name="tar_scm"><param name="extract">../../x</param></service></services>"#;
    assert!(validate_service_xml(bad.as_bytes()).is_err());
}

#[test]
fn node_modules_params_pass_and_paths_are_confined() {
    let xml = r#"<services>
  <service name="node_modules" mode="manual">
    <param name="input">package-lock.json</param>
    <param name="node-dir">node_modules</param>
    <param name="omit-optional">true</param>
    <param name="legacy-container"/>
    <param name="cpio">node_modules.obscpio</param>
    <param name="output">node_modules.spec.inc</param>
    <param name="source-offset">10000</param>
  </service>
</services>"#;
    validate_service_xml(xml.as_bytes()).unwrap();
    for p in ["input", "node-dir", "cpio", "output"] {
        for v in ["../x", "/etc/x", "&#46;&#46;/x"] {
            let bad = format!(
                r#"<services><service name="node_modules"><param name="{p}">{v}</param></service></services>"#
            );
            assert!(validate_service_xml(bad.as_bytes()).is_err(), "{bad}");
        }
    }
}

#[test]
fn non_utf8_input_is_refused() {
    assert!(validate_service_xml(b"<services><!-- \xff --></services>").is_err());
}

#[test]
fn a_param_is_accepted_only_for_the_services_that_take_it() {
    for (service, param, accepted) in [
        ("cargo_vendor", "src", true),
        ("cargo_vendor", "exclude-re", false),
        ("cargo_vendor", "version", false),
        ("cargo_vendor", "scm", false),
        ("cargo_vendor", "file", false),
        ("tar_scm", "history-depth", true),
        ("tar_scm", "depth", false),
        ("tar_scm", "filter", false),
        ("tar_scm", "basename", false),
        ("tar_scm", "regex", false),
        ("obs_scm", "exclude-re", true),
        ("set_version", "basename", true),
        ("set_version", "regex", true),
        ("set_version", "url", false),
        ("recompress", "file", true),
        ("recompress", "src", false),
        ("download_files", "changesgenerate", true),
        ("download_files", "compression", false),
        ("format_spec_file", "file", false),
        ("node_modules", "input", true),
        ("go_modules", "input", false),
    ] {
        let xml = format!(
            r#"<services><service name="{service}"><param name="{param}">x</param></service></services>"#
        );
        assert_eq!(
            validate_service_xml(xml.as_bytes()).is_ok(),
            accepted,
            "{service} {param}"
        );
    }
}

#[test]
fn a_src_that_names_a_url_is_checked_like_a_url() {
    for src in [
        "file:///home/x/repo",
        "http://github.com/o/r",
        "https://evil.example/o/r",
        "ssh://github.com/o/r",
        "git@github.com:o/r.git",
    ] {
        let xml = format!(
            r#"<services><service name="cargo_vendor"><param name="src">{src}</param></service></services>"#
        );
        assert!(validate_service_xml(xml.as_bytes()).is_err(), "{src}");
    }
    for src in [
        "https://github.com/o/r",
        "rustsec",
        "*.tar.gz",
        "v0*",
        "sub/dir",
    ] {
        let xml = format!(
            r#"<services><service name="cargo_vendor"><param name="src">{src}</param></service></services>"#
        );
        assert!(validate_service_xml(xml.as_bytes()).is_ok(), "{src}");
    }
}

#[test]
fn decoded_values_are_what_is_checked() {
    let xml = r#"<services><service name="tar_scm"><param name="url">https://github.com/a&amp;b</param><param name="revision"><![CDATA[v1]]></param></service></services>"#;
    validate_service_xml(xml.as_bytes()).unwrap();
}

#[test]
fn unknown_services_params_and_urls_are_refused() {
    for xml in [
        r#"<services><service name="evil"/></services>"#,
        r#"<services><service name="tar_scm"><param name="command">id</param></service></services>"#,
        r#"<services><service name="tar_scm"><param name="url">file:///etc</param></service></services>"#,
        r#"<services><service name="tar_scm"><param name="url">git://x/y</param></service></services>"#,
        r#"<services><service name="cargo_vendor"><param name="src">../x</param></service></services>"#,
        r#"<!DOCTYPE x [<!ENTITY a "b">]><services/>"#,
        r#"<services><service name="cargo_vendor"><param name="src">&#46;&#46;/x</param></service></services>"#,
        r#"<services><service name="tar_scm"><param name="revision">&#45;&#45;opt</param></service></services>"#,
        r#"<services><service name="tar_scm"><param name="url">&xxe;</param></service></services>"#,
        r#"<services><service name="tar_scm"><param name="url">https://x</service></services>"#,
        r#"<services><service name="tar_scm"><?pi x?></service></services>"#,
        r#"<services><service name="tar_scm"><evil/></service></services>"#,
    ] {
        assert!(validate_service_xml(xml.as_bytes()).is_err(), "{xml}");
    }
}

/// The `_service` of OBS packages `awww` and `jay` (read from OBS).
const AWWW_SERVICE: &str = r#"<services><service mode="manual" name="cargo_vendor"><param name="url">https://codeberg.org/LGFae/awww</param><param name="revision">v0.12.0</param><param name="versionrewriteregex">^v?(.*)</param><param name="versionrewritepattern">$1</param><param name="changesgenerate">true</param><param name="changesauthor">Soc Virnyl Estela</param><param name="changesemail">uncomfyhalomacro@opensuse.org</param><param name="update">true</param></service></services>"#;

const JAY_SERVICE: &str = r#"<services>
  <service name="cargo_vendor" mode="manual">
     <param name="url">https://github.com/mahkoh/jay</param>
     <param name="revision">v1.9.1</param>
     <param name="compression">zst</param>
     <param name="update">true</param>
     <param name="versionrewriteregex">^v?(.*)</param>
     <param name="versionrewritepattern">$1</param>
     <param name="changesgenerate">true</param>
     <param name="changesauthor">Soc Virnyl Estela</param>
     <param name="changesemail">uncomfyhalomacro@opensuse.org</param>
  </service>
</services>"#;

#[test]
fn awww_and_jay_services_are_accepted() {
    validate_service_xml(AWWW_SERVICE.as_bytes()).unwrap();
    validate_service_xml(JAY_SERVICE.as_bytes()).unwrap();
}

#[test]
fn cargo_vendor_params_that_open_paths_or_outputs_are_refused() {
    for p in [
        "outdir",
        "changesoutfile",
        "import-cargo-config",
        "set-name",
        "set-version",
        "fromfile",
        "keyring-passphrase",
        "sslverify",
    ] {
        let xml = format!(
            r#"<services><service name="cargo_vendor"><param name="{p}">x</param></service></services>"#
        );
        assert!(validate_service_xml(xml.as_bytes()).is_err(), "{p}");
    }
}

#[test]
fn tar_scm_regex_filters_pass_and_output_names_are_refused() {
    let xml = r#"<services><service name="tar_scm">
    <param name="scm">git</param>
    <param name="url">https://github.com/o/r.git</param>
    <param name="exclude-re">^docs/.*</param>
    <param name="include-re">^src/.*\.rs$</param>
  </service></services>"#;
    validate_service_xml(xml.as_bytes()).unwrap();
    for p in ["extension", "extract-rename", "user", "sslverify"] {
        let xml = format!(
            r#"<services><service name="tar_scm"><param name="{p}">x</param></service></services>"#
        );
        assert!(validate_service_xml(xml.as_bytes()).is_err(), "{p}");
    }
}

#[test]
fn awww_and_jay_services_run_the_helpers_with_their_params() {
    use crate::service_sandbox::service_argv;
    use std::path::Path;
    let out = Path::new("/c/.x.cargo_vendor.service");
    let awww = super::parse_service_xml(AWWW_SERVICE.as_bytes()).unwrap();
    assert_eq!(awww.len(), 1);
    assert_eq!(
        service_argv(&awww[0], out),
        [
            "/usr/lib/obs/service/cargo_vendor",
            "--url",
            "https://codeberg.org/LGFae/awww",
            "--revision",
            "v0.12.0",
            "--versionrewriteregex",
            "^v?(.*)",
            "--versionrewritepattern",
            "$1",
            "--changesgenerate",
            "true",
            "--changesauthor",
            "Soc Virnyl Estela",
            "--changesemail",
            "uncomfyhalomacro@opensuse.org",
            "--update",
            "true",
            "--outdir",
            "/c/.x.cargo_vendor.service",
        ]
    );
    let jay = super::parse_service_xml(JAY_SERVICE.as_bytes()).unwrap();
    let argv = service_argv(&jay[0], out);
    assert!(argv.windows(2).any(|w| w == ["--compression", "zst"]));
    assert!(
        argv.windows(2)
            .any(|w| w == ["--url", "https://github.com/mahkoh/jay"])
    );
}

#[test]
fn awww_helper_runs_through_the_sandbox_with_its_argv() {
    use crate::service_sandbox::{Profile, bwrap_args, service_argv};
    use std::path::Path;
    let services = super::parse_service_xml(AWWW_SERVICE.as_bytes()).unwrap();
    let argv = service_argv(
        &services[0],
        Path::new("/work/awww/.o.cargo_vendor.service"),
    );
    let p = Profile {
        checkout: Path::new("/work/awww"),
        scratch: Path::new("/work/awww/.ap-service-x"),
        proxy_dir: Path::new("/tmp/ap-service-proxy-x"),
        extra_env: &[],
    };
    let args = bwrap_args(&p, &argv).unwrap();
    assert!(args.ends_with(&argv));
    assert!(
        args.windows(3)
            .any(|w| w == ["--bind", "/work/awww", "/work/awww"])
    );
}

#[test]
fn a_refused_param_in_an_otherwise_valid_service_refuses_the_file() {
    let xml = AWWW_SERVICE.replace(
        "</service>",
        r#"<param name="outdir">/etc</param></service>"#,
    );
    assert!(super::parse_service_xml(xml.as_bytes()).is_err());
}
