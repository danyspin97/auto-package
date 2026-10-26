//! A probable cause, read from the build log in Rust.
//!
//! Most failures are one of a few mechanical shapes — a third-party module the
//! tests import that is not a BuildRequires, a header or tool the build cannot
//! find, a CMake/pkg-config package that is absent. Spending an agent turn
//! (reading `build.log`, grepping the sources, working out which line matters)
//! to rediscover that costs time and money and sometimes goes wrong. So the
//! extractor states the cause and the first thing to do about it, with the log
//! lines that justify it, and the prompt leads with that.
//!
//! This is a pure function of the extracted log, the package name and the spec
//! text: no I/O, so every shape is unit-tested.

use std::sync::LazyLock;

use regex::Regex;

use super::{ExtractedLog, PathClass, PythonFrame};

/// What kind of mechanical failure was recognised.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CauseKind {
    /// A third-party Python module is imported but not installed.
    MissingPythonModule {
        module: String,
        requirement: String,
    },
    /// The package's own module cannot be imported after install.
    OwnModuleNotImportable {
        module: String,
    },
    /// A module removed from the standard library in a recent Python.
    RemovedStdlibModule {
        module: String,
    },
    MissingHeader {
        header: String,
    },
    MissingCommand {
        command: String,
    },
    MissingCMakePackage {
        package: String,
    },
    MissingPkgConfig {
        package: String,
    },
    /// A test run with many failures: a digest grouped by file and reason, so
    /// the agent fixes the whole set in one edit instead of one test at a time.
    PytestFailures {
        failed: usize,
    },
    /// A shared library fails to load because a symbol it needs is not there:
    /// built against one release of a library, run against another.
    UndefinedSymbol {
        symbol: String,
    },
    /// A failure with one known fix recipe (see [`known_recipe`]); `id` names
    /// the recipe.
    KnownRecipe {
        id: &'static str,
    },
}

/// A recognised cause, ready to be put in front of the agent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProbableCause {
    pub kind: CauseKind,
    /// One short paragraph: what is wrong.
    pub summary: String,
    /// What to do first.
    pub action: String,
    /// The few log lines that justify the diagnosis, including where it was hit.
    pub evidence: Vec<String>,
}

static UNDEFINED_SYMBOL: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"undefined symbol: (?<s>[A-Za-z0-9_@.$]+)").expect("static pattern")
});

static NO_MODULE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?:ModuleNotFoundError|ImportError): No module named '(?<m>[A-Za-z0-9_.]+)'")
        .expect("static pattern")
});
static HEADER: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"fatal error: (?<h>[A-Za-z0-9_./+-]+\.(?:h|hpp|hh|hxx)): No such file or directory")
        .expect("static pattern")
});
static CMD_NOT_FOUND: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?:^|[\s:])(?<c>[A-Za-z0-9_.+-]+): (?:command not found|not found)\s*$")
        .expect("static pattern")
});
static PY_NO_FILE_CMD: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"FileNotFoundError: \[Errno 2\] No such file or directory: '(?<c>[A-Za-z0-9_.+-]+)'",
    )
    .expect("static pattern")
});
static CMAKE_PKG: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r#"Could not find a package configuration file provided by "(?<p>[A-Za-z0-9_.+-]+)""#,
    )
    .expect("static pattern")
});
static PKGCONFIG: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?:No package '(?<a>[A-Za-z0-9_.+-]+)' found|Package '(?<b>[A-Za-z0-9_.+-]+)', required by .*, not found)")
        .expect("static pattern")
});

static PYTEST_LINE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^(?<kind>FAILED|ERROR) (?<path>[^\s:]+)(?:::\S+)?(?: - (?<msg>.*))?$")
        .expect("static pattern")
});

/// At least this many FAILED/ERROR lines before the failure is treated as "a
/// broken test environment" (digest) rather than one specific test.
const DIGEST_THRESHOLD: usize = 5;

#[derive(Default)]
struct FileGroup {
    count: usize,
    network: usize,
    missing_file: usize,
    missing_module: usize,
    other: usize,
}

/// `msg` either contains `needle` outright, or is a truncated prefix of it.
///
/// pytest's one-line failure summary is cut to terminal width with a
/// trailing `...`, so a long exception name like `FileNotFoundError` often
/// survives as `FileN...` or shorter. A plain substring check misses those;
/// this also accepts `msg` (minus the ellipsis) as a prefix of `needle`,
/// with a minimum length so a stray fragment like `res...` can't match.
/// The prefix must be one whole token (no spaces) of at least 4 characters:
/// `filen...` is real, but a 3-letter fragment like `res` is also the start
/// of unrelated words and would be ambiguous across needles.
fn truncated_match(msg: &str, needle: &str) -> bool {
    if msg.contains(needle) {
        return true;
    }
    match msg.strip_suffix("...") {
        Some(prefix) if prefix.len() >= 4 && !prefix.contains(char::is_whitespace) => {
            needle.starts_with(prefix)
        }
        _ => false,
    }
}

