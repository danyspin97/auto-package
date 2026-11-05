//! What to do with each carried patch (plan 1.3 and 4.1): where it comes from,
//! the forge's state of that request, the release that holds the merge, and,
//! for an open request, whether its head moved with different code. Nothing is
//! written here; the verdict and the header plan are what lint, update and
//! review act on.

use super::cache::{containment_cached, state_cached};
use super::header::{Claim, github_reference, spec_with_comment, with_header};
use super::reason::{reason_from_changes, reason_from_subject};
use super::status::{Containment, ForgeNet, PrState, PrStatus, diff_url};
use super::{OriginEvidence, OriginInputs, OriginKind, OriginRead, PatchOrigin, read_origin};
use crate::backport::is_commit_id;
use crate::bugfix::code_lines;
use crate::pipeline::{header_field, header_lines};

/// What a patch's header gets: the words of `Upstream:`, the commit of
/// `Git-commit:`, and the reason of `Description:`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HeaderPlan {
    /// `submitted` or `merged`; `None` for a bare URL.
    pub status: Option<&'static str>,
    pub commit: Option<String>,
    pub reason: Option<String>,
}

/// What is to be done about one patch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    /// Nothing to act on: no request, unchanged, or merged but in no release.
    Nothing,
    /// Merged, and the packaged release holds it: the patch should be redundant.
    Redundant { tag: String },
    /// Merged in a newer release than the packaged one.
    ReleasedLater { tag: String, version: String },
    /// Open, and the head moved with different code: the request's `.patch`.
    HeadMoved { head: String, patch_url: String },
    /// The head moved, but this file cannot be compared (the reason).
    MovedNotComparable(String),
    /// Closed without a merge: a maintainer decides.
    Closed,
    /// Not checked (the reason): nothing is written, dropped or replaced.
    NotChecked(String),
}

/// Everything known about one carried patch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Provenance {
    pub name: String,
    pub origin: Option<PatchOrigin>,
    /// The file has `Upstream:` or `Patch-Mainline:`: its header is never written.
    pub has_upstream_line: bool,
    pub status: Result<PrStatus, String>,
    /// Which release holds the merge commit, when it was asked.
    pub containment: Option<Containment>,
    pub verdict: Verdict,
    /// The header this patch gets, when it has one (Open, merged, commit).
    pub header: Option<HeaderPlan>,
    /// The spec comment above the patch line is due (see [`spec_comment_for`]).
    pub spec_comment_due: bool,
}

/// What `assess` reads: the package and its declared patches.
pub struct Inputs<'a> {
    pub pkg: &'a str,
    pub spec: &'a str,
    pub changes: &'a str,
    /// The spec's `Version:`.
    pub version: &'a str,
    /// The package's own project (https), from the spec, if it has one.
    pub repo: Option<String>,
    /// Each declared patch: its file name and its text.
    pub patches: &'a [(String, String)],
}

/// The provenance of each declared patch, in order.
pub fn assess(inputs: &Inputs<'_>, net: &dyn ForgeNet, refresh: bool) -> Vec<Provenance> {
    inputs
        .patches
        .iter()
        .map(|(name, text)| {
            let mut p = assess_one(inputs, name, text, net, refresh);
            p.spec_comment_due = spec_comment_for(&p, inputs.spec).is_some();
            p
        })
        .collect()
}

fn plain(name: &str, text: &str, verdict: Verdict) -> Provenance {
    let header = lines_of(text);
    let has_upstream_line = header_field(&header, "Upstream").is_some()
        || header_field(&header, "Patch-Mainline").is_some();
    Provenance {
        name: name.to_string(),
        origin: None,
        has_upstream_line,
        status: Err("no upstream request is named".to_string()),
        containment: None,
        verdict,
        header: None,
        spec_comment_due: false,
    }
}

fn lines_of(text: &str) -> Vec<&str> {
    header_lines(text)
}

