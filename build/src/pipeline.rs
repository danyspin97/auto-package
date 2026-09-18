//! Single-package build pipeline (REDESIGN §2.4, §3.2).
//!
//! Loop: Checkout → FetchSdist → GenerateSpec → ValidateSpec →
//!       (Build → ExtractFailure → AgentTurn → ApplyAndValidate)* → Done

use std::path::{Path, PathBuf};
use std::time::Instant;

use ap_core::event::{Event, SelectionReason, StepKind, StepOutcome};
use ap_core::run::{PkgRunDir, StepDir};
use ap_core::sink::Reporter;

use crate::proc::Emit;

use crate::osc;
use crate::workspace::Workspace;

// ── public types ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct PipelineConfig {
    pub pkg_name: String,
    /// The openSUSE package name. Derived from `pkg_name` for PyPI packages,
    /// but a repository decides it once its manifest has been read — a Rust
    /// crate takes no `python-` prefix.
    pub rpm_name: String,
    pub version: String,
    pub project: String,
    /// Canonicalized work directory.
    pub work_dir: PathBuf,
    /// `work_dir/state/`
    pub state_root: PathBuf,
    /// `work_dir/rpms/`
    pub rpms_dir: PathBuf,
    pub distro: String,
    pub arch: String,
    pub max_attempts: usize,
    pub budget_usd: f64,
    pub agent_backend: AgentBackend,
    pub no_agent: bool,
    /// Rebuild even if state.json says already built.
    pub force: bool,
    /// Regenerate spec from scratch.
    pub regen: bool,
    /// Build from this source repository instead of a PyPI sdist. Whether it
    /// holds a Rust crate or a Python project is decided by looking inside the
    /// release archive, not by which flag was used.
    pub source_repo: Option<String>,
    /// Sibling projects inside a monorepo source, as `(directory, name)`.
    /// Discovered when the spec is generated and shown to the agent, which
    /// decides how many of them to package.
    pub subprojects: Vec<(String, String)>,
    /// RPM names of dependencies this run already tried to build and failed
    /// to. Shown to the agent as the evidence that justifies working around
    /// them rather than queueing them again.
    pub failed_deps: Vec<String>,
    /// User-supplied source repositories, for packages PyPI ships as wheels
    /// with no repository in their metadata. Shared across the whole run so a
    /// dependency discovered mid-run picks up its override too.
    pub source_urls: std::sync::Arc<crate::sources::SourceMap>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentBackend {
    Cli,
}

impl Default for AgentBackend {
    fn default() -> Self {
        Self::Cli
    }
}

#[derive(Debug)]
pub enum PipelineResult {
    Built { rpm_name: String },
    AlreadyBuilt { rpm_name: String },
    MissingDeps { rpm_name: String, deps: Vec<String> },
    Failed { rpm_name: String, reason: String },
}

// ── durable package state ─────────────────────────────────────────────────────

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, Default)]
pub struct PkgState {
    pub last_built_version: Option<String>,
    pub last_outcome: Option<String>,
}