fn classify_message(msg: &str) -> &'static str {
    let m = msg.to_lowercase();
    // Real network phrases only: a bare `connect` or `socket` also matches a
    // file name such as `connect_fixture.csv`, and a bare `network` matches
    // `networkx`. Matched in lower case.
    const NETWORK: &[&str] = &[
        "connectionerror",
        "connecterror",
        "connection refused",
        "connection reset",
        "connection aborted",
        "connection timed out",
        "readtimeout",
        "connecttimeout",
        "timed out",
        "socket.timeout",
        "socket.gaierror",
        "sslerror",
        "urllib",
        "httpx",
        "resolutionimpossible",
        "name or service not known",
        "temporary failure in name resolution",
        "network is unreachable",
        "no route to host",
    ];
    const MISSING_FILE: &[&str] = &["filenotfounderror", "no such file"];
    const MISSING_MODULE: &[&str] = &["modulenotfounderror", "importerror"];

    if NETWORK.iter().any(|n| truncated_match(&m, n)) {
        "network"
    } else if MISSING_FILE.iter().any(|n| truncated_match(&m, n)) {
        "missing_file"
    } else if MISSING_MODULE.iter().any(|n| truncated_match(&m, n)) {
        "missing_module"
    } else {
        "other"
    }
}

/// Digest of a pytest summary: files with their failure counts and reasons.
fn pytest_digest(haystack: &[&str]) -> Option<ProbableCause> {
    let mut groups: std::collections::BTreeMap<String, FileGroup> = Default::default();
    let mut seen = std::collections::HashSet::new();
    for l in haystack {
        let Some(c) = PYTEST_LINE.captures(l.trim()) else {
            continue;
        };
        // The same summary can appear in both the excerpt and the tail.
        if !seen.insert(l.trim().to_string()) {
            continue;
        }
        let g = groups.entry(c["path"].to_string()).or_default();
        g.count += 1;
        match classify_message(c.name("msg").map(|m| m.as_str()).unwrap_or("")) {
            "network" => g.network += 1,
            "missing_file" => g.missing_file += 1,
            "missing_module" => g.missing_module += 1,
            _ => g.other += 1,
        }
    }
    let failed: usize = groups.values().map(|g| g.count).sum();
    if failed < DIGEST_THRESHOLD {
        return None;
    }
    let is_test_tree = |p: &str| p.starts_with("tests/") || p.starts_with("test/");
    let helper_files: Vec<&String> = groups.keys().filter(|p| !is_test_tree(p)).collect();
    let mut rows: Vec<String> = groups
        .iter()
        .map(|(path, g)| {
            let mut why = Vec::new();
            if g.missing_file > 0 {
                why.push(format!("{} missing file/fixture", g.missing_file));
            }
            if g.network > 0 {
                why.push(format!("{} need network", g.network));
            }
            if g.missing_module > 0 {
                why.push(format!("{} missing module", g.missing_module));
            }
            if g.other > 0 {
                why.push(format!("{} other", g.other));
            }
            format!("{path}: {} failed ({})", g.count, why.join(", "))
        })
        .collect();
    rows.sort();
    let mut steps: Vec<String> = Vec::new();
    if !helper_files.is_empty() {
        steps.push(format!(
            "{} of the failing files are outside `tests/` ({}) — pytest collected helper \
             scripts as tests. Restrict collection to the real test directory (`%pytest tests/`) \
             rather than deselecting them one by one.",
            helper_files.len(),
            helper_files
                .iter()
                .map(|s| s.as_str())
                .take(3)
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    let missing_files: usize = groups.values().map(|g| g.missing_file).sum();
    if missing_files > 0 {
        steps.push(format!(
            "{missing_files} failures are a missing file or fixture (data the sdist does not ship, \
             or a tool run from the wrong directory): fetch the data from the upstream tag as a \
             `SourceN` (convention 44) or ignore those files — do not patch the tests."
        ));
    }
    let network: usize = groups.values().map(|g| g.network).sum();
    if network > 0 {
        steps.push(format!(
            "{network} failures need network access, which build workers lack (convention 11): \
             deselect exactly those tests or files."
        ));
    }
    let modules: usize = groups.values().map(|g| g.missing_module).sum();
    if modules > 0 {
        steps.push(format!(
            "{modules} failures are an import error: add EVERY missing third-party module as a \
             BuildRequires in this same edit (convention 10)."
        ));
    }
    if steps.is_empty() {
        steps.push("Group the failures by file and reason before changing anything.".into());
    }
    Some(ProbableCause {
        summary: format!(
            "{failed} tests failed across {} files — a test ENVIRONMENT problem, not one bug. \
             Decide the whole set in a single edit; fixing one test per build wastes an \
             attempt each.",
            groups.len()
        ),
        action: steps
            .iter()
            .enumerate()
            .map(|(i, s)| format!("({}) {s}", i + 1))
            .collect::<Vec<_>>()
            .join(" "),
        evidence: rows.into_iter().take(12).collect(),
        kind: CauseKind::PytestFailures { failed },
    })
}

/// Modules whose import name is not their PyPI/RPM distribution name.
const IMPORT_TO_DIST: &[(&str, &str)] = &[
    ("yaml", "PyYAML"),
    ("PIL", "Pillow"),
    ("cv2", "opencv-python"),
    ("sklearn", "scikit-learn"),
    ("bs4", "beautifulsoup4"),
    ("dateutil", "python-dateutil"),
    ("attr", "attrs"),
    ("attrs", "attrs"),
    ("google", "protobuf"),
    ("jwt", "PyJWT"),
    ("OpenSSL", "pyOpenSSL"),
    ("Crypto", "pycryptodome"),
    ("serial", "pyserial"),
    ("magic", "python-magic"),
    ("dns", "dnspython"),
    ("git", "GitPython"),
    ("docx", "python-docx"),
    ("lxml", "lxml"),
    ("zmq", "pyzmq"),
    ("psycopg2", "psycopg2"),
    ("usb", "pyusb"),
    ("skimage", "scikit-image"),
    ("markdown", "Markdown"),
    ("mock", "mock"),
    ("tomllib", "tomli"),
];

/// Modules removed from the standard library (convention 38's table).
const REMOVED_STDLIB: &[&str] = &[
    "distutils",
    "imp",
    "asynchat",
    "asyncore",
    "smtpd",
    "pipes",
    "cgi",
    "cgitb",
    "telnetlib",
    "lib2to3",
    "crypt",
    "nntplib",
    "audioop",
    "aifc",
    "chunk",
    "imghdr",
    "mailcap",
    "msilib",
    "nis",
    "ossaudiodev",
    "sndhdr",
    "spwd",
    "sunau",
    "uu",
    "xdrlib",
];

fn normalize(s: &str) -> String {
    s.to_lowercase().replace(['-', '.'], "_")
}

/// The distribution name to put in `%{python_module …}` for an import name.
fn requirement_for(module: &str) -> String {
    IMPORT_TO_DIST
        .iter()
        .find(|(m, _)| *m == module)
        .map(|(_, d)| (*d).to_string())
        .unwrap_or_else(|| module.replace('_', "-"))
}

/// Whether the spec already names `dist` as a (python_module) dependency.
fn spec_declares(spec: &str, dist: &str) -> bool {
    let want = normalize(dist);
    spec.lines()
        .filter(|l| {
            let l = l.trim_start();
            l.starts_with("BuildRequires:") || l.starts_with("Requires:")
        })
        .any(|l| {
            let value = l.split_once(':').map(|(_, v)| v).unwrap_or("");
            value
                .replace(['%', '{', '}'], " ")
                .split_whitespace()
                .any(|tok| normalize(tok) == want)
        })
}

/// Where the failing import happened: the innermost frame in the unpacked
/// sources, else the innermost frame.
fn import_site(frames: &[PythonFrame]) -> Option<String> {
    let frame = frames
        .iter()
        .rev()
        .find(|f| matches!(f.class, PathClass::BuildTree { .. }))
        .or_else(|| frames.last())?;
    let path = match &frame.class {
        PathClass::BuildTree { relative } => relative.as_str(),
        _ => frame.path.as_str(),
    };
    Some(match &frame.function {
        Some(func) => format!("imported at {path}:{} (in {func})", frame.line),
        None => format!("imported at {path}:{}", frame.line),
    })
}

static CYTHON_LIMITED_API: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"Cython (?<c>3\.\d+) requires the Python Limited API version to be (?<v>3\.\d+) or greater")
        .expect("static pattern")
});
static CMAKE_LICENCE_GUARD: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"I_CONFIRM_THIS_IS_NOT_A_LICENSE_VIOLATION|BUILD_AGAINST_ALL_FFMPEG_FROM_S3")
        .expect("static pattern")
});
static SPLIT_TREE_SUBMODULE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"No module named '(?<m>cutlass\.[A-Za-z0-9_.]+)'").expect("static pattern")
});

