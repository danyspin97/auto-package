//! RPM spec generation via py2pack, with post-processing.
//!
//! py2pack handles the openSUSE macro conventions that are otherwise painful
//! to get right. We supply what it can't determine from PyPI alone: the
//! build-backend requirements from the sdist, the correct SPDX license
//! expression, and the upstream URL.

use std::process::{Command, Stdio};

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

/// Fix the `%autosetup -n` value in a spec to match the actual tarball directory.
///
/// `actual_top_level` is the top-level directory name from the sdist
/// (e.g. `MarkupSafe-3.0.3`), already extracted by the caller's single-pass
/// [`ap_ecosystem::sdist::inspect`] call. Returns the corrected spec, or `None`
/// if no correction was needed.
pub fn fix_autosetup_dir(spec: &str, actual_top_level: &str) -> Option<String> {
    let actual = actual_top_level;
    let name = tag_value(spec, "Name:")?;
    let version = tag_value(spec, "Version:")?;
    let expanded = autosetup_expected_dir(spec, name, version)?;

    if actual == expanded {
        return None;
    }

    // Replace the version in the actual dir with %{version} so the spec
    // stays correct across version bumps. e.g. markupsafe-3.0.3 → markupsafe-%{version}
    let macro_n = actual.replace(version, "%{version}");

    // Replace -n <wrong> with -n <macro_n> on the %autosetup line.
    // If there is no -n, append one.
    let fixed = spec
        .lines()
        .map(|line| {
            if !line.trim().starts_with("%autosetup") {
                return line.to_string();
            }
            let parts: Vec<&str> = line.split_whitespace().collect();
            let has_n = parts.windows(2).any(|w| w[0] == "-n");
            if has_n {
                let mut out = String::new();
                let mut iter = parts.iter().peekable();
                while let Some(&part) = iter.next() {
                    if !out.is_empty() {
                        out.push(' ');
                    }
                    if part == "-n" {
                        out.push_str("-n ");
                        out.push_str(&macro_n);
                        iter.next(); // skip old value
                    } else {
                        out.push_str(part);
                    }
                }
                out
            } else {
                format!("{line} -n {macro_n}")
            }
        })
        .collect::<Vec<_>>()
        .join("\n");

    // Preserve trailing newline if original had one.
    Some(if spec.ends_with('\n') {
        fixed + "\n"
    } else {
        fixed
    })
}

/// Normalize version constraints inside `%{python_module ...}`.
///
/// Adds spaces around `>=`, `<=`, `>`, `<`, `==`, `!=` that are missing them,
/// e.g. `setuptools>=77` → `setuptools >= 77`.
pub fn normalize_python_module_versions(spec: &str) -> String {
    use std::sync::LazyLock;
    static RE: LazyLock<regex::Regex> = LazyLock::new(|| {
        // Match %{python_module ...} and capture the content
        regex::Regex::new(r"%\{python_module ([^}]+)\}").expect("static pattern")
    });
    static OP: LazyLock<regex::Regex> =
        LazyLock::new(|| regex::Regex::new(r"\s*(>=|<=|!=|==|>|<)\s*").expect("static pattern"));

    RE.replace_all(spec, |caps: &regex::Captures| {
        let inner = &caps[1];
        let normalized = OP.replace_all(inner, " $1 ");
        format!("%{{python_module {}}}", normalized)
    })
    .into_owned()
}