impl PkgState {
    pub fn load(ws: &Workspace) -> Self {
        std::fs::read_to_string(ws.state_json())
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    pub fn save(&self, ws: &Workspace) -> std::io::Result<()> {
        let s = serde_json::to_string_pretty(self).unwrap_or_default();
        std::fs::write(ws.state_json(), s)
    }

    pub fn is_built(&self, version: &str) -> bool {
        self.last_built_version.as_deref() == Some(version)
            && matches!(self.last_outcome.as_deref(), Some("success") | Some("wheel-only"))
    }
}

// ── target selection ─────────────────────────────────────────────────────────

pub struct TargetInfo {
    /// PyPI display name (as returned by the API, e.g. `MarkupSafe`).
    pub name: String,
    pub version: String,
    pub reason: SelectionReason,
    pub blocked_by: Vec<String>,
}

/// Resolve the dependency closure and return all currently buildable targets
/// (the "frontier" — nodes whose declared deps in the closure are all resolved).
///
/// Uses `pip install --dry-run` once to get the full runtime closure. A node
/// is "resolvable" if it is already built locally (state.json) or present in
/// the OBS project. A node is in the frontier if it is not yet resolvable and
/// every package it declares in `requires_dist` that also appears in the
/// closure is already resolvable.
///
/// `pinned_version` is the user's `--version`; it constrains the resolve so a
/// pin actually pins rather than being overwritten by whatever pip picks.
///
/// Returns `Ok(vec![])` when every package in the closure is already resolvable.
pub fn select_target(
    name: &str,
    explicit_target: Option<&str>,
    // Pin the root package to this version, if the user asked for one.
    pinned_version: Option<&str>,
    config: &PipelineConfig,
    reporter: &dyn Reporter,
) -> Result<Vec<TargetInfo>, String> {
    reporter.report(&Event::ResolveStarted { root: name.to_string() });

    // Kick off the package availability fetch concurrently with pip resolve.
    // Checks both the user's own project and all configured build/path projects
    // (from osc meta prj) — the union is the complete set of resolvable packages.
    let obs_project = config.project.clone();
    let (nodes, available_packages) = std::thread::scope(|scope| {
        let handle = scope.spawn(|| {
            osc::get_available_packages(&obs_project, Emit::run(reporter))
        });
        let nodes = ap_ecosystem::pip::resolve(name, pinned_version);
        // Join the fetch (likely already done while pip was resolving).
        (nodes, handle.join().unwrap_or_default())
    });
    let nodes = nodes.map_err(|e| format!("resolve failed: {e}"))?;

    let total = nodes.len();

    // A node is resolvable if present in the user's project or any build
    // project, OR if its local state.json records a prior "wheel-only" or
    // "success" outcome — wheel-only packages can't be built from source and
    // should not be re-selected on every run.
    let resolvable: Vec<bool> = nodes
        .iter()
        .map(|n| {
            let rpm = osc::rpm_name(&n.name);
            if available_packages.contains(&rpm) {
                return true;
            }
            // Check local workspace state.
            let ws_name = ap_core::run::sanitize_name(&rpm);
            let state_path = config.state_root.join(ws_name).join("state.json");
            std::fs::read_to_string(&state_path)
                .ok()
                .and_then(|s| serde_json::from_str::<PkgState>(&s).ok())
                .map(|st| st.is_built(&n.version))
                .unwrap_or(false)
        })
        .collect();

    let already_built_count = resolvable.iter().filter(|&&r| r).count();
    let remaining = total - already_built_count;

    reporter.report(&Event::ResolveFinished {
        total,
        built: already_built_count,
        remaining,
    });

    // Explicit target override — skip the resolvability gate.
    if let Some(target) = explicit_target {
        let node = nodes
            .iter()
            .find(|n| n.name.eq_ignore_ascii_case(target))
            .ok_or_else(|| format!("package '{target}' not in dependency closure"))?
            .clone();
        return Ok(vec![TargetInfo {
            name: node.name,
            version: node.version,
            reason: SelectionReason::ExplicitOverride,
            blocked_by: vec![],
        }]);
    }

    // All done.
    if resolvable.iter().all(|&r| r) {
        return Ok(vec![]);
    }

    let root_idx = nodes.iter().position(|n| n.is_requested).unwrap_or(0);

    // Build a normalized name → index map for dep lookups.
    let name_to_idx: std::collections::HashMap<String, usize> = nodes
        .iter()
        .enumerate()
        .map(|(i, n)| (normalize_pkg_name(&n.name), i))
        .collect();

    // Collect the full frontier: nodes that are not yet resolvable but whose
    // declared deps (within the closure) are all resolvable.
    let mut frontier: Vec<TargetInfo> = Vec::new();
    for (i, node) in nodes.iter().enumerate() {
        if resolvable[i] {
            continue;
        }

        let deps_satisfied = dep_names_from_requires_dist(&node.requires_dist)
            .into_iter()
            .all(|dep| {
                name_to_idx
                    .get(&dep)
                    .map(|&j| resolvable[j])
                    .unwrap_or(true) // not in closure → provided by system
            });

        if !deps_satisfied {
            continue;
        }

        let reason = if i == root_idx {
            SelectionReason::RequestedRoot
        } else {
            SelectionReason::FirstUnbuiltDependency
        };

        frontier.push(TargetInfo {
            name: node.name.clone(),
            version: node.version.clone(),
            reason,
            blocked_by: vec![],
        });
    }

    if frontier.is_empty() {
        // Nothing buildable but packages remain — dependency cycle or unresolvable.
        let blocked: Vec<String> = nodes
            .iter()
            .enumerate()
            .filter(|(i, _)| !resolvable[*i])
            .map(|(_, n)| n.name.clone())
            .collect();
        return Err(format!(
            "cannot build {name}: stuck — unresolvable packages: {}",
            blocked.join(", ")
        ));
    }

    Ok(frontier)
}

/// Normalize a PyPI package name to a consistent lookup key (lowercase, `-` → `_`).
fn normalize_pkg_name(name: &str) -> String {
    name.to_lowercase().replace('-', "_")
}

/// Extract bare package names from `requires_dist` lines.
///
/// Skips extras-only lines (`; extra == ...`). For all other lines, the name
/// is the portion before any version specifier, extras bracket, env marker,
/// or whitespace.
fn dep_names_from_requires_dist(requires_dist: &[String]) -> Vec<String> {
    requires_dist
        .iter()
        .filter_map(|r| {
            // Skip optional (extras-gated) deps.
            if r.contains("extra ==") || r.contains("extra==") {
                return None;
            }
            let name = r
                .split(|c: char| {
                    c == '(' || c == '[' || c == ';' || c == '>' || c == '<' || c == '='
                        || c == '!' || c == ' '
                })
                .next()?
                .trim();
            if name.is_empty() { None } else { Some(normalize_pkg_name(name)) }
        })
        .collect()
}

// ── main pipeline ─────────────────────────────────────────────────────────────

pub fn run_package(
    config: &PipelineConfig,
    pkg_run: &PkgRunDir,
    reporter: &dyn Reporter,
) -> PipelineResult {
    let rpm_name = config.rpm_name.clone();
    let ws = match Workspace::open(&config.state_root, &rpm_name) {
        Ok(w) => w,
        Err(e) => {
            return PipelineResult::Failed {
                rpm_name,
                reason: e.to_string(),
            };
        }
    };

    if !config.force && PkgState::load(&ws).is_built(&config.version) {
        return PipelineResult::AlreadyBuilt { rpm_name };
    }

    let checkout_dir = config.work_dir.join(&config.project).join(&rpm_name);
    let spec_name = format!("{rpm_name}.spec");
    let spec_path = checkout_dir.join(&spec_name);

    // ── Step: Checkout ───────────────────────────────────────────────────────
    {
        let step = step_or_fail!(pkg_run, "checkout", rpm_name);
        let t = Instant::now();
        reporter.report(&Event::StepStarted { step: step.id, kind: StepKind::Checkout });

        let emit = Emit::new(reporter, step.id);
        let result = osc::ensure_package(&config.project, &rpm_name, &config.work_dir, emit)
            .and_then(|()| {
                osc::checkout(&config.project, &rpm_name, &config.work_dir, emit).map(|_| ())
            })
            .and_then(|()| {
                let cd = config.work_dir.join(&config.project).join(&rpm_name);
                osc::pull(&cd, emit)
            });

        match result {
            Ok(()) => {
                reporter.report(&Event::StepFinished {
                    step: step.id,
                    outcome: StepOutcome::Ok,
                    elapsed: t.elapsed(),
                });
            }
            Err(e) => {
                reporter.report(&Event::StepFinished {
                    step: step.id,
                    outcome: StepOutcome::Failed,
                    elapsed: t.elapsed(),
                });
                reporter.report(&Event::Warning {
                    step: Some(step.id),
                    message: e.to_string(),
                });
                return PipelineResult::Failed {
                    rpm_name,
                    reason: e.to_string(),
                };
            }
        }
        // Keep spec symlink fresh.
        let abs = spec_path
            .canonicalize()
            .unwrap_or_else(|_| spec_path.clone());
        let _ = ws.link_spec(&abs);

        // Fetch the previous build log if OBS reports a failed build.
        // This is non-fatal — the pre-fix step uses ws.build_log() if present.
        if let Ok(status) = osc::get_build_status(
            &config.project,
            &rpm_name,
            &config.distro,
            &config.arch,
            emit,
        ) && matches!(status, osc::BuildStatus::Failed)
        {
            let log_path = ws.build_log();
            let _ = osc::fetch_buildlog(
                &config.project,
                &rpm_name,
                &config.distro,
                &config.arch,
                &log_path,
                emit,
            );
        }
    }

    // ── Step: GenerateSpec (if needed) ───────────────────────────────────────
    let need_spec =
        config.regen || !spec_path.exists() || spec_version_differs(&spec_path, &config.version);

    let (sdist_top_level, is_maturin, from_repo_archive, is_rust) = if need_spec {
        let step = step_or_fail!(pkg_run, "spec", rpm_name);
        let t = Instant::now();
        reporter.report(&Event::StepStarted { step: step.id, kind: StepKind::GenerateSpec });

        match generate_spec(config, &checkout_dir, &spec_path, &step) {
            Ok(res) => {
                reporter.report(&Event::StepFinished {
                    step: step.id,
                    outcome: StepOutcome::Ok,
                    elapsed: t.elapsed(),
                });
                // Create a .changes entry for new packages.
                let _ = osc::create_initial_changes(
                    &checkout_dir,
                    &rpm_name,
                    Emit::new(reporter, step.id),
                );
                // Link spec.generated into the workspace so the review agent can compare.
                #[cfg(unix)]
                {
                    let src = step.file("spec.generated");
                    let link = ws.dir().join("spec.generated");
                    if link.symlink_metadata().is_ok() {
                        let _ = std::fs::remove_file(&link);
                    }
                    let _ = std::os::unix::fs::symlink(&src, &link);
                }
                (res.sdist_top_level, res.is_maturin, res.from_repo_archive, res.is_rust)
            }
            Err(GenSpecError::WheelOnly { pkg_name, version, candidates }) => {
                reporter.report(&Event::StepFinished {
                    step: step.id,
                    outcome: StepOutcome::Ok,
                    elapsed: t.elapsed(),
                });

                let mut message = format!(
                    "{pkg_name} {version} is published on PyPI as wheels only and its \
                     metadata names no source repository."
                );

                if !candidates.is_empty() {
                    message.push_str(&format!(
                        "\n\nAuto-discovered {} candidate repositories:\n{}",
                        candidates.len(),
                        candidates.iter()
                            .map(|url| format!("  - {}", url))
                            .collect::<Vec<_>>()
                            .join("\n")
                    ));
                    message.push_str(
                        "\n\nThese are suggestions - use WebSearch to verify the correct repository.\n\
                         Once confirmed, report the URL in your response."
                    );
                } else {
                    message.push_str(
                        "\n\nNo candidate repositories auto-discovered. Use WebSearch to find \
                         the source repository."
                    );
                }

                message.push_str(&format!(
                    "\n\nAlternatively, provide it manually with:\n\
                     \x20   --source-url {pkg_name}=https://github.com/<owner>/<repo>"
                ));

                reporter.report(&Event::Warning {
                    step: Some(step.id),
                    message: message.clone(),
                });

                // TODO: Call agent to validate candidates and find repository
                // For now, mark as wheel-only and skip
                let mut state = PkgState::load(&ws);
                state.last_built_version = Some(version);
                state.last_outcome = Some("wheel-only".to_string());
                let _ = state.save(&ws);
                return PipelineResult::Failed {
                    rpm_name,
                    reason: format!(
                        "wheel-only package - candidates: {}",
                        if !candidates.is_empty() {
                            candidates.join(", ")
                        } else {
                            "none found, needs WebSearch".to_string()
                        }
                    ),
                };
            }
            Err(GenSpecError::Other(e)) => {
                reporter.report(&Event::StepFinished {
                    step: step.id,
                    outcome: StepOutcome::Failed,
                    elapsed: t.elapsed(),
                });
                reporter.report(&Event::Warning {
                    step: Some(step.id),
                    message: e.clone(),
                });
                return PipelineResult::Failed { rpm_name, reason: e };
            }
        }
    } else {
        // Sdist may already be extracted; find top-level dir name from the tarball
        // in the checkout dir (or fall back to a reasonable guess).
        let top = find_sdist_top_level(&checkout_dir, &config.pkg_name, &config.version);
        let spec_text = std::fs::read_to_string(&spec_path).unwrap_or_default();
        let is_maturin = spec_text.contains("maturin");
        let is_rust = spec_text.contains("cargo_build");
        (top, is_maturin, false, is_rust)
    };

    // ── Step: OscService (Rust crates) ───────────────────────────────────────
    // `tar_scm` fetches the tag and `cargo_vendor` produces vendor.tar.zst, the
    // two sources the Rust spec template references. It has to run before the
    // commit, not before spec generation — the spec is a template either way.
    if is_rust && let Some(ref repo_url) = config.source_repo {
        let need_service = need_spec || !checkout_dir.join("_service").exists();
        if need_service {
            let step = step_or_fail!(pkg_run, "service", rpm_name);
            let t = Instant::now();
            reporter.report(&Event::StepStarted { step: step.id, kind: StepKind::OscService });

            let service_xml = render_service_file(repo_url, &config.version, &config.pkg_name);
            let result = std::fs::write(checkout_dir.join("_service"), &service_xml)
                .map_err(|e| format!("writing _service failed: {e}"))
                .and_then(|()| {
                    osc::run_service(&checkout_dir, Emit::new(reporter, step.id))
                        .map_err(|e| e.to_string())
                });

            match result {
                Ok(()) => reporter.report(&Event::StepFinished {
                    step: step.id,
                    outcome: StepOutcome::Ok,
                    elapsed: t.elapsed(),
                }),
                Err(e) => {
                    reporter.report(&Event::StepFinished {
                        step: step.id,
                        outcome: StepOutcome::Failed,
                        elapsed: t.elapsed(),
                    });
                    return PipelineResult::Failed { rpm_name, reason: e };
                }
            }
        }
    }

    // ── Step: OscService (maturin packages) ──────────────────────────────────
    if is_maturin {
        let need_service = need_spec || !checkout_dir.join("registry.tar.zst").exists();
        if need_service {
            let step = step_or_fail!(pkg_run, "service", rpm_name);
            let t = Instant::now();
            reporter.report(&Event::StepStarted { step: step.id, kind: StepKind::OscService });

            let normalized = config.pkg_name.replace('-', "_").to_lowercase();
            let service_xml = render_maturin_service_file(&normalized);
            let result = std::fs::write(checkout_dir.join("_service"), &service_xml)
                .map_err(|e| format!("writing _service failed: {e}"))
                .and_then(|()| {
                    osc::run_service(&checkout_dir, Emit::new(reporter, step.id))
                        .map_err(|e| e.to_string())
                });

            match result {
                Ok(()) => {
                    reporter.report(&Event::StepFinished {
                        step: step.id,
                        outcome: StepOutcome::Ok,
                        elapsed: t.elapsed(),
                    });
                }
                Err(e) => {
                    reporter.report(&Event::StepFinished {
                        step: step.id,
                        outcome: StepOutcome::Failed,
                        elapsed: t.elapsed(),
                    });
                    return PipelineResult::Failed { rpm_name, reason: e };
                }
            }
        }
    }

    // ── Step: FetchSdist (extract for log analysis, if not done) ─────────────
    if !ws.sdist_extracted() {
        let sdist_step = step_or_fail!(pkg_run, "sdist", rpm_name);
        let t = Instant::now();
        reporter.report(&Event::StepStarted { step: sdist_step.id, kind: StepKind::FetchSdist });

        // Find tarball in checkout and extract to ws.sdist_dir()
        if let Some(tarball) = find_tarball(&checkout_dir) {
            let sdist_dir = ws.sdist_dir();
            let _ = std::fs::create_dir_all(&sdist_dir);
            let _ = ap_ecosystem::sdist::extract(&tarball, &sdist_dir);
        }

        reporter.report(&Event::StepFinished {
            step: sdist_step.id,
            outcome: StepOutcome::Ok,
            elapsed: t.elapsed(),
        });
    }

    // ── Quilt setup: create source/ symlink ───────────────────────────────────
    // Keep this fresh every run so the symlink is correct even when the sdist
    // top-level dir changes (e.g. after --regen with a new version).
    #[cfg(unix)]
    {
        let source_link = ws.dir().join("source");
        if source_link.symlink_metadata().is_ok() {
            let _ = std::fs::remove_file(&source_link);
        }
        let source_target = ws.sdist_dir().join(&sdist_top_level);
        if source_target.exists() {
            let _ = std::os::unix::fs::symlink(&source_target, &source_link);
        }
    }

    // ── Build + fix loop ──────────────────────────────────────────────────────
    let mut attempts = 0usize;
    let mut spent_usd = 0.0f64;
    let mut session_id: Option<String> = None;

    let build_log_nonempty = ws
        .build_log()
        .metadata()
        .map(|m| m.len() > 0)
        .unwrap_or(false);

    // ── Step: Review (freshly generated specs, or existing spec with no prior build) ──
    if !config.no_agent && (need_spec || !build_log_nonempty) {
        let spec_before = std::fs::read_to_string(&spec_path).unwrap_or_default();

        let turn = agent_turn(
            TurnCtx {
                label: "review",
                task: crate::agent::Task::Review,
                event_task: ap_core::event::Task::Review,
                attempt: 0,
                hint: None,
                unresolvable_deps: &[],
                checkout_dir: Some(&checkout_dir),
                from_repo_archive,
                resume: false,
                context: if !need_spec {
                    Some(
                        "The build log is empty — OBS rejected the build during \
                         dependency resolution before any compilation started. \
                         The spec may reference a package that does not exist in \
                         OBS yet, or the BuildRequires names may be wrong.",
                    )
                } else {
                    None
                },
            },
            config,
            &ws,
            pkg_run,
            reporter,
            &mut spent_usd,
            &mut session_id,
        );

        if let Ok((review_step, outcome)) = turn {
            if let crate::agent::Action::Edited = &outcome.action {
                accept_or_restore(
                    &spec_path,
                    &spec_before,
                    &review_step,
                    reporter,
                    &format!(
                        "\n## Review — pre-build\nReasoning: {}\nConfidence: {:?}\n",
                        outcome.reasoning, outcome.confidence
                    ),
                    ws.notes(),
                );
            }
            if let crate::agent::Action::Blocked { why } = outcome.action {
                return PipelineResult::Failed {
                    rpm_name,
                    reason: format!("review blocked: {why}"),
                };
            }
        }
    }

    // ── Step: PreFix (restart with previous build log) ───────────────────────
    // When this is a restart (spec was not regenerated) and a previous non-empty
    // build log exists, run an agent turn to fix the spec before a new OBS build.
    if !need_spec && !config.no_agent && build_log_nonempty {
        let clean = strip_and_rewrite(&spec_path);
        let extract = extract_build_log(&ws.build_log(), &sdist_top_level, reporter, None);

        let turn = agent_turn(
            TurnCtx {
                label: "prefix",
                task: crate::agent::Task::FixAfterBuild,
                event_task: ap_core::event::Task::FixAfterBuild {
                    attempt: attempts as u32 + 1,
                    max_attempts: config.max_attempts as u32,
                },
                attempt: attempts + 1,
                hint: Some(&extract),
                unresolvable_deps: &[],
                checkout_dir: Some(&checkout_dir),
                from_repo_archive,
                resume: false,
                context: None,
            },
            config,
            &ws,
            pkg_run,
            reporter,
            &mut spent_usd,
            &mut session_id,
        );

        if let Ok((prefix_step, outcome)) = turn
            && let crate::agent::Action::Edited = &outcome.action
            && accept_or_restore(
                &spec_path,
                &clean,
                &prefix_step,
                reporter,
                &format!(
                    "\n## PreFix — restart from previous log\nReasoning: {}\nConfidence: {:?}\n",
                    outcome.reasoning, outcome.confidence
                ),
                ws.notes(),
            )
        {
            attempts += 1;
        }
    }

    loop {
        // Step: ValidateSpec
        let validate_step = step_or_fail!(pkg_run, "validate", rpm_name);
        let t = Instant::now();
        reporter.report(&Event::StepStarted {
            step: validate_step.id,
            kind: StepKind::ValidateSpec,
        });

        match osc::rpmspec_check(&spec_path, Emit::new(reporter, validate_step.id)) {
            Ok(expanded) => {
                // The fully expanded spec is the single most useful artifact
                // for diagnosing a spec problem — keep it rather than drop it.
                let artifact = validate_step.file("rpmspec.stdout");
                let _ = std::fs::write(&artifact, &expanded);
                reporter.report(&Event::Artifact {
                    step: validate_step.id,
                    name: "rpmspec.stdout".to_string(),
                    path: artifact,
                });
                reporter.report(&Event::StepFinished {
                    step: validate_step.id,
                    outcome: StepOutcome::Ok,
                    elapsed: t.elapsed(),
                });
            }
            Err(e) => {
                reporter.report(&Event::StepFinished {
                    step: validate_step.id,
                    outcome: StepOutcome::Failed,
                    elapsed: t.elapsed(),
                });
                let _ = std::fs::write(validate_step.file("rpmspec.stderr"), e.to_string());
                let _ = std::fs::write(ws.rpmspec_log(), e.to_string());
                reporter.report(&Event::SpecRejected {
                    step: validate_step.id,
                    reason: ap_core::event::SpecRejection::RpmspecParseFailed {
                        stderr: e.to_string(),
                    },
                    restored_from: spec_path.clone(),
                });
                if config.no_agent
                    || attempts >= config.max_attempts
                    || spent_usd >= config.budget_usd
                {
                    return PipelineResult::Failed {
                        rpm_name,
                        reason: format!("spec invalid: {e}"),
                    };
                }

                // Never commit a spec rpm cannot parse — OBS would spend a
                // whole remote build to tell us what rpmspec just said. Fix it
                // here and re-validate on the next pass of the loop.
                attempts += 1;
                let clean = strip_and_rewrite(&spec_path);
                let turn = agent_turn(
                    TurnCtx {
                        label: "agent",
                        task: crate::agent::Task::FixAfterValidation,
                        event_task: ap_core::event::Task::FixAfterBuild {
                            attempt: attempts as u32,
                            max_attempts: config.max_attempts as u32,
                        },
                        attempt: attempts,
                        hint: None,
                        unresolvable_deps: &[],
                        checkout_dir: Some(&checkout_dir),
                        from_repo_archive,
                        resume: true,
                        context: None,
                    },
                    config,
                    &ws,
                    pkg_run,
                    reporter,
                    &mut spent_usd,
                    &mut session_id,
                );

                match turn {
                    Ok((agent_step, outcome)) => {
                        if let crate::agent::Action::Blocked { why } = &outcome.action {
                            return PipelineResult::Failed {
                                rpm_name,
                                reason: format!("agent blocked: {why}"),
                            };
                        }
                        if let crate::agent::Action::Edited = &outcome.action {
                            accept_or_restore(
                                &spec_path,
                                &clean,
                                &agent_step,
                                reporter,
                                &format!(
                                    "\n## Attempt {attempts} — FixAfterValidation\n\
                                     Reasoning: {}\nConfidence: {:?}\n",
                                    outcome.reasoning, outcome.confidence
                                ),
                                ws.notes(),
                            );
                        }
                    }
                    Err(e) => {
                        return PipelineResult::Failed {
                            rpm_name,
                            reason: format!("spec invalid and agent failed: {e}"),
                        };
                    }
                }
                continue;
            }
        }

        // Step: Commit
        {
            let commit_step = step_or_fail!(pkg_run, "commit", rpm_name);
            let t = Instant::now();
            reporter.report(&Event::StepStarted {
                step: commit_step.id,
                kind: StepKind::Commit,
            });
            match osc::commit(
                &checkout_dir,
                "Automated packaging",
                Emit::new(reporter, commit_step.id),
            ) {
                Ok(()) => {
                    reporter.report(&Event::StepFinished {
                        step: commit_step.id,
                        outcome: StepOutcome::Ok,
                        elapsed: t.elapsed(),
                    });
                }
                Err(e) => {
                    reporter.report(&Event::StepFinished {
                        step: commit_step.id,
                        outcome: StepOutcome::Failed,
                        elapsed: t.elapsed(),
                    });
                    return PipelineResult::Failed {
                        rpm_name,
                        reason: e.to_string(),
                    };
                }
            }
        }

        // Step: Build (wait for OBS remote build, fetch log, download RPMs)
        let build_step = step_or_fail!(pkg_run, "build", rpm_name);
        let build_log = build_step.file("build.log");
        let t = Instant::now();
        reporter.report(&Event::StepStarted { step: build_step.id, kind: StepKind::Build });

        let build_result = osc::obs_build(
            &config.project,
            &rpm_name,
            &config.distro,
            &config.arch,
            &build_log,
            &config.rpms_dir,
            Emit::new(reporter, build_step.id),
        );

        match build_result {
            Ok(()) => {
                reporter.report(&Event::StepFinished {
                    step: build_step.id,
                    outcome: StepOutcome::Ok,
                    elapsed: t.elapsed(),
                });
                let mut state = PkgState::load(&ws);
                state.last_built_version = Some(config.version.clone());
                state.last_outcome = Some("success".to_string());
                let _ = state.save(&ws);
                let _ = ws.link_latest(&build_step.path);
                return PipelineResult::Built { rpm_name };
            }
            Err(osc::Error::UnresolvableDeps(deps)) => {
                reporter.report(&Event::StepFinished {
                    step: build_step.id,
                    outcome: StepOutcome::Failed,
                    elapsed: t.elapsed(),
                });
                reporter.report(&Event::MissingPackages {
                    step: build_step.id,
                    names: deps.clone(),
                    source: ap_core::event::MissingSource::BuildLog,
                });
                // Filter out deps already satisfied by an RPM in rpms_dir so the
                // agent only sees packages that are genuinely not available yet.
                let unresolvable_deps: Vec<String> = deps
                    .into_iter()
                    .filter(|dep| !rpm_satisfies_dep(&config.rpms_dir, dep))
                    .collect();

                if unresolvable_deps.is_empty() {
                    // Every dep has a local RPM, so OBS should pick them up on
                    // retry. Count the round anyway — if OBS keeps saying
                    // unresolvable we would otherwise spin here forever.
                    attempts += 1;
                    if attempts > config.max_attempts {
                        return PipelineResult::Failed {
                            rpm_name,
                            reason: format!(
                                "OBS keeps reporting unresolvable dependencies after \
                                 {attempts} attempt(s) even though every one has a local \
                                 RPM; log: {}",
                                build_log.display()
                            ),
                        };
                    }
                    continue;
                }

                // Give the agent a chance to fix the spec before giving up —
                // the dep name or version constraint may be wrong (e.g. requires
                // cython >= 3.0 but only Cython0 is available).
                let mut agent_edited = false;
                if !config.no_agent
                    && attempts < config.max_attempts
                    && spent_usd < config.budget_usd
                {
                    attempts += 1;
                    link_build_log(&ws, &build_log);

                    let extract = extract_build_log(
                        &build_log,
                        &sdist_top_level,
                        reporter,
                        Some(build_step.id),
                    );
                    reporter.report(&Event::BuildFailed {
                        step: build_step.id,
                        classification: classify_failure(&extract),
                        log: build_log.clone(),
                    });

                    let clean = strip_and_rewrite(&spec_path);
                    let turn = agent_turn(
                        TurnCtx {
                            label: "agent",
                            task: crate::agent::Task::FixAfterBuild,
                            event_task: ap_core::event::Task::FixAfterBuild {
                                attempt: attempts as u32,
                                max_attempts: config.max_attempts as u32,
                            },
                            attempt: attempts,
                            hint: Some(&extract),
                            unresolvable_deps: &unresolvable_deps,
                            checkout_dir: Some(&checkout_dir),
                            from_repo_archive,
                            resume: true,
                            context: None,
                        },
                        config,
                        &ws,
                        pkg_run,
                        reporter,
                        &mut spent_usd,
                        &mut session_id,
                    );

                    if let Ok((agent_step, outcome)) = turn
                        && let crate::agent::Action::Edited = &outcome.action
                    {
                        agent_edited = accept_or_restore(
                            &spec_path,
                            &clean,
                            &agent_step,
                            reporter,
                            &format!(
                                "\n## Attempt {attempts} — unresolvable BuildRequires\n\
                                 Reasoning: {}\nConfidence: {:?}\n",
                                outcome.reasoning, outcome.confidence
                            ),
                            ws.notes(),
                        );
                    }
                }

                if agent_edited {
                    // Agent fixed the spec (wrong dep name/version) — rebuild.
                    continue;
                }

                // Unresolvable deps cannot be fixed by spec edits — strip the
                // python<ver>- prefix to get the PyPI/package name and skip
                // this package until its dependencies are built.
                let mut to_build: Vec<String> = unresolvable_deps
                    .iter()
                    .map(|d| osc::strip_rpm_prefix(d).to_string())
                    .collect();
                to_build.sort_unstable();
                to_build.dedup();
                return PipelineResult::MissingDeps {
                    rpm_name,
                    deps: to_build,
                };
            }
            Err(osc::Error::NotActionable(msg)) => {
                reporter.report(&Event::StepFinished {
                    step: build_step.id,
                    outcome: StepOutcome::Failed,
                    elapsed: t.elapsed(),
                });
                return PipelineResult::Failed { rpm_name, reason: msg };
            }
            Err(_) => {
                reporter.report(&Event::StepFinished {
                    step: build_step.id,
                    outcome: StepOutcome::Failed,
                    elapsed: t.elapsed(),
                });

                if config.no_agent {
                    return PipelineResult::Failed {
                        rpm_name,
                        reason: format!("build failed; log: {}", build_log.display()),
                    };
                }

                attempts += 1;
                if attempts > config.max_attempts || spent_usd >= config.budget_usd {
                    return PipelineResult::Failed {
                        rpm_name,
                        reason: format!(
                            "gave up after {} attempt(s); log: {}",
                            attempts,
                            build_log.display()
                        ),
                    };
                }

                link_build_log(&ws, &build_log);

                let extract = extract_build_log(
                    &build_log,
                    &sdist_top_level,
                    reporter,
                    Some(build_step.id),
                );
                reporter.report(&Event::BuildFailed {
                    step: build_step.id,
                    classification: classify_failure(&extract),
                    log: build_log.clone(),
                });

                let clean = strip_and_rewrite(&spec_path);
                let turn = agent_turn(
                    TurnCtx {
                        label: "agent",
                        task: crate::agent::Task::FixAfterBuild,
                        event_task: ap_core::event::Task::FixAfterBuild {
                            attempt: attempts as u32,
                            max_attempts: config.max_attempts as u32,
                        },
                        attempt: attempts,
                        hint: Some(&extract),
                        unresolvable_deps: &[],
                        checkout_dir: Some(&checkout_dir),
                        from_repo_archive,
                        resume: true,
                        context: None,
                    },
                    config,
                    &ws,
                    pkg_run,
                    reporter,
                    &mut spent_usd,
                    &mut session_id,
                );

                let (agent_step, outcome) = match turn {
                    Ok(v) => v,
                    Err(e) => {
                        return PipelineResult::Failed {
                            rpm_name,
                            reason: format!("agent error: {e}"),
                        };
                    }
                };

                if !outcome.unresolvable_packages.is_empty() {
                    reporter.report(&Event::MissingPackages {
                        step: agent_step.id,
                        names: outcome.unresolvable_packages.clone(),
                        source: ap_core::event::MissingSource::Agent,
                    });
                    // Strip python<ver>- prefix so the caller gets PyPI names.
                    let mut to_build: Vec<String> = outcome
                        .unresolvable_packages
                        .iter()
                        .map(|d| osc::strip_rpm_prefix(d).to_string())
                        .collect();
                    to_build.sort_unstable();
                    to_build.dedup();
                    return PipelineResult::MissingDeps {
                        rpm_name,
                        deps: to_build,
                    };
                }

                match &outcome.action {
                    crate::agent::Action::Blocked { why } => {
                        return PipelineResult::Failed {
                            rpm_name,
                            reason: format!("agent blocked: {why}"),
                        };
                    }
                    crate::agent::Action::NoChange => {
                        // Rebuild with the same spec.
                    }
                    crate::agent::Action::Edited => {
                        accept_or_restore(
                            &spec_path,
                            &clean,
                            &agent_step,
                            reporter,
                            &format!(
                                "\n## Attempt {attempts} — FixAfterBuild\n\
                                 Reasoning: {}\nConfidence: {:?}\n",
                                outcome.reasoning, outcome.confidence
                            ),
                            ws.notes(),
                        );
                    }
                }
            }
        }
    }
}

// ── helpers ───────────────────────────────────────────────────────────────────

macro_rules! step_or_fail {
    ($pkg_run:expr, $label:expr, $rpm_name:expr) => {
        match $pkg_run.step($label) {
            Ok(s) => s,
            Err(e) => {
                return PipelineResult::Failed {
                    rpm_name: $rpm_name,
                    reason: e.to_string(),
                }
            }
        }
    };
}
use step_or_fail;

/// What one agent turn needs beyond [`PipelineConfig`].
struct TurnCtx<'a> {
    /// Step-directory label, e.g. `"review"` or `"agent"`.
    label: &'a str,
    task: crate::agent::Task,
    event_task: ap_core::event::Task,
    attempt: usize,
    hint: Option<&'a crate::log::ExtractedLog>,
    unresolvable_deps: &'a [String],
    checkout_dir: Option<&'a Path>,
    from_repo_archive: bool,
    /// Resume the previous `claude` session rather than starting a fresh one.
    resume: bool,
    /// Optional short note injected into the prompt before the hint section.
    context: Option<&'a str>,
}

