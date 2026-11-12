//! The OBS user profile as the last source of a changelog author: what
//! `osc vc` falls back to when neither the environment nor the oscrc names
//! the user ("read from BuildService instance, or defined in oscrc").

use super::changes_author::ObsProfile;
use super::*;
use std::collections::BTreeMap;

/// The text of the first `<tag>` element of `xml`, unescaped and trimmed.
fn element_text(xml: &str, tag: &str) -> Option<String> {
    let open = format!("<{tag}>");
    let start = xml.find(&open)? + open.len();
    let end = start + xml[start..].find(&format!("</{tag}>"))?;
    let text = crate::obs_xml::unescape(&xml[start..end]);
    let text = text.trim();
    (!text.is_empty()).then(|| text.to_string())
}

/// The `realname` and `email` of a `GET /person/<user>` answer.
pub fn parse_person(xml: &str) -> ObsProfile {
    ObsProfile {
        realname: element_text(xml, "realname"),
        email: element_text(xml, "email"),
    }
}

/// `GET /person/<user>` on `apiurl`, through `osc api` (read-only, with the
/// timeout of every query).
fn fetch_uncached(apiurl: &str, user: &str) -> Result<ObsProfile, String> {
    let path = format!("/person/{}", enc(user));
    match osc_api_checked_at(Some(apiurl), &path) {
        Ok(xml) => Ok(parse_person(&xml)),
        Err(ApiFailure::Http(_, m) | ApiFailure::Other(m)) => Err(m),
    }
}

/// What one run learned of each profile, by (API, user): the answer, or the
/// failure.
static ANSWERS: Mutex<BTreeMap<(String, String), Result<ObsProfile, String>>> =
    Mutex::new(BTreeMap::new());

/// `fetch`'s answer for (`apiurl`, `user`), asked once per process. The lock
/// is held across the call so packages running in parallel share one call,
/// and a failure is kept too: a server that hangs costs one timeout per run,
/// not one per gate of every package.
pub(super) fn answer_once(
    apiurl: &str,
    user: &str,
    fetch: &dyn Fn(&str, &str) -> Result<ObsProfile, String>,
) -> Result<ObsProfile, String> {
    let mut answers = ANSWERS.lock().unwrap_or_else(|e| e.into_inner());
    answers
        .entry((apiurl.to_string(), user.to_string()))
        .or_insert_with(|| fetch(apiurl, user))
        .clone()
}

/// The OBS profile of `user` on `apiurl`, read once per run.
pub fn fetch_profile(apiurl: &str, user: &str) -> Result<ObsProfile, String> {
    answer_once(apiurl, user, &fetch_uncached)
}
