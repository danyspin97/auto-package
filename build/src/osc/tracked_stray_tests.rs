
use super::*;

#[test]
fn a_tracked_file_missing_from_the_checkout_is_committed_as_deleted() {
    let dir = tempfile::tempdir().unwrap();
    fs::create_dir(dir.path().join(".osc")).unwrap();
    fs::write(
        dir.path().join(".osc/_files"),
        r#"<directory name="python-mcp">
  <entry name="mcp-1.28.1.tar.gz" md5="a" size="1"/>
  <entry name="mcp-2.3.0.tar.gz" md5="b" size="1"/>
  <entry name="python-mcp.spec" md5="c" size="1"/>
</directory>"#,
    )
    .unwrap();
    fs::write(dir.path().join("mcp-2.3.0.tar.gz"), b"").unwrap();
    fs::write(dir.path().join("python-mcp.spec"), b"").unwrap();
    let removed = remove_tracked_strays(dir.path(), &[], Emit::quiet());
    assert_eq!(removed, ["mcp-1.28.1.tar.gz"]);
}
