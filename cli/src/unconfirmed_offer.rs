//! Finding the repository for a bare name that PyPI does not carry.

use crate::unconfirmed::Unconfirmed;

/// The repository for a name PyPI does not carry and the search could not
/// verify: a verified one. Otherwise the refusal that lists what was found,
/// or the plain message when nothing was.
pub(crate) fn discover(name: &str) -> color_eyre::Result<String> {
    let repos = ap_build::pipeline::find_source_repo_candidates(name, name);
    if let Some(url) = repos.verified.first() {
        return Ok(url.clone());
    }
    let mut found = ap_build::pipeline::candidate_list(&repos, name);
    if found.is_empty() {
        return Err(color_eyre::eyre::eyre!(
            crate::unconfirmed::no_repository_message(name)
        ));
    }
    ap_build::pipeline::fetch_metadata(&mut found);
    Err(Unconfirmed {
        name: name.to_string(),
        candidates: found,
    }
    .into())
}
