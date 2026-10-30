use super::*;

/// One published release of a forge project: its tag, the page that shows it
/// and the notes its author wrote (empty when there are none).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepoRelease {
    pub tag: String,
    pub url: String,
    pub body: String,
}

/// How many releases the listing asks for: enough for the releases between
/// two updates of a package, small enough for one page.
const RELEASE_PAGE: u32 = 30;

/// The URL of a forge project's release listing, chosen the same way as
/// [`release_endpoints`].
pub(super) fn release_list_url(host: &str, project_path: &str) -> String {
    if is_github_host(host) {
        format!("https://api.github.com/repos/{project_path}/releases?per_page={RELEASE_PAGE}")
    } else if is_gitlab_host(host) {
        let encoded = project_path.replace('/', "%2F");
        format!("https://{host}/api/v4/projects/{encoded}/releases?per_page={RELEASE_PAGE}")
    } else {
        format!("https://{host}/api/v1/repos/{project_path}/releases?limit={RELEASE_PAGE}")
    }
}

/// The published releases of the forge project at `repo_url`, newest first as
/// the forge lists them. Drafts are left out. A URL that is not a forge
/// project has none.
pub fn list_releases(repo_url: &str) -> Result<Vec<RepoRelease>, String> {
    if source_kind(repo_url) != SourceKind::Forge {
        return Ok(Vec::new());
    }
    let (host, project_path, _) = owner_repo(repo_url)?;
    let json = get_json(&release_list_url(&host, &project_path))?;
    Ok(parse_releases(&json, &host, &project_path))
}

/// The releases in a forge's listing: GitHub and Gitea name the notes `body`,
/// GitLab `description`.
pub(super) fn parse_releases(
    json: &serde_json::Value,
    host: &str,
    project_path: &str,
) -> Vec<RepoRelease> {
    let text = |v: &serde_json::Value, key: &str| {
        v.get(key)
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_string()
    };
    json.as_array()
        .into_iter()
        .flatten()
        .filter(|r| r.get("draft").and_then(serde_json::Value::as_bool) != Some(true))
        .filter_map(|r| {
            let tag = text(r, "tag_name");
            if tag.is_empty() {
                return None;
            }
            let body = if r.get("body").is_some() {
                text(r, "body")
            } else {
                text(r, "description")
            };
            // The page of the release itself. Only a link on the forge's own
            // host is kept: the notes go into a package's changelog.
            let own_page = format!("https://{host}/{project_path}/");
            let url = [text(r, "html_url"), text(&r["_links"], "self")]
                .into_iter()
                .find(|u| u.starts_with(&own_page))
                .unwrap_or_else(|| format!("{own_page}releases/tag/{tag}"));
            Some(RepoRelease { tag, url, body })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn the_listing_url_follows_the_forge() {
        assert_eq!(
            release_list_url("github.com", "o/r"),
            "https://api.github.com/repos/o/r/releases?per_page=30"
        );
        assert_eq!(
            release_list_url("gitlab.com", "g/sub/p"),
            "https://gitlab.com/api/v4/projects/g%2Fsub%2Fp/releases?per_page=30"
        );
        assert_eq!(
            release_list_url("codeberg.org", "o/r"),
            "https://codeberg.org/api/v1/repos/o/r/releases?limit=30"
        );
    }

    #[test]
    fn github_and_gitlab_listings_parse_alike() {
        let gh = json!([
            {"tag_name": "v2", "body": "- a", "html_url": "https://github.com/o/r/releases/tag/v2"},
            {"tag_name": "v1.5", "body": "wip", "draft": true},
            {"tag_name": "v1", "body": null, "html_url": "https://evil.example/x"},
        ]);
        let got = parse_releases(&gh, "github.com", "o/r");
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].url, "https://github.com/o/r/releases/tag/v2");
        assert_eq!(got[0].body, "- a");
        // A link to another host is replaced by the forge's own tag page.
        assert_eq!(got[1].url, "https://github.com/o/r/releases/tag/v1");
        assert_eq!(got[1].body, "");

        let gl = json!([{"tag_name": "1.0", "description": "- b",
            "_links": {"self": "https://gitlab.com/g/p/-/releases/1.0"}}]);
        let got = parse_releases(&gl, "gitlab.com", "g/p");
        assert_eq!(got[0].body, "- b");
        assert_eq!(got[0].url, "https://gitlab.com/g/p/-/releases/1.0");
    }

    #[test]
    fn a_reply_that_is_not_a_list_has_no_releases() {
        assert!(parse_releases(&json!({"message": "Not Found"}), "github.com", "o/r").is_empty());
    }
}
