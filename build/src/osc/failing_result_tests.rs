use super::*;
use crate::osc::binaries::*;

const XML: &str = r#"<resultlist state="x">
  <result project="p" repository="openSUSE_Tumbleweed" arch="x86_64" code="published" state="published">
    <status package="python-mcp" code="succeeded"/>
    <status package="python-mcp:test" code="failed"/>
  </result>
  <result project="p" repository="openSUSE_Tumbleweed" arch="i586" code="published" state="published">
    <status package="python-mcp" code="failed"/>
  </result>
</resultlist>"#;

#[test]
fn a_failed_flavor_is_the_result_whose_log_explains_the_failure() {
    let name = failing_entry_name(
        XML,
        "openSUSE_Tumbleweed",
        "x86_64",
        "python-mcp",
        &BuildStatus::Failed,
    );
    assert_eq!(name.as_deref(), Some("python-mcp:test"));
}

#[test]
fn the_package_itself_wins_when_it_has_the_status() {
    let name = failing_entry_name(
        XML,
        "openSUSE_Tumbleweed",
        "i586",
        "python-mcp",
        &BuildStatus::Failed,
    );
    assert_eq!(name.as_deref(), Some("python-mcp"));
    assert_eq!(
        failing_entry_name(
            XML,
            "openSUSE_Tumbleweed",
            "x86_64",
            "python-mcp",
            &BuildStatus::Unresolvable
        ),
        None
    );
}
