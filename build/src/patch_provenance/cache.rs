//! Forge answers kept between runs (plan 3.4), through `upstream_cache`: the
//! state of a request (an open one for an hour, a merged one for 30 days) and
//! whether a release contains a commit (yes for a year, no for 30 days).
//! Without a cache directory every call is asked again.

use std::time::Duration;

use super::status::{Containment, ForgeNet, PrState, PrStatus, containment_of, state_url};
use super::{PatchOrigin, parse_state};
use crate::upstream_cache::{fresh, lookup_with_ttl};

const HOUR: Duration = Duration::from_secs(60 * 60);
const DAY: Duration = Duration::from_secs(24 * 60 * 60);
const MERGED_TTL: Duration = Duration::from_secs(30 * DAY.as_secs());
const CONTAINS_YES_TTL: Duration = Duration::from_secs(365 * DAY.as_secs());
const CONTAINS_NO_TTL: Duration = Duration::from_secs(30 * DAY.as_secs());

/// The state of a request, from the cache when it is kept, else from the
/// forge. A merged state is also kept under its own key, which is read first.
pub fn state_cached(
    net: &dyn ForgeNet,
    origin: &PatchOrigin,
    refresh: bool,
) -> Result<PrStatus, String> {
    let url = state_url(origin).ok_or("a commit has no state call")?;
    let final_key = format!("pr-final:{}", origin.url);
    let key = format!("pr:{}", origin.url);
    if !refresh && let Some(status) = fresh(&final_key).and_then(|t| serde_json::from_str(&t).ok())
    {
        return Ok(status);
    }
    let text = lookup_with_ttl(&key, HOUR, refresh, || {
        let json = net.api_json(&url)?;
        let status = parse_state(origin, &json)?;
        serde_json::to_string(&status).map_err(|e| e.to_string())
    })?;
    let status: PrStatus = serde_json::from_str(&text)
        .map_err(|e| format!("the kept answer is not a request state: {e}"))?;
    if status.state == PrState::Merged {
        let kept = serde_json::to_string(&status).map_err(|e| e.to_string())?;
        let _ = lookup_with_ttl(&final_key, MERGED_TTL, true, || Ok(kept.clone()));
    }
    Ok(status)
}

/// [`containment_of`], kept: the answer is read from the cache first, and
/// written after a check (not a failed one).
pub fn containment_cached(
    net: &dyn ForgeNet,
    origin: &PatchOrigin,
    spec_repo: &str,
    version: &str,
    sha: &str,
    refresh: bool,
) -> Containment {
    let base = format!("contains:{}:{version}:{sha}", origin.project);
    let (yes, no) = (format!("{base}:yes"), format!("{base}:no"));
    if !refresh {
        let kept = [&yes, &no].iter().find_map(|k| fresh(k));
        if let Some(c) = kept.as_deref().and_then(decode) {
            return c;
        }
    }
    let got = containment_of(net, origin, spec_repo, version, sha);
    match &got {
        Containment::NotChecked(_) => {}
        Containment::NotReleased => {
            let _ = lookup_with_ttl(&no, CONTAINS_NO_TTL, true, || Ok(encode(&got)));
        }
        _ => {
            let _ = lookup_with_ttl(&yes, CONTAINS_YES_TTL, true, || Ok(encode(&got)));
        }
    }
    got
}

fn encode(c: &Containment) -> String {
    match c {
        Containment::InPackaged { tag } => format!("packaged\t{tag}"),
        Containment::InNewer { tag, version } => format!("newer\t{tag}\t{version}"),
        Containment::NotReleased => "none".to_string(),
        Containment::NotChecked(_) => String::new(),
    }
}

fn decode(text: &str) -> Option<Containment> {
    let mut parts = text.split('\t');
    match parts.next()? {
        "packaged" => Some(Containment::InPackaged {
            tag: parts.next()?.to_string(),
        }),
        "newer" => Some(Containment::InNewer {
            tag: parts.next()?.to_string(),
            version: parts.next()?.to_string(),
        }),
        "none" => Some(Containment::NotReleased),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_kept_containment_round_trips() {
        for c in [
            Containment::InPackaged { tag: "v1.2".into() },
            Containment::InNewer {
                tag: "v1.3".into(),
                version: "1.3".into(),
            },
            Containment::NotReleased,
        ] {
            assert_eq!(decode(&encode(&c)), Some(c));
        }
        assert_eq!(decode("garbage"), None);
    }
}
