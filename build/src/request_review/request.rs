//! The request document, read for the review: its actions, state, reviews and
//! description, and the findings that need only the document and the list of
//! the other open requests.

use super::{Finding, Severity, ids};
use crate::obs_xml::{attr, unescape};
use crate::osc::OpenRequest;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequestDoc {
    pub id: String,
    pub creator: String,
    pub state: RequestState,
    /// Unescaped, without carriage returns.
    pub description: String,
    pub actions: Vec<RequestAction>,
    pub reviews: Vec<RequestReview>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RequestState {
    pub name: String,
    pub who: String,
    pub when: String,
    pub created: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequestAction {
    /// `submit`, `delete`, `maintenance_incident`, ...
    pub kind: String,
    pub source: Option<ActionSource>,
    pub target: Option<ActionTarget>,
    pub sourceupdate: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActionSource {
    pub project: String,
    pub package: String,
    /// A revision number or a srcmd5. Absent when the request does not pin one.
    pub rev: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActionTarget {
    pub project: String,
    pub package: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequestReview {
    pub state: String,
    /// The user, group, project or package the review is asked of.
    pub by: String,
}

/// The text of the start tag that opens with `name` (`"<state "`), without the
/// closing `>`.
fn tag<'a>(text: &'a str, name: &str) -> Option<&'a str> {
    let rest = text.split_once(name)?.1;
    Some(&rest[..rest.find('>').unwrap_or(rest.len())])
}

/// Parse a request document. A document without a request id is an error.
pub fn parse_request(xml: &str) -> Result<RequestDoc, String> {
    let head = tag(xml, "<request ").ok_or("not a request document: no <request> element")?;
    let id = attr(head, "id").ok_or("the request document has no id")?;
    let mut actions = Vec::new();
    for chunk in xml.split("<action ").skip(1) {
        let body = chunk.split("</action>").next().unwrap_or(chunk);
        let open = &body[..body.find('>').unwrap_or(body.len())];
        let source = tag(body, "<source ").and_then(|t| {
            Some(ActionSource {
                project: attr(t, "project")?,
                package: attr(t, "package")?,
                rev: attr(t, "rev").filter(|r| !r.is_empty()),
            })
        });
        let target = tag(body, "<target ").and_then(|t| {
            Some(ActionTarget {
                project: attr(t, "project")?,
                package: attr(t, "package")?,
            })
        });
        let sourceupdate = body
            .split_once("<sourceupdate>")
            .and_then(|(_, r)| r.split_once("</sourceupdate>"))
            .map(|(v, _)| v.trim().to_string());
        actions.push(RequestAction {
            kind: attr(open, "type").unwrap_or_default(),
            source,
            target,
            sourceupdate,
        });
    }
    // Everything after the last action: the state, the reviews, the description.
    let tail = xml.rsplit("</action>").next().unwrap_or(xml);
    let state = tag(tail, "<state ").map_or_else(RequestState::default, |t| RequestState {
        name: attr(t, "name").unwrap_or_default(),
        who: attr(t, "who").unwrap_or_default(),
        when: attr(t, "when").unwrap_or_default(),
        created: attr(t, "created").unwrap_or_default(),
    });
    let reviews = tail
        .split("<review ")
        .skip(1)
        .map(|r| {
            let t = &r[..r.find('>').unwrap_or(r.len())];
            let by = ["by_user", "by_group", "by_project", "by_package"]
                .iter()
                .find_map(|k| attr(t, k))
                .unwrap_or_default();
            RequestReview {
                state: attr(t, "state").unwrap_or_default(),
                by,
            }
        })
        .collect();
    let description = tail
        .split_once("<description>")
        .and_then(|(_, r)| r.split_once("</description>"))
        .map(|(d, _)| unescape(d).replace('\r', ""))
        .unwrap_or_default();
    Ok(RequestDoc {
        creator: attr(head, "creator").unwrap_or_default(),
        id,
        state,
        description,
        actions,
        reviews,
    })
}

impl RequestDoc {
    /// The submit actions that name a source and a target.
    pub fn submit_actions(&self) -> impl Iterator<Item = (&ActionSource, &ActionTarget)> {
        self.actions
            .iter()
            .filter(|a| a.kind == "submit")
            .filter_map(|a| Some((a.source.as_ref()?, a.target.as_ref()?)))
    }
}

/// The states a request does not leave.
const FINAL_STATES: [&str; 4] = ["accepted", "declined", "revoked", "superseded"];

/// Whether an action of this type hands out rights or removes or redirects a
/// package: the request does more than submit sources.
fn is_dangerous(kind: &str) -> bool {
    matches!(
        kind,
        "add_role" | "set_bugowner" | "delete" | "change_devel"
    ) || kind.starts_with("maintenance")
}

/// The findings that need only the request document.
pub fn request_findings(doc: &RequestDoc) -> Vec<Finding> {
    let mut out = Vec::new();
    if FINAL_STATES.contains(&doc.state.name.as_str()) {
        out.push(Finding::check(
            ids::REQUEST_FINAL_STATE,
            Severity::Note,
            "",
            &format!(
                "the request is {} (by {}); it is reviewed as far as the source can be read",
                doc.state.name, doc.state.who
            ),
        ));
    }
    for action in &doc.actions {
        let package = action.target.as_ref().map_or("", |t| t.package.as_str());
        if action.kind != "submit" {
            // Only submit actions are reviewed: any other one is something the
            // reader must look at, and one that changes who may write to a
            // package, removes it or redirects its devel project stops it.
            let severity = if is_dangerous(&action.kind) {
                Severity::Blocker
            } else {
                Severity::Warning
            };
            out.push(Finding::check(
                ids::REQUEST_ACTION_NOT_REVIEWED,
                severity,
                package,
                &format!("a {} action is not reviewed by this command", action.kind),
            ));
        } else if action.source.is_none() || action.target.is_none() {
            out.push(Finding::check(
                ids::REQUEST_ACTION_MALFORMED,
                Severity::Blocker,
                package,
                "a submit action does not name both a source and a target package: it is not reviewed",
            ));
        } else if let Some(source) = action.source.as_ref().filter(|s| s.rev.is_none()) {
            out.push(Finding::check(
                ids::REQUEST_NO_SOURCE_REV,
                Severity::Warning,
                package,
                &format!(
                    "the action names no revision of {}/{}: the source can change under the request",
                    source.project, source.package
                ),
            ));
        }
    }
    out
}

/// The other open submit requests to `package` of the same target, as
/// warnings. `open` is the target's collection, which holds the reviewed
/// request itself too.
pub fn competing_findings(open: &[OpenRequest], self_id: &str, package: &str) -> Vec<Finding> {
    open.iter()
        .filter(|r| r.id != self_id && r.target_package == package)
        .map(|r| {
            Finding::check(
                ids::REQUEST_COMPETING,
                Severity::Warning,
                package,
                &format!(
                    "request {} ({}) also submits {}/{} to {}/{}",
                    r.id,
                    r.state,
                    r.source_project,
                    r.source_package,
                    r.target_project,
                    r.target_package
                ),
            )
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const REAL: &str = include_str!("../testdata/request_review/request_1394662.xml");
    const MULTI: &str = include_str!("../testdata/request_review/request_multi.xml");
    const ACCEPTED: &str = include_str!("../testdata/request_review/request_accepted.xml");
    const COLLECTION: &str = include_str!("../testdata/request_review/collection_competing.xml");

    #[test]
    fn the_real_request_has_one_submit_action_with_a_revision_number() {
        let doc = parse_request(REAL).unwrap();
        assert_eq!(doc.id, "1394662");
        assert_eq!(doc.creator, "dspinella");
        assert_eq!(doc.state.name, "new");
        assert_eq!(doc.state.created, "2026-11-03T11:14:44");
        let (s, t) = doc.submit_actions().next().unwrap();
        assert_eq!(s.package, "cracklib");
        assert_eq!(s.rev.as_deref(), Some("3"));
        assert_eq!(t.project, "Base:System");
        assert_eq!(doc.actions[0].sourceupdate.as_deref(), Some("cleanup"));
        assert!(!doc.description.contains('\r'));
        assert!(doc.description.starts_with("- Update to 2.10.4\n"));
        assert!(request_findings(&doc).is_empty());
    }

    #[test]
    fn a_request_with_a_delete_gives_a_blocker_for_it_and_keeps_the_submit() {
        let doc = parse_request(MULTI).unwrap();
        assert_eq!(doc.submit_actions().count(), 1);
        assert_eq!(doc.description, "Two actions & a delete");
        assert_eq!(doc.reviews.len(), 2);
        assert_eq!(doc.reviews[0].by, "factory-staging");
        let found = request_findings(&doc);
        let seen: Vec<_> = found.iter().map(|f| (f.id, f.severity)).collect();
        assert_eq!(
            seen,
            [(ids::REQUEST_ACTION_NOT_REVIEWED, Severity::Blocker)]
        );
    }

    fn with_actions(actions: &str) -> RequestDoc {
        parse_request(&format!(
            "<request id=\"9\" creator=\"x\">{actions}<state name=\"new\" who=\"x\"/></request>"
        ))
        .unwrap()
    }

    const CLEAN: &str = "<action type=\"submit\"><source project=\"a\" package=\"p\" rev=\"1\"/>\
                         <target project=\"b\" package=\"p\"/></action>";

    #[test]
    fn a_clean_submit_with_a_role_or_devel_change_beside_it_is_not_clean() {
        for kind in [
            "add_role",
            "set_bugowner",
            "delete",
            "change_devel",
            "maintenance_incident",
        ] {
            let doc = with_actions(&format!(
                "{CLEAN}<action type=\"{kind}\"><target project=\"b\" package=\"p\"/></action>"
            ));
            let found = request_findings(&doc);
            assert_eq!(found.len(), 1, "{kind}");
            assert_eq!(found[0].severity, Severity::Blocker, "{kind}");
        }
        let doc = with_actions(&format!(
            "{CLEAN}<action type=\"release\"><target project=\"b\" package=\"p\"/></action>"
        ));
        let found = request_findings(&doc);
        assert_eq!(found[0].id, ids::REQUEST_ACTION_NOT_REVIEWED);
        assert_eq!(found[0].severity, Severity::Warning);
    }

    #[test]
    fn a_submit_without_a_source_or_a_target_is_a_malformed_blocker() {
        for action in [
            "<action type=\"submit\"><target project=\"b\" package=\"p\"/></action>",
            "<action type=\"submit\"><source project=\"a\" package=\"p\"/></action>",
            "<action type=\"submit\"><source project=\"a\"/><target project=\"b\" package=\"p\"/></action>",
        ] {
            let doc = with_actions(&format!("{CLEAN}{action}"));
            assert_eq!(doc.submit_actions().count(), 1);
            let found = request_findings(&doc);
            assert_eq!(found.len(), 1, "{action}");
            assert_eq!(
                (found[0].id, found[0].severity),
                (ids::REQUEST_ACTION_MALFORMED, Severity::Blocker)
            );
        }
    }

    #[test]
    fn an_accepted_request_without_a_revision_gives_two_findings() {
        let doc = parse_request(ACCEPTED).unwrap();
        let found = request_findings(&doc);
        let ids: Vec<_> = found.iter().map(|f| (f.id, f.severity)).collect();
        assert_eq!(
            ids,
            [
                (ids::REQUEST_FINAL_STATE, Severity::Note),
                (ids::REQUEST_NO_SOURCE_REV, Severity::Warning),
            ]
        );
        assert_eq!(found[1].package, "foo");
    }

    #[test]
    fn a_document_that_is_not_a_request_is_an_error() {
        assert!(parse_request("garbage").is_err());
        assert!(parse_request("<request creator=\"a\"></request>").is_err());
    }

    #[test]
    fn the_request_itself_is_not_a_competing_one() {
        let open = crate::osc::parse_open_requests(COLLECTION);
        assert_eq!(open.len(), 2);
        let found = competing_findings(&open, "1394662", "cracklib");
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].id, ids::REQUEST_COMPETING);
        assert!(found[0].message.contains("1394700"));
        assert!(competing_findings(&open, "1394662", "other").is_empty());
    }
}