/// Failures that have one known recipe, each seen in a real run: the cause and
/// the exact fix, so the first attempt is right.
fn known_recipe(haystack: &[&str]) -> Option<ProbableCause> {
    // The whole line is matched; only the evidence is cut to 300 characters.
    let find = |re: &Regex| {
        haystack
            .iter()
            .find(|l| re.is_match(l))
            .map(|l| l.trim().to_string())
    };
    let short = |line: &str| line.chars().take(300).collect::<String>();
    // scikit-build-core `wheel.py-api = "cp38"` with Cython 3.3 (python-tilelang).
    // Each recipe is a branch of its own: a line that does not capture here
    // must not end the search for the others.
    if let Some(line) = find(&CYTHON_LIMITED_API)
        && let Some(c) = CYTHON_LIMITED_API.captures(&line)
    {
        let (cython, floor) = (c["c"].to_string(), c["v"].to_string());
        let tag = format!("cp{}", floor.replace('.', ""));
        return Some(ProbableCause {
            summary: format!(
                "Cython {cython} refuses a Limited API older than {floor}, and the project's \
                 build settings ask for an older one (scikit-build-core `wheel.py-api = \
                 \"cp38\"`, or a `Py_LIMITED_API` define): the stable-ABI floor is too low for \
                 the Cython the distro ships."
            ),
            action: format!(
                "Raise the floor in `%prep` with `sed -i -E 's/(py-api[[:space:]]*=[[:space:]]*\")\
                 cp3[0-8]\"/\\1{tag}\"/' pyproject.toml` (or the same change as a quilt patch; \
                 check with `grep -rn 'py-api\\|Py_LIMITED_API' pyproject.toml CMakeLists.txt`). \
                 A floor of {floor} loses nothing: every flavor built here is newer. Do not pin \
                 an older Cython."
            ),
            evidence: vec![short(&line)],
            kind: CauseKind::KnownRecipe {
                id: "cython-limited-api",
            },
        });
    }
    // torchcodec's wheel-state licence guard.
    if let Some(line) = find(&CMAKE_LICENCE_GUARD)
        && haystack
            .iter()
            .any(|l| l.contains("FATAL_ERROR") || l.contains("CMake Error"))
    {
        return Some(ProbableCause {
            summary: "An upstream CMake licence guard stops the wheel build until a variable \
                      confirms the FFmpeg that is linked is licence-compatible. That is a fact \
                      to check, not a reason to give up."
                .to_string(),
            action: "Follow convention 78: read the licence lines of the `ffmpeg-<N>` recipe \
                     (the prompt's \"Licence facts\" section has them), and when it is \
                     LGPL-only `export I_CONFIRM_THIS_IS_NOT_A_LICENSE_VIOLATION=1` in `%build` \
                     with the `# licence-guard-verified:` comment; when it is GPL, report \
                     `ffmpeg-<N>` in `patch_dependencies` for a licence-clean copy. Never use the \
                     S3 download option (no network) and never return `blocked`."
                .to_string(),
            evidence: vec![short(&line)],
            kind: CauseKind::KnownRecipe {
                id: "cmake-licence-guard",
            },
        });
    }
    // A submodule of the CUTLASS DSL missing although `cutlass` imports: the
    // split `nvidia-cutlass-dsl-libs-*` wheels are not in one site tree.
    if let Some(line) = find(&SPLIT_TREE_SUBMODULE) {
        return Some(ProbableCause {
            summary: "The `cutlass` package imports but one of its submodules is missing: the \
                      split `nvidia-cutlass-dsl-libs-*` wheels (core, base, cu12, cu13) share the \
                      namespace `nvidia_cutlass_dsl/`, and `nvidia-cutlass-dsl`'s `.pth` takes the \
                      FIRST site directory that holds it, so a tree split between lib and lib64 \
                      loses `cutlass/cute`."
                .to_string(),
            action: "The defect is in the sibling packages, not in this spec and not a \
                     `PYTHONPATH` workaround in `%check`: every `python-nvidia-cutlass-dsl-libs-*` \
                     spec must install into `%{python_sitelib}` (convention 83; the pipeline moves \
                     a sibling generated alone onto that tree). Report the sibling in \
                     `patch_dependencies` if its spec still uses `%{python_sitearch}`; otherwise \
                     wait for the rebuilt siblings."
                .to_string(),
            evidence: vec![short(&line)],
            kind: CauseKind::KnownRecipe {
                id: "cutlass-split-tree",
            },
        });
    }
    None
}

