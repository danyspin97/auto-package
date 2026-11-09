//! The record of an attempt's commit to a package with a `_service`.
//!
//! osc waits for the server's service run after such a commit and then writes
//! the package's expanded revision to `.osc/_files`. The baseline compares that
//! expanded revision with OBS's head (see `expanded_revision`), so it is the
//! value an attempt's commit must be recorded under. The commit itself is
//! identified by the source revision OBS keeps in `_history` (for a link, the
//! link's own `lsrcmd5`, see `own_srcmd5`): the newest revision by the osc user
//! with the message the commit was sent with. The expanded revision is recorded
//! only when that source revision is the package's own revision and the
//! checkout holds the expanded head; anything else is left to the adoption
//! rule (see `baseline_adopt`).

use super::*;

/// The source revision of the commit sent with `message` by `user`: the newest
/// `<revision>` of `history_xml` by that user whose comment is that message.
/// A service run that OBS commits after it is not one of these.
pub fn commit_source(history_xml: &str, user: &str, message: &str) -> Option<LatestRevision> {
    let wanted = history_text(message);
    history_xml
        .split("<revision ")
        .skip(1)
        .filter_map(|entry| latest_revision(&format!("<revision {entry}")))
        .filter(|r| r.user == user && r.comment == wanted)
        .last()
}

/// The expanded revision to record for a commit sent with `message` by `user`,
/// given OBS's `_history` (`history_xml`), its expanded listing after the commit
/// (`listing_xml`) and the checkout's revision `local`. `Err` says which check
/// failed; the commit is then not recorded.
pub fn service_commit_head(
    history_xml: &str,
    listing_xml: &str,
    local: &str,
    user: &str,
    message: &str,
) -> Result<String, String> {
    let source = commit_source(history_xml, user, message).ok_or_else(|| {
        format!("no revision by {user} in its history has the message \"{message}\"")
    })?;
    let head = expanded_revision(listing_xml)
        .ok_or_else(|| "OBS returned no revision for the package".to_string())?;
    if head != local {
        return Err(format!("the checkout holds {local}, not {head}"));
    }
    if own_srcmd5(listing_xml).as_deref() != Some(source.srcmd5.as_str()) {
        return Err(format!(
            "the package's own revision is not the commit's source revision {}",
            source.srcmd5
        ));
    }
    Ok(head)
}

/// [`service_commit_head`] for the package of `checkout` on OBS, read after the
/// commit `message` was sent with. Any failed read is `Err` with its reason.
pub fn service_commit_head_on_obs(
    checkout: &Path,
    message: &str,
    emit: Emit<'_>,
) -> Result<String, String> {
    let (project, package) = checkout_project_package(checkout)?;
    let user = current_user(emit).map_err(|e| format!("the osc user is not known: {e}"))?;
    let history = source_history_xml(&project, &package, emit)
        .map_err(|e| format!("its history is not readable: {e}"))?;
    let listing = source_listing_expanded_xml(&project, &package, emit)
        .map_err(|e| format!("its listing is not readable: {e}"))?;
    let local = checkout_revision(checkout)
        .ok_or_else(|| format!("the checkout of {project}/{package} records no revision"))?;
    service_commit_head(&history, &listing, &local, &user, message)
}

/// A commit message as OBS keeps it in `_history`: white space collapsed.
fn history_text(message: &str) -> String {
    message.split_whitespace().collect::<Vec<_>>().join(" ")
}
