//! Capability BuildRequires (`pkgconfig(glib-2.0) >= 2.70`, `cmake(X)`,
//! `perl(Y)`): which RPMs of a repository's build paths provide one. A
//! capability is not a package name, so the answer comes from the RPMs'
//! `Provides:` (see [`crate::osc::rpm_provides`]), not from a name search.
//!
//! A warning needs positive evidence: an RPM the paths reach names the
//! capability with a version the bound does not accept, and no RPM the paths
//! reach satisfies it. Anything else (no RPM found by name, an unreadable
//! RPM, a candidate left unread) is unchecked, never reported as missing.
//!
//! The decisions here are pure: the caller fetches the [`Candidate`]s and this
//! module says what they settle, per repository. Nothing here adds a build
//! path; a capability that is missing is only reported.

use super::*;
use crate::spec::Requirement;

/// One RPM the lookup read: its name, where it is, and its `Provides:` texts,
/// or why they could not be read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    pub name: String,
    pub hit: Hit,
    pub provides: Result<Vec<String>, String>,
}

/// A repository the branch builds in, with the projects its build paths reach
/// (the branch itself included) and how many reachable candidates were not
/// read, so a verdict that rests on the read ones alone is not a warning.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reach {
    pub repo: String,
    pub projects: Vec<String>,
    pub unread: usize,
}

/// A provider that is in the paths but does not meet the bound.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Found {
    pub repo: String,
    /// The project the RPM is published in.
    pub project: String,
    /// The RPM's name.
    pub rpm: String,
    /// The version its `Provides:` gives the capability.
    pub version: String,
}

/// What the providers of one capability come to, over the target repositories.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CapState {
    /// Every target repository reaches a provider that meets the requirement;
    /// `provider` is one of them.
    Provided { provider: Hit },
    /// Some repositories reach a provider that does not meet the requirement
    /// (`found`), and no reachable provider meets it there. `unknown` says,
    /// per other repository, why it was not judged.
    NoProvider {
        found: Vec<Found>,
        unknown: Vec<String>,
        name: String,
    },
    /// Nothing settled it: no repository has positive evidence either way.
    /// The string says why.
    Unchecked(String),
}

/// The word RPM names of a capability's providers contain: the leading run of
/// lowercase letters and digits of its name (`glib` for `pkgconfig(glib-2.0)`,
/// `sdbus` for `pkgconfig(sdbus-c++)`). `None` when the capability cannot be
/// looked up this way: a macro, a file path, or a name with capitals, which
/// RPM names do not keep (`cmake(Qt6Core)`).
pub fn name_needle(capability: &str) -> Option<String> {
    if capability.contains(['%', '/']) {
        return None;
    }
    let (prefix, rest) = capability.split_once('(')?;
    let inner = rest.strip_suffix(')')?;
    if prefix.is_empty() || inner.chars().any(|c| c.is_ascii_uppercase()) {
        return None;
    }
    let word: String = inner
        .chars()
        .take_while(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
        .collect();
    (word.len() >= 3).then_some(word)
}

/// Whether an RPM named `rpm` can provide `capability`: a `-devel` package
/// for the build-time kinds (`pkgconfig()`, `cmake()`, `…()` in general),
/// any package for the language ones, which ship their modules in
/// `perl-Foo` or `python3-foo`.
pub fn may_provide(capability: &str, rpm: &str) -> bool {
    let language = capability.starts_with("perl(")
        || (capability.starts_with("python") && capability.contains("dist("));
    language || rpm.contains("devel")
}

/// How one RPM's `Provides:` settle a requirement.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Match {
    /// Some entry names the capability with a version the bound accepts.
    Satisfied,
    /// Entries name the capability, all with versions the bound rejects; the
    /// first such version is given.
    Older(String),
    /// No entry names the capability.
    NotNamed,
    /// An entry names it, but its version cannot be compared or is missing.
    Unknown(String),
}

