//! The delete request behind a `_link` whose target OBS no longer has: where
//! the link points, and the accepted request that removed the target. Reads
//! only (`osc api` GET).

use super::*;
use crate::osc::query::*;
use quick_xml::Reader;
use quick_xml::events::{BytesStart, Event};

/// An accepted request that deleted a package.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeleteRequest {
    pub id: String,
    /// The day the request was accepted, `YYYY-MM-DD`.
    pub accepted: String,
    /// The request's own description, empty when it has none.
    pub description: String,
    /// The `(project, package)` each delete action names.
    pub targets: Vec<(String, String)>,
}

/// The `(project, package)` a `<linkinfo>` in a `?expand=0` listing names, for
/// the package `pkg`. The package is left out when it has the same name.
pub fn parse_linkinfo(listing: &str, pkg: &str) -> Option<(String, String)> {
    let (_, rest) = listing.split_once("<linkinfo ")?;
    let tag = &rest[..rest.find('>').unwrap_or(rest.len())];
    let project = crate::obs_xml::attr(tag, "project").filter(|p| !p.is_empty())?;
    let package = crate::obs_xml::attr(tag, "package")
        .filter(|p| !p.is_empty())
        .unwrap_or_else(|| pkg.to_string());
    Some((project, package))
}

/// The query for the accepted requests that delete `project/package`. `None`
/// when a name is not a valid OBS name: the names are written into the XPath,
/// and a quote in one would change the query.
pub fn delete_requests_path(project: &str, package: &str) -> Option<String> {
    if !crate::osc::is_valid_obs_name(project) || !crate::osc::is_valid_obs_name(package) {
        return None;
    }
    let xpath = format!(
        "(action/@type='delete' and state/@name='accepted' and \
         action/target/@project='{project}' and action/target/@package='{package}')"
    );
    Some(format!("/search/request?match={}", enc(&xpath)))
}

fn attr_value(e: &BytesStart, key: &str) -> Option<String> {
    e.attributes()
        .flatten()
        .find(|a| a.key.as_ref() == key)
        .and_then(|a| {
            a.normalized_value(quick_xml::XmlVersion::Implicit1_0)
                .ok()
                .map(|v| v.into_owned())
        })
}

/// The accepted requests in a `/search/request` collection. The description
/// is the request's own: the `<description>` directly under `<request>`, not
/// those of its reviews and their histories.
pub fn parse_accepted_deletes(xml: &str) -> Vec<DeleteRequest> {
    let mut out = Vec::new();
    let mut reader = Reader::from_str(xml);
    // The open elements, outermost first: collection, request, and so on.
    let mut open: Vec<String> = Vec::new();
    let mut req: Option<DeleteRequest> = None;
    let mut accepted = false;
    let mut action_is_delete = false;
    let mut in_description = false;
    let mut text = String::new();
    loop {
        let (e, is_start) = match reader.read_event() {
            Ok(Event::Start(e)) => (e, true),
            Ok(Event::Empty(e)) => (e, false),
            Ok(Event::Text(t)) => {
                if in_description {
                    text.push_str(&t);
                }
                continue;
            }
            // A character reference (`&gt;`) is an event of its own, so the
            // text of a description is put together from both.
            Ok(Event::GeneralRef(r)) => {
                if in_description {
                    text.push_str(&crate::obs_xml::unescape(&format!("&{};", &*r)));
                }
                continue;
            }
            Ok(Event::End(e)) => {
                let qname = e.name();
                let name: &str = qname.as_ref();
                let name = name.to_string();
                let depth = open.len();
                if name == "description" && depth == 3 && in_description {
                    in_description = false;
                    if let Some(r) = req.as_mut() {
                        r.description = text.trim().to_string();
                    }
                }
                if name == "request" && depth == 2 {
                    if let Some(r) = req.take().filter(|_| accepted) {
                        out.push(r);
                    }
                }
                open.pop();
                continue;
            }
            // A CDATA section is character data as written: nothing to decode.
            Ok(Event::CData(t)) => {
                if in_description {
                    text.push_str(&t);
                }
                continue;
            }
            Ok(Event::Eof) | Err(_) => break,
            Ok(_) => continue,
        };
        let qname = e.name();
        let name: &str = qname.as_ref();
        let name = name.to_string();
        // The depth of this element: 2 for a request, 3 for its children, 4
        // for the target of an action.
        match (name.as_str(), open.len() + 1) {
            ("request", 2) => {
                req = attr_value(&e, "id").map(|id| DeleteRequest {
                    id,
                    accepted: String::new(),
                    description: String::new(),
                    targets: Vec::new(),
                });
                accepted = false;
            }
            ("action", 3) => {
                action_is_delete = attr_value(&e, "type").as_deref() == Some("delete");
            }
            ("target", 4) if action_is_delete => {
                if let (Some(r), Some(p), Some(pkg)) = (
                    req.as_mut(),
                    attr_value(&e, "project"),
                    attr_value(&e, "package"),
                ) {
                    r.targets.push((p, pkg));
                }
            }
            ("state", 3) => {
                accepted = attr_value(&e, "name").as_deref() == Some("accepted");
                if let (true, Some(r), Some(when)) =
                    (accepted, req.as_mut(), attr_value(&e, "when"))
                {
                    r.accepted = when.split('T').next().unwrap_or("").to_string();
                }
            }
            ("description", 3) if is_start => {
                in_description = true;
                text.clear();
            }
            _ => {}
        }
        if is_start {
            open.push(name);
        }
    }
    out
}

