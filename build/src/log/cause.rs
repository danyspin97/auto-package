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
}
