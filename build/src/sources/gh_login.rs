//! What to do when `gh` is installed but cannot authenticate.

/// Why a `gh api` call failed.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum GhFailure {
    /// `gh` has no login and no `GH_TOKEN`/`GITHUB_TOKEN`: it refused to run
    /// the request at all. The unauthenticated REST API may still answer.
    NotLoggedIn,
    /// `gh` ran the request and it failed (the message is final).
    Failed(String),
}

/// The line that tells the user how to get `gh` (or the raw API) authenticated.
pub(super) const LOGIN_HINT: &str =
    "gh is not logged in: run `gh auth login` or set GITHUB_TOKEN/GH_TOKEN";

/// Whether `gh`'s stderr is its "not logged in" banner ("To get started with
/// GitHub CLI, please run: gh auth login ... populate the GH_TOKEN ...").
pub(super) fn is_not_logged_in(stderr: &str) -> bool {
    stderr.contains("gh auth login") && stderr.contains("GH_TOKEN")
}

/// Fetch `url` as JSON through `gh` when it can, else through `http`. A `gh`
/// that is not logged in falls through to `http`; a failure there carries the
/// login hint, because that is the likely fix.
pub(super) fn get_json_via(
    url: &str,
    gh: &dyn Fn(&str) -> Option<Result<String, GhFailure>>,
    http: &dyn Fn(&str) -> Result<String, String>,
) -> Result<serde_json::Value, String> {
    let mut not_logged_in = false;
    let body = match gh(url) {
        Some(Ok(body)) => body,
        Some(Err(GhFailure::Failed(e))) => return Err(e),
        Some(Err(GhFailure::NotLoggedIn)) => {
            not_logged_in = true;
            http(url).map_err(|e| format!("{e} ({LOGIN_HINT})"))?
        }
        None => http(url)?,
    };
    serde_json::from_str(&body).map_err(|e| {
        let hint = if not_logged_in { LOGIN_HINT } else { "" };
        format!(
            "{url}: {e}{}",
            if hint.is_empty() {
                String::new()
            } else {
                format!(" ({hint})")
            }
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    const BANNER: &str = "To get started with GitHub CLI, please run:  gh auth login\n\
        Alternatively, populate the GH_TOKEN environment variable with a token";

    #[test]
    fn banner_is_recognised() {
        assert!(is_not_logged_in(BANNER));
        assert!(!is_not_logged_in("HTTP 404: Not Found"));
    }

    #[test]
    fn not_logged_in_gh_falls_back_to_raw_http() {
        let http_calls = Cell::new(0);
        let got = get_json_via(
            "https://api.github.com/search/repositories?q=x",
            &|_| Some(Err(GhFailure::NotLoggedIn)),
            &|_| {
                http_calls.set(http_calls.get() + 1);
                Ok(r#"{"items":[]}"#.to_string())
            },
        );
        assert_eq!(http_calls.get(), 1);
        assert!(got.unwrap().get("items").is_some());
    }

    #[test]
    fn fallback_failure_is_shown_with_the_login_hint() {
        let err = get_json_via(
            "https://api.github.com/search/repositories?q=x",
            &|_| Some(Err(GhFailure::NotLoggedIn)),
            &|_| Err("url: http status: 403".to_string()),
        )
        .unwrap_err();
        assert!(err.contains("http status: 403"), "{err}");
        assert!(err.contains("gh auth login"), "{err}");
        assert!(!err.contains('\n'));
    }

    #[test]
    fn other_gh_failure_does_not_fall_back() {
        let err = get_json_via(
            "https://api.github.com/search/repositories?q=x",
            &|_| Some(Err(GhFailure::Failed("gh api: boom".into()))),
            &|_| panic!("raw http must not run"),
        )
        .unwrap_err();
        assert_eq!(err, "gh api: boom");
    }

    #[test]
    fn no_gh_uses_raw_http() {
        let got = get_json_via("u", &|_| None, &|_| Ok("[1]".to_string())).unwrap();
        assert_eq!(got, serde_json::json!([1]));
    }
}