/// The accepted request that deleted `project/package`, the latest one when
/// there are several, from the answer to [`delete_requests_path`].
pub fn accepted_delete_request(xml: &str, project: &str, package: &str) -> Option<DeleteRequest> {
    parse_accepted_deletes(xml)
        .into_iter()
        .filter(|r| r.targets.iter().any(|(p, n)| p == project && n == package))
        .max_by(|a, b| a.accepted.cmp(&b.accepted).then(a.id.cmp(&b.id)))
}

/// What the summary says about a `_link` that cannot be expanded. `get` reads
/// an `osc api` path and returns its text. `fallback` is the reason when the
/// link cannot be read, so the package is still reported.
///
/// Only a link whose `<linkinfo>` error says the target does not exist gets
/// the delete request search; any other error (a conflict, say) is reported as
/// it is.
pub fn broken_link_report_with(
    project: &str,
    pkg: &str,
    fallback: &str,
    get: &dyn Fn(&str) -> Result<String, String>,
) -> String {
    let Ok(listing) = get(&format!("/source/{project}/{}?expand=0", enc(pkg))) else {
        return fallback.to_string();
    };
    let Some((tp, tpkg)) = parse_linkinfo(&listing, pkg) else {
        return fallback.to_string();
    };
    let Some(error) = crate::osc::branch::rebase::linkinfo_error(&listing) else {
        return fallback.to_string();
    };
    let link = format!("{project}/{pkg} links to {tp}/{tpkg}");
    if !error.contains("does not exist") {
        return format!(
            "{link}, which cannot be expanded: {error}. The maintainer must relink or \
             unlink it."
        );
    }
    let Some(search) = delete_requests_path(&tp, &tpkg) else {
        return fallback.to_string();
    };
    let found = match get(&search) {
        Ok(xml) => accepted_delete_request(&xml, &tp, &tpkg),
        Err(e) => {
            return format!(
                "{link}, which does not exist; its delete requests could not be read ({e})"
            );
        }
    };
    match found {
        Some(r) => {
            let about = if r.description.is_empty() {
                String::new()
            } else {
                format!(": \"{}\"", r.description.replace('\n', " "))
            };
            format!(
                "{link}, deleted from {tp} by request #{} ({}){about}. The maintainer can \
                 delete the devel package, or unlink it (replace the link with files) if it \
                 stays for the devel project's own users.",
                r.id, r.accepted
            )
        }
        None => format!(
            "{link}, which does not exist; no accepted delete request for {tp}/{tpkg} was \
             found. The maintainer must relink or unlink it."
        ),
    }
}

/// [`broken_link_report_with`] reading OBS through `osc api`.
pub fn broken_link_report(project: &str, pkg: &str, fallback: &str, emit: Emit<'_>) -> String {
    broken_link_report_with(project, pkg, fallback, &|path| {
        api_get(path, emit).map_err(|e| e.to_string())
    })
}
