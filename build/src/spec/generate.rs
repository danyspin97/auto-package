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

/// A runtime dependency read from a release's `Requires-Dist`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeDep {
    pub name: String,
    /// `>= 1.2`, when the release names a lower bound.
    pub bound: Option<String>,
}

/// The dependencies a release needs on every install: its `Requires-Dist`
/// entries without a marker (an extra, a Python version or a platform narrows
/// an entry down, and which narrowing applies is a decision for the review).
///
/// Only a lower bound is kept: an exact pin (`==1.2`) and a compatible release
/// (`~=1.2`) become `>= 1.2`, and `<`, `<=` and `!=` are dropped, because a
/// rolling distro ships one release and an upper bound would make the package
/// unresolvable the day it moves on (convention 59). The review agent was
/// writing exactly this by hand on package after package.
pub fn runtime_dependencies(requires_dist: &[String], own_name: &str) -> Vec<RuntimeDep> {
    let own = crate::osc::fuzzy_key(own_name);
    let mut seen = std::collections::HashSet::new();
    let mut deps = Vec::new();
    for entry in requires_dist {
        let (spec, marker) = match entry.split_once(';') {
            Some((spec, marker)) => (spec, marker.trim()),
            None => (entry.as_str(), ""),
        };
        if !marker.is_empty() && !marker_is_always_true_on_linux(marker) {
            continue;
        }
        let spec = spec.trim();
        let Some(m) = leading_name_re().find(spec) else {
            continue;
        };
        let name = m.as_str().to_string();
        let key = crate::osc::fuzzy_key(&name);
        if key == own || !seen.insert(key) {
            continue;
        }
        // `[extra]` selects extras of the dependency; its own pins follow.
        let rest = spec[m.end()..].trim();
        let (extras, rest): (Vec<&str>, &str) = match (rest.starts_with('['), rest.find(']')) {
            (true, Some(close)) => (
                rest[1..close].split(',').map(str::trim).collect(),
                &rest[close + 1..],
            ),
            _ => (Vec::new(), rest),
        };
        let rest = rest.trim().trim_start_matches('(').trim_end_matches(')');
        // A CUDA component's version is NVIDIA's, not the wheel's, so the
        // wheel's bound does not carry over (`--nvidia-redist`).
        let is_cuda_component = crate::redist::component(&name).is_some();
        let bound = rest.split(',').map(str::trim).find_map(|clause| {
            if is_cuda_component {
                return None;
            }
            let version = ["===", "==", "~=", ">=", ">"]
                .iter()
                .find_map(|op| clause.strip_prefix(op))?
                .trim();
            let usable = !version.is_empty()
                && !version.contains('*')
                && version
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '!' | '+' | '-' | '_'));
            usable.then(|| format!(">= {version}"))
        });
        deps.push(RuntimeDep {
            name: recase_python_module_name(&name),
            bound,
        });
        // A required extra pulls in packages of its own (`psycopg[binary]` needs
        // `psycopg-binary`): the ones the project is known to need.
        for extra in extras {
            for package in extra_requirements(&name, extra) {
                if seen.insert(crate::osc::fuzzy_key(package)) {
                    deps.push(RuntimeDep {
                        name: recase_python_module_name(package),
                        bound: None,
                    });
                }
            }
        }
    }
    deps
}

/// Whether a PEP 508 marker is true on every Linux install: a single clause on
/// the platform that Linux satisfies (`sys_platform == "linux"`,
/// `platform_system != "Windows"`). Anything else (a Python version, an
/// architecture, an `extra`) is for the review to decide.
fn marker_is_always_true_on_linux(marker: &str) -> bool {
    let m: String = marker
        .chars()
        .filter(|c| !c.is_whitespace() && !matches!(c, '"' | '\'' | '(' | ')'))
        .collect();
    matches!(
        m.as_str(),
        "sys_platform==linux"
            | "platform_system==Linux"
            | "os_name==posix"
            | "sys_platform!=win32"
            | "sys_platform!=darwin"
            | "platform_system!=Windows"
            | "platform_system!=Darwin"
            | "os_name!=nt"
    )
}

