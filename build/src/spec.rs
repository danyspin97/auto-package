//! RPM spec generation via py2pack, with post-processing.
//!
//! py2pack handles the openSUSE macro conventions that are otherwise painful
//! to get right. We supply what it can't determine from PyPI alone: the
//! build-backend requirements from the sdist, the correct SPDX license
//! expression, and the upstream URL.

use std::path::Path;
use std::process::Command;

use ap_ecosystem::pypi::{License, Release};
use ap_ecosystem::sdist::BuildSystem;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("could not run py2pack: {0}")]
    Launch(std::io::Error),

    #[error("py2pack exited {code}:\n{stderr}")]
    Failed { code: i32, stderr: String },

    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

/// Check that the directory named in `%autosetup -n` matches what the sdist
/// tarball actually contains.
///
/// `%autosetup` without `-n` defaults to `%{name}-%{version}`. Either way,
/// the value is compared (after expanding `%{name}` and `%{version}`) against
/// the real top-level directory in the tarball. A mismatch causes `%prep` to
/// fail with "no such file or directory", which is one of the most common
/// errors in generated specs.
///
/// Returns `None` when the spec has no `%autosetup` line or the tarball has
/// no readable top-level directory — both mean we cannot check.
pub fn check_autosetup_dir(spec: &str, tarball: &Path) -> Option<Result<(), String>> {
    let actual = ap_ecosystem::sdist::top_level_dir(tarball).ok()??;

    let name = tag_value(spec, "Name:")?;
    let version = tag_value(spec, "Version:")?;
    let expanded = autosetup_expected_dir(spec, name, version)?;

    Some(if actual == expanded {
        Ok(())
    } else {
        Err(format!(
            "%autosetup -n expects '{expanded}' but sdist top-level dir is '{actual}'"
        ))
    })
}

fn tag_value<'a>(spec: &'a str, tag: &str) -> Option<&'a str> {
    spec.lines()
        .find(|l| l.starts_with(tag))
        .map(|l| l[tag.len()..].trim())
}

/// The expanded `%autosetup -n` directory for a spec with the given name and
/// version. Used by `check_autosetup_dir` and exposed for testing.
pub fn autosetup_expected_dir(spec: &str, name: &str, version: &str) -> Option<String> {
    let raw = spec
        .lines()
        .find(|l| l.trim().starts_with("%autosetup"))?
        .split_whitespace()
        .collect::<Vec<_>>()
        .windows(2)
        .find(|w| w[0] == "-n")
        .map(|w| w[1].to_string())
        .unwrap_or_else(|| format!("{name}-%{{version}}"));

    Some(
        raw.replace("%{name}", name)
            .replace("%{version}", version)
            .replace("%{upstream_version}", version),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const SPEC_WITH_N: &str = "\
Name:           python-MarkupSafe
Version:        3.0.3
%prep
%autosetup -p1 -n MarkupSafe-%{version}
";

    const SPEC_WITHOUT_N: &str = "\
Name:           python-markupsafe
Version:        3.0.3
%prep
%autosetup -p1
";

    #[test]
    fn explicit_n_is_expanded() {
        assert_eq!(
            autosetup_expected_dir(SPEC_WITH_N, "python-MarkupSafe", "3.0.3").as_deref(),
            Some("MarkupSafe-3.0.3")
        );
    }

    #[test]
    fn missing_n_defaults_to_name_version() {
        assert_eq!(
            autosetup_expected_dir(SPEC_WITHOUT_N, "python-markupsafe", "3.0.3").as_deref(),
            Some("python-markupsafe-3.0.3")
        );
    }

    #[test]
    fn mismatch_is_detected() {
        // MarkupSafe-3.0.3 expected, markupsafe-3.0.3 actual — case mismatch.
        let expected = autosetup_expected_dir(SPEC_WITH_N, "python-MarkupSafe", "3.0.3").unwrap();
        let actual = "markupsafe-3.0.3";
        assert_ne!(expected, actual);
    }
}

/// Generate an openSUSE RPM spec for `release`.
///
/// Invokes `py2pack generate` and post-processes the result to fix the fields
/// py2pack cannot determine without the sdist: license, URL, and build
/// backend requirements.
pub fn generate(release: &Release, build_system: &BuildSystem) -> Result<String, Error> {
    let dir = tempfile::tempdir().map_err(Error::Io)?;
    let spec_path = dir
        .path()
        .join(format!("python-{}.spec", release.name.to_lowercase()));

    let out = Command::new("py2pack")
        .args([
            "generate",
            "-t",
            "opensuse.spec",
            "-f",
            spec_path.to_str().unwrap(),
            &release.name,
            &release.version,
        ])
        .output()
        .map_err(Error::Launch)?;

    if !out.status.success() {
        let code = out.status.code().unwrap_or(-1);
        let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
        return Err(Error::Failed { code, stderr });
    }

    let spec = std::fs::read_to_string(&spec_path).map_err(Error::Io)?;
    Ok(post_process(spec, release, build_system))
}

fn post_process(spec: String, release: &Release, build_system: &BuildSystem) -> String {
    let mut out = String::with_capacity(spec.len() + 256);

    let mut in_description = false;
    let mut description_written = false;

    for line in spec.lines() {
        // Fix License.
        if line.starts_with("License:") {
            let license = license_tag(&release.license);
            out.push_str(&format!("License:        {license}\n"));
            continue;
        }

        // Fix URL.
        if line.starts_with("URL:") {
            let url = release
                .homepage
                .as_deref()
                .unwrap_or("https://pypi.org/project/%{name}");
            out.push_str(&format!("URL:            {url}\n"));
            continue;
        }

        // Inject build-backend BuildRequires after the pip line.
        if line.contains("%{python_module pip}") {
            out.push_str(line);
            out.push('\n');
            for req in &build_system.requires {
                out.push_str(&format!("BuildRequires:  %{{python_module {req}}}\n"));
            }
            continue;
        }

        // Replace the %description body with the one-line summary.
        if line.starts_with("%description") {
            in_description = true;
            description_written = false;
            out.push_str(line);
            out.push('\n');
            continue;
        }

        if in_description {
            if line.starts_with('%') {
                // Next section — emit the summary if we haven't yet, then end.
                if !description_written {
                    if let Some(s) = &release.summary {
                        out.push_str(s);
                        out.push('\n');
                    }
                    description_written = true;
                }
                in_description = false;
                out.push_str(line);
                out.push('\n');
            }
            // Skip the original (possibly multi-page) description body.
            continue;
        }

        out.push_str(line);
        out.push('\n');
    }

    out
}

fn license_tag(license: &License) -> String {
    match license {
        License::Spdx(expr) => expr.clone(),
        License::FreeText(text) => text.clone(),
        License::Classifiers(classifiers) => {
            // Best-effort: strip the classifier prefix and join.
            classifiers
                .iter()
                .filter_map(|c| c.strip_prefix("License :: OSI Approved :: "))
                .collect::<Vec<_>>()
                .join(" AND ")
        }
        License::Unknown => "FIXME".to_string(),
    }
}
