use super::*;

/// Directory names that hold a language binding, in the order they are
/// preferred when no directory is named after the project.
const BINDING_DIR_NAMES: &[&str] = &["python", "py", "pkg", "package", "src", "lib"];

/// Directory names that hold examples, docs, tests and tooling rather than the
/// product. They are chosen only when nothing else is a candidate.
const NON_PRODUCT_DIR_NAMES: &[&str] = &[
    "examples",
    "example",
    "docs",
    "doc",
    "tests",
    "test",
    "benchmarks",
    "bench",
    "scripts",
    "tools",
    "contrib",
    "samples",
];

/// Choose the project directory among `dirs`, the directories holding a
/// packaging manifest at the fallback depth, when no packaging pyproject.toml
/// names one. In order:
///
/// 1. a directory named after the normalized project name;
/// 2. among the product directories (not in [`NON_PRODUCT_DIR_NAMES`]), the
///    first of [`BINDING_DIR_NAMES`] in that order;
/// 3. the only candidate, when there is exactly one;
/// 4. the only product directory, when there is exactly one.
///
/// Anything else is a guess between several directories, and `None` is
/// returned instead.
pub(super) fn choose_fallback_dir(
    mut dirs: Vec<String>,
    normalized: Option<&str>,
) -> Option<String> {
    dirs.sort();
    dirs.dedup();
    let leaf = |d: &str| {
        d.rsplit('/')
            .next()
            .unwrap_or(d)
            .to_lowercase()
            .replace(['-', '.'], "_")
    };
    if let Some(want) = normalized
        && let Some(d) = dirs.iter().find(|d| leaf(d) == want)
    {
        return Some(d.clone());
    }
    if dirs.len() == 1 {
        return dirs.pop();
    }
    let product: Vec<&String> = dirs
        .iter()
        .filter(|d| !NON_PRODUCT_DIR_NAMES.contains(&leaf(d).as_str()))
        .collect();
    for pref in BINDING_DIR_NAMES {
        if let Some(d) = product.iter().find(|d| leaf(d) == *pref) {
            return Some((*d).clone());
        }
    }
    match product.as_slice() {
        [only] => Some((*only).clone()),
        _ => {
            tracing::warn!(
                "several candidate project directories ({}); not choosing one",
                dirs.join(", ")
            );
            None
        }
    }
}

impl BuildSystem {
    pub(crate) fn setuptools_default() -> Self {
        Self {
            requires: vec!["setuptools".to_string()],
            backend: Some("setuptools.build_meta".to_string()),
        }
    }

    pub fn is_maturin(&self) -> bool {
        self.backend.as_deref() == Some("maturin")
    }
}

impl From<RawBuildSystem> for BuildSystem {
    fn from(raw: RawBuildSystem) -> Self {
        // An entry a spec cannot carry verbatim is dropped, not the whole list.
        // A marker false for the build target drops its requirement; any other
        // marker is dropped and the requirement kept (see `build_requirement_part`).
        let requires = raw
            .requires
            .into_iter()
            .filter_map(|r| match build_requirement_part(&r) {
                Some(BuildRequirement::Keep(part)) => Some(part.to_string()),
                Some(BuildRequirement::NotForTarget) => {
                    tracing::info!(
                        "not installing build-system requirement {r:?}: its marker is false for the build target"
                    );
                    None
                }
                None => {
                    if r.split(';').next().is_some_and(|req| req.contains('@')) {
                        tracing::warn!(
                            "ignoring build-system requirement {r:?}: a direct reference (`@ url`) cannot be written in a spec"
                        );
                    } else {
                        tracing::warn!(
                            "ignoring build-system requirement {r:?}: not a plain PEP 508 requirement"
                        );
                    }
                    None
                }
            })
            .collect();
        Self {
            requires,
            backend: raw.build_backend,
        }
    }
}

// --- TOML wire types -------------------------------------------------------

#[derive(Deserialize)]
pub(super) struct PyprojectToml {
    #[serde(rename = "build-system")]
    pub(super) build_system: Option<RawBuildSystem>,
    project: Option<RawProject>,
    // Deliberately loose: one malformed `[tool.*]` field must not fail the
    // whole parse. Only `tool.poetry.name` is read, and only if a string.
    tool: Option<toml::Value>,
}

#[derive(Deserialize)]
struct RawProject {
    name: Option<String>,
    // Loose like `tool`: a malformed table must not fail the parse.
    scripts: Option<toml::Value>,
    #[serde(rename = "gui-scripts")]
    gui_scripts: Option<toml::Value>,
}

impl PyprojectToml {
    /// Keys of `[project.scripts]`, `[project.gui-scripts]` and the legacy
    /// `[tool.poetry.scripts]`, in order, without a name that is not a plain
    /// file name (it ends up in spec lines and paths).
    pub(super) fn script_names(&self) -> Vec<String> {
        let mut tables: Vec<&toml::Value> = Vec::new();
        if let Some(p) = &self.project {
            tables.extend(p.scripts.iter().chain(p.gui_scripts.iter()));
        }
        if let Some(t) = self
            .tool
            .as_ref()
            .and_then(|t| t.get("poetry")?.get("scripts"))
        {
            tables.push(t);
        }
        let mut out: Vec<String> = Vec::new();
        for table in tables {
            let Some(table) = table.as_table() else {
                continue;
            };
            for name in table.keys() {
                let plain = !name.starts_with(['-', '.'])
                    && name.len() <= 100
                    && name.bytes().all(|b| {
                        b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-' | b'+')
                    });
                if plain && !out.contains(name) {
                    out.push(name.clone());
                }
            }
        }
        out
    }