/// The packages a well-known project's extra requires (`httpx[brotli]` needs
/// `brotli`). Only extras whose packages are known; an unknown extra adds
/// nothing, as it did before. Project and extra names compare normalised.
fn extra_requirements(project: &str, extra: &str) -> &'static [&'static str] {
    let norm = |s: &str| s.to_ascii_lowercase().replace(['_', '.'], "-");
    match (norm(project).as_str(), norm(extra).as_str()) {
        ("psycopg", "binary") => &["psycopg-binary"],
        ("psycopg", "c") => &["psycopg-c"],
        ("psycopg", "pool") => &["psycopg-pool"],
        ("httpx", "brotli") => &["brotli"],
        ("httpx", "zstd") => &["zstandard"],
        ("httpx", "socks") => &["socksio"],
        ("httpx", "http2") => &["h2"],
        ("httpx", "cli") => &["click", "pygments", "rich"],
        ("requests", "socks") => &["pysocks"],
        ("pydantic", "email") => &["email-validator"],
        ("pydantic", "timezone") => &["tzdata"],
        ("sqlalchemy", "asyncio") => &["greenlet"],
        ("redis", "hiredis") => &["hiredis"],
        _ => &[],
    }
}

/// Add the release's runtime dependencies as `Requires` (before
/// `%python_subpackages`, as plain `python-<name>` so a flavor requires its own
/// build) and as `BuildRequires` (the tests and `%check` import them). A
/// dependency the spec already names in either place is left as it is.
pub fn add_runtime_dependencies(spec: &str, requires_dist: &[String], own_name: &str) -> String {
    let deps = runtime_dependencies(requires_dist, own_name);
    if deps.is_empty() {
        return spec.to_string();
    }
    let names = |tag: &str| -> std::collections::HashSet<String> {
        spec.lines()
            .filter_map(|l| l.trim_start().strip_prefix(tag))
            .flat_map(|v| {
                v.replace(['{', '}'], " ")
                    .split_whitespace()
                    .map(|t| crate::osc::fuzzy_key(crate::osc::strip_rpm_prefix(t)))
                    .collect::<Vec<_>>()
            })
            .collect()
    };
    let (have_requires, have_build) = (names("Requires:"), names("BuildRequires:"));
    let mut out = spec.to_string();
    let mut requires_lines = String::new();
    for dep in &deps {
        // `--nvidia-redist`: a CUDA wheel is the package built from NVIDIA's
        // archive, which is not a Python package and has no flavors.
        let cuda = crate::redist::package_name(&dep.name);
        let key = crate::osc::fuzzy_key(cuda.as_deref().unwrap_or(&dep.name));
        let bound = dep
            .bound
            .as_deref()
            .map(|b| format!(" {b}"))
            .unwrap_or_default();
        if !have_requires.contains(&key) {
            let package = cuda
                .clone()
                .unwrap_or_else(|| format!("python-{}", dep.name));
            requires_lines.push_str(&format!("Requires:       {package}{bound}\n"));
        }
        if !have_build.contains(&key) {
            let package = cuda
                .clone()
                .unwrap_or_else(|| format!("%{{python_module {}{bound}}}", dep.name));
            if let Some(next) = add_build_requires(&out, &package) {
                out = next;
            }
        }
    }
    if !requires_lines.is_empty()
        && let Some(at) = out.find("%python_subpackages")
    {
        out.insert_str(at, &requires_lines);
    }
    out
}

