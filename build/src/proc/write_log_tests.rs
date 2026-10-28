use super::*;
use std::os::unix::fs::PermissionsExt;

#[test]
fn a_log_is_redacted_and_private() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("build.log");
    // A pre-existing, world-readable file with older content.
    std::fs::write(&path, "old old old old old old old old").unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
    write_log(
        &path,
        b"cloning https://user:hunter2@example.com/r.git\nAuthorization: Bearer abc\nGITHUB_TOKEN=ghp_x ok\n",
    )
    .unwrap();
    let text = std::fs::read_to_string(&path).unwrap();
    for secret in ["hunter2", "Bearer abc", "ghp_x"] {
        assert!(!text.contains(secret), "{secret} leaked: {text}");
    }
    assert!(text.contains("https://***@example.com/r.git"), "{text}");
    assert!(text.contains(" ok"), "{text}");
    assert!(!text.contains("old old"));
    let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600);
}

#[test]
fn a_new_log_is_created_private() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("new.log");
    write_log(&path, b"plain\xff").unwrap();
    assert_eq!(
        std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o600
    );
    assert!(std::fs::read_to_string(&path).unwrap().starts_with("plain"));
}
