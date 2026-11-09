//! What a package's new release will require, read from upstream's metadata
//! before the package is updated.
//!
//! `--update-deps` decides which dependencies to branch first when a package
//! is prepared, and the spec at that point is the one before the update: a
//! bound the new release adds is not in it. For a PyPI source the release's
//! `Requires-Dist` lines say what it will require, so they are read too and
//! judged with [`dependency_need`] as the spec's bounds are.
//!
//! The prediction only re-judges a dependency the spec already names (the
//! caller finds those from the spec's `BuildRequires`): a runtime requirement
//! the build does not install is not a reason to branch anything. A
//! dependency is matched by its source package name, compared with
//! [`fuzzy_key`] against the name [`rpm_name`] gives the PyPI name.

use super::*;
use crate::distro_bounds::{marker_applies, parse_requirement};
use crate::osc::rpm_name;
use crate::spec::{Bound, Requirement};
use crate::update::Ecosystem;

/// What upstream's metadata says a package's new release requires.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Prediction {
    /// The release's requirements, for the version named.
    Requirements {
        version: String,
        requirements: Vec<PredictedRequirement>,
    },
    /// The source has no metadata to predict from (a forge, an index): the
    /// spec decides alone.
    Unknown,
    /// The metadata could not be fetched or read, for the reason given: the
    /// spec decides alone.
    Failed(String),
}

/// One version clause of a requirement of the new release.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PredictedRequirement {
    /// The name as the release writes it (`ruamel.yaml`).
    pub pypi_name: String,
    /// `python3-<name>` and the bound, as a spec would require it.
    pub requirement: Requirement,
}

/// What the release `new_version` of a package from `ecosystem` requires.
/// `fetch` gets the PyPI name and the version and returns the release's JSON,
/// `None` when PyPI does not have it; it is only called for a PyPI source.
pub fn predicted_requirements(
    ecosystem: &Ecosystem,
    new_version: &str,
    fetch: impl FnOnce(&str, &str) -> Result<Option<String>, String>,
) -> Prediction {
    let Ecosystem::Pypi { pypi_name } = ecosystem else {
        return Prediction::Unknown;
    };
    let json = match fetch(pypi_name, new_version) {
        Ok(Some(json)) => json,
        Ok(None) => {
            return Prediction::Failed(format!("PyPI has no release {new_version} of {pypi_name}"));
        }
        Err(e) => return Prediction::Failed(e),
    };
    match requirements_from_json(&json, new_version) {
        Ok(requirements) => Prediction::Requirements {
            version: new_version.to_string(),
            requirements,
        },
        Err(why) => Prediction::Failed(format!(
            "PyPI's metadata of {pypi_name} {new_version}: {why}"
        )),
    }
}

/// The requirements PyPI's JSON for the release `version` lists. An error
/// when the JSON is not PyPI's, describes another release, has no
/// `Requires-Dist` (`null`, which PyPI also gives a release that has
/// requirements) or holds a line that cannot be read, since then the list may
/// miss a bound.
pub fn requirements_from_json(
    json: &str,
    version: &str,
) -> Result<Vec<PredictedRequirement>, String> {
    #[derive(serde::Deserialize)]
    struct Envelope {
        info: Info,
    }
    #[derive(serde::Deserialize)]
    struct Info {
        version: String,
        #[serde(default)]
        requires_dist: Option<Vec<String>>,
    }
    let envelope: Envelope =
        serde_json::from_str(json).map_err(|e| format!("not PyPI's JSON: {e}"))?;
    let info = envelope.info;
    let same = info.version == version
        || crate::version::cmp(&info.version, version, crate::version::Scheme::Pep440)
            == Some(std::cmp::Ordering::Equal);
    if !same {
        return Err(format!("it describes {}, not {version}", info.version));
    }
    let lines = info
        .requires_dist
        .ok_or("it lists no Requires-Dist (null), which does not say there are none")?;
    let mut out = Vec::new();
    for line in &lines {
        out.extend(line_requirements(line)?);
    }
    Ok(out)
}

/// The requirements of one `Requires-Dist` line: one per version clause, or
/// one without a bound. None when its marker rules it out on Linux and
/// CPython, or names an extra; a marker on the Python version keeps it.
fn line_requirements(line: &str) -> Result<Vec<PredictedRequirement>, String> {
    let Some((name, spec, marker)) = parse_requirement(line) else {
        return Err(format!("cannot read the requirement {line:?}"));
    };
    if marker.as_deref().is_some_and(|m| !marker_applies(m)) {
        return Ok(Vec::new());
    }
    let package = python3_name(&name);
    let predicted = |bound: Option<Bound>| PredictedRequirement {
        pypi_name: name.clone(),
        requirement: Requirement {
            dep: BuildRequire::Package(package.clone()),
            bound,
        },
    };
    let spec = spec.trim();
    if spec.is_empty() {
        return Ok(vec![predicted(None)]);
    }
    let clauses = spec.trim_start_matches('(').trim_end_matches(')');
    let mut out = Vec::new();
    for clause in clauses.split(',') {
        match clause_bound(clause) {
            Some(Some(bound)) => out.push(predicted(Some(bound))),
            Some(None) => {}
            None => return Err(format!("cannot read the bound {clause:?} of {name}")),
        }
    }
    Ok(out)
}

/// The RPM bound for one PEP 440 version clause. `Some(None)` for one that
/// never needs a newer version (`!=`); `None` when it cannot be read. Only the
/// lower part of `~=` and of a `==` prefix match is kept, as that part is what
/// may need the update.
fn clause_bound(clause: &str) -> Option<Option<Bound>> {
    const OPS: [&str; 8] = ["===", "~=", "==", "!=", "<=", ">=", "<", ">"];
    let clause = clause.trim();
    let op = OPS.iter().find(|op| clause.starts_with(**op))?;
    let version = clause[op.len()..].trim();
    let readable = version.starts_with(|c: char| c.is_ascii_alphanumeric())
        && version
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '*' | '+' | '!' | '_' | '-'));
    if !readable {
        return None;
    }
    let bound = |op: &str, version: &str| {
        Some(Some(Bound {
            op: op.to_string(),
            version: version.to_string(),
        }))
    };
    match *op {
        "!=" => Some(None),
        "~=" => bound(">=", version),
        "==" => match version.strip_suffix(".*") {
            Some(prefix) => bound(">=", prefix),
            None => bound("=", version),
        },
        "===" => bound("=", version),
        op => bound(op, version),
    }
}

/// The name a spec requires a PyPI project by: `python3-` and the name
/// [`rpm_name`] gives it (which a CUDA component under `--nvidia-redist`
/// replaces).
pub fn python3_name(pypi_name: &str) -> String {
    let rpm = rpm_name(pypi_name);
    match rpm.strip_prefix("python-") {
        Some(rest) => format!("python3-{rest}"),
        None => rpm,
    }
}