/// The `%files` entries for what a Python source tree installs into
/// site-packages, read from the sdist's file list, with `@SITE@` standing for
/// the site directory. `None` unless the tree says it without guessing: one
/// import tree under the project root (or its `src/`), no loose top-level module.
///
/// The generator guessed `%{python_sitelib}/<pypi_name>/`, which was wrong for
/// every PEP 420 namespace distribution (`opentelemetry-instrumentation-flask`
/// installs `opentelemetry/instrumentation/flask/`) and wherever the import name
/// differs (`python-docx` -> `docx`, `z3-solver` -> `z3`); the review corrected
/// it on a dozen packages.
pub fn sdist_files_entries(
    files: &[String],
    top_level_dir: &str,
    project_dir: Option<&str>,
) -> Option<Vec<String>> {
    use std::collections::BTreeMap;

    const NOT_INSTALLED: &[&str] = &[
        "tests",
        "test",
        "testing",
        "docs",
        "doc",
        "examples",
        "example",
        "scripts",
        "script",
        "benchmarks",
        "benchmark",
        "tools",
        "build",
        "dist",
        "ci",
        "typings",
        "stubs",
        "tasks",
        "noxfile.py",
        "setup.py",
        "conftest.py",
        "fabfile.py",
        "docs_src",
        "bench",
        "dev",
    ];
    let prefix = match project_dir {
        Some(d) => format!("{top_level_dir}/{d}/"),
        None => format!("{top_level_dir}/"),
    };
    let rel: Vec<&str> = files
        .iter()
        .filter_map(|f| f.strip_prefix(prefix.as_str()))
        .filter(|f| f.ends_with(".py"))
        .collect();
    // A `src/` layout when the Python lives there.
    let root = if rel
        .iter()
        .any(|f| f.starts_with("src/") && f.matches('/').count() >= 2)
    {
        "src/"
    } else {
        ""
    };
    #[derive(Default)]
    struct Dir {
        files: Vec<String>,
        dirs: BTreeMap<String, Dir>,
    }
    let mut tree = Dir::default();
    for f in rel.iter().filter_map(|f| f.strip_prefix(root)) {
        let first = f.split('/').next().unwrap_or("");
        if first.starts_with('.') || NOT_INSTALLED.contains(&first) {
            continue;
        }
        if f.chars()
            .any(|c| c.is_whitespace() || matches!(c, '%' | '$' | '*' | '?' | '['))
        {
            return None;
        }
        // A `.`, `..` or empty component anywhere can step out of the package
        // directory (`pkg/../../x.py`): not listed blind.
        if f.split('/').any(|p| p.is_empty() || p == "." || p == "..") {
            return None;
        }
        let mut node = &mut tree;
        let mut parts = f.split('/').peekable();
        while let Some(part) = parts.next() {
            if parts.peek().is_some() {
                node = node.dirs.entry(part.to_string()).or_default();
            } else {
                node.files.push(part.to_string());
            }
        }
    }
    // A loose module (`six.py`) brings its own `__pycache__` entry: not guessed.
    if !tree.files.is_empty() || tree.dirs.len() != 1 {
        return None;
    }
    fn emit(dir: &Dir, path: &str, out: &mut Vec<String>) {
        let own_files = dir.files.iter().any(|f| f == "__init__.py");
        if own_files || dir.dirs.is_empty() {
            out.push(format!("@SITE@/{path}/"));
            return;
        }
        out.push(format!("%dir @SITE@/{path}"));
        let mut files = dir.files.clone();
        files.sort();
        for f in files {
            out.push(format!("@SITE@/{path}/{f}"));
        }
        for (name, child) in &dir.dirs {
            emit(child, &format!("{path}/{name}"), out);
        }
    }
    let mut out = Vec::new();
    for (name, child) in &tree.dirs {
        emit(child, name, &mut out);
    }
    Some(out)
}

/// Replace the module-directory entry of the generated `%files` with the
/// entries read from the sdist, when the generated one names a directory that
/// is not part of the install tree. The site directory the line used is kept.
pub(super) fn apply_install_entries(spec: &str, entries: &[String]) -> String {
    let real_tops: std::collections::HashSet<&str> = entries
        .iter()
        .filter_map(|e| {
            let path = e.strip_prefix("%dir ").unwrap_or(e);
            path.strip_prefix("@SITE@/")?.split('/').next()
        })
        .collect();
    let mut out = String::with_capacity(spec.len());
    let mut replaced = false;
    let mut in_files = false;
    for line in spec.lines() {
        const SECTIONS: &[&str] = &[
            "%files",
            "%description",
            "%prep",
            "%build",
            "%install",
            "%check",
            "%changelog",
            "%package",
            "%pre",
            "%post",
            "%preun",
            "%postun",
            "%pretrans",
            "%posttrans",
        ];
        if let Some(section) = SECTIONS.iter().find(|s| {
            line.strip_prefix(**s)
                .is_some_and(|rest| rest.is_empty() || rest.starts_with(char::is_whitespace))
        }) {
            in_files = *section == "%files";
        }
        let trimmed = line.trim().trim_end_matches('/');
        let guessed_site =
            if in_files && !replaced && trimmed.starts_with('%') && !trimmed.contains(".dist-info")
            {
                trimmed
                    .rsplit_once('/')
                    .filter(|(site, last)| {
                        (site.contains("sitelib") || site.contains("sitearch"))
                            && !real_tops.contains(last)
                    })
                    .map(|(site, _)| site)
            } else {
                None
            };
        if let Some(site) = guessed_site {
            for e in entries {
                out.push_str(&e.replace("@SITE@", site));
                out.push('\n');
            }
            replaced = true;
        } else {
            out.push_str(line);
            out.push('\n');
        }
    }
    if !spec.ends_with('\n') {
        out.pop();
    }
    out
}