/// Run one agent turn and emit every event it produces.
///
/// Allocates the step directory, writes `prompt.md`, `output.md`,
/// `response.json` and — when a hint was supplied — `extract.json`, reports
/// `AgentPrompt`/`ExtractResult`/`AgentToolUse`/`AgentReply`/`StepFinished`,
/// and folds the turn's cost into `spent_usd`. On failure the step is reported
/// failed and the error returned; the caller decides whether that is fatal.
fn agent_turn(
    ctx: TurnCtx<'_>,
    config: &PipelineConfig,
    ws: &Workspace,
    pkg_run: &PkgRunDir,
    reporter: &dyn Reporter,
    spent_usd: &mut f64,
    session_id: &mut Option<String>,
) -> Result<(StepDir, crate::agent::TurnOutcome), String> {
    let step = pkg_run.step(ctx.label).map_err(|e| e.to_string())?;
    let t = Instant::now();
    reporter.report(&Event::StepStarted {
        step: step.id,
        kind: StepKind::AgentTurn,
    });

    if let Some(hint) = ctx.hint {
        let extract = hint.to_event_extract();
        let artifact = step.file("extract.json");
        if let Ok(json) = serde_json::to_string_pretty(&extract) {
            let _ = std::fs::write(&artifact, json);
        }
        reporter.report(&Event::ExtractResult {
            step: step.id,
            extract,
            artifact,
        });
    }

    let notes = std::fs::read_to_string(ws.notes()).unwrap_or_default();
    let req = crate::agent::TurnRequest {
        workspace: ws.dir(),
        pkg_name: &config.pkg_name,
        version: &config.version,
        task: ctx.task,
        attempt: ctx.attempt,
        max_attempts: config.max_attempts,
        spent_usd: *spent_usd,
        budget_usd: config.budget_usd,
        hint: ctx.hint,
        notes: &notes,
        budget: crate::agent::TurnBudget::default(),
        from_repo_archive: ctx.from_repo_archive,
        subprojects: &config.subprojects,
        unresolvable_deps: ctx.unresolvable_deps,
        failed_deps: &config.failed_deps,
        session_id: if ctx.resume { session_id.as_deref() } else { None },
        checkout_dir: ctx.checkout_dir,
        context: ctx.context,
        on_tool_use: Some(&|tool, input| {
            reporter.report(&Event::AgentToolUse {
                step: step.id,
                tool: tool.to_string(),
                input: input.to_string(),
            });
        }),
    };

    let outcome = match crate::agent::turn(req) {
        Ok(o) => o,
        Err(e) => {
            reporter.report(&Event::StepFinished {
                step: step.id,
                outcome: StepOutcome::Failed,
                elapsed: t.elapsed(),
            });
            reporter.report(&Event::Warning {
                step: Some(step.id),
                message: format!("agent error: {e}"),
            });
            return Err(e.to_string());
        }
    };

    *session_id = outcome.session_id.clone();
    *spent_usd += outcome.usage.cost_usd();

    let prompt_path = step.file("prompt.md");
    let _ = std::fs::write(&prompt_path, &outcome.prompt);
    let _ = std::fs::write(step.file("output.md"), &outcome.output);

    reporter.report(&Event::AgentPrompt {
        step: step.id,
        task: ctx.event_task,
        prompt: outcome.prompt.clone(),
        artifact: prompt_path,
    });

    let response = ap_core::event::AgentResponseData {
        action: format!("{:?}", outcome.action),
        reasoning: outcome.reasoning.clone(),
        evidence: outcome.evidence.clone(),
        confidence: Some(format!("{:?}", outcome.confidence)),
    };
    let response_path = step.file("response.json");
    if let Ok(json) = serde_json::to_string_pretty(&response) {
        let _ = std::fs::write(&response_path, json);
    }
    reporter.report(&Event::AgentReply {
        step: step.id,
        response,
        raw: outcome.raw.clone(),
        usage: ap_core::event::Usage {
            input_tokens: outcome.usage.input_tokens,
            output_tokens: outcome.usage.output_tokens,
            cost_usd: outcome.usage.cost_usd(),
        },
        artifact: response_path,
    });

    reporter.report(&Event::StepFinished {
        step: step.id,
        outcome: StepOutcome::Ok,
        elapsed: t.elapsed(),
    });

    Ok((step, outcome))
}

