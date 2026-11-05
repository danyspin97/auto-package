//! The raw https GET of a forge API answer, with its redirects followed by
//! hand. Each hop is checked by the caller (the allowed-hosts policy for the
//! patch provenance check; nothing for the other callers) before it is asked,
//! and a token is attached only to the host it was given for.

use super::forge::{ForgeError, describe_forge_http_error, forge_auth_header};

/// Requests one GET may make: the original and at most four redirects.
const MAX_REQUESTS: usize = 5;

/// GET `url` as JSON text (see [`http_get_text`]).
pub(super) fn http_get_text(
    url: &str,
    check: &dyn Fn(&str) -> Result<(), String>,
) -> Result<String, String> {
    let agent = crate::http::agent(crate::http::API_TIMEOUT, 0);
    let mut current = url.to_string();
    for _ in 0..MAX_REQUESTS {
        check(&current)?;
        let origin_host = crate::http::host_of(url);
        let auth = if crate::http::host_of(&current) == origin_host {
            forge_auth_header(&current)
        } else {
            None
        };
        let resp = ap_ecosystem::net::call_with_retries_detailed(|| {
            let mut req = agent
                .get(&current)
                .header("User-Agent", "auto-package")
                .header("Accept", "application/json");
            if let Some((name, value)) = &auth {
                req = req.header(*name, value.as_str());
            }
            req
        })
        .map_err(|e| describe_forge_http_error(url, &ForgeError::from_call_failure(&e)))?;
        let status = resp.status().as_u16();
        if (300..400).contains(&status)
            && let Some(loc) = resp.headers().get("location").and_then(|v| v.to_str().ok())
        {
            current = redirect_target(&current, loc).map_err(|e| format!("{url}: {e}"))?;
            continue;
        }
        if status >= 400 {
            return Err(describe_forge_http_error(
                url,
                &ForgeError::from_status(status),
            ));
        }
        return crate::http::read_text(resp).map_err(|e| format!("{url}: {e}"));
    }
    Err(format!("{url}: too many redirects"))
}

/// Where a redirect from `current` to `loc` leads. An https URL may not be
/// redirected to plain http: that would send the request (and any token) in
/// the clear, and a network attacker could forge the answer. A relative
/// location must be a path (`/...`) on the host of `current`.
pub(super) fn redirect_target(current: &str, loc: &str) -> Result<String, String> {
    let lower = loc.to_ascii_lowercase();
    if lower.starts_with("https://") {
        return Ok(loc.to_string());
    }
    if lower.starts_with("http://") {
        if current.to_ascii_lowercase().starts_with("https://") {
            return Err(format!("refusing redirect from https to insecure {loc}"));
        }
        return Ok(loc.to_string());
    }
    if !loc.starts_with('/') || loc.starts_with("//") {
        return Err(format!(
            "refusing redirect to {loc:?}: not a path on the same host"
        ));
    }
    let host = crate::http::host_of(current).unwrap_or_default();
    Ok(format!("https://{host}{loc}"))
}

#[cfg(test)]
mod tests {
    use super::redirect_target;

    #[test]
    fn a_relative_redirect_must_be_a_path_on_the_same_host() {
        let cur = "https://api.github.com/repos/o/r/pulls/1";
        assert_eq!(
            redirect_target(cur, "/repos/o/r/pulls/2").unwrap(),
            "https://api.github.com/repos/o/r/pulls/2"
        );
        assert!(redirect_target(cur, "evil.com/x").is_err());
        assert!(redirect_target(cur, "//evil.com/x").is_err());
    }

    #[test]
    fn a_refused_host_is_not_asked() {
        let refused = |hop: &str| Err(format!("{hop} is not allowed"));
        let err = super::http_get_text("https://gitlab.gnome.org/api/v4/x", &refused).unwrap_err();
        assert!(err.contains("not allowed"), "{err}");
    }
}