/// Recognise the cause of the failure in `log`, if it is one of the mechanical
/// shapes. `own_module` is this package's normalized import name (so its own
/// module failing to import is told apart from a dependency), and `spec` is
/// the current spec text (to tell "not declared" from "declared but absent").
pub fn detect(log: &ExtractedLog, own_module: &str, spec: &str) -> Option<ProbableCause> {
    let haystack: Vec<&str> = log
        .error_lines
        .iter()
        .map(String::as_str)
        .chain(log.excerpt.lines())
        // When no phase was located the excerpt is empty; the raw tail still
        // holds the failure.
        .chain(log.log_tail.lines())
        .collect();
    let section = log.effective_section().to_string();

    if let Some(known) = known_recipe(&haystack) {
        return Some(known);
    }

    // Many failing tests: the cause is the test environment as a whole, and
    // any single missing module is just one row of it.
    if let Some(digest) = pytest_digest(&haystack) {
        return Some(digest);
    }

    // A library that cannot be loaded for want of a symbol: the ABI of a
    // dependency differs from the one it was built for. This is not a missing
    // package, and dropping the dependency's upper bound hides it.
    if let Some(line) = haystack.iter().find(|l| UNDEFINED_SYMBOL.is_match(l)) {
        let symbol = UNDEFINED_SYMBOL
            .captures(line)
            .map(|c| c["s"].to_string())
            .unwrap_or_default();
        return Some(ProbableCause {
            summary: format!(
                "A shared library fails to load: `undefined symbol: {symbol}`. It was built \
                 against one release of a library it links and is run against another; \
                 nothing is missing from the repositories."
            ),
            action: "Find which dependency's release removed or changed the symbol (the \
                     package's own metadata bounds that dependency: an upper bound such as \
                     `<= 0.1.11` is the hint). Keep the bound. If the distro ships a newer \
                     release, report that dependency in `patch_dependencies` (convention 62) \
                     with this error line, the symbol, the version that still had it, the \
                     version that dropped it and the upstream repository, so it is branched \
                     and the code restored from upstream's history. Do not drop the bound to \
                     make the error go away."
                .to_string(),
            evidence: vec![line.trim().chars().take(300).collect()],
            kind: CauseKind::UndefinedSymbol { symbol },
        });
    }

    // Python import of something that is not installed.
    if let Some(module) = haystack
        .iter()
        .find_map(|l| NO_MODULE.captures(l).map(|c| c["m"].to_string()))
    {
        let top = module.split('.').next().unwrap_or(&module).to_string();
        let error_line = haystack
            .iter()
            .find(|l| NO_MODULE.is_match(l))
            .map(|l| l.trim().to_string())
            .unwrap_or_default();
        let mut evidence = vec![error_line];
        evidence.extend(import_site(&log.python_frames));
        evidence.push(format!("failed in {section}"));

        if REMOVED_STDLIB.contains(&top.as_str()) {
            return Some(ProbableCause {
                summary: format!(
                    "`{top}` was removed from the standard library in the Python this build \
                     runs; nothing is missing from the repositories."
                ),
                action: "Follow convention 38: patch the code (quilt) to the replacement, or \
                         add the PyPI backport as a BuildRequires if one exists. Do not report \
                         it as an unresolvable package."
                    .to_string(),
                kind: CauseKind::RemovedStdlibModule { module: top },
                evidence,
            });
        }
        if !own_module.is_empty() && normalize(&top) == normalize(own_module) {
            return Some(ProbableCause {
                summary: format!(
                    "The package's own module `{top}` cannot be imported — the tests ran against \
                     a tree where it is not installed or not on the path."
                ),
                action: "Run the tests from a fresh directory with \
                         `PYTHONPATH=%{buildroot}%{$python_sitelib}` (sitearch for arch \
                         packages) inside `%python_expand`, and check %install actually put the \
                         module there (convention 48)."
                    .to_string(),
                kind: CauseKind::OwnModuleNotImportable { module: top },
                evidence,
            });
        }
        let requirement = requirement_for(&top);
        let declared = spec_declares(spec, &requirement) || spec_declares(spec, &top);
        let (summary, action) = if declared {
            (
                format!(
                    "`{top}` is imported but not installed, although the spec already lists \
                     `{requirement}`: the dependency is not reaching the build (wrong name, \
                     flavor gap, or an extras/marker problem)."
                ),
                format!(
                    "Check the OBS build log for how `{requirement}` resolved (package \
                     installed for this flavor?). Verify the PyPI name; the import name \
                     `{top}` may differ from the distribution name."
                ),
            )
        } else {
            (
                format!(
                    "Third-party module `{top}` is imported by the {section} step but is not a \
                     BuildRequires."
                ),
                format!(
                    "Add `BuildRequires:  %{{python_module {requirement}}}` (verify the PyPI \
                     name — the import name is `{top}`) together with EVERY other third-party \
                     module the tests import in the same edit (convention 10). If OBS then \
                     cannot resolve it, report it in `unresolvable_packages` so it is built."
                ),
            )
        };
        return Some(ProbableCause {
            kind: CauseKind::MissingPythonModule {
                module: top,
                requirement,
            },
            summary,
            action,
            evidence,
        });
    }

    if let Some(header) = haystack
        .iter()
        .find_map(|l| HEADER.captures(l).map(|c| c["h"].to_string()))
    {
        return Some(ProbableCause {
            summary: format!("The compiler cannot find the header `{header}`."),
            action: format!(
                "`zypper search --provides --match-exact \"/usr/include/{header}\"` names the \
                 -devel package; BuildRequire that (not a guessed name), or report the library \
                 in `unresolvable_packages` only if the distro has none (convention 27)."
            ),
            evidence: haystack
                .iter()
                .filter(|l| l.contains("fatal error:"))
                .take(2)
                .map(|l| l.trim().to_string())
                .collect(),
            kind: CauseKind::MissingHeader { header },
        });
    }

    if let Some(package) = haystack
        .iter()
        .find_map(|l| CMAKE_PKG.captures(l).map(|c| c["p"].to_string()))
    {
        return Some(ProbableCause {
            summary: format!("CMake cannot find the package `{package}`."),
            action: format!(
                "`zypper search --provides --match-exact 'cmake({package})'` (and \
                 `'pkgconfig({package})'`) names the -devel package to BuildRequire; check the \
                 project's CMake options for a switch that disables the dependency."
            ),
            evidence: haystack
                .iter()
                .filter(|l| l.contains("Could not find a package configuration file"))
                .take(1)
                .map(|l| l.trim().to_string())
                .collect(),
            kind: CauseKind::MissingCMakePackage { package },
        });
    }

    if let Some(package) = haystack.iter().find_map(|l| {
        PKGCONFIG
            .captures(l)
            .and_then(|c| c.name("a").or(c.name("b")).map(|m| m.as_str().to_string()))
    }) {
        return Some(ProbableCause {
            summary: format!("pkg-config cannot find `{package}`."),
            action: format!(
                "`zypper search --provides --match-exact 'pkgconfig({package})'` gives the \
                 package; add `BuildRequires:  pkgconfig({package})`."
            ),
            evidence: Vec::new(),
            kind: CauseKind::MissingPkgConfig { package },
        });
    }

    if let Some(command) = haystack.iter().find_map(|l| {
        CMD_NOT_FOUND
            .captures(l)
            .or_else(|| PY_NO_FILE_CMD.captures(l))
            .map(|c| c["c"].to_string())
    }) {
        return Some(ProbableCause {
            summary: format!("The build runs `{command}` and it is not installed."),
            action: format!(
                "`zypper search --provides --match-exact /usr/bin/{command}` names the package \
                 that ships it; BuildRequire that exact package — plain `-devel` or `-mini` \
                 variants often lack the tool."
            ),
            evidence: haystack
                .iter()
                .filter(|l| l.contains(&command))
                .take(2)
                .map(|l| l.trim().to_string())
                .collect(),
            kind: CauseKind::MissingCommand { command },
        });
    }

    None
}