/// Drop stale `# ap: fix` comments from the spec on disk and return the result.
///
/// The returned text is the roll-back point for the edit the agent is about to
/// make, so the agent's own comment lines never accumulate across attempts.
fn strip_and_rewrite(spec_path: &Path) -> String {
    let before = std::fs::read_to_string(spec_path).unwrap_or_default();
    let clean = crate::spec::strip_ap_fix_comments(&before);
    if clean != before {
        let _ = std::fs::write(spec_path, &clean);
    }
    clean
}

/// Point the durable workspace's `build.log` at this run's build log.
fn link_build_log(ws: &Workspace, build_log: &Path) {
    #[cfg(unix)]
    {
        let link = ws.build_log();
        if link.symlink_metadata().is_ok() {
            let _ = std::fs::remove_file(&link);
        }
        let _ = std::os::unix::fs::symlink(build_log, &link);
    }
}

/// Run the extractor over a build log, warning when the sdist root never matched.
///
/// The whole log is handed to [`crate::log::extract`] — it segments the phases
/// itself and knows that a post-build check failure lives *after* `%check`, so
/// pre-scoping the text here would cut the failure out of some logs.
fn extract_build_log(
    build_log: &Path,
    sdist_top_level: &str,
    reporter: &dyn Reporter,
    step: Option<ap_core::event::StepId>,
) -> crate::log::ExtractedLog {
    let log_text = std::fs::read_to_string(build_log).unwrap_or_default();
    let mapper = crate::log::PathMapper::with_sdist_root(sdist_top_level);
    let extracted = crate::log::extract(&log_text, &mapper);

    if !mapper.did_match_root() && !log_text.is_empty() {
        reporter.report(&Event::Warning {
            step,
            message: format!(
                "no path in the build log sits under the sdist root '{sdist_top_level}'; \
                 source files cannot be located and traceback frames may be over-elided"
            ),
        });
    }
    if extracted.format_suspect {
        reporter.report(&Event::Warning {
            step,
            message: "build log has no osc timestamp prefixes — the extractor's \
                      anchors may under-match"
                .to_string(),
        });
    }

    extracted
}