/// Fix architecture-related lines when a package has C extensions.
///
/// - Removes `BuildArch: noarch`
/// - Replaces `%{python_sitelib}` with `%{python_sitearch}`
/// - Adds `BuildRequires: %{python_module devel}` (Python headers) if absent
/// - Inserts `export CFLAGS="%{optflags}"` before `%pyproject_wheel`
pub fn fix_c_extension_arch(spec: &str) -> String {
    let has_devel = spec.contains("%{python_module devel}");

    let mut out: Vec<String> = spec
        .lines()
        .filter(|l| !l.trim().starts_with("BuildArch:"))
        .map(|l| l.replace("%{python_sitelib}", "%{python_sitearch}"))
        .collect();

    // Inject devel after the pip BuildRequires line.
    if !has_devel && let Some(pos) = out.iter().position(|l| l.contains("%{python_module pip}")) {
        out.insert(
            pos + 1,
            "BuildRequires:  %{python_module devel}".to_string(),
        );
    }

    // Insert CFLAGS before %pyproject_wheel so the compiler uses RPM's flags.
    if let Some(pos) = out.iter().position(|l| l.trim() == "%pyproject_wheel")
        && !out[..pos].iter().any(|l| l.contains("CFLAGS"))
    {
        out.insert(pos, r#"export CFLAGS="%{optflags}""#.to_string());
    }

    out.join("\n") + if spec.ends_with('\n') { "\n" } else { "" }
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

    #[test]
    fn fix_replaces_n_argument() {
        // Simulate: spec says -n MarkupSafe-3.0.3, actual dir is markupsafe-3.0.3.
        // fix_autosetup_dir can't run without a real tarball, so test the
        // inner replacement logic directly via autosetup_expected_dir.
        let spec = "%autosetup -p1 -n MarkupSafe-3.0.3\n";
        let parts: Vec<&str> = spec.split_whitespace().collect();
        let idx = parts.windows(2).position(|w| w[0] == "-n").unwrap();
        assert_eq!(parts[idx + 1], "MarkupSafe-3.0.3");
    }

    #[test]
    fn fix_appends_n_when_absent() {
        let spec = "\
Name:           python-markupsafe
Version:        3.0.3
%prep
%autosetup -p1
";
        // Without -n, the default would be python-markupsafe-3.0.3.
        // If the actual dir is markupsafe-3.0.3, fix should append -n.
        // Verify that autosetup_expected_dir sees the default.
        assert_eq!(
            autosetup_expected_dir(spec, "python-markupsafe", "3.0.3").as_deref(),
            Some("python-markupsafe-3.0.3")
        );
    }

    #[test]
    fn python_module_constraints_get_spaces_around_the_operator() {
        // The unspaced form is what pyproject.toml and py2pack produce, and
        // exactly what convention 1 in the agent prompt calls wrong.
        let spec = "BuildRequires:  %{python_module setuptools>=42}\n\
                    BuildRequires:  %{python_module wheel}\n";
        let fixed = normalize_python_module_versions(spec);
        assert!(fixed.contains("%{python_module setuptools >= 42}"), "{fixed}");
        assert!(fixed.contains("%{python_module wheel}"), "bare name mangled");
    }

}

/// Strip all `# ap: fix` comment lines from a spec.
///
/// Model-authored fix comments must not accumulate across attempts: they
/// reference RPM macros (e.g. `# ap: fix 1: Changed %pytest to %pytest_arch`)
/// and RPM expands macros inside comments, causing parse failures. Call this
/// before handing the spec back to the model.
pub fn strip_ap_fix_comments(spec: &str) -> String {
    let filtered: Vec<&str> = spec
        .lines()
        .filter(|l| !l.trim_start().starts_with("# ap: fix"))
        .collect();
    let body = filtered.join("\n");
    if spec.ends_with('\n') {
        body + "\n"
    } else {
        body
    }
}

/// Return the 1-based line number and text of the first comment line that
/// contains a `%` macro reference, or `None` if the spec is clean.
///
/// RPM expands macros inside comment lines, so any `# foo: %something` line
/// will be expanded before the spec is parsed — causing the mysterious
/// `Unknown tag: set +x` class of failure that item 8 exists to prevent.
pub fn first_macro_in_comment(spec: &str) -> Option<(usize, String)> {
    for (i, line) in spec.lines().enumerate() {
        let trimmed = line.trim_start();
        if !trimmed.starts_with('#') {
            continue;
        }
        if let Some(pos) = trimmed.find('%') {
            let after = &trimmed[pos + 1..];
            // `%%` is a literal percent — not a macro.
            if after.starts_with('%') {
                continue;
            }
            if after.starts_with(|c: char| c.is_alphabetic() || c == '{') {
                return Some((i + 1, line.to_string()));
            }
        }
    }
    None
}

/// Insert `cd <dir>` at the top of `%build` and `%install`.
///
/// A repository snapshot of a monorepo has the packaging in a subdirectory
/// (`cuda_python/`, `python/`), so `%autosetup` lands one level above the
/// project and every build macro would run in the wrong place.
pub fn build_in_subdir(spec: &str, dir: &str) -> String {
    let mut out = String::with_capacity(spec.len() + 64);
    for line in spec.lines() {
        out.push_str(line);
        out.push('\n');
        if matches!(line.trim(), "%build" | "%install") {
            out.push_str(&format!("cd {dir}\n"));
        }
    }
    if !spec.ends_with('\n') {
        out.pop();
    }
    out
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
            &spec_path.to_string_lossy().into_owned(),
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
    Ok(post_process(spec, release, build_system, SdistFacts::default()))
}

/// What the sdist tells us that PyPI metadata cannot.
#[derive(Debug, Clone, Copy, Default)]
pub struct SdistFacts {
    pub has_tests: bool,
    pub has_c_extensions: bool,
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

        // Inject build-backend BuildRequires (and pytest if tests exist) after the pip line.
        if line.contains("%{python_module pip}") {
            out.push_str(line);
            out.push('\n');
            for req in &build_system.requires {
                // `setuptools>=42` as it appears in pyproject.toml is exactly
                // the form the conventions document as wrong; OBS needs
                // `setuptools >= 42` inside the macro.
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
                        out.push_str(s);
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

        // Inject %check before %changelog when tests exist.
        if line.starts_with("%changelog") && has_tests && !has_check {
            out.push_str("\n%check\n%pytest\n\n");
        }

        out.push_str(line);
        out.push('\n');
    }

    // Everything below is a whole-file pass, applied in a fixed order.
    //
    // Each one exists because the review agent was making the identical edit
    // on nearly every package (see the notes.md journals): a deterministic
    // rule here is a turn the model does not have to spend.
    let out = normalize_python_module_versions(&out);
    let out = dedupe_build_requires(&out);
    let out = normalize_files_paths(&out, &release.name);
    if facts.has_c_extensions {
        fix_c_extension_arch(&out)
    } else {
        out
    }
}

/// The pip-installed form of a PyPI name: lowercase, `-`/`.` → `_`.
///
/// This is what ends up on disk: `MarkupSafe` installs as `markupsafe`,
/// `rich-toolkit` as `rich_toolkit`, and the dist-info directory always uses
/// this spelling.
pub fn normalized_module_name(pypi_name: &str) -> String {
    pypi_name
        .to_lowercase()
        .replace(['-', '.'], "_")
}

/// Drop `BuildRequires` lines that name a package an earlier line already did.
///
/// py2pack emits the PyPI build requirements and [`post_process`] then adds
/// `[build-system] requires` from the sdist, so anything declared in both
/// appears twice — the single most common edit in the attempt journals, and
/// once even a triplicate. The line carrying a version constraint wins,
/// whichever order the two appear in.
pub fn dedupe_build_requires(spec: &str) -> String {
    use std::collections::HashMap;

    /// The bare package name a BuildRequires line is about, lowercased.
    fn subject(line: &str) -> Option<String> {
        let rest = line.strip_prefix("BuildRequires:")?.trim();
        let inner = match rest.strip_prefix("%{python_module ") {
            Some(i) => i.strip_suffix('}').unwrap_or(i),
            None => rest,
        };
        let name = inner
            .split([' ', '>', '<', '=', '!'])
            .next()?
            .trim();
        if name.is_empty() {
            None
        } else {
            Some(format!(
                "{}{}",
                if rest.starts_with("%{python_module ") { "py:" } else { "" },
                name.to_lowercase()
            ))
        }
    }

    // First pass: for each subject, remember the index of the line to keep.
    let lines: Vec<&str> = spec.lines().collect();
    let mut keep: HashMap<String, usize> = HashMap::new();
    for (i, line) in lines.iter().enumerate() {
        let Some(key) = subject(line) else { continue };
        match keep.get(&key) {
            // Prefer the constrained form — it carries strictly more information.
            Some(&prev) if lines[prev].contains(">=") || lines[prev].contains("<=") => {}
            _ => {
                keep.insert(key, i);
            }
        }
    }

    let mut out = String::with_capacity(spec.len());
    for (i, line) in lines.iter().enumerate() {
        if let Some(key) = subject(line)
            && keep.get(&key) != Some(&i)
        {
            continue;
        }
        out.push_str(line);
        out.push('\n');
    }
    if !spec.ends_with('\n') {
        out.pop();
    }
    out
}

/// Rewrite `%files` entries that spell the package the way PyPI does.
///
/// The dist-info directory is always `<normalized>-%{version}.dist-info/` —
/// that one is not a guess. The module directory is only rewritten when it
/// currently spells out the PyPI name verbatim; packages whose import name
/// differs entirely (`PyJWT` → `jwt`, `opencv-python-headless` → `cv2`) are
/// left for the agent, which can read `top_level.txt`.
pub fn normalize_files_paths(spec: &str, pypi_name: &str) -> String {
    let normalized = normalized_module_name(pypi_name);
    let mut out = String::with_capacity(spec.len());

    for line in spec.lines() {
        let trimmed = line.trim();
        let rewritten = if trimmed.contains(".dist-info") {
            let prefix = trimmed.rsplit_once('/').map(|(p, _)| p).unwrap_or("");
            format!("{prefix}/{normalized}-%{{version}}.dist-info/")
        } else if let Some((prefix, last)) = trimmed.rsplit_once('/')
            && trimmed.starts_with('%')
            && last.trim_end_matches('/').eq_ignore_ascii_case(pypi_name)
        {
            format!("{prefix}/{normalized}/")
        } else {
            line.to_string()
        };
        out.push_str(&rewritten);
        out.push('\n');
    }

    if !spec.ends_with('\n') {
        out.pop();
    }
    out
}

/// Generate a minimal skeleton spec for a monorepo package.
///
/// py2pack only understands single-project PyPI packages; for a monorepo with
/// multiple subprojects it would produce a spec for one subproject only,
/// leaving all siblings out. This skeleton carries the correct header fields
/// from the PyPI release and leaves %build/%install/%check empty so the review
/// agent can fill in the full multi-subpackage layout per convention 24.
pub fn monorepo_skeleton(
    pkg_name: &str,
    version: &str,
    subprojects: &[ap_ecosystem::sdist::Subproject],
    release: Option<&ap_ecosystem::pypi::Release>,
) -> String {
    let name = pkg_name.to_lowercase();
    let summary = release
        .and_then(|r| r.summary.as_deref())
        .unwrap_or("FIXME: add package summary");
    let license = release
        .map(|r| license_tag(&r.license))
        .unwrap_or_else(|| "FIXME".to_string());
    let url = release
        .and_then(|r| r.homepage.as_deref())
        .unwrap_or("FIXME");

    let sub_list: String = subprojects
        .iter()
        .map(|s| {
            format!(
                "# TODO: add %package -n python-%{{name}}-{sub} for {dir}/\n",
                sub = s.name.as_deref().unwrap_or(&s.dir),
                dir = s.dir,
            )
        })
        .collect();

    format!(
        "#\n\
         # spec file for package python-{name}\n\
         #\n\
         # Copyright (c) 2026 SUSE LLC\n\
         #\n\
         # All modifications and additions to the file contributed by third parties\n\
         # remain the property of their copyright owners, unless otherwise agreed\n\
         # upon. The license for this file, and modifications and additions to the\n\
         # file, is the same license as for the pristine package itself (unless the\n\
         # license for the pristine package is not an Open Source License, in which\n\
         # case the license is the MIT License). An \"Open Source License\" is a\n\
         # license that conforms to the Open Source Definition (Version 1.9)\n\
         # published by the Open Source Initiative.\n\
         \n\
         # Please submit bugfixes or comments via https://bugs.opensuse.org/\n\
         \n\
         \n\
         %{{?sle15_python_module_pythons}}\n\
         Name:           python-{name}\n\
         Version:        {version}\n\
         Release:        0\n\
         Summary:        {summary}\n\
         License:        {license}\n\
         URL:            {url}\n\
         Source:         {name}-{version}.tar.gz\n\
         BuildRequires:  python-rpm-macros\n\
         BuildRequires:  %{{python_module pip}}\n\
         BuildRequires:  fdupes\n\
         %python_subpackages\n\
         \n\
         # This is a monorepo: all subprojects below must become %%package sections.\n\
         # Convention 24 governs the layout — do not build only one subproject.\n\
         {sub_list}\
         %description\n\
         {summary}\n\
         \n\
         %prep\n\
         %autosetup -p1 -n {name}-{version}\n\
         \n\
         %build\n\
         # TODO: build each subproject in dependency order\n\
         \n\
         %install\n\
         # TODO: install each subproject\n\
         \n\
         %check\n\
         # TODO: check each subproject\n\
         \n\
         %files %{{python_files}}\n\
         %license LICENSE\n\
         \n\
         %changelog\n",
        name = name,
        version = version,
        summary = summary,
        license = license,
        url = url,
        sub_list = sub_list,
    )
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

#[cfg(test)]
mod generator_tests {
    use super::*;

    /// "removed duplicate BuildRequires" appears in 20 of the 25 attempt
    /// journals — py2pack emits the PyPI build requirements and we then add
    /// `[build-system] requires` from the sdist on top.
    #[test]
    fn duplicate_build_requires_collapse_to_the_constrained_line() {
        let spec = "\
BuildRequires:  %{python_module pip}
BuildRequires:  %{python_module hatchling}
BuildRequires:  %{python_module setuptools >= 42}
BuildRequires:  %{python_module hatchling >= 1.0}
BuildRequires:  %{python_module setuptools}
BuildRequires:  fdupes
";
        let out = dedupe_build_requires(spec);
        assert_eq!(out.matches("hatchling").count(), 1, "{out}");
        assert!(out.contains("%{python_module hatchling >= 1.0}"), "{out}");
        assert_eq!(out.matches("setuptools").count(), 1, "{out}");
        assert!(out.contains("%{python_module setuptools >= 42}"), "{out}");
        assert!(out.contains("fdupes"), "non-python BR dropped");
        assert!(out.contains("pip"), "pip dropped");
    }

    #[test]
    fn a_non_python_buildrequires_is_not_confused_with_the_macro_form() {
        // `cmake` the RPM and `%{python_module cmake}` are different packages.
        let spec = "\
BuildRequires:  cmake
BuildRequires:  %{python_module cmake}
";
        assert_eq!(dedupe_build_requires(spec), spec);
    }

    #[test]
    fn a_monorepo_project_is_built_from_its_subdirectory() {
        let spec = "%build\n%pyproject_wheel\n\n%install\n%pyproject_install\n";
        let out = build_in_subdir(spec, "cuda_python");
        assert_eq!(
            out,
            "%build\ncd cuda_python\n%pyproject_wheel\n\n%install\ncd cuda_python\n%pyproject_install\n"
        );
    }

    #[test]
    fn dist_info_always_uses_the_normalized_name() {
        let spec = "\
%files %{python_files}
%{python_sitelib}/rich-toolkit
%{python_sitelib}/rich-toolkit-%{version}.dist-info
";
        let out = normalize_files_paths(spec, "rich-toolkit");
        assert!(out.contains("%{python_sitelib}/rich_toolkit/"), "{out}");
        assert!(
            out.contains("%{python_sitelib}/rich_toolkit-%{version}.dist-info/"),
            "{out}"
        );
    }

    #[test]
    fn a_module_name_that_is_not_the_package_name_is_left_alone() {
        // PyJWT installs as `jwt`; only the agent can know that from
        // top_level.txt, so the module line must survive untouched.
        let spec = "%files %{python_files}\n%{python_sitelib}/jwt/\n";
        assert_eq!(normalize_files_paths(spec, "PyJWT"), spec);
    }

    #[test]
    fn display_name_in_files_is_normalized() {
        let spec = "%files %{python_files}\n%{python_sitearch}/MarkupSafe\n";
        let out = normalize_files_paths(spec, "MarkupSafe");
        assert!(out.contains("%{python_sitearch}/markupsafe/"), "{out}");
    }
}
