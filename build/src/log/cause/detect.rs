use super::*;

/// Failures that have one known recipe, each seen in a real run: the cause and
/// the exact fix, so the first attempt is right.
pub(super) fn known_recipe(haystack: &[&str]) -> Option<ProbableCause> {
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
                action: "Follow convention 116: patch the code (quilt) to the replacement, or \
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