/// Validate an agent's spec edit, keep it or restore `before`, and report.
///
/// Returns true when the edit was kept. `rpmspec --parse` is the judge: a
/// macro reference inside a comment is only warned about, because RPM expands
/// comments but the great majority of such lines still parse fine, and
/// throwing away an otherwise good fix costs an attempt.
fn accept_or_restore(
    spec_path: &Path,
    before: &str,
    step: &StepDir,
    reporter: &dyn Reporter,
    notes_entry: &str,
    notes_path: PathBuf,
) -> bool {
    let after = std::fs::read_to_string(spec_path).unwrap_or_else(|_| before.to_string());

    if let Some((line_no, text)) = crate::spec::first_macro_in_comment(&after) {
        reporter.report(&Event::Warning {
            step: Some(step.id),
            message: format!(
                "spec comment at line {line_no} references a macro; \
                 RPM expands it during parse: {}",
                text.trim()
            ),
        });
    }

    if let Err(e) = osc::rpmspec_check(spec_path, Emit::new(reporter, step.id)) {
        reporter.report(&Event::SpecRejected {
            step: step.id,
            reason: ap_core::event::SpecRejection::RpmspecParseFailed {
                stderr: e.to_string(),
            },
            restored_from: spec_path.to_path_buf(),
        });
        let _ = std::fs::write(spec_path, before);
        return false;
    }

    let diff = unified_diff(before, &after);
    let (added, removed) = diff_counts(&diff);
    let diff_path = step.file("spec.diff");
    let _ = std::fs::write(&diff_path, &diff);
    reporter.report(&Event::SpecChanged {
        step: step.id,
        diff,
        lines_added: added,
        lines_removed: removed,
        artifact: diff_path,
    });
    append_notes(notes_path, notes_entry);
    true
}

/// Count added/removed lines in a diff produced by [`unified_diff`].
fn diff_counts(diff: &str) -> (usize, usize) {
    diff.lines().fold((0, 0), |(a, r), l| {
        if l.starts_with("+ ") {
            (a + 1, r)
        } else if l.starts_with("- ") {
            (a, r + 1)
        } else {
            (a, r)
        }
    })
}

/// Returns true if any RPM file in `rpms_dir` provides `dep`.
///
/// `dep` is a raw RPM dependency string like `python313-Cython>=3.1`. We strip
/// any version operator and compare the bare name against the prefix of each
/// filename: `python313-Cython-3.2.9-1.1.x86_64.rpm` satisfies
/// `python313-Cython>=3.1` because the filename starts with `python313-Cython-`.
fn rpm_satisfies_dep(rpms_dir: &Path, dep: &str) -> bool {
    let pkg_name = dep
        .split(|c: char| c == '>' || c == '<' || c == '=' || c == ' ')
        .next()
        .unwrap_or(dep)
        .trim();
    let prefix = format!("{pkg_name}-");
    std::fs::read_dir(rpms_dir)
        .into_iter()
        .flatten()
        .filter_map(|e| e.ok())
        .any(|e| {
            e.file_name()
                .to_string_lossy()
                .starts_with(prefix.as_str())
        })
}

fn spec_version_differs(spec_path: &Path, version: &str) -> bool {
    std::fs::read_to_string(spec_path)
        .map(|s| {
            !s.contains(&format!("Version:        {version}"))
                && !s.contains(&format!("Version: {version}"))
        })
        .unwrap_or(true)
}

