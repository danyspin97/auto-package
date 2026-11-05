//! The forge calls of a real run (plan 3.2 and 3.3): `gh api` first for
//! GitHub, then raw https (in `sources`). Each call's host goes through the
//! allowed-hosts policy as a source, and the API calls go through the run's
//! budget. A patch is fetched through the ordinary patch download, which
//! checks it as a patch.

use serde_json::Value;

use super::budget;
use super::status::ForgeNet;
use crate::allowed_hosts::{AllowedHosts, Asker, Use};
use crate::backport::{MAX_PATCH_BYTES, check_patch};

/// Requests one download may make: the original and at most four redirects
/// (the same bound as the JSON path in `sources`).
const MAX_REQUESTS: usize = 5;

/// The next URL of a redirect: an absolute https URL, or a path on the same
/// host. Anything else is refused.
pub(super) fn next_hop(current: &str, location: &str) -> Result<String, String> {
    if location.starts_with("https://") {
        return Ok(location.to_string());
    }
    if location.starts_with('/') && !location.starts_with("//") {
        let host = current
            .strip_prefix("https://")
            .and_then(|r| r.split('/').next())
            .ok_or_else(|| format!("{current} is not an https URL"))?;
        return Ok(format!("https://{host}{location}"));
    }
    Err(format!("the redirect to {location:?} is not an https URL"))
}

/// The body of `url`, following its redirects by hand so that `check` sees the
/// host of every hop before it is asked. Size-capped; the caller checks the
/// patch.
pub(super) fn fetch_checked(
    url: &str,
    check: &dyn Fn(&str) -> Result<(), String>,
) -> Result<Vec<u8>, String> {
    let agent = crate::http::agent(crate::http::DOWNLOAD_TIMEOUT, 0);
    let mut current = url.to_string();
    for _ in 0..MAX_REQUESTS {
        check(&current)?;
        let resp = agent
            .get(&current)
            .call()
            .map_err(|e| format!("{current}: {e}"))?;
        if resp.status().is_redirection() {
            let location = resp
                .headers()
                .get("location")
                .and_then(|v| v.to_str().ok())
                .ok_or_else(|| format!("{current}: a redirect without a location"))?
                .to_string();
            current = next_hop(&current, &location)?;
            continue;
        }
        return resp
            .into_body()
            .with_config()
            .limit(MAX_PATCH_BYTES + 1)
            .read_to_vec()
            .map_err(|e| format!("{current}: {e}"));
    }
    Err(format!("{url}: too many redirects"))
}

/// The forge of one package, with its host policy.
pub struct LiveForge<'a> {
    pub package: &'a str,
    pub hosts: &'a AllowedHosts,
}

impl LiveForge<'_> {
    /// Refuse a URL whose host the policy does not allow for a source.
    fn allowed(&self, url: &str) -> Result<(), String> {
        let host = url
            .strip_prefix("https://")
            .and_then(|rest| rest.split(['/', '?', '#']).next())
            .filter(|h| !h.is_empty())
            .ok_or_else(|| format!("{url} is not an https URL"))?
            .to_ascii_lowercase();
        let asker = Asker {
            package: self.package.to_string(),
            what: "patch provenance check".to_string(),
        };
        self.hosts
            .check_host(&host, Use::Source, &asker)
            .map(|_| ())
    }
}

impl ForgeNet for LiveForge<'_> {
    fn api_json(&self, url: &str) -> Result<Value, String> {
        self.allowed(url)?;
        budget::spend()?;
        let answer = crate::sources::get_json_checked(url, &|hop| self.allowed(hop));
        if let Err(e) = &answer {
            budget::note_error(e);
        }
        answer
    }

    fn patch_text(&self, url: &str) -> Result<String, String> {
        self.allowed(url)?;
        // Not an API call, but the same run and the same forge: it is counted.
        budget::spend()?;
        let answer = fetch_checked(url, &|hop| self.allowed(hop));
        if let Err(e) = &answer {
            budget::note_error(e);
        }
        let bytes = answer?;
        check_patch(&bytes)?;
        Ok(String::from_utf8_lossy(&bytes).into_owned())
    }

    fn release_tags(&self, repo: &str) -> Result<Vec<String>, String> {
        self.allowed(repo)?;
        budget::spend()?;
        let releases = crate::sources::list_releases(repo);
        if let Err(e) = &releases {
            budget::note_error(e);
        }
        Ok(releases?.into_iter().map(|r| r.tag).collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_host_the_policy_refuses_is_not_asked() {
        let hosts = AllowedHosts::empty();
        let forge = LiveForge {
            package: "foo",
            hosts: &hosts,
        };
        let url = "https://gitlab.gnome.org/api/v4/projects/x/merge_requests/1";
        let err = forge.api_json(url).unwrap_err();
        assert!(err.contains("gitlab.gnome.org"), "{err}");
        let err = forge
            .patch_text("https://gitlab.gnome.org/x/-/merge_requests/1.patch")
            .unwrap_err();
        assert!(err.contains("gitlab.gnome.org"), "{err}");
        assert!(forge.release_tags("https://gitlab.gnome.org/x/y").is_err());
    }

    #[test]
    fn a_redirect_is_followed_only_to_an_https_url_or_a_path_on_the_host() {
        assert_eq!(
            next_hop("https://github.com/o/r/pull/1.patch", "/o/r/pull/1.diff").unwrap(),
            "https://github.com/o/r/pull/1.diff"
        );
        assert_eq!(
            next_hop(
                "https://github.com/o/r/pull/1.patch",
                "https://evil.example/x"
            )
            .unwrap(),
            "https://evil.example/x"
        );
        assert!(next_hop("https://github.com/o/r", "http://github.com/x").is_err());
        assert!(next_hop("https://github.com/o/r", "//evil.example/x").is_err());
    }

    #[test]
    fn a_host_the_policy_refuses_is_checked_before_any_request() {
        let refused = |hop: &str| Err(format!("{hop} is not allowed"));
        let err = fetch_checked("https://gitlab.gnome.org/x.patch", &refused).unwrap_err();
        assert!(err.contains("not allowed"), "{err}");
    }
}
