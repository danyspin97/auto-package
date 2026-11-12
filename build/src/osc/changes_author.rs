//! The author of a `.changes` entry: who it is resolved from, what makes one
//! usable, and the repair of an unpublished entry whose author is not.
//! An RPM changelog needs `Name <email>`; without a name and an address rpmlint
//! reports `no-changelogname-tag`, so a placeholder is never written.

use super::run_start_copy;
use crate::service_sandbox::{oscrc_identity, oscrc_paths, oscrc_user};
use std::collections::BTreeSet;
use std::fmt;
use std::fs;
use std::path::Path;

const DEFAULT_APIURL: &str = "https://api.opensuse.org";

/// A changelog author: a name and an address, both usable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Author {
    pub name: String,
    pub email: String,
}

impl fmt::Display for Author {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} <{}>", self.name, self.email)
    }
}

fn is_placeholder_text(text: &str) -> bool {
    let low = text.to_ascii_lowercase();
    low.contains("your email") || low.contains("your name") || low.contains("email here")
}

/// `text` without surrounding whitespace and one pair of matching quotes.
pub(crate) fn unquote(text: &str) -> &str {
    let text = text.trim();
    for q in ['"', '\''] {
        if let Some(inner) = text.strip_prefix(q).and_then(|t| t.strip_suffix(q)) {
            return inner.trim();
        }
    }
    text
}

fn usable_name(name: &str) -> Option<String> {
    let name = unquote(name);
    // A control character (NUL, tab, a line break) or a Unicode line or
    // paragraph separator would split or corrupt the changelog line.
    let ok = !name.is_empty()
        && !name
            .contains(|c: char| c.is_control() || matches!(c, '<' | '>' | '\u{2028}' | '\u{2029}'))
        && !is_placeholder_text(name);
    ok.then(|| name.to_string())
}

fn usable_email(email: &str) -> Option<String> {
    let email = unquote(email);
    let (local, domain) = email.split_once('@')?;
    let ok = !local.is_empty()
        && domain.contains('.')
        && !domain.contains('@')
        && !email.contains(|c: char| c.is_whitespace() || c == '<' || c == '>')
        && !is_placeholder_text(email);
    ok.then(|| email.to_string())
}

/// `Name <email>` read strictly: both parts usable, nothing else on the line.
pub fn parse_author(text: &str) -> Option<Author> {
    let text = text.trim();
    let body = text.strip_suffix('>')?;
    let (name, email) = body.rsplit_once('<')?;
    Some(Author {
        name: usable_name(name)?,
        email: usable_email(email)?,
    })
}

/// The name and the address of an address setting (`$EMAIL`, `VC_MAILADDR`,
/// `email` of the oscrc), which is `addr` or `Name <addr>`.
fn split_env_email(value: &str) -> (Option<String>, Option<String>) {
    match parse_author(value) {
        Some(a) => (Some(a.name), Some(a.email)),
        None => (None, usable_email(value)),
    }
}

/// The API server `oscrc` names as its default, else the openSUSE one.
pub fn default_apiurl(oscrc: &str) -> String {
    let mut in_general = false;
    for line in oscrc.lines().map(str::trim) {
        if let Some(name) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            in_general = name.trim() == "general";
        } else if in_general
            && !line.starts_with(['#', ';'])
            && let Some((key, value)) = line.split_once(['=', ':'])
            && key.trim() == "apiurl"
            && !value.trim().is_empty()
        {
            return value.trim().to_string();
        }
    }
    DEFAULT_APIURL.to_string()
}

/// The name and the address of an OBS user, as the server holds them.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ObsProfile {
    pub realname: Option<String>,
    pub email: Option<String>,
}