fn find_tarball(dir: &Path) -> Option<PathBuf> {
    std::fs::read_dir(dir).ok()?.find_map(|e| {
        let e = e.ok()?;
        let name = e.file_name();
        let n = name.to_string_lossy();
        // Exclude Cargo dep bundles — not the Python/Rust source tarball.
        if n == "vendor.tar.zst" || n == "registry.tar.zst" {
            return None;
        }
        if n.ends_with(".tar.gz")
            || n.ends_with(".tar.bz2")
            || n.ends_with(".tar.zst")
            || n.ends_with(".zip")
        {
            Some(e.path())
        } else {
            None
        }
    })
}

fn find_sdist_top_level(checkout_dir: &Path, pkg_name: &str, version: &str) -> String {
    if let Some(tb) = find_tarball(checkout_dir) {
        let fname = tb.file_name().unwrap_or_default().to_string_lossy();
        for ext in &[".tar.gz", ".tar.bz2", ".tar.zst", ".zip"] {
            if let Some(stem) = fname.strip_suffix(ext) {
                return stem.to_string();
            }
        }
    }
    format!("{}-{}", pkg_name.to_lowercase(), version)
}

enum GenSpecError {
    WheelOnly {
        pkg_name: String,
        version: String,
        candidates: Vec<String>,
    },
    Other(String),
}

impl From<String> for GenSpecError {
    fn from(s: String) -> Self {
        GenSpecError::Other(s)
    }
}

struct GenSpecResult {
    sdist_top_level: String,
    is_maturin: bool,
    /// A standalone Rust crate, built with cargo-packaging rather than the
    /// Python macros.
    is_rust: bool,
    /// The source came from a repository archive rather than a PyPI sdist, so
    /// it has a repository's shape and not an sdist's (agent convention 22).
    from_repo_archive: bool,
}

fn generate_spec(
    config: &PipelineConfig,
    checkout_dir: &Path,
    spec_path: &Path,
    step: &StepDir,
) -> Result<GenSpecResult, GenSpecError> {
    // An explicit repository bypasses PyPI entirely: there may be no PyPI
    // project at all, and if there is, the user asked for the repo anyway.
    if let Some(ref repo_url) = config.source_repo {
        return generate_from_repo(config, repo_url, checkout_dir, spec_path, step);
    }

    use ap_ecosystem::pypi;
    use ap_ecosystem::sdist;

    let pypi_url = format!(
        "https://pypi.org/pypi/{}/{}/json",
        config.pkg_name, config.version
    );
    let json = ureq::get(&pypi_url)
        .call()
        .map_err(|e| GenSpecError::Other(format!("PyPI fetch failed: {e}")))?
        .into_body()
        .read_to_string()
        .map_err(|e| GenSpecError::Other(format!("PyPI read failed: {e}")))?;
    let release = match pypi::parse_release(&json) {
        Ok(r) => r,
        Err(pypi::Error::NoSdist { .. }) => {
            // Wheel-only on PyPI: build from a source repository instead.
            // An explicit --source-url wins over whatever the metadata says —
            // the user looked it up, we guessed.
            let source_url = config
                .source_urls
                .get(&config.pkg_name)
                .map(str::to_string)
                .or_else(|| pypi::source_url_from_json(&json));

            let source_url = match source_url {
                Some(url) => url,
                None => {
                    // Auto-discover candidate URLs for the agent to validate
                    let candidates = find_source_repo_candidates(&config.pkg_name);
                    if !candidates.is_empty() {
                        eprintln!("info: found {} candidate repository URLs for agent to validate", candidates.len());
                    }
                    return Err(GenSpecError::WheelOnly {
                        pkg_name: config.pkg_name.clone(),
                        version: config.version.clone(),
                        candidates,
                    });
                }
            };

            let tarball =
                checkout_dir.join(format!("{}-{}.tar.gz", config.pkg_name.to_lowercase(), config.version));
            let (bytes, url) = fetch_release_archive(&source_url, &config.version)?;
            std::fs::write(&tarball, &bytes)
                .map_err(|e| GenSpecError::Other(format!("writing {}: {e}", tarball.display())))?;
            let info =
                ap_ecosystem::sdist::inspect_preferring(&tarball, Some(&config.pkg_name))
                    .map_err(|e| GenSpecError::Other(format!("inspecting {url}: {e}")))?;

            return generate_repo_python_spec(config, &tarball, &info, &json, spec_path, step);
        }
        Err(e) => return Err(GenSpecError::Other(format!("PyPI parse failed: {e}"))),
    };

    // Download tarball to checkout dir.
    let tarball = release
        .sdist
        .fetch(checkout_dir)
        .map_err(|e| GenSpecError::Other(format!("tarball download failed: {e}")))?;

    // Inspect sdist for build system info and top-level dir.
    let sdist_info = sdist::inspect(&tarball)
        .map_err(|e| GenSpecError::Other(format!("sdist inspect failed: {e}")))?;

    let is_maturin = sdist_info.build_system.is_maturin();

    // For monorepos, skip py2pack (it only handles one project) and emit a
    // skeleton the review agent will fill in with all subpackages.
    if !sdist_info.subprojects.is_empty() {
        let spec_text = crate::spec::monorepo_skeleton(
            &config.pkg_name,
            &config.version,
            &sdist_info.subprojects,
            Some(&release),
        );
        let spec_text = crate::spec::strip_ap_fix_comments(&spec_text);
        std::fs::write(spec_path, &spec_text)
            .map_err(|e| GenSpecError::Other(format!("writing spec failed: {e}")))?;
        let _ = std::fs::write(step.file("spec.generated"), &spec_text);
        let sdist_top_level = sdist_info
            .top_level_dir
            .unwrap_or_else(|| format!("{}-{}", config.pkg_name.to_lowercase(), config.version));
        return Ok(GenSpecResult {
            sdist_top_level,
            is_maturin,
            is_rust: false,
            from_repo_archive: false,
        });
    }

    // Generate spec via py2pack.
    let spec_text = crate::spec::generate(&release, &sdist_info.build_system)
        .map_err(|e| GenSpecError::Other(format!("spec generation failed: {e}")))?;

    // Post-process with test info.
    let spec_text = crate::spec::post_process_with_sdist(
        &spec_text,
        &release,
        &sdist_info.build_system,
        crate::spec::SdistFacts {
            has_tests: sdist_info.has_tests,
            has_c_extensions: sdist_info.has_c_extensions,
        },
    );

    // Fix autosetup -n if needed.
    let spec_text = if let Some(top) = &sdist_info.top_level_dir {
        crate::spec::fix_autosetup_dir(&spec_text, top).unwrap_or(spec_text)
    } else {
        spec_text
    };

    // Strip any stray ap:fix comments.
    let spec_text = crate::spec::strip_ap_fix_comments(&spec_text);

    // Write to checkout dir.
    std::fs::write(spec_path, &spec_text)
        .map_err(|e| GenSpecError::Other(format!("writing spec failed: {e}")))?;

    // Save artifact copy.
    let _ = std::fs::write(step.file("spec.generated"), &spec_text);

    let sdist_top_level = sdist_info
        .top_level_dir
        .unwrap_or_else(|| format!("{}-{}", config.pkg_name.to_lowercase(), config.version));

    Ok(GenSpecResult {
        sdist_top_level,
        is_maturin,
        is_rust: false,
        from_repo_archive: false,
    })
}

/// Build from a source repository the user pointed at.
///
/// Which generator to use is decided by what the release archive contains, not
/// by which flag was passed: a repository is not Rust just because it was
/// given as a URL, and a maturin project has both a `Cargo.toml` and a
/// `pyproject.toml`, so Python wins whenever Python packaging is present.
fn generate_from_repo(
    config: &PipelineConfig,
    repo_url: &str,
    checkout_dir: &Path,
    spec_path: &Path,
    step: &StepDir,
) -> Result<GenSpecResult, GenSpecError> {
    let name = config.pkg_name.to_lowercase();
    let tarball = checkout_dir.join(format!("{name}-{}.tar.gz", config.version));

    let (bytes, url) = fetch_release_archive(repo_url, &config.version)?;
    std::fs::write(&tarball, &bytes)
        .map_err(|e| GenSpecError::Other(format!("writing {}: {e}", tarball.display())))?;

    // Same preference the probe used, so generation and naming agree on which
    // subproject of a monorepo is being built.
    let info = ap_ecosystem::sdist::inspect_preferring(&tarball, Some(&config.pkg_name))
        .map_err(|e| GenSpecError::Other(format!("inspecting {url}: {e}")))?;

    if info.python_packaging {
        // py2pack needs PyPI metadata for the summary, license and deps. When
        // the project is not on PyPI at all there is nothing to fetch, and the
        // agent fills those in from the source during the review turn.
        let pypi_json = fetch_pypi_json(&config.pkg_name, &config.version).unwrap_or_default();
        return generate_repo_python_spec(config, &tarball, &info, &pypi_json, spec_path, step);
    }

    if info.cargo_manifest {
        // The Rust path re-fetches through `osc service ra`, which is what
        // produces the vendored sources OBS builds from.
        let _ = std::fs::remove_file(&tarball);
        return generate_rust_spec(config, repo_url, spec_path, step);
    }

    Err(GenSpecError::Other(format!(
        "{url} contains no pyproject.toml, setup.py or Cargo.toml, so \
         auto-package cannot tell how to build it"
    )))
}