/// Give the commands a source build installs (`[project.scripts]`) the
/// `update-alternatives` handling convention 18 asks for, when py2pack did not
/// (it only reads `setup.py` metadata): `%python_clone` after
/// `%pyproject_install`, `%post`/`%postun` registration and a
/// `%python_alternative` in `%files`. A script the spec already handles is
/// left alone.
pub fn add_console_script_alternatives(spec: &str, scripts: &[String]) -> String {
    let missing: Vec<&String> = scripts
        .iter()
        .filter(|s| {
            !spec.contains(&format!(
                "%python_clone -a %{{buildroot}}%{{_bindir}}/{s}\n"
            )) && !spec.contains(&format!("%python_alternative %{{_bindir}}/{s}\n"))
        })
        .collect();
    let has_install = spec.lines().any(|l| l.trim() == "%pyproject_install");
    let files_pos = spec
        .lines()
        .position(|l| l.starts_with("%files") && l.contains("%{python_files}"));
    if missing.is_empty() || !has_install || files_pos.is_none() {
        return spec.to_string();
    }
    let each =
        |f: &dyn Fn(&str) -> String| -> Vec<String> { missing.iter().map(|s| f(s)).collect() };
    let clones = each(&|s| format!("%python_clone -a %{{buildroot}}%{{_bindir}}/{s}"));
    let installs = each(&|s| format!("%python_install_alternative {s}"));
    let uninstalls = each(&|s| format!("%python_uninstall_alternative {s}"));
    let owned = each(&|s| format!("%python_alternative %{{_bindir}}/{s}"));

    let mut lines: Vec<String> = spec.lines().map(str::to_string).collect();
    // Bottom-up, so earlier indices stay valid.
    let files_pos = files_pos.unwrap_or(0);
    lines.splice(files_pos + 1..files_pos + 1, owned);
    for (header, add) in [("%postun", uninstalls), ("%post", installs)] {
        match lines.iter().position(|l| l.trim() == header) {
            Some(pos) => {
                lines.splice(pos + 1..pos + 1, add);
            }
            None => {
                let files_pos = lines
                    .iter()
                    .position(|l| l.starts_with("%files") && l.contains("%{python_files}"))
                    .unwrap_or(0);
                let mut block = vec![header.to_string()];
                block.extend(add);
                block.push(String::new());
                lines.splice(files_pos..files_pos, block);
            }
        }
    }
    if let Some(pos) = lines.iter().position(|l| l.trim() == "%pyproject_install") {
        lines.splice(pos + 1..pos + 1, clones);
    }
    let out = lines.join("\n") + if spec.ends_with('\n') { "\n" } else { "" };
    fix_missing_alternatives_requires(&out)
}