fn assess_one(
    inputs: &Inputs<'_>,
    name: &str,
    text: &str,
    net: &dyn ForgeNet,
    refresh: bool,
) -> Provenance {
    let read = read_origin(&OriginInputs {
        pkg: inputs.pkg,
        spec: inputs.spec,
        changes: inputs.changes,
        name,
        text,
    });
    let origin = match read {
        OriginRead::None => return plain(name, text, Verdict::Nothing),
        OriginRead::Conflict(urls) => {
            let why = format!("the sources name different requests: {}", urls.join(", "));
            return plain(name, text, Verdict::NotChecked(why));
        }
        OriginRead::Found(o) => o,
    };
    let mut out = Provenance {
        status: Err("not asked".into()),
        ..plain(name, text, Verdict::Nothing)
    };
    out.origin = Some(origin.clone());
    if origin.evidence == OriginEvidence::FileName
        && let Some(why) = not_corroborated(&origin, text, net)
    {
        out.origin = None;
        out.verdict = Verdict::NotChecked(why);
        return out;
    }
    let reason_of = |title: Option<&str>| {
        reason_from_changes(inputs.changes, name)
            .or_else(|| title.map(str::to_string).filter(|t| !t.is_empty()))
            .or_else(|| reason_from_subject(text))
    };
    if origin.kind == OriginKind::Commit {
        let sha = origin.commit.clone().unwrap_or_default();
        return commit_verdict(inputs, out, &origin, &sha, reason_of(None), net, refresh);
    }
    let status = match state_cached(net, &origin, refresh) {
        Ok(s) => s,
        Err(why) => {
            out.status = Err(why.clone());
            out.verdict = Verdict::NotChecked(why);
            return out;
        }
    };
    let reason = reason_of(Some(&status.title));
    out.status = Ok(status.clone());
    match status.state {
        PrState::Closed => out.verdict = Verdict::Closed,
        PrState::Open => {
            out.verdict = head_moved(text, &status.head_sha, &origin, net);
            if !matches!(out.verdict, Verdict::NotChecked(_)) {
                out.header = Some(HeaderPlan {
                    status: Some("submitted"),
                    commit: None,
                    reason,
                });
            }
        }
        PrState::Merged => {
            let Some(merge) = status.merge_sha.clone() else {
                out.verdict = Verdict::NotChecked("merged, but the merge commit is unknown".into());
                return out;
            };
            let repo = inputs.repo.as_deref().unwrap_or_default();
            let c = containment_cached(net, &origin, repo, inputs.version, &merge, refresh);
            out.verdict = verdict_of_containment(&c);
            // Not checked, nothing is written (plan 4.1).
            if !matches!(c, Containment::NotChecked(_)) {
                out.header = Some(HeaderPlan {
                    status: Some("merged"),
                    commit: Some(merge),
                    reason,
                });
            }
            out.containment = Some(c);
        }
    }
    out
}

/// The verdict of a merged change by where a release holds it.
fn verdict_of_containment(c: &Containment) -> Verdict {
    match c {
        Containment::InPackaged { tag } => Verdict::Redundant { tag: tag.clone() },
        Containment::InNewer { tag, version } => Verdict::ReleasedLater {
            tag: tag.clone(),
            version: version.clone(),
        },
        Containment::NotReleased => Verdict::Nothing,
        Containment::NotChecked(why) => Verdict::NotChecked(why.clone()),
    }
}

/// A commit is merged by definition: its release is asked for the commit itself.
fn commit_verdict(
    inputs: &Inputs<'_>,
    mut out: Provenance,
    origin: &PatchOrigin,
    sha: &str,
    reason: Option<String>,
    net: &dyn ForgeNet,
    refresh: bool,
) -> Provenance {
    if !is_commit_id(sha) {
        out.verdict = Verdict::NotChecked("the commit id is not valid".into());
        return out;
    }
    let repo = inputs.repo.as_deref().unwrap_or_default();
    let c = containment_cached(net, origin, repo, inputs.version, sha, refresh);
    out.verdict = verdict_of_containment(&c);
    if !matches!(c, Containment::NotChecked(_)) {
        let contained = !matches!(c, Containment::NotReleased);
        out.header = Some(HeaderPlan {
            status: contained.then_some("merged"),
            commit: Some(sha.to_string()),
            reason,
        });
    }
    out.containment = Some(c);
    out
}

/// The `From <sha>` ids of the mail(s) in a patch file.
fn mbox_shas(text: &str) -> Vec<String> {
    text.lines()
        .filter_map(|l| l.strip_prefix("From "))
        .filter_map(|rest| rest.split_whitespace().next())
        .filter(|id| id.len() == 40 && is_commit_id(id))
        .map(str::to_ascii_lowercase)
        .collect()
}

/// The `[PATCH k/n]` marks of the subjects in a patch file.
fn series_marks(text: &str) -> Vec<(u32, u32)> {
    text.lines()
        .filter(|l| l.starts_with("Subject:"))
        .filter_map(|l| {
            let tag = l.split('[').nth(1)?.split(']').next()?;
            let frac = tag.split_whitespace().find(|w| w.contains('/'))?;
            let (k, n) = frac.split_once('/')?;
            Some((k.parse().ok()?, n.parse().ok()?))
        })
        .collect()
}

/// Whether a file holds the whole series of its request: one mail (no mark),
/// or a mark for every part `1..n`.
fn holds_whole_series(text: &str) -> bool {
    let marks = series_marks(text);
    let Some(n) = marks.iter().map(|m| m.1).max() else {
        return true;
    };
    (1..=n).all(|k| marks.iter().any(|m| m.0 == k && m.1 == n))
}