/// Download the first release archive that exists, returning its bytes and URL.
fn fetch_release_archive(repo_url: &str, version: &str) -> Result<(Vec<u8>, String), GenSpecError> {
    let candidates = crate::sources::archive_urls(repo_url, version);
    for url in &candidates {
        let Ok(resp) = ureq::get(url).call() else { continue };
        use std::io::Read;
        let mut buf = Vec::new();
        if resp.into_body().into_reader().read_to_end(&mut buf).is_ok() && !buf.is_empty() {
            return Ok((buf, url.clone()));
        }
    }
    Err(GenSpecError::Other(format!(
        "no release archive for version {version} at {repo_url}. Tried:\n{}",
        candidates
            .iter()
            .map(|u| format!("  {u}"))
            .collect::<Vec<_>>()
            .join("\n"),
    )))
}

fn fetch_pypi_json(name: &str, version: &str) -> Option<String> {
    ureq::get(&format!("https://pypi.org/pypi/{name}/{version}/json"))
        .call()
        .ok()?
        .into_body()
        .read_to_string()
        .ok()
}

fn generate_rust_spec(
    config: &PipelineConfig,
    github_url: &str,
    spec_path: &Path,
    step: &StepDir,
) -> Result<GenSpecResult, GenSpecError> {
    let name = &config.pkg_name;
    let version = &config.version;
    let spec_text = format!(
        "%global rustflags '-Clink-arg=-Wl,-z,relro,-z,now'\n\
         Name:           {name}\n\
         Version:        {version}\n\
         Release:        0\n\
         Summary:        TODO: fill in summary\n\
         License:        TODO: fill in license\n\
         URL:            {github_url}\n\
         Source0:        %{{name}}-%{{version}}.tar.zst\n\
         Source1:        vendor.tar.zst\n\
         Source999:      README.suse-maint.md\n\
         BuildRequires:  cargo-packaging\n\
         BuildRequires:  pkgconf\n\
         BuildRequires:  pkgconfig\n\
         \n\
         %description\n\
         TODO: fill in description\n\
         \n\
         %prep\n\
         %autosetup -p1 -a1\n\
         \n\
         %build\n\
         %{{cargo_build}}\n\
         \n\
         %install\n\
         install -D -d -m 0755 %{{buildroot}}%{{_bindir}}\n\
         install -m 0755 %{{_builddir}}/%{{name}}-%{{version}}/target/release/{name} \\\n\
                 %{{buildroot}}%{{_bindir}}/{name}\n\
         \n\
         %files\n\
         %license LICENSE\n\
         %{{_bindir}}/{name}\n\
         \n\
         %changelog\n"
    );

    std::fs::write(spec_path, &spec_text)
        .map_err(|e| GenSpecError::Other(format!("writing spec failed: {e}")))?;
    let _ = std::fs::write(step.file("spec.generated"), &spec_text);

    Ok(GenSpecResult {
        sdist_top_level: format!("{name}-{version}"),
        is_maturin: false,
        is_rust: true,
        from_repo_archive: true,
    })
}

/// Generate a Python spec from a repository archive rather than a PyPI sdist,
/// using py2pack for the spec body so macros, dependencies and license
/// handling match the normal PyPI-sourced flow.
///
/// `tarball_path` is an already-downloaded archive inside the checkout, and
/// `pypi_json` the project's PyPI metadata — empty when the project is not on
/// PyPI at all, in which case py2pack fills in what it can and the review turn
/// supplies the rest from the source.
fn generate_repo_python_spec(
    config: &PipelineConfig,
    tarball_path: &Path,
    sdist_info: &ap_ecosystem::sdist::SdistInfo,
    pypi_json: &str,
    spec_path: &Path,
    step: &StepDir,
) -> Result<GenSpecResult, GenSpecError> {
    let name = config.pkg_name.to_lowercase();
    let version = &config.version;

    let tarball_name = tarball_path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| format!("{name}-{version}.tar.gz"));

    let sdist_top_level = sdist_info
        .top_level_dir
        .clone()
        .unwrap_or_else(|| format!("{name}-{version}"));

    // For monorepos, skip py2pack entirely — it only handles one project.
    // Emit a skeleton the review agent fills in; PyPI metadata is optional.
    if !sdist_info.subprojects.is_empty() {
        let release = ap_ecosystem::pypi::parse_release_with_tarball(pypi_json, &tarball_name).ok();
        let spec_text = crate::spec::monorepo_skeleton(
            &config.pkg_name,
            version,
            &sdist_info.subprojects,
            release.as_ref(),
        );
        let spec_text = crate::spec::strip_ap_fix_comments(&spec_text);
        std::fs::write(spec_path, &spec_text)
            .map_err(|e| GenSpecError::Other(format!("writing spec failed: {e}")))?;
        let _ = std::fs::write(step.file("spec.generated"), &spec_text);
        return Ok(GenSpecResult {
            sdist_top_level,
            is_maturin: sdist_info.build_system.is_maturin(),
            is_rust: false,
            from_repo_archive: true,
        });
    }

    // Build a Release from the PyPI JSON metadata, substituting the forge
    // tarball name so py2pack writes the correct Source0.
    let release = ap_ecosystem::pypi::parse_release_with_tarball(pypi_json, &tarball_name)
        .map_err(|e| {
            GenSpecError::Other(format!(
                "no usable metadata for {name} {version}: {e}. The project is not on \
                 PyPI, so py2pack has nothing to describe it with."
            ))
        })?;

    // Generate spec via py2pack (same path as a normal PyPI-sourced package).
    let spec_text = crate::spec::generate(&release, &sdist_info.build_system)
        .map_err(|e| GenSpecError::Other(format!("spec generation failed: {e}")))?;

    let spec_text = crate::spec::post_process_with_sdist(
        &spec_text,
        &release,
        &sdist_info.build_system,
        crate::spec::SdistFacts {
            has_tests: sdist_info.has_tests,
            has_c_extensions: sdist_info.has_c_extensions,
        },
    );

    let spec_text = crate::spec::fix_autosetup_dir(&spec_text, &sdist_top_level)
        .unwrap_or(spec_text);

    // A repository snapshot of a monorepo keeps the project one or more levels
    // down; %build and %install have to change into it.
    let spec_text = match &sdist_info.project_dir {
        Some(dir) => crate::spec::build_in_subdir(&spec_text, dir),
        None => spec_text,
    };

    let spec_text = crate::spec::strip_ap_fix_comments(&spec_text);

    std::fs::write(spec_path, &spec_text)
        .map_err(|e| GenSpecError::Other(format!("writing spec failed: {e}")))?;
    let _ = std::fs::write(step.file("spec.generated"), &spec_text);

    Ok(GenSpecResult {
        sdist_top_level,
        is_maturin: sdist_info.build_system.is_maturin(),
        is_rust: false,
        from_repo_archive: true,
    })
}

/// Render the OBS `_service` XML for a Rust package.
///
/// `github_url` is the project URL (without `.git`), `version` is the crate
/// version, and `pkg_name` is used as the `cargo_vendor` `src` directory name
/// (the directory `tar_scm` creates matches the repo name).
fn render_service_file(github_url: &str, version: &str, pkg_name: &str) -> String {
    let git_url = if github_url.ends_with(".git") {
        github_url.to_string()
    } else {
        format!("{github_url}.git")
    };
    let repo_name = github_url
        .trim_end_matches('/')
        .rsplit('/')
        .next()
        .unwrap_or(pkg_name);

    format!(
        "<services>\n\
           <service mode=\"manual\" name=\"tar_scm\">\n\
         \x20   <param name=\"url\">{git_url}</param>\n\
         \x20   <param name=\"versionformat\">@PARENT_TAG@</param>\n\
         \x20   <param name=\"scm\">git</param>\n\
         \x20   <param name=\"revision\">v{version}</param>\n\
         \x20   <param name=\"match-tag\">*</param>\n\
         \x20   <param name=\"versionrewrite-pattern\">v(\\d+\\.\\d+\\.\\d+)</param>\n\
         \x20   <param name=\"versionrewrite-replacement\">\\1</param>\n\
         \x20   <param name=\"changesgenerate\">enable</param>\n\
         \x20   <param name=\"changesauthor\"> YOUR EMAIL HERE </param>\n\
         \x20 </service>\n\
         \x20 <service mode=\"manual\" name=\"recompress\">\n\
         \x20   <param name=\"file\">*.tar</param>\n\
         \x20   <param name=\"compression\">zst</param>\n\
         \x20 </service>\n\
         \x20 <service mode=\"manual\" name=\"set_version\"/>\n\
         \x20 <service name=\"cargo_vendor\" mode=\"manual\">\n\
         \x20    <param name=\"src\">{repo_name}</param>\n\
         \x20    <param name=\"compression\">zst</param>\n\
         \x20    <param name=\"update\">true</param>\n\
         \x20 </service>\n\
         </services>\n"
    )
}

fn classify_failure(
    extracted: &crate::log::ExtractedLog,
) -> ap_core::event::BuildFailure {
    use crate::log::{Genre, Phase};
    if let Some(failure) = &extracted.failure {
        if failure.phase == Phase::Section("%check".into()) {
            return ap_core::event::BuildFailure::CheckFailed;
        }
        match failure.genre {
            Genre::PythonTraceback => ap_core::event::BuildFailure::BuildFailed,
            Genre::ShellError => ap_core::event::BuildFailure::PrepFailed,
            Genre::PostBuildCheck => ap_core::event::BuildFailure::InstallFailed,
            Genre::Unknown => ap_core::event::BuildFailure::Unclassified {
                raw: extracted.error_lines.first().cloned().unwrap_or_default(),
            },
        }
    } else {
        ap_core::event::BuildFailure::Unclassified {
            raw: extracted.error_lines.first().cloned().unwrap_or_default(),
        }
    }
}