/// Point a bare-filename `Source:` at the archive actually in the checkout.
///
/// py2pack names the tarball after PyPI's own sdist (`Events-%{version}.zip`)
/// even when the checkout holds a different file — a forge release archive
/// (`events-0.5.tar.gz`) that the pipeline downloaded because the project has
/// no sdist. `%prep` then fails on the very first build, and `unzip` is
/// requested for nothing. A URL `Source:` is left alone: it names where the
/// file comes from, not which file is on disk.
pub(super) fn fix_bare_source_filename(spec: &str, release: &Release) -> String {
    let actual = release.sdist.filename.as_str();
    if actual.is_empty() {
        return spec.to_string();
    }
    let templated = actual.replace(&release.version, "%{version}");
    let mut changed = false;
    let mut out = String::with_capacity(spec.len());
    for line in spec.lines() {
        let rewritten = ["Source:", "Source0:"].iter().find_map(|tag| {
            let value = line.strip_prefix(tag)?.trim();
            (!value.is_empty() && !value.contains("://") && value != templated)
                .then(|| format!("{tag:<16}{templated}"))
        });
        match rewritten {
            Some(l) if !changed => {
                changed = true;
                out.push_str(&l);
            }
            _ => out.push_str(line),
        }
        out.push('\n');
    }
    if !spec.ends_with('\n') {
        out.pop();
    }
    if changed && !actual.ends_with(".zip") {
        out = out
            .lines()
            .filter(|l| l.trim() != "BuildRequires:  unzip")
            .collect::<Vec<_>>()
            .join("\n")
            + if spec.ends_with('\n') { "\n" } else { "" };
    }
    out
}

/// The pip-installed form of a PyPI name: lowercase, `-`/`.` → `_`.
///
/// This is what ends up on disk: `MarkupSafe` installs as `markupsafe`,
/// `rich-toolkit` as `rich_toolkit`, and the dist-info directory always uses
/// this spelling.
pub fn normalized_module_name(pypi_name: &str) -> String {
    pypi_name.to_lowercase().replace(['-', '.'], "_")
}

