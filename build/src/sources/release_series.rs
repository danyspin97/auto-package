//! The newest release of a forge repository within one series, read from its tags.

use super::*;

/// The newest release of the forge repository `repo_url` whose version starts
/// with `series` (`[2, 7]` is the 2.7.x releases), read from its tag list.
/// Pre-releases are not taken. Only the first page of tags is read.
pub fn latest_release_in_series(repo_url: &str, series: &[u64]) -> Result<String, String> {
    if source_kind(repo_url) != SourceKind::Forge {
        return Err(format!(
            "{repo_url} is not a git forge, so its tags are not read"
        ));
    }
    let (_host, _project_path, repo_name) = owner_repo(repo_url)?;
    let newest = get_repo_tags(repo_url)?
        .iter()
        .map(|tag| version_from_tag(tag, &repo_name))
        .filter(|v| {
            let numbers = crate::version::leading_numbers(v);
            !numbers.is_empty() && numbers.starts_with(series) && !crate::version::is_prerelease(v)
        })
        .max_by(|a, b| {
            let scheme = crate::version::Scheme::Generic;
            crate::version::cmp(a, b, scheme).unwrap_or(std::cmp::Ordering::Equal)
        });
    newest.ok_or_else(|| {
        let series = series.iter().map(u64::to_string).collect::<Vec<_>>();
        format!(
            "{repo_url} has no release in the series {}",
            series.join(".")
        )
    })
}
