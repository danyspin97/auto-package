//! A read-only fetch of a CVE record and its fix references. Neither host is
//! allowed by default: the user allows them (`hosts allow`, `--allow-host`),
//! and only then are they asked. `www.suse.com` is never read.

use serde_json::Value;

use crate::allowed_hosts::{AllowedHosts, Asker, Use};
use crate::backport;
use crate::bugfix::{BugText, GetError};
use crate::maintenance::is_cve;
use crate::patch_check::truncate;

/// The CVE JSON 5 service of the CVE programme.
pub const MITRE_HOST: &str = "cveawg.mitre.org";
/// The OSV database: its records carry the fixing commits.
pub const OSV_HOST: &str = "api.osv.dev";

/// The longest description kept.
const MAX_DESCRIPTION_BYTES: usize = 8 * 1024;

/// The URL a record of `cve` is read from on `host`.
pub fn cve_url(host: &str, cve: &str) -> String {
    match host {
        OSV_HOST => format!("https://{OSV_HOST}/v1/vulns/{cve}"),
        _ => format!("https://{MITRE_HOST}/api/cve/{cve}"),
    }
}

fn strings(v: Option<&Value>, key: &str) -> Vec<String> {
    v.and_then(|v| v.get(key))
        .and_then(Value::as_str)
        .map(str::to_string)
        .into_iter()
        .collect()
}

fn push_unique(out: &mut Vec<String>, url: &str) {
    if !url.is_empty() && !out.iter().any(|u| u == url) {
        out.push(url.to_string());
    }
}

/// The record of the CVE programme (CVE JSON 5): description and references.
pub fn parse_mitre(json: &str) -> Result<BugText, String> {
    let v: Value = serde_json::from_str(json).map_err(|e| format!("not JSON: {e}"))?;
    let cna = v
        .pointer("/containers/cna")
        .ok_or("no containers.cna in the record")?;
    let description = cna
        .get("descriptions")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .find(|d| {
            d.get("lang")
                .and_then(Value::as_str)
                .is_some_and(|l| l.starts_with("en"))
        })
        .and_then(|d| d.get("value").and_then(Value::as_str))
        .unwrap_or_default();
    let mut urls = Vec::new();
    let adp = v
        .pointer("/containers/adp")
        .and_then(Value::as_array)
        .into_iter()
        .flatten();
    for container in std::iter::once(cna).chain(adp) {
        for r in container
            .get("references")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            push_unique(
                &mut urls,
                r.get("url").and_then(Value::as_str).unwrap_or_default(),
            );
        }
    }
    Ok(BugText {
        source: MITRE_HOST.to_string(),
        summary: description.lines().next().unwrap_or_default().to_string(),
        urls,
        comments: vec![truncate(description, MAX_DESCRIPTION_BYTES)],
        ..Default::default()
    })
}

/// The record of OSV: summary, details, every reference and the commit of
/// each `GIT` range's `fixed` event as the patch URL its forge serves.
pub fn parse_osv(json: &str) -> Result<BugText, String> {
    let v: Value = serde_json::from_str(json).map_err(|e| format!("not JSON: {e}"))?;
    if v.get("id").is_none() {
        return Err("no id in the record".to_string());
    }
    let mut urls = Vec::new();
    // Fixing commits first: they are what the lookup wants.
    for a in v
        .get("affected")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        for r in a
            .get("ranges")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            if r.get("type").and_then(Value::as_str) != Some("GIT") {
                continue;
            }
            let Some(repo) = r.get("repo").and_then(Value::as_str) else {
                continue;
            };
            for e in r
                .get("events")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                if let Some(sha) = e.get("fixed").and_then(Value::as_str)
                    && backport::is_commit_id(sha)
                    && let Ok(url) = backport::commit_patch_url(repo, sha)
                {
                    push_unique(&mut urls, &url);
                }
            }
        }
    }
    let refs = v.get("references").and_then(Value::as_array);
    for kind_fix in [true, false] {
        for r in refs.into_iter().flatten() {
            let is_fix = r.get("type").and_then(Value::as_str) == Some("FIX");
            if is_fix == kind_fix {
                push_unique(
                    &mut urls,
                    r.get("url").and_then(Value::as_str).unwrap_or_default(),
                );
            }
        }
    }
    let summary = strings(Some(&v), "summary").pop().unwrap_or_default();
    let details = strings(Some(&v), "details").pop().unwrap_or_default();
    Ok(BugText {
        source: OSV_HOST.to_string(),
        summary,
        urls,
        comments: vec![truncate(&details, MAX_DESCRIPTION_BYTES)],
        ..Default::default()
    })
}

/// What reading one CVE on one host gave.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CveRead {
    Read(BugText),
    NotRead { host: String, why: String },
}

/// Ask each source of a CVE the user allowed. A host that is not allowed is
/// recorded as refused (`pkg` asked) and reported as not read.
pub fn read_cve(
    pkg: &str,
    cve: &str,
    hosts: &AllowedHosts,
    get: &dyn Fn(&str) -> Result<String, GetError>,
) -> Vec<CveRead> {
    if !is_cve(cve) {
        return vec![CveRead::NotRead {
            host: String::new(),
            why: format!("{cve} is not a CVE"),
        }];
    }
    let asker = Asker {
        package: pkg.to_string(),
        what: "CVE lookup".to_string(),
    };
    let mut out = Vec::new();
    for host in [OSV_HOST, MITRE_HOST] {
        let not_read = |why: String| CveRead::NotRead {
            host: host.to_string(),
            why,
        };
        if hosts.check_host(host, Use::Source, &asker).is_err() {
            out.push(not_read(format!(
                "{host} is not an allowed host (auto-package hosts allow {host}, or --allow-host {host})"
            )));
            continue;
        }
        let parsed = match get(&cve_url(host, cve)) {
            Ok(body) if host == OSV_HOST => parse_osv(&body),
            Ok(body) => parse_mitre(&body),
            Err(GetError::Status(404)) => Err(format!("{cve} is not known there")),
            Err(GetError::Status(code)) => Err(format!("answered {code}")),
            Err(GetError::Other(e)) => Err(e),
        };
        out.push(match parsed {
            Ok(mut t) => {
                t.source = format!("{host} {cve}");
                CveRead::Read(t)
            }
            Err(e) => not_read(format!("{cve}: {e}")),
        });
    }
    out
}