impl ProbableCause {
    /// The prompt section: lead with the diagnosis so the agent acts on it
    /// instead of re-deriving it from the log.
    pub fn render(&self) -> String {
        let mut s = String::from("## Probable cause (read from the build log by auto-package)\n\n");
        s.push_str(&self.summary);
        s.push_str("\n\n");
        if !self.evidence.is_empty() {
            s.push_str("Evidence:\n");
            for e in &self.evidence {
                s.push_str("  ");
                s.push_str(e);
                s.push('\n');
            }
            s.push('\n');
        }
        s.push_str("Do this first: ");
        s.push_str(&self.action);
        s.push_str(
            "\n\nThis was extracted mechanically; you do not need to read build.log to confirm \
             it. Only look further if the evidence above does not fit what you see in the spec.\n\n",
        );
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::log::PathMapper;

    #[test]
    fn a_cython_limited_api_error_gets_the_floor_recipe() {
        let log = log_from(&[
            "Executing(%build): /usr/bin/bash -e /var/tmp/rpm-tmp.abc",
            "tilelang_cython_wrapper.cpp:1:2: error: #error \"Cython 3.3 requires the Python Limited API version to be 3.9 or greater.\"",
            "error: Bad exit status from /var/tmp/rpm-tmp.abc (%build)",
        ]);
        let c = detect(&log, "tilelang", "").expect("recognised");
        assert_eq!(
            c.kind,
            CauseKind::KnownRecipe {
                id: "cython-limited-api"
            }
        );
        assert!(c.action.contains("cp39"), "{}", c.action);
        assert!(c.action.contains("sed -i"), "{}", c.action);
    }

    #[test]
    fn a_cmake_licence_guard_points_to_convention_78() {
        let log = log_from(&[
            "Executing(%build): /usr/bin/bash -e /var/tmp/rpm-tmp.abc",
            "CMake Error at src/torchcodec/_core/CMakeLists.txt:13 (message):",
            "  set BUILD_AGAINST_ALL_FFMPEG_FROM_S3 or I_CONFIRM_THIS_IS_NOT_A_LICENSE_VIOLATION",
            "-- Configuring incomplete, errors occurred!",
            "error: Bad exit status from /var/tmp/rpm-tmp.abc (%build)",
        ]);
        let c = detect(&log, "torchcodec", "").expect("recognised");
        assert_eq!(
            c.kind,
            CauseKind::KnownRecipe {
                id: "cmake-licence-guard"
            }
        );
        assert!(c.action.contains("convention 78"), "{}", c.action);
    }

    #[test]
    fn a_missing_cutlass_submodule_is_the_split_tree() {
        let log = log_from(&[
            "Executing(%check): /usr/bin/bash -e /var/tmp/rpm-tmp.abc",
            "    import cutlass.cute as cute",
            "ModuleNotFoundError: No module named 'cutlass.cute'",
            "error: Bad exit status from /var/tmp/rpm-tmp.abc (%check)",
        ]);
        let c = detect(&log, "tokenspeed_mla", "").expect("recognised");
        assert_eq!(
            c.kind,
            CauseKind::KnownRecipe {
                id: "cutlass-split-tree"
            }
        );
        assert!(c.action.contains("%{python_sitelib}"), "{}", c.action);
    }

    #[test]
    fn a_file_named_like_a_network_word_is_not_network() {
        let msg = "FileNotFoundError: [Errno 2] No such file or directory: 'connect_fixture.csv'";
        assert_eq!(classify_message(msg), "missing_file");
        assert_eq!(
            classify_message("ModuleNotFoundError: No module named 'socket_helpers'"),
            "missing_module"
        );
        assert_eq!(
            classify_message("networkx.exception.NetworkXError: graph is empty"),
            "other"
        );
    }

    #[test]
    fn real_network_failures_are_network() {
        for msg in [
            "ConnectionRefusedError: [Errno 111] Connection refused",
            "ConnectionResetError: [Errno 104] Connection reset by peer",
            "socket.timeout: timed out",
            "socket.gaierror: [Errno -2] Name or service not known",
            "[Errno -3] Temporary failure in name resolution",
            "ssl.SSLError: [SSL: CERTIFICATE_VERIFY_FAILED]",
            "httpx2.ConnectError: boom",
        ] {
            assert_eq!(classify_message(msg), "network", "{msg}");
        }
    }

    /// The Cython recipe's line starts past character 230, so its text is
    /// cut off by the 300-character limit. The match must still be found, and
    /// the later recipe's line must still be reachable.
    #[test]
    fn a_long_line_does_not_hide_the_recipes_after_it() {
        let cython = format!(
            "{}Cython 3.3 requires the Python Limited API version to be 3.8 or greater",
            "x".repeat(230)
        );
        assert!(cython.len() > 300);
        let haystack = [
            cython.as_str(),
            "CMake Error at CMakeLists.txt:3 (message):",
            "  set I_CONFIRM_THIS_IS_NOT_A_LICENSE_VIOLATION",
        ];
        let c = known_recipe(&haystack).expect("recognised");
        assert_eq!(
            c.kind,
            CauseKind::KnownRecipe {
                id: "cython-limited-api"
            }
        );
        assert!(c.evidence[0].chars().count() <= 300);

        // Without the Cython line, the licence guard further down is found.
        let haystack = [
            "CMake Error at CMakeLists.txt:3 (message):",
            "  set I_CONFIRM_THIS_IS_NOT_A_LICENSE_VIOLATION",
        ];
        let c = known_recipe(&haystack).expect("recognised");
        assert_eq!(
            c.kind,
            CauseKind::KnownRecipe {
                id: "cmake-licence-guard"
            }
        );
    }

    #[test]
    fn truncated_match_rejects_short_fragments() {
        assert!(truncated_match("connectione...", "connectionerror"));
        assert!(!truncated_match("res...", "resolutionimpossible"));
        assert!(
            !truncated_match("res...", "resolutionimpossible")
                && classify_message("res...") != "network"
        );
    }

    fn log_from(lines: &[&str]) -> ExtractedLog {
        let text = lines
            .iter()
            .enumerate()
            .map(|(i, l)| format!("[{:>4}s] {l}", i))
            .collect::<Vec<_>>()
            .join("\n");
        crate::log::extract(&text, &PathMapper::with_sdist_root("accelerate-1.0"))
    }

    fn accelerate_log() -> ExtractedLog {
        log_from(&[
            "Executing(%check): /usr/bin/bash -e /var/tmp/rpm-tmp.abc",
            "+ pytest",
            "Traceback (most recent call last):",
            "  File \"/home/abuild/rpmbuild/BUILD/python-accelerate-1.0-build/accelerate-1.0/tests/test_big_modeling.py\", line 20, in <module>",
            "    from transformers import AutoModel",
            "ModuleNotFoundError: No module named 'transformers'",
            "error: Bad exit status from /var/tmp/rpm-tmp.abc (%check)",
        ])
    }

    /// The case that prompted this module.
    #[test]
    fn a_test_importing_an_undeclared_module_is_a_missing_build_requires() {
        let spec = "BuildRequires:  %{python_module pytest}\n";
        let c = detect(&accelerate_log(), "accelerate", spec).expect("recognised");
        assert_eq!(
            c.kind,
            CauseKind::MissingPythonModule {
                module: "transformers".into(),
                requirement: "transformers".into()
            }
        );
        assert!(
            c.action.contains("%{python_module transformers}"),
            "{}",
            c.action
        );
        assert!(
            c.action.contains("EVERY other third-party module"),
            "{}",
            c.action
        );
        assert!(
            c.evidence
                .iter()
                .any(|e| e.contains("No module named 'transformers'")),
            "{:?}",
            c.evidence
        );
        assert!(
            c.evidence
                .iter()
                .any(|e| e.contains("tests/test_big_modeling.py:20")),
            "{:?}",
            c.evidence
        );
        let rendered = c.render();
        assert!(rendered.starts_with("## Probable cause"), "{rendered}");
        assert!(
            rendered.contains("you do not need to read build.log"),
            "{rendered}"
        );
    }

    #[test]
    fn a_module_the_spec_already_declares_points_at_resolution_not_a_new_line() {
        let spec = "BuildRequires:  %{python_module transformers >= 4.30}\n";
        let c = detect(&accelerate_log(), "accelerate", spec).unwrap();
        assert!(c.summary.contains("already lists"), "{}", c.summary);
        assert!(!c.action.starts_with("Add `BuildRequires"), "{}", c.action);
    }

    #[test]
    fn import_names_map_to_their_distribution_names() {
        let log = log_from(&[
            "Executing(%check): /usr/bin/bash -e /var/tmp/x",
            "ModuleNotFoundError: No module named 'yaml'",
            "error: Bad exit status from /var/tmp/x (%check)",
        ]);
        match detect(&log, "foo", "").unwrap().kind {
            CauseKind::MissingPythonModule { requirement, .. } => assert_eq!(requirement, "PyYAML"),
            k => panic!("{k:?}"),
        }
    }

    #[test]
    fn the_packages_own_module_failing_is_a_path_problem() {
        let log = log_from(&[
            "Executing(%check): /usr/bin/bash -e /var/tmp/x",
            "ModuleNotFoundError: No module named 'nvidia'",
            "error: Bad exit status from /var/tmp/x (%check)",
        ]);
        let c = detect(&log, "nvidia", "").unwrap();
        assert!(matches!(c.kind, CauseKind::OwnModuleNotImportable { .. }));
        assert!(c.action.contains("PYTHONPATH"));
    }

    #[test]
    fn a_removed_stdlib_module_is_not_a_missing_package() {
        let log = log_from(&[
            "Executing(%build): /usr/bin/bash -e /var/tmp/x",
            "ModuleNotFoundError: No module named 'distutils'",
            "error: Bad exit status from /var/tmp/x (%build)",
        ]);
        let c = detect(&log, "foo", "").unwrap();
        assert!(matches!(c.kind, CauseKind::RemovedStdlibModule { .. }));
        assert!(c.action.contains("convention 38"));
    }

    #[test]
    fn an_undefined_symbol_points_at_a_dependency_abi_not_a_missing_package() {
        let l = log_from(&[
            "OSError: /build/tilelang/lib/libtvm_runtime.so: undefined symbol: _ZN3tvm3ffi4json9StringifyERKN",
        ]);
        let cause = detect(&l, "tilelang", "").unwrap();
        match cause.kind {
            CauseKind::UndefinedSymbol { symbol } => assert!(symbol.starts_with("_ZN3tvm")),
            k => panic!("{k:?}"),
        }
        assert!(cause.action.contains("patch_dependencies"));
    }

    #[test]
    fn missing_headers_commands_and_cmake_packages_are_recognised() {
        let h = log_from(&["foo.c:1:10: fatal error: libpq-fe.h: No such file or directory"]);
        match detect(&h, "", "").unwrap().kind {
            CauseKind::MissingHeader { header } => assert_eq!(header, "libpq-fe.h"),
            k => panic!("{k:?}"),
        }
        let c = log_from(&["/var/tmp/rpm-tmp.x: line 9: ninja: command not found"]);
        match detect(&c, "", "").unwrap().kind {
            CauseKind::MissingCommand { command } => assert_eq!(command, "ninja"),
            k => panic!("{k:?}"),
        }
        let p = log_from(&["FileNotFoundError: [Errno 2] No such file or directory: 'pg_config'"]);
        match detect(&p, "", "").unwrap().kind {
            CauseKind::MissingCommand { command } => assert_eq!(command, "pg_config"),
            k => panic!("{k:?}"),
        }
        let m = log_from(&[
            "CMake Error at CMakeLists.txt:12 (find_package):",
            "  Could not find a package configuration file provided by \"MLIR\" with any of",
        ]);
        match detect(&m, "", "").unwrap().kind {
            CauseKind::MissingCMakePackage { package } => assert_eq!(package, "MLIR"),
            k => panic!("{k:?}"),
        }
    }

    #[test]
    fn an_unrecognised_failure_yields_no_cause() {
        let log = log_from(&[
            "Executing(%check): /usr/bin/bash -e /var/tmp/x",
            "AssertionError: 1 != 2",
            "error: Bad exit status from /var/tmp/x (%check)",
        ]);
        assert_eq!(detect(&log, "foo", ""), None);
    }

    /// python-accelerate's real %check: dozens of failures from four different
    /// environment causes, which the agent had been peeling off one per build.
    #[test]
    fn many_failing_tests_become_one_digest_with_a_single_plan() {
        let mut lines =
            vec!["Executing(%check): /usr/bin/bash -e /var/tmp/rpm-tmp.abc".to_string()];
        for t in ["a", "b", "c"] {
            lines.push(format!(
                "FAILED src/accelerate/test_utils/scripts/test_sync.py::test_{t} - AssertionError"
            ));
        }
        for t in ["a", "b", "c", "d"] {
            lines.push(format!(
                "FAILED tests/test_cli.py::TpuConfigTester::test_{t} - FileNotFoundError: x"
            ));
        }
        lines.push(
            "FAILED tests/test_cli.py::ModelEstimatorTester::test_gated - httpx2.ConnectError"
                .into(),
        );
        lines.push(
            "FAILED tests/test_utils.py::UtilsTester::test_fp32 - ModuleNotFoundError: triton"
                .into(),
        );
        lines.push("error: Bad exit status from /var/tmp/rpm-tmp.abc (%check)".into());
        let refs: Vec<&str> = lines.iter().map(String::as_str).collect();
        let log = log_from(&refs);
        let c = detect(&log, "accelerate", "").expect("digest");
        assert_eq!(c.kind, CauseKind::PytestFailures { failed: 9 });
        assert!(c.summary.contains("test ENVIRONMENT"), "{}", c.summary);
        assert!(c.action.contains("`%pytest tests/`"), "{}", c.action);
        assert!(c.action.contains("missing file or fixture"), "{}", c.action);
        assert!(c.action.contains("need network"), "{}", c.action);
        assert!(
            c.action.contains("EVERY missing third-party module"),
            "{}",
            c.action
        );
        assert!(
            c.evidence
                .iter()
                .any(|e| e.starts_with("tests/test_cli.py: 5 failed")),
            "{:?}",
            c.evidence
        );
    }

    /// pytest truncates its one-line failure summary to terminal width, so a
    /// long exception name often arrives as `FileN...` rather than in full.
    #[test]
    fn truncated_exception_names_still_classify() {
        let mut lines =
            vec!["Executing(%check): /usr/bin/bash -e /var/tmp/rpm-tmp.abc".to_string()];
        for (i, tail) in [
            "FileN...",
            "FileNo...",
            "FileNotFoun...",
            "FileNotFoundErro...",
        ]
        .into_iter()
        .enumerate()
        {
            lines.push(format!(
                "FAILED tests/test_cli.py::Tester::test_{i} - {tail}"
            ));
        }
        lines.push("FAILED tests/test_cli.py::Tester::test_net - httpx2.ConnectEr...".into());
        lines.push("error: Bad exit status from /var/tmp/rpm-tmp.abc (%check)".into());
        let refs: Vec<&str> = lines.iter().map(String::as_str).collect();
        let log = log_from(&refs);
        let c = detect(&log, "accelerate", "").expect("digest");
        assert!(
            c.evidence
                .iter()
                .any(|e| e.contains("4 missing file/fixture") && e.contains("1 need network")),
            "{:?}",
            c.evidence
        );
    }

    #[test]
    fn a_handful_of_failures_is_not_a_digest() {
        let log = log_from(&[
            "Executing(%check): /usr/bin/bash -e /var/tmp/x",
            "FAILED tests/test_a.py::test_x - AssertionError",
            "FAILED tests/test_a.py::test_y - AssertionError",
            "error: Bad exit status from /var/tmp/x (%check)",
        ]);
        assert_eq!(detect(&log, "foo", ""), None);
    }
}
