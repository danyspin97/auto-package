use super::source_repo_from_metadata as repo;

#[test]
fn source_keys_beat_homepage_and_other_hosts_are_skipped() {
    let pi = "Metadata-Version: 2.1\nProject-URL: Documentation, https://docs.example.org/x\n\
              Project-URL: Homepage, https://github.com/o/home\n\
              Project-URL: Source, git@github.com:o/real.git\n\nProject-URL: Source, https://github.com/o/body\n";
    assert_eq!(repo(pi, "").as_deref(), Some("https://github.com/o/real"));
    let py = "[project.urls]\nHomepage = \"https://gitlab.com/g/sub/proj/-/issues\"\n";
    assert_eq!(
        repo("", py).as_deref(),
        Some("https://gitlab.com/g/sub/proj")
    );
    assert_eq!(
        repo("Project-URL: Home, https://bitbucket.org/o/r\n", ""),
        None
    );
    assert_eq!(repo("Home-page: https://github.com/onlyowner\n", ""), None);
    assert_eq!(repo("", "not toml ["), None);
}
