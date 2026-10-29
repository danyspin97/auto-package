use super::*;

fn overall(xml: &str) -> BuildStatus {
    overall_status(&obs_status::parse_results(xml), "foo").0
}

#[test]
fn every_repository_and_arch_must_be_built_or_out_of_scope() {
    let doc = |second: &str| {
        format!(
            r#"<resultlist>
  <result repository="r" arch="x86_64"><status package="foo" code="succeeded"/></result>
  <result repository="r" arch="i586"><status package="foo" code="{second}"/></result>
  <result repository="r" arch="aarch64"><status package="bar" code="failed"/></result>
</resultlist>"#
        )
    };
    assert_eq!(overall(&doc("excluded")), BuildStatus::Succeeded);
    assert_eq!(overall(&doc("disabled")), BuildStatus::Succeeded);
    assert_eq!(overall(&doc("failed")), BuildStatus::Failed);
    assert_eq!(overall(&doc("building")), BuildStatus::Building);
}

#[test]
fn the_blocking_repository_and_arch_is_named_and_a_failure_outranks_a_wait() {
    let xml = r#"<resultlist>
  <result repository="r" arch="x86_64"><status package="foo" code="building"/></result>
  <result repository="r" arch="i586"><status package="foo" code="failed"/></result>
</resultlist>"#;
    assert_eq!(
        overall_status(&obs_status::parse_results(xml), "foo"),
        (BuildStatus::Failed, Some("r/i586".to_string()))
    );
    let xml = r#"<resultlist>
  <result repository="r" arch="x86_64"><status package="foo" code="succeeded"/></result>
  <result repository="r" arch="i586"><status package="foo" code="scheduled"/></result>
</resultlist>"#;
    assert_eq!(
        overall_status(&obs_status::parse_results(xml), "foo"),
        (BuildStatus::Scheduled, Some("r/i586".to_string()))
    );
}

#[test]
fn an_unresolvable_flavor_keeps_the_package_out_of_the_built_set() {
    let xml = r#"<resultlist>
  <result repository="r" arch="x86_64">
    <status package="python-mcp" code="succeeded"/>
    <status package="python-mcp:test" code="unresolvable"/>
    <status package="python-ok" code="succeeded"/>
    <status package="python-ok:test" code="succeeded"/>
  </result>
</resultlist>"#;
    let folded = fold_flavor_statuses(project_statuses_from_xml(xml, "r", "x86_64").unwrap());
    assert!(!folded["python-mcp"].is_success());
    assert!(folded["python-ok"].is_success());
    assert!(folded.contains_key("python-mcp:test"));
}

/// python-redis: tests failing in `:test` do not unbuild the package.
#[test]
fn a_failing_test_flavor_does_not_unbuild_its_package() {
    let xml = r#"<resultlist>
  <result repository="r" arch="x86_64">
    <status package="python-redis" code="succeeded"/>
    <status package="python-redis:test" code="failed"/>
    <status package="python-foo" code="succeeded"/>
    <status package="python-foo:openmpi4" code="failed"/>
  </result>
</resultlist>"#;
    let folded = fold_flavor_statuses(project_statuses_from_xml(xml, "r", "x86_64").unwrap());
    assert!(folded["python-redis"].is_success());
    assert_eq!(folded["python-redis:test"], BuildStatus::Failed);
    // Any other flavor that fails still counts.
    assert!(!folded["python-foo"].is_success());
}

#[test]
fn a_failed_multibuild_flavor_counts_against_its_package() {
    let xml = r#"<resultlist>
  <result repository="r" arch="x86_64">
    <status package="foo" code="succeeded"/>
    <status package="foo:py313" code="failed"/>
    <status package="foobar" code="failed"/>
  </result>
</resultlist>"#;
    assert_eq!(overall(xml), BuildStatus::Failed);
}

#[test]
fn nothing_built_is_not_a_success() {
    let xml = r#"<resultlist><result repository="r" arch="a"><status package="foo" code="excluded"/></result></resultlist>"#;
    assert!(!overall(xml).is_success());
    assert!(!overall("<resultlist/>").is_success());
}
