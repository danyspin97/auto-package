use super::*;

/// The dotted-number version in a `redistrib_X.Y.Z.json` name.
fn manifest_version(name: &str) -> Option<&str> {
    let v = name.strip_prefix("redistrib_")?.strip_suffix(".json")?;
    v.split('.')
        .all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()))
        .then_some(v)
}

/// The highest `redistrib_X.Y.Z.json` named in a directory listing.
pub fn latest_manifest_name(index_html: &str) -> Option<String> {
    index_html
        .split("redistrib_")
        .skip(1)
        .filter_map(|rest| {
            let end = rest.find(".json")? + ".json".len();
            let name = format!("redistrib_{}", &rest[..end]);
            manifest_version(&name)?;
            Some(name)
        })
        .max_by(|a, b| {
            crate::version::sort_cmp(
                manifest_version(a).unwrap_or_default(),
                manifest_version(b).unwrap_or_default(),
            )
        })
}

pub(super) fn get(url: &str) -> Result<String, Error> {
    let err = |e| Error::Http {
        url: url.to_string(),
        source: Box::new(e),
    };
    let agent = crate::net::agent(ureq::Agent::config_builder().timeout_global(Some(TIMEOUT)));
    crate::net::call_with_retries(|| agent.get(url))
        .map_err(err)?
        .into_body()
        .with_config()
        .limit(MAX_MANIFEST_BYTES)
        .read_to_string()
        .map_err(err)
}

/// Fetch a manifest: the pinned `version` (`"13.4.3"`), else the latest listed.
pub fn fetch_manifest(product: Product, version: Option<&str>) -> Result<Manifest, Error> {
    let base = product.base_url();
    let name = match version {
        Some(v) => format!("redistrib_{v}.json"),
        None => latest_manifest_name(&get(&format!("{base}/"))?)
            .ok_or_else(|| Error::NoManifest(base.to_string()))?,
    };
    parse(&get(&format!("{base}/{name}"))?, base)
}
