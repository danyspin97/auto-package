use super::*;

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

pub(super) static UNDEFINED_SYMBOL: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"undefined symbol: (?<s>[A-Za-z0-9_@.$]+)").expect("static pattern")
});

pub(super) static NO_MODULE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?:ModuleNotFoundError|ImportError): No module named '(?<m>[A-Za-z0-9_.]+)'")
        .expect("static pattern")
});

pub(super) static HEADER: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"fatal error: (?<h>[A-Za-z0-9_./+-]+\.(?:h|hpp|hh|hxx)): No such file or directory")
        .expect("static pattern")
});

pub(super) static CMD_NOT_FOUND: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?:^|[\s:])(?<c>[A-Za-z0-9_.+-]+): (?:command not found|not found)\s*$")
        .expect("static pattern")
});

pub(super) static PY_NO_FILE_CMD: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"FileNotFoundError: \[Errno 2\] No such file or directory: '(?<c>[A-Za-z0-9_.+-]+)'",
    )
    .expect("static pattern")
});

pub(super) static CMAKE_PKG: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r#"Could not find a package configuration file provided by "(?<p>[A-Za-z0-9_.+-]+)""#,
    )
    .expect("static pattern")
});

pub(super) static PKGCONFIG: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?:No package '(?<a>[A-Za-z0-9_.+-]+)' found|Package '(?<b>[A-Za-z0-9_.+-]+)', required by .*, not found)")
        .expect("static pattern")
});

pub(super) static PYTEST_LINE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^(?<kind>FAILED|ERROR) (?<path>[^\s:]+)(?:::\S+)?(?: - (?<msg>.*))?$")
        .expect("static pattern")
});

/// At least this many FAILED/ERROR lines before the failure is treated as "a
/// broken test environment" (digest) rather than one specific test.
pub(super) const DIGEST_THRESHOLD: usize = 5;

#[derive(Default)]
pub(super) struct FileGroup {
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
pub(super) fn truncated_match(msg: &str, needle: &str) -> bool {
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

pub(super) fn classify_message(msg: &str) -> &'static str {
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
pub(super) fn pytest_digest(haystack: &[&str]) -> Option<ProbableCause> {
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
             `SourceN` (convention 11 (section 44)) or ignore those files — do not patch the tests."
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