fn unified_diff(before: &str, after: &str) -> String {
    let mut d = String::new();
    for line in diff::lines(before, after) {
        match line {
            diff::Result::Left(l) => {
                d.push_str("- ");
                d.push_str(l);
                d.push('\n');
            }
            diff::Result::Right(r) => {
                d.push_str("+ ");
                d.push_str(r);
                d.push('\n');
            }
            diff::Result::Both(_, _) => {}
        }
    }
    d
}

/// Render the OBS `_service` XML for a maturin-backed Python package.
///
/// `normalized_name` is the PyPI package name with hyphens replaced by
/// underscores (e.g. `outlines_core`), matching the sdist tarball filename.
fn render_maturin_service_file(normalized_name: &str) -> String {
    format!(
        "<?xml version=\"1.0\"?>\n\
         <services>\n\
         \x20 <service mode=\"manual\" name=\"download_files\" />\n\
         \x20 <service name=\"cargo_vendor\" mode=\"manual\">\n\
         \x20    <param name=\"src\">{normalized_name}-*.tar.gz</param>\n\
         \x20    <param name=\"method\">registry</param>\n\
         \x20    <param name=\"update\">true</param>\n\
         \x20    <param name=\"compression\">zst</param>\n\
         \x20    <param name=\"no-root-manifest\">false</param>\n\
         \x20    <param name=\"cargotoml\">Cargo.toml</param>\n\
         \x20 </service>\n\
         </services>\n"
    )
}

fn append_notes(notes_path: PathBuf, section: &str) {
    let mut existing = std::fs::read_to_string(&notes_path).unwrap_or_default();
    existing.push_str(section);
    let _ = std::fs::write(notes_path, existing);
}

/// Find candidate source repositories for a package.
///
/// Returns a list of likely repository URLs for the agent to validate.
/// Uses GitHub API search, known package mappings, and common patterns.
fn find_source_repo_candidates(pkg_name: &str) -> Vec<String> {
    eprintln!("info: searching for source repository candidates for: {}", pkg_name);

    let lower = pkg_name.to_lowercase();
    let mut candidates = Vec::new();
    let mut verified_candidates = Vec::new();

    // Known package mappings for special cases
    let known_mappings: &[(&str, &str)] = &[
        // NVIDIA CUDA packages
        ("cuda-python", "https://github.com/NVIDIA/cuda-python"),
        ("nvmath-python", "https://github.com/NVIDIA/nvmath-python"),
        ("nvidia-ml-py", "https://github.com/NVIDIA/nvidia-ml-py"),
        ("nvidia-nvshmem-cu", "https://github.com/NVIDIA/nvshmem"),
        ("nvshmem4py", "https://github.com/NVIDIA/nvshmem"),
        ("nvidia-cuda-cccl", "https://github.com/NVIDIA/cccl"),
        ("nvidia-cuda-nvrtc", "https://github.com/NVIDIA/cuda-python"),
        ("nvidia-cuda-runtime", "https://github.com/NVIDIA/cuda-python"),
        ("nvidia-cuda-cupti", "https://github.com/NVIDIA/cuda-python"),
        ("nvidia-cuda-nvcc", "https://github.com/NVIDIA/cuda-python"),
        ("nvidia-nvjitlink", "https://github.com/NVIDIA/nvjitlink-python"),
    ];

    // Check known mappings first
    for (pattern, repo_url) in known_mappings {
        if lower.starts_with(pattern) {
            eprintln!("  checking known mapping: {}", repo_url);
            if verify_repo_exists(repo_url) {
                eprintln!("  ✓ verified: {}", repo_url);
                verified_candidates.push(repo_url.to_string());
            } else {
                candidates.push(repo_url.to_string());
            }
        }
    }

    // Try to get repository URLs from GitHub API search
    if let Some(urls) = search_github_repos(pkg_name) {
        for url in urls {
            if !verified_candidates.contains(&url) && !candidates.contains(&url) {
                eprintln!("  found via GitHub search: {}", url);
                verified_candidates.push(url);
            }
        }
    }

    // Generate candidate URLs based on package name patterns
    let normalized_name = pkg_name.replace("_", "-").to_lowercase();

    // Try common organization patterns for vendor-prefixed packages
    if lower.starts_with("nvidia-") || lower.starts_with("cuda-") {
        let base = lower
            .strip_prefix("nvidia-")
            .or_else(|| lower.strip_prefix("cuda-"))
            .unwrap_or(&lower);

        let clean = base
            .replace("-cu11", "").replace("-cu12", "").replace("-cu13", "")
            .replace("_", "-");

        candidates.push(format!("https://github.com/NVIDIA/{}", clean));
        if let Some(after_cuda) = clean.strip_prefix("cuda-") {
            candidates.push(format!("https://github.com/NVIDIA/{}", after_cuda));
        }
    } else if lower.starts_with("google-") {
        if let Some(after) = lower.strip_prefix("google-") {
            candidates.push(format!("https://github.com/googleapis/{}", after));
            candidates.push(format!("https://github.com/google/{}", after));
        }
    } else if lower.starts_with("aws-") {
        if let Some(after) = lower.strip_prefix("aws-") {
            candidates.push(format!("https://github.com/aws/{}", after));
            candidates.push(format!("https://github.com/awslabs/{}", after));
        }
    } else if lower.starts_with("azure-") {
        if let Some(after) = lower.strip_prefix("azure-") {
            candidates.push(format!("https://github.com/Azure/{}", after));
        }
    }

    // Try the exact package name with common Python project organizations
    for org in &["python", "pypa", "psf"] {
        candidates.push(format!("https://github.com/{}/{}", org, normalized_name));
    }

    // Try just the package name under various popular organizations
    candidates.push(format!("https://github.com/{}/{}", normalized_name, normalized_name));

    // Also try common name variations
    let name_parts: Vec<&str> = normalized_name.split('-').collect();
    if name_parts.len() > 1 {
        // Try first part as org, rest as repo (e.g. "foo-bar" -> foo/bar)
        candidates.push(format!("https://github.com/{}/{}", name_parts[0], name_parts[1..].join("-")));
    }

    // Try the package name directly
    candidates.push(format!("https://github.com/{}", normalized_name));

    // Check all generated candidates
    for url in &candidates {
        if !verified_candidates.contains(url) {
            eprintln!("  checking: {}", url);
            if verify_repo_exists(url) {
                eprintln!("  ✓ verified: {}", url);
                verified_candidates.push(url.clone());
            }
        }
    }

    if verified_candidates.is_empty() {
        eprintln!("  ✗ no verified repositories found after trying {} URLs", candidates.len());
        eprintln!("  returning {} unverified candidates for agent review", candidates.len());
        candidates
    } else {
        eprintln!("  ✓ found {} verified candidates", verified_candidates.len());
        verified_candidates
    }
}

/// Search GitHub for repositories matching the package name.
/// Returns multiple candidates ranked by relevance.
fn search_github_repos(pkg_name: &str) -> Option<Vec<String>> {
    let normalized = pkg_name.replace("_", "-");
    let search_url = format!(
        "https://api.github.com/search/repositories?q={}&sort=stars&order=desc",
        urlencoding::encode(&normalized)
    );

    eprintln!("  searching GitHub API for: {}", normalized);

    let body = ureq::get(&search_url)
        .call()
        .ok()?
        .into_body()
        .read_to_string()
        .ok()?;

    let json: serde_json::Value = serde_json::from_str(&body).ok()?;
    let items = json.get("items")?.as_array()?;

    let mut matches = Vec::new();

    // Look for exact or close matches in the top results
    for item in items.iter().take(10) {
        let repo_name = item.get("name")?.as_str()?;
        let html_url = item.get("html_url")?.as_str()?;

        // Check if the repo name matches (case-insensitive, allowing _ vs -)
        let repo_normalized = repo_name.replace("_", "-").to_lowercase();
        let pkg_normalized = normalized.to_lowercase();

        // Exact match gets priority
        if repo_normalized == pkg_normalized {
            matches.insert(0, html_url.to_string());
        }
        // Close matches
        else if repo_normalized == pkg_normalized.replace("-", "")
            || repo_normalized.ends_with(&format!("-{}", pkg_normalized))
            || repo_normalized.starts_with(&format!("{}-", pkg_normalized))
        {
            matches.push(html_url.to_string());
        }
    }

    if !matches.is_empty() {
        eprintln!("  found {} matching repos via GitHub API", matches.len());
        Some(matches)
    } else {
        None
    }
}

/// Verify that a repository URL exists by making a HEAD request.
fn verify_repo_exists(url: &str) -> bool {
    ureq::head(url)
        .call()
        .map(|r| r.status() == 200)
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn diff_counts_reports_both_directions() {
        // The old code hard-coded lines_removed to 0 and derived lines_added
        // from the line-count delta, so a pure replacement reported +0 -0.
        let diff = unified_diff("a\nb\nc\n", "a\nX\nc\n");
        assert_eq!(diff_counts(&diff), (1, 1));

        let diff = unified_diff("a\n", "a\nb\nc\n");
        assert_eq!(diff_counts(&diff), (2, 0));

        assert_eq!(diff_counts(&unified_diff("a\n", "a\n")), (0, 0));
    }

    #[test]
    fn dep_names_skip_extras_and_strip_specifiers() {
        let reqs = vec![
            "pytest>=7.0; extra == 'test'".to_string(),
            "requests[socks] >= 2.0".to_string(),
            "typing-extensions".to_string(),
        ];
        assert_eq!(
            dep_names_from_requires_dist(&reqs),
            vec!["requests", "typing_extensions"]
        );
    }
}