/// Whether two patches have the same changed lines, as sorted multisets.
fn same_code(a: &str, b: &str) -> bool {
    let (mut x, mut y) = (code_lines(a), code_lines(b));
    x.sort();
    y.sort();
    !x.is_empty() && x == y
}

/// What an open request's head did against the carried file (plan 4.1).
fn head_moved(text: &str, head: &str, origin: &PatchOrigin, net: &dyn ForgeNet) -> Verdict {
    let shas = mbox_shas(text);
    if shas.iter().any(|s| s == head) {
        return Verdict::Nothing;
    }
    let theirs = match net.patch_text(&diff_url(origin)) {
        Ok(t) => t,
        Err(why) => return Verdict::NotChecked(why),
    };
    if same_code(text, &theirs) {
        return Verdict::Nothing;
    }
    if shas.is_empty() {
        return Verdict::MovedNotComparable("the file is not the request's format-patch".into());
    }
    if holds_whole_series(text) {
        Verdict::HeadMoved {
            head: head.to_string(),
            patch_url: diff_url(origin),
        }
    } else if code_subset(text, &theirs) {
        // A part of the request whose lines all still stand in it: unchanged.
        Verdict::Nothing
    } else {
        Verdict::MovedNotComparable("the package carries part of the request".into())
    }
}

/// Whether every changed line of `a` is a changed line of `b`, counted (a
/// sub-series of the request).
fn code_subset(a: &str, b: &str) -> bool {
    let mut have = code_lines(b);
    code_lines(a)
        .iter()
        .all(|line| match have.iter().position(|h| h == line) {
            Some(i) => {
                have.swap_remove(i);
                true
            }
            None => false,
        })
}

/// The files a patch changes, from its `+++` lines.
fn touched_files(text: &str) -> Vec<&str> {
    text.lines()
        .filter_map(|l| l.strip_prefix("+++ "))
        .map(|f| f.trim_start_matches("b/").split('\t').next().unwrap_or(""))
        .collect()
}

/// Why a file name alone is not believed (plan 1.3), or `None` when the request's
/// `.patch` confirms it: a shared commit, a shared subject, or the same code.
fn not_corroborated(origin: &PatchOrigin, text: &str, net: &dyn ForgeNet) -> Option<String> {
    let theirs = match net.patch_text(&diff_url(origin)) {
        Ok(t) => t,
        Err(why) => return Some(format!("the request's patch cannot be read: {why}")),
    };
    let shared_sha = mbox_shas(text)
        .iter()
        .any(|s| mbox_shas(&theirs).contains(s));
    let shared_subject = crate::backport::subject_of_patch(text).is_some_and(|s| {
        theirs
            .lines()
            .filter_map(|l| l.strip_prefix("Subject:"))
            .any(|t| subject_key(t) == subject_key(&s))
    });
    // A subject alone is a generic phrase too often: it also needs a file in common.
    let files = touched_files(text);
    let shared_files = touched_files(&theirs).iter().any(|f| files.contains(f));
    if shared_sha || (shared_subject && shared_files) || same_code(text, &theirs) {
        return None;
    }
    Some(format!(
        "the file name alone names {}, and the request's patch does not match the file",
        origin.url
    ))
}

/// A subject with its `[...]` tags dropped, for comparing.
fn subject_key(s: &str) -> String {
    let mut s = s.trim();
    while s.starts_with('[') {
        match s.find(']') {
            Some(i) => s = s[i + 1..].trim_start(),
            None => break,
        }
    }
    s.to_string()
}

/// The `Upstream:` claim of a patch's header (the plan's header lines), as a
/// `Claim` of `origin`; `None` when the patch has no header to write.
pub fn claim_of<'a>(p: &'a Provenance) -> Option<Claim<'a>> {
    let origin = p.origin.as_ref()?;
    let plan = p.header.as_ref()?;
    Some(Claim {
        origin,
        status: plan.status,
        commit: plan.commit.clone(),
        reason: plan.reason.clone(),
    })
}

/// `text` (the patch file) with the header lines of its provenance, if any are
/// due. `None` when nothing changes.
pub fn header_for(p: &Provenance, text: &str) -> Option<String> {
    with_header(text, &claim_of(p)?)
}

/// `spec` with the `PATCH-FIX-UPSTREAM` comment above the patch, if one is due
/// (the plan's spec comment: the reference, else the URL, and the reason).
pub fn spec_comment_for(p: &Provenance, spec: &str) -> Option<String> {
    let origin = p.origin.as_ref()?;
    p.header.as_ref()?;
    let reference = github_reference(origin).unwrap_or_else(|| origin.url.clone());
    let reason = p.header.as_ref().and_then(|h| h.reason.clone());
    spec_with_comment(spec, &p.name, &reference, reason.as_deref())
}