/// Where a part of the author came from, for the stop message.
type Found = Option<(String, &'static str)>;

/// What `osc vc` would read from the OBS server: called with the API server
/// and the user of the oscrc section, only when a part is still missing.
pub type ProfileLookup<'a> = &'a dyn Fn(&str, &str) -> Result<ObsProfile, String>;

fn found(part: &Found) -> String {
    part.as_ref()
        .map_or_else(String::new, |(v, src)| format!(" (found {v} in {src})"))
}

/// The text that says what is missing, what was found where, and what to set.
fn unresolved(name: &Found, email: &Found, apiurl: &str, profile_err: Option<&str>) -> String {
    let missing = match (name, email) {
        (None, Some(_)) => format!("no usable name{}", found(email)),
        (Some(_), None) => format!("no usable email address{}", found(name)),
        _ => "no usable name or email address".to_string(),
    };
    let profile = profile_err.map_or_else(String::new, |e| {
        format!("; could not read your OBS profile: {e}")
    });
    format!(
        "no changelog author: {missing}{profile}. Set `realname` and `email` in the [{apiurl}] \
         section of your oscrc, or export VC_REALNAME and VC_MAILADDR (or EMAIL=\"Name <address>\")"
    )
}

/// The author, from `env` (`VC_REALNAME`, `VC_MAILADDR`, as osc reads them),
/// then the `realname` and `email` of the oscrc section of `apiurl` (the
/// default API when `None`), then `EMAIL`, then, for a part still missing, the
/// OBS profile of the oscrc user (`profile`, as `osc vc` does). Each part is
/// the first usable one; a placeholder is skipped, and `Err` says what was
/// found where, what is missing and what to set. A profile that cannot be read
/// is not fatal while the other sources suffice.
pub fn resolve_author(
    env: &dyn Fn(&str) -> Option<String>,
    oscrc: &str,
    apiurl: Option<&str>,
    profile: ProfileLookup<'_>,
) -> Result<Author, String> {
    let apiurl = apiurl.map_or_else(|| default_apiurl(oscrc), |u| u.trim().to_string());
    let from_rc = oscrc_identity(oscrc, &apiurl);
    let rc = |var: &str| {
        from_rc
            .iter()
            .find(|(k, _)| k == var)
            .map(|(_, v)| v.clone())
    };
    let sources: [(Option<String>, &'static str); 3] = [
        (env("VC_MAILADDR"), "VC_MAILADDR"),
        (rc("VC_MAILADDR"), "the oscrc email"),
        (env("EMAIL"), "EMAIL"),
    ];
    let addresses: Vec<_> = sources
        .into_iter()
        .filter_map(|(v, src)| Some((split_env_email(&v?), src)))
        .collect();
    let mut name: Found = [
        (env("VC_REALNAME"), "VC_REALNAME"),
        (rc("VC_REALNAME"), "the oscrc realname"),
    ]
    .into_iter()
    .find_map(|(v, src)| Some((usable_name(&v?)?, src)))
    .or_else(|| {
        addresses
            .iter()
            .find_map(|((n, _), src)| Some((n.clone()?, *src)))
    });
    let mut email: Found = addresses
        .iter()
        .find_map(|((_, e), src)| Some((e.clone()?, *src)));
    let mut profile_err = None;
    if name.is_none() || email.is_none() {
        let user = oscrc_user(oscrc, &apiurl);
        match user.as_deref().map(|u| profile(&apiurl, u)) {
            Some(Ok(p)) => {
                name = name.or_else(|| Some((usable_name(&p.realname?)?, "your OBS profile")));
                email = email.or_else(|| Some((usable_email(&p.email?)?, "your OBS profile")));
            }
            Some(Err(e)) => profile_err = Some(e),
            None => {
                profile_err = Some(format!("the oscrc section [{apiurl}] names no `user`"));
            }
        }
    }
    match (name, email) {
        (Some((name, _)), Some((email, _))) => Ok(Author { name, email }),
        (n, e) => Err(unresolved(&n, &e, &apiurl, profile_err.as_deref())),
    }
}

/// The API server of a checkout: its `.osc/_apiurl`, when there is one.
fn checkout_apiurl(checkout: &Path) -> Option<String> {
    fs::read_to_string(checkout.join(".osc").join("_apiurl"))
        .ok()
        .map(|u| u.trim().to_string())
        .filter(|u| !u.is_empty())
}

/// [`resolve_author`] for the API of `checkout` (the default API when it has
/// none), so every gate of a run resolves against the same section.
pub fn resolve_author_for(
    checkout: Option<&Path>,
    env: &dyn Fn(&str) -> Option<String>,
    oscrc: &str,
    profile: ProfileLookup<'_>,
) -> Result<Author, String> {
    let apiurl = checkout.and_then(checkout_apiurl);
    resolve_author(env, oscrc, apiurl.as_deref(), profile)
}

/// The profile lookup of a host that reads none: nothing to add.
fn no_profile(_apiurl: &str, _user: &str) -> Result<ObsProfile, String> {
    Ok(ObsProfile::default())
}

/// [`resolve_author_for`] with this machine's environment and oscrc; the OBS
/// profile is not read.
pub fn host_author(checkout: Option<&Path>) -> Result<Author, String> {
    let oscrc = oscrc_paths()
        .into_iter()
        .find_map(|p| fs::read_to_string(p).ok())
        .unwrap_or_default();
    resolve_author_for(
        checkout,
        &|k| std::env::var(k).ok().filter(|v| !v.trim().is_empty()),
        &oscrc,
        &no_profile,
    )
}

/// The headers (date and author line) of the entries of `text`.
fn header_set(text: &str) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    let mut lines = text.lines();
    while let Some(l) = lines.next() {
        if super::is_changes_separator(l)
            && let Some(h) = lines.next()
        {
            out.insert(h.trim().to_string());
        }
    }
    out
}

/// `text` with the author line of each unpublished entry set to `author` when
/// it is not a usable `Name <email>`. An entry is unpublished when its header
/// is not one of `published`'s. Only the part after the date's ` - ` changes;
/// line endings stay. `None` when nothing needs repair.
pub(super) fn with_repaired_authors(
    text: &str,
    published: &str,
    author: &Author,
) -> Option<String> {
    let known = header_set(published);
    let mut out = String::with_capacity(text.len());
    let mut changed = false;
    let mut after_separator = false;
    for raw in text.split_inclusive('\n') {
        let line = raw.trim_end_matches(['\r', '\n']);
        let ending = &raw[line.len()..];
        if after_separator {
            after_separator = false;
            if !known.contains(line.trim())
                && let Some((date, who)) = line.split_once(" - ")
                && !date.trim().is_empty()
                && parse_author(who).is_none()
            {
                out.push_str(&format!("{date} - {author}{ending}"));
                changed = true;
                continue;
            }
        } else if super::is_changes_separator(line) {
            after_separator = true;
        }
        out.push_str(raw);
    }
    changed.then_some(out)
}

/// Sets `author` on the unpublished entries of the `.changes` files of
/// `checkout_dir` whose author line is a placeholder or has no name or address
/// (what `baseline` held at the start of the run is published). Older entries
/// are never touched. Returns the files rewritten.
pub fn repair_unpublished_authors(
    checkout_dir: &Path,
    baseline: Option<&Path>,
    author: &Author,
) -> Vec<String> {
    let Ok(rd) = fs::read_dir(checkout_dir) else {
        return Vec::new();
    };
    let mut done = Vec::new();
    for file in rd
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "changes"))
    {
        let Some(name) = file.file_name() else {
            continue;
        };
        let published = run_start_copy(checkout_dir, baseline, name);
        if let Ok(text) = fs::read_to_string(&file)
            && let Some(fixed) = with_repaired_authors(&text, &published, author)
            && fs::write(&file, fixed).is_ok()
        {
            done.push(name.to_string_lossy().into_owned());
        }
    }
    done.sort();
    done
}