/// Judge `req` against a `Provides:` list (`capability` is the name it asks
/// for). With no bound, any entry naming the capability satisfies it.
pub fn provided_within(req: &Requirement, capability: &str, provides: &[String]) -> Match {
    let mut older = None;
    let mut unknown = None;
    for text in provides {
        let (name, version) = crate::osc::split_provide(text);
        if name != capability {
            continue;
        }
        let Some(bound) = &req.bound else {
            return Match::Satisfied;
        };
        let Some((_, have)) = version else {
            unknown = Some(format!("{text} has no version to compare"));
            continue;
        };
        // A bound without a release compares the version part only, so
        // `foo-devel = 1.2.3-4` meets `= 1.2.3` as RPM does.
        let compared = if bound.version.contains('-') {
            have.as_str()
        } else {
            strip_release(&have)
        };
        match crate::version::cmp(compared, &bound.version, crate::version::Scheme::Generic) {
            Some(ord) if accepts(&bound.op, ord) => return Match::Satisfied,
            Some(_) => {
                older.get_or_insert(have);
            }
            None => unknown = Some(format!("cannot compare {have} with {}", bound.version)),
        }
    }
    match (older, unknown) {
        (_, Some(why)) => Match::Unknown(why),
        (Some(have), None) => Match::Older(have),
        (None, None) => Match::NotNamed,
    }
}

/// `version` without its release: `1.2.3-4` is `1.2.3`, `2:1.0-1` is `2:1.0`.
fn strip_release(version: &str) -> &str {
    version.rsplit_once('-').map_or(version, |(v, _)| v)
}

fn accepts(op: &str, ord: std::cmp::Ordering) -> bool {
    use std::cmp::Ordering::*;
    match op {
        ">=" => ord != Less,
        "<=" => ord != Greater,
        "=" | "==" => ord == Equal,
        "<" => ord == Less,
        ">" => ord == Greater,
        _ => false,
    }
}

/// What one repository's reach settles.
enum RepoVerdict {
    Ok(Hit),
    Older(Found),
    Unknown(String),
}

fn judge_repo(
    req: &Requirement,
    capability: &str,
    reach: &Reach,
    candidates: &[Candidate],
) -> RepoVerdict {
    let mut older: Option<Found> = None;
    let mut problems: Vec<String> = Vec::new();
    let mut considered = 0usize;
    for c in candidates
        .iter()
        .filter(|c| reach.projects.contains(&c.hit.project))
    {
        considered += 1;
        let verdict = match &c.provides {
            Ok(provides) => provided_within(req, capability, provides),
            Err(why) => Match::Unknown(format!("reading the provides of {}: {why}", c.name)),
        };
        match verdict {
            Match::Satisfied => return RepoVerdict::Ok(c.hit.clone()),
            Match::Older(version) => {
                older.get_or_insert_with(|| Found {
                    repo: reach.repo.clone(),
                    project: c.hit.project.clone(),
                    rpm: c.name.clone(),
                    version,
                });
            }
            Match::Unknown(why) => problems.push(format!("{}: {why}", c.name)),
            Match::NotNamed => {}
        }
    }
    if !problems.is_empty() {
        return RepoVerdict::Unknown(format!("{}: {}", reach.repo, problems.join(", ")));
    }
    match older {
        Some(found) if reach.unread == 0 => RepoVerdict::Older(found),
        Some(_) => RepoVerdict::Unknown(format!(
            "{}: not every provider was read ({} left out)",
            reach.repo, reach.unread
        )),
        None if considered == 0 => RepoVerdict::Unknown(format!(
            "{}: no RPM in the paths is named for it",
            reach.repo
        )),
        None => RepoVerdict::Unknown(format!("{}: no RPM in the paths names it", reach.repo)),
    }
}

/// Decide `req` (named `capability`) for each target repository, from the
/// candidates read. A candidate counts in a repository when its project is
/// one the repository's paths reach; the RPM's repository name does not
/// matter, as with the package check (a distribution project is referenced
/// as a whole, and its repositories are named differently by its dependents).
pub fn judge_capability(
    req: &Requirement,
    capability: &str,
    reaches: &[Reach],
    candidates: &[Candidate],
) -> CapState {
    if reaches.is_empty() {
        return CapState::Unchecked("no repository builds for this architecture".to_string());
    }
    let mut provider: Option<Hit> = None;
    let mut found = Vec::new();
    let mut unknown = Vec::new();
    for reach in reaches {
        match judge_repo(req, capability, reach, candidates) {
            RepoVerdict::Ok(hit) => {
                provider.get_or_insert(hit);
            }
            RepoVerdict::Older(f) => found.push(f),
            RepoVerdict::Unknown(why) => unknown.push(why),
        }
    }
    if found.is_empty() {
        return match provider {
            Some(provider) if unknown.is_empty() => CapState::Provided { provider },
            _ => CapState::Unchecked(unknown.join("; ")),
        };
    }
    CapState::NoProvider {
        found,
        unknown,
        name: name_needle(capability).unwrap_or_default(),
    }
}
