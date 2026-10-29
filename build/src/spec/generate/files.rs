//! The file entries of a generated spec: the sdist and install entries, the
//! console script alternatives and the file path normalisation.

use super::*;

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
pub(in crate::spec) fn apply_install_entries(spec: &str, entries: &[String]) -> String {
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
pub(in crate::spec) fn fix_bare_source_filename(spec: &str, release: &Release) -> String {
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
