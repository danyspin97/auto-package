use super::*;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Manifest {
    pub product: String,
    pub release_label: String,
    pub base_url: String,
    pub components: Vec<Component>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Component {
    /// The JSON key, e.g. `cuda_cudart`.
    pub key: String,
    pub name: String,
    pub version: String,
    pub license: String,
    pub license_path: String,
    pub archives: Vec<Archive>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Archive {
    pub platform: String,
    /// CUDA major (`"12"`) for the nested per-CUDA shape, else `None`.
    pub cuda_variant: Option<String>,
    pub relative_path: String,
    pub sha256: String,
    pub size: u64,
}

impl Component {
    /// The archive for `platform`. With `cuda_major` (`"13"` or `"cuda13"`)
    /// only that variant matches; without it a non-variant archive is
    /// preferred, else the newest variant.
    pub fn archive_for(&self, platform: &str, cuda_major: Option<&str>) -> Option<&Archive> {
        let mut it = self.archives.iter().filter(|a| a.platform == platform);
        match cuda_major {
            Some(want) => {
                let want = want.strip_prefix("cuda").unwrap_or(want);
                it.find(|a| a.cuda_variant.as_deref() == Some(want))
            }
            None => {
                let all: Vec<&Archive> = it.collect();
                all.iter()
                    .find(|a| a.cuda_variant.is_none())
                    .or_else(|| {
                        all.iter().max_by_key(|a| {
                            a.cuda_variant
                                .as_deref()
                                .and_then(|v| v.parse::<u64>().ok())
                        })
                    })
                    .copied()
            }
        }
    }
}

impl Archive {
    pub fn url(&self, base: &str) -> String {
        format!("{}/{}", base.trim_end_matches('/'), self.relative_path)
    }
}

fn str_field(obj: &Value, key: &str) -> Option<String> {
    obj.get(key)?.as_str().map(str::to_string)
}

/// A `relative_path` that can be joined onto a base URL: non-empty `/`-separated
/// segments, none of them `.` or `..`, no leading `/` or drive letter, no
/// scheme (`://`) and no backslashes.
fn valid_relative_path(p: &str) -> bool {
    !p.is_empty()
        && !p.starts_with('/')
        && !p.contains("://")
        && !p.contains('\\')
        && p.as_bytes().get(1) != Some(&b':')
        && p.split('/')
            .all(|seg| !seg.is_empty() && seg != "." && seg != "..")
}

fn parse_archive(
    v: &Value,
    platform: &str,
    variant: Option<&str>,
    ctx: &str,
) -> Result<Archive, Error> {
    let bad = |f: &str| Error::Field(format!("{ctx}.{platform}.{f}"));
    let size = match v.get("size") {
        Some(Value::String(s)) => s.parse().map_err(|_| bad("size"))?,
        Some(Value::Number(n)) => n.as_u64().ok_or_else(|| bad("size"))?,
        _ => return Err(bad("size")),
    };
    let relative_path = str_field(v, "relative_path")
        .filter(|p| valid_relative_path(p))
        .ok_or_else(|| bad("relative_path"))?;
    Ok(Archive {
        platform: platform.to_string(),
        cuda_variant: variant.map(|v| v.strip_prefix("cuda").unwrap_or(v).to_string()),
        relative_path,
        sha256: str_field(v, "sha256").ok_or_else(|| bad("sha256"))?,
        size,
    })
}

/// Push the archive `v` describes onto `archives`. A malformed entry is
/// skipped with a warning, so one bad platform does not fail the whole
/// manifest; a lookup for that platform then finds no archive.
fn keep_archive(
    archives: &mut Vec<Archive>,
    v: &Value,
    platform: &str,
    variant: Option<&str>,
    component: &str,
) {
    match parse_archive(v, platform, variant, component) {
        Ok(a) => archives.push(a),
        Err(e) => {
            let variant = variant.map(|v| format!(" (CUDA {v})")).unwrap_or_default();
            tracing::warn!("skipping {component} {platform}{variant} in NVIDIA's manifest: {e}");
        }
    }
}

/// Parse a manifest. `base_url` is where `relative_path`s are rooted.
pub fn parse(json: &str, base_url: &str) -> Result<Manifest, Error> {
    let root: Value = serde_json::from_str(json)?;
    let obj = root
        .as_object()
        .ok_or_else(|| Error::Field("<root>".into()))?;
    let mut components = Vec::new();
    for (key, comp) in obj {
        // Components are the objects that carry a name and a version.
        let (Some(name), Some(version)) = (str_field(comp, "name"), str_field(comp, "version"))
        else {
            continue;
        };
        let mut archives = Vec::new();
        for (platform, entry) in comp.as_object().into_iter().flatten() {
            if !entry.is_object() {
                continue;
            }
            if entry.get("relative_path").is_some() {
                keep_archive(&mut archives, entry, platform, None, key);
            } else {
                for (variant, inner) in entry.as_object().into_iter().flatten() {
                    if inner.get("relative_path").is_some() {
                        keep_archive(&mut archives, inner, platform, Some(variant), key);
                    }
                }
            }
        }
        components.push(Component {
            key: key.clone(),
            name,
            version,
            license: str_field(comp, "license").unwrap_or_default(),
            license_path: str_field(comp, "license_path").unwrap_or_default(),
            archives,
        });
    }
    Ok(Manifest {
        product: str_field(&root, "release_product")
            .ok_or_else(|| Error::Field("release_product".into()))?,
        release_label: str_field(&root, "release_label")
            .ok_or_else(|| Error::Field("release_label".into()))?,
        base_url: base_url.trim_end_matches('/').to_string(),
        components,
    })
}
