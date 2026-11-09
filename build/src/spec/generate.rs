//! Generating a spec: the generate entry point, the sdist facts, the post-processing
//! with them, and the runtime dependencies and file entries of a spec.

use super::*;
use crate::spec::deps::*;
use crate::spec::license::*;

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
            &spec_path.to_string_lossy(),
            // The name comes from an index; `--` keeps it a positional even
            // when it starts with `-`.
            "--",
            &release.name,
            &release.version,
        ])
        .stdin(Stdio::null())
        .output()
        .map_err(Error::Launch)?;

    if !out.status.success() {
        let code = out.status.code().unwrap_or(-1);
        let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
        return Err(Error::Failed { code, stderr });
    }

    let spec = std::fs::read_to_string(&spec_path).map_err(Error::Io)?;
    Ok(post_process(
        spec,
        release,
        build_system,
        SdistFacts::default(),
    ))
}

/// What the sdist tells us that PyPI metadata cannot.
#[derive(Debug, Clone, Default)]
pub struct SdistFacts {
    pub has_tests: bool,
    pub has_c_extensions: bool,
    /// See `ap_ecosystem::sdist::SdistInfo::top_level_tests_dir`.
    pub top_level_tests_dir: Option<String>,
    /// What the project installs into site-packages, from [`sdist_files_entries`];
    /// `None` when the sdist's layout does not say it unambiguously.
    pub install_entries: Option<Vec<String>>,
    /// Commands the project declares in `[project.scripts]`; see
    /// `ap_ecosystem::sdist::SdistInfo::console_scripts`.
    pub console_scripts: Vec<String>,
}

/// Re-run post-processing once the tarball has been inspected.
pub fn post_process_with_sdist(
    spec: &str,
    release: &Release,
    build_system: &BuildSystem,
    facts: SdistFacts,
) -> String {
    post_process(spec.to_string(), release, build_system, facts)
}

fn post_process(
    spec: String,
    release: &Release,
    build_system: &BuildSystem,
    facts: SdistFacts,
) -> String {
    let has_tests = facts.has_tests;
    let mut out = String::with_capacity(spec.len() + 512);

    let mut in_description = false;
    let mut description_written = false;
    let has_check = spec.contains("%check");

    for line in spec.lines() {
        // Inject %{?sle15_python_module_pythons} before the Name: tag.
        if line.starts_with("Name:") && !spec.contains("sle15_python_module_pythons") {
            out.push_str("%{?sle15_python_module_pythons}\n");
        }

        // Fix License.
        // Both values come from PyPI metadata: `rpm_text` keeps them on one line
        // and doubles `%`, so upstream text cannot open a new spec line or expand
        // a macro. The default URL is a deliberate macro and is not escaped.
        if line.starts_with("License:") {
            let license = rpm_text(&license_tag(&release.license));
            out.push_str(&format!("License:        {license}\n"));
            continue;
        }

        // Fix URL.
        if line.starts_with("URL:") {
            let url = match release.homepage.as_deref() {
                Some(homepage) => rpm_text(homepage),
                None => "https://pypi.org/project/%{name}".to_string(),
            };
            out.push_str(&format!("URL:            {url}\n"));
            continue;
        }

        // Inject build-backend BuildRequires (and pytest if tests exist) after the pip line.
        if line.contains("%{python_module pip}") {
            out.push_str(line);
            out.push('\n');
            for req in &build_system.requires {
                // A newline in a TOML string would open new spec lines, and a
                // `%` or `{` would expand: only a plain PEP 508 requirement
                // goes in, and a bad one is dropped without failing the spec.
                // A marker is not written (a BuildRequires cannot carry one);
                // one false for the build target (Linux, CPython) is not written
                // at all.
                let req = match ap_ecosystem::sdist::build_requirement_part(req) {
                    Some(ap_ecosystem::sdist::BuildRequirement::Keep(req)) => req,
                    Some(ap_ecosystem::sdist::BuildRequirement::NotForTarget) => {
                        tracing::info!(
                            "not writing build requirement {req:?}: its marker is false for the build target"
                        );
                        continue;
                    }
                    None => {
                        tracing::warn!(
                            "dropping build requirement {req:?}: not a plain PEP 508 requirement"
                        );
                        continue;
                    }
                };
                // `setuptools>=42` as it appears in pyproject.toml is exactly
                // the form the conventions document as wrong; OBS needs
                // `setuptools >= 42` inside the macro.
                let req = super::build_requires::build_requires_spelling(&req);
                out.push_str(&format!("BuildRequires:  %{{python_module {req}}}\n"));
            }
            if has_tests && !spec.contains("%{python_module pytest}") {
                out.push_str("BuildRequires:  %{python_module pytest}\n");
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
                if !description_written {
                    if let Some(s) = &release.summary {
                        // One line with every `%` doubled: a summary with a
                        // newline or `%` must not open a section or expand a macro.
                        out.push_str(&rpm_text(s));
                        out.push('\n');
                    }
                    description_written = true;
                }
                in_description = false;
                // rpm wants a blank line between the description body and the
                // next section; without it the agent adds one on every package.
                out.push('\n');
                out.push_str(line);
                out.push('\n');
            }
            continue;
        }

        // Inject %check before %changelog when tests exist. Scoped to the
        // top-level tests/ directory when there is one — otherwise pytest
        // collects from the whole source tree, including anything merely
        // named test_*.py elsewhere (fixture helpers, scripts) as if it
        // were a real test (observed on python-accelerate: 58 spurious
        // failures from src/accelerate/test_utils/scripts/).
        if line.starts_with("%changelog") && has_tests && !has_check {
            match &facts.top_level_tests_dir {
                Some(dir) => out.push_str(&format!("\n%check\n%pytest {dir}/\n\n")),
                None => out.push_str("\n%check\n%pytest\n\n"),
            }
        }

        out.push_str(line);
        out.push('\n');
    }

    // Everything below is a whole-file pass, applied in a fixed order.
    //
    // Each one exists because the review agent was making the identical edit
    // on nearly every package (see the notes.md journals): a deterministic
    // rule here is a turn the model does not have to spend.
    let out = add_runtime_dependencies(&out, &release.requires_dist, &release.name);
    let out = normalize_python_module_versions(&out);
    let out = dedupe_build_requires(&out);
    let out = normalize_files_paths(&out, &release.name);
    let out = match &facts.install_entries {
        Some(entries) => apply_install_entries(&out, entries),
        None => out,
    };
    let out = fix_bare_source_filename(&out, release);
    let out = add_console_script_alternatives(&out, &facts.console_scripts);
    if facts.has_c_extensions {
        fix_c_extension_arch(&out)
    } else {
        out
    }
}
mod runtime;
pub use runtime::*;
mod files;
pub use files::*;
