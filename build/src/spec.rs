//! RPM spec generation via py2pack, with post-processing.
//!
//! py2pack handles the openSUSE macro conventions that are otherwise painful
//! to get right. We supply what it can't determine from PyPI alone: the
//! build-backend requirements from the sdist, the correct SPDX license
//! expression, and the upstream URL.

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
