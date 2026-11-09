//! A head OBS holds that this package's own attempt committed, which the
//! baseline never recorded. A commit is recorded only when the package has no
//! `_service` (see `append_landed`), so an attempt's commit to such a package
//! leaves a revision the baseline does not know, and every later restore
//! refuses it. Such a head is adopted as an attempt revision only when it is
//! provably the tool's own; anything else stays refused.

use super::*;

/// The comment every commit of an attempt starts with.
pub const ATTEMPT_PREFIX: &str = "auto-package: ";

/// Whether the newest revision `latest` of the package's history, which OBS
/// holds as `head` (expanded), is the tool's own attempt commit that the
/// baseline may adopt. `own` is the package's own srcmd5 (for a link, its
/// `lsrcmd5`), `local` the checkout's revision, `user` the osc user, and
/// `recorded` the revisions the baseline already holds. `Err` says which
/// check failed.
pub fn own_attempt_commit(
    head: &str,
    local: &str,
    own: Option<&str>,
    latest: &LatestRevision,
    user: &str,
    recorded: &[String],
) -> Result<(), String> {
    if head != local {
        return Err(format!("the checkout holds {local}, not {head}"));
    }
    if recorded.iter().any(|r| r == head) {
        return Err(format!("{head} is already recorded"));
    }
    if own != Some(latest.srcmd5.as_str()) {
        return Err(
            "the package's newest revision in its history is not the one OBS links to".into(),
        );
    }
    if latest.user != user {
        return Err(format!(
            "it was committed by {}, not by {user}",
            latest.user
        ));
    }
    if !latest.comment.starts_with(ATTEMPT_PREFIX) {
        return Err(format!(
            "its message is not one an attempt writes: \"{}\"",
            latest.comment
        ));
    }
    Ok(())
}

/// The package's own srcmd5 from an expanded listing: a link's `lsrcmd5`, the
/// revision of the link itself, or the directory's srcmd5 for a package that is
/// not a link.
pub fn own_srcmd5(listing_xml: &str) -> Option<String> {
    listing_xml
        .find("<linkinfo")
        .map(|i| {
            let rest = &listing_xml[i..];
            &rest[..rest.find('>').unwrap_or(rest.len())]
        })
        .and_then(|tag| crate::obs_xml::attr(tag, "lsrcmd5"))
        .filter(|r| !r.is_empty())
        .or_else(|| parse_directory_srcmd5(listing_xml))
}
