//! The source repository PyPI records for a project, read from its JSON
//! (`project_urls`): the lookup the update stage and `lint` share.

/// The source repository PyPI records for `pkg_name`, from its project URLs.
pub fn recorded_repo(pkg_name: &str) -> Option<String> {
    let url = format!(
        "https://pypi.org/pypi/{}/json",
        urlencoding::encode(pkg_name)
    );
    let body = crate::http::read_text(crate::http::get_with_retries(&url).ok()?).ok()?;
    ap_ecosystem::pypi::source_url_from_json(&body)
}