    /// `[project].name`, else the legacy `[tool.poetry].name`.
    pub(super) fn declared_name(self) -> Option<String> {
        self.project
            .and_then(|p| p.name)
            .or_else(|| {
                self.tool?
                    .get("poetry")?
                    .get("name")?
                    .as_str()
                    .map(str::to_string)
            })
            .filter(|n| is_plain_project_name(n))
    }
}

#[derive(Deserialize)]
pub(super) struct CargoToml {
    pub(super) package: Option<CargoPackage>,
    #[serde(default)]
    bin: Vec<CargoBin>,
}

#[derive(Deserialize)]
pub(super) struct CargoPackage {
    pub(super) name: Option<String>,
    /// `autobins = false` turns off finding binaries from the file layout.
    autobins: Option<bool>,
}

#[derive(Deserialize)]
struct CargoBin {
    name: Option<String>,
    path: Option<String>,
}

/// `path` without a leading `./` or empty components, the way cargo compares
/// target paths.
fn clean_path(path: &str) -> String {
    path.split('/')
        .filter(|c| !c.is_empty() && *c != ".")
        .collect::<Vec<_>>()
        .join("/")
}

impl CargoToml {
    /// The executables a root crate builds: its `[[bin]]` tables and, unless
    /// `autobins = false`, the default `src/main.rs` binary (named after the
    /// package) and the `src/bin` ones. A file a `[[bin]]` names by `path` is
    /// that target, not one more. `files` are the archive's paths below
    /// `top`. A name that is not plain is dropped.
    pub(super) fn bin_names(&self, files: &[String], top: &str) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        let mut add = |n: &str| {
            if is_plain_project_name(n) && !out.iter().any(|o| o == n) {
                out.push(n.to_string());
            }
        };
        let claimed: Vec<String> = self
            .bin
            .iter()
            .filter_map(|b| b.path.as_deref().map(clean_path))
            .collect();
        for b in &self.bin {
            if let Some(n) = &b.name {
                add(n);
            }
        }
        let pkg = self.package.as_ref();
        if pkg.and_then(|p| p.autobins) == Some(false) {
            return out;
        }
        let rel = |f: &str| {
            f.strip_prefix(top)
                .map(|r| r.trim_start_matches('/').to_string())
        };
        let free = |path: &str| !claimed.iter().any(|c| c == path);
        if let Some(p) = pkg.and_then(|p| p.name.as_deref())
            && free("src/main.rs")
            && files
                .iter()
                .any(|f| rel(f).as_deref() == Some("src/main.rs"))
        {
            add(p);
        }
        for f in files {
            let Some(path) = rel(f) else { continue };
            let Some(rest) = path.strip_prefix("src/bin/") else {
                continue;
            };
            if !free(&path) {
                continue;
            }
            match rest.split('/').collect::<Vec<_>>().as_slice() {
                [file] => {
                    if let Some(stem) = file.strip_suffix(".rs") {
                        add(stem);
                    }
                }
                [dir, "main.rs"] => add(dir),
                _ => {}
            }
        }
        out
    }
}

#[derive(Deserialize)]
pub(super) struct RawBuildSystem {
    #[serde(default)]
    requires: Vec<String>,
    #[serde(rename = "build-backend")]
    build_backend: Option<String>,
}

/// The project's source repository on GitHub or GitLab, from the metadata an
/// unpacked sdist carries: `Project-URL:` lines of `PKG-INFO` and
/// `[project.urls]` of `pyproject.toml`, then `Home-page`. Keys that name the
/// source (`Source`, `Repository`, `Code`) win over `Homepage`; a URL that is
/// not a repository on those two hosts (docs, an issue tracker on another
/// host) is skipped. The result is the canonical `https://host/owner/repo`.
pub fn source_repo_from_metadata(pkg_info: &str, pyproject: &str) -> Option<String> {
    let mut urls: Vec<(String, String)> = Vec::new();
    for line in pkg_info.lines() {
        // Metadata headers end at the first blank line; the body is the readme.
        if line.trim().is_empty() {
            break;
        }
        if let Some(v) = line.strip_prefix("Project-URL:") {
            if let Some((k, u)) = v.split_once(',') {
                urls.push((k.trim().to_string(), u.trim().to_string()));
            }
        } else if let Some(v) = line.strip_prefix("Home-page:") {
            urls.push(("Homepage".to_string(), v.trim().to_string()));
        }
    }
    if let Ok(doc) = pyproject.parse::<toml::Table>()
        && let Some(t) = doc
            .get("project")
            .and_then(|p| p.get("urls"))
            .and_then(|u| u.as_table())
    {
        for (k, v) in t {
            if let Some(u) = v.as_str() {
                urls.push((k.clone(), u.trim().to_string()));
            }
        }
    }
    let rank = |k: &str| -> usize {
        let k = k.to_ascii_lowercase();
        [
            "source",
            "source code",
            "repository",
            "code",
            "github",
            "gitlab",
        ]
        .iter()
        .position(|s| *s == k)
        .unwrap_or(10)
    };
    urls.sort_by_key(|(k, _)| rank(k));
    urls.iter().find_map(|(_, u)| {
        let f = crate::forge::parse(u)?;
        let host_ok = f.host == "github.com"
            || f.host == "gitlab.com"
            || (f.host.starts_with("gitlab.") && crate::forge::is_known_host(&f.host));
        (host_ok && f.project_path.contains('/')).then(|| f.canonical_https())
    })
}