/// Drop `BuildRequires` lines that repeat a package an earlier line already
/// required.
///
/// py2pack emits the PyPI build requirements and [`post_process`] then adds
/// `[build-system] requires` from the sdist, so anything declared in both
/// appears twice — the single most common edit in the attempt journals, and
/// once even a triplicate. A duplicate is the same package with the same
/// version constraint, or a bare line next to a constrained one (the
/// constrained line wins, whichever order the two appear in). Lines in
/// different `%if`/`%else` branches never duplicate each other, and a line
/// naming several packages (`BuildRequires: gcc make`) is left untouched
/// rather than risk dropping one of them.
pub fn dedupe_build_requires(spec: &str) -> String {
    use std::collections::HashMap;

    /// Conditional scope plus package name, the unit duplicates are found in.
    type Group = (Vec<u32>, String);

    /// Split `BuildRequires` text into tokens, keeping `%{...}` whole.
    fn tokens(text: &str) -> Vec<&str> {
        let mut out = Vec::new();
        let (mut start, mut depth) = (None, 0usize);
        for (i, c) in text.char_indices() {
            match c {
                '{' => depth += 1,
                '}' => depth = depth.saturating_sub(1),
                _ => {}
            }
            let sep = depth == 0 && (c.is_whitespace() || c == ',');
            match (sep, start) {
                (true, Some(s)) => {
                    out.push(&text[s..i]);
                    start = None;
                }
                (false, None) if !sep => start = Some(i),
                _ => {}
            }
        }
        if let Some(s) = start {
            out.push(&text[s..]);
        }
        out
    }

    /// `(name, constraint)` when `text` names exactly one package.
    fn single(text: &str) -> Option<(String, String)> {
        let toks = tokens(text);
        let is_op = |t: &str| !t.is_empty() && t.chars().all(|c| "<>=!".contains(c));
        let (name, constraint) = match toks.as_slice() {
            [one] => match one.find(['<', '>', '=', '!']) {
                Some(i) if i > 0 => (&one[..i], one[i..].to_string()),
                Some(_) => return None,
                None => (*one, String::new()),
            },
            [name, op, ver] if is_op(op) => (*name, format!("{op}{ver}")),
            _ => return None,
        };
        // Any other macro or a rich dependency is not something to compare.
        (!name.starts_with('%') && !name.starts_with('('))
            .then(|| (name.to_lowercase(), constraint))
    }

    /// The package a BuildRequires line is about and its constraint, keyed so
    /// `cmake` and `%{python_module cmake}` stay different packages.
    fn subject(line: &str) -> Option<(String, String)> {
        let rest = line.strip_prefix("BuildRequires:")?.trim();
        match rest.strip_prefix("%{python_module ") {
            Some(inner) => {
                let (name, constraint) = single(inner.strip_suffix('}')?)?;
                Some((format!("py:{name}"), constraint))
            }
            None => single(rest),
        }
    }

    // Each line's conditional scope: the path of branch ids it sits in.
    let lines: Vec<&str> = spec.lines().collect();
    let mut stack: Vec<u32> = Vec::new();
    let mut next_branch = 0u32;
    let mut groups: HashMap<Group, Vec<(usize, String)>> = HashMap::new();
    for (i, line) in lines.iter().enumerate() {
        let word = line.split_whitespace().next().unwrap_or("");
        match word {
            "%if" | "%ifarch" | "%ifnarch" | "%ifos" | "%ifnos" => {
                next_branch += 1;
                stack.push(next_branch);
            }
            "%else" | "%elif" | "%elifarch" | "%elifos" => {
                next_branch += 1;
                if let Some(top) = stack.last_mut() {
                    *top = next_branch;
                }
            }
            "%endif" => {
                stack.pop();
            }
            _ => {}
        }
        if let Some((name, constraint)) = subject(line) {
            groups
                .entry((stack.clone(), name))
                .or_default()
                .push((i, constraint));
        }
    }

    let mut drop = vec![false; lines.len()];
    for entries in groups.values() {
        let any_constrained = entries.iter().any(|(_, c)| !c.is_empty());
        let mut seen: Vec<&str> = Vec::new();
        for (i, constraint) in entries {
            if (constraint.is_empty() && any_constrained) || seen.contains(&constraint.as_str()) {
                drop[*i] = true;
            } else {
                seen.push(constraint);
            }
        }
    }

    let mut out = String::with_capacity(spec.len());
    for (i, line) in lines.iter().enumerate() {
        if drop[i] {
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
    let mut in_files = false;

    for line in spec.lines() {
        if let Some(keyword) = section_keyword(line) {
            in_files = keyword == "%files";
        }
        // py2pack writes these entries with a trailing `/`; splitting at the
        // last `/` of the untrimmed line made the "prefix" the whole path
        // and doubled the directory (`x.dist-info/x.dist-info/`) in every
        // generated spec.
        let trimmed = line.trim().trim_end_matches('/');
        let rewritten = if !in_files || section_keyword(line).is_some() {
            line.to_string()
        } else if trimmed.contains(".dist-info") {
            // Split at the `/` ending the dist-info component so a trailing
            // file (`.dist-info/LICENSE`) survives the rewrite.
            let idx = trimmed.find(".dist-info").unwrap();
            let dir_start = trimmed[..idx].rfind('/').map(|i| i + 1).unwrap_or(0);
            let after = &trimmed[idx + ".dist-info".len()..];
            let prefix = &trimmed[..dir_start];
            // Directory entries lost their `/` to the trim above.
            let tail = if after.is_empty() { "/" } else { after };
            format!("{prefix}{normalized}-%{{version}}.dist-info{tail}")
        } else if let Some((prefix, last)) = trimmed.rsplit_once('/')
            && trimmed.starts_with('%')
            // Only module directories: `%{_bindir}/black` is a script, not a
            // package directory. Covers `%{python_sitelib}`, `%{$python_sitearch}`
            // and the `%python_expand` spellings.
            && (prefix.contains("sitelib") || prefix.contains("sitearch"))
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

/// The section a spec line opens (`%package`, `%files -n foo`, `%install`, …),
/// or `None` for any other line. A macro use such as `%python_expand` or
/// `%prefix` is not a section.
fn section_keyword(line: &str) -> Option<&str> {
    let word = line.trim_start().split_whitespace().next()?;
    let is_section = match word {
        "%package"
        | "%description"
        | "%prep"
        | "%build"
        | "%install"
        | "%check"
        | "%files"
        | "%changelog"
        | "%clean"
        | "%conf"
        | "%generate_buildrequires"
        | "%verifyscript"
        | "%pre"
        | "%post"
        | "%preun"
        | "%postun"
        | "%pretrans"
        | "%posttrans" => true,
        _ => {
            let suffix = word
                .strip_prefix("%trigger")
                .or_else(|| word.strip_prefix("%filetrigger"));
            suffix.is_some_and(|s| !s.is_empty() && s.chars().all(|c| c.is_ascii_alphabetic()))
        }
    };
    is_section.then_some(word)
}
