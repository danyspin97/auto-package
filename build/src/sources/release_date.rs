//! The day the newest release of a forge repository was published, for
//! comparing it with the date of a snapshot. Tags carry no date here: only a
//! release counts.

use super::*;

/// The newest release of the forge repository `repo_url`, as its version and
/// the day it was published (`YYYY-MM-DD`). A repository with no release is an
/// error, as is a forge that does not say when its releases came out.
pub fn latest_release_date(repo_url: &str) -> Result<(String, String), String> {
    if source_kind(repo_url) != SourceKind::Forge {
        return Err(format!("{repo_url} is not a forge repository"));
    }
    let (host, project_path, repo_name) = owner_repo(repo_url)?;
    let url = release_endpoints(&host, &project_path)[0].clone();
    let answer = get_json(&url)?;
    release_day(&answer, &repo_name).map_err(|why| format!("{url}: {why}"))
}

/// The version and publication day of the newest release in a forge answer:
/// one release object, or a list of them (GitLab's `releases?per_page=1`).
pub(super) fn release_day(
    answer: &serde_json::Value,
    repo_name: &str,
) -> Result<(String, String), String> {
    let release = match answer {
        serde_json::Value::Array(items) => items.first(),
        other => Some(other),
    }
    .ok_or("no release in the answer")?;
    let tag = first_tag(release).ok_or("no release tag in the answer")?;
    let version = version_from_tag(&tag, repo_name);
    if !version.chars().any(|c| c.is_ascii_digit()) {
        return Err(format!("the newest release {tag:?} is not a version"));
    }
    let published = ["published_at", "released_at", "created_at"]
        .iter()
        .find_map(|key| release.get(*key).and_then(|v| v.as_str()))
        .ok_or_else(|| format!("the release {tag:?} has no date"))?;
    let day = published
        .get(..10)
        .filter(|d| d.as_bytes().get(4) == Some(&b'-') && d.as_bytes().get(7) == Some(&b'-'))
        .ok_or_else(|| format!("the date {published:?} is not YYYY-MM-DD"))?;
    Ok((version, day.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_github_release_gives_its_version_and_publication_day() {
        let answer = json!({
            "tag_name": "v1.4.9",
            "published_at": "2026-11-01T08:30:00Z",
        });
        assert_eq!(
            release_day(&answer, "btop"),
            Ok(("1.4.9".to_string(), "2026-11-01".to_string()))
        );
    }

    #[test]
    fn a_gitlab_list_answer_uses_its_first_release_and_released_at() {
        let answer = json!([{ "tag_name": "0.9.8", "released_at": "2026-09-30T00:00:00.000Z" }]);
        assert_eq!(
            release_day(&answer, "please"),
            Ok(("0.9.8".to_string(), "2026-09-30".to_string()))
        );
    }

    #[test]
    fn an_answer_with_no_release_is_an_error() {
        assert!(release_day(&json!([]), "btop").is_err());
        assert!(release_day(&json!({ "message": "Not Found" }), "btop").is_err());
    }

    #[test]
    fn a_release_without_a_date_is_an_error() {
        let answer = json!({ "tag_name": "v1.0" });
        assert!(release_day(&answer, "btop").is_err());
    }
}
