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
    <param name="depth">1</param>
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
