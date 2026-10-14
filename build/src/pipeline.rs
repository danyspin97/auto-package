//! Single-package build pipeline (REDESIGN §2.4, §3.2).
//!
//! Loop: Checkout → GenerateSpec → FetchSdist → ValidateSpec →
//!       (Commit → Build → AgentTurn)* → Done
//!
//! Every step is a [`StepGuard`](crate::step::StepGuard); the step kinds, their
//! labels and directory names are the table `ap_core::step::STEPS`.

use std::path::{Path, PathBuf};
use std::time::Duration;

use ap_core::event::{Event, SelectionReason, SkipReason, StepKind, StepOutcome};
use ap_core::run::{PkgRunDir, StepDir};
use ap_core::sink::Reporter;

use crate::proc::Emit;
use crate::step::{Attempts, StepGuard};

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
    /// Names this run confirmed cannot ever be built by this pipeline at
    /// all — no PyPI project and no discoverable public source repository
    /// (see `resolve_dependency_config` returning `None`). Almost always a
    /// proprietary vendor SDK/compiler (NVIDIA's `cuda-devel`/`cuda-runtime`
    /// are the running example). Distinct from `failed_deps`: those were
    /// attempted and failed mid-build, so "skip the optional feature/tests
    /// that need it" applies; these were never buildable in the first place,
    /// so re-reporting them in `unresolvable_packages` is not just wasted —
    /// it can never succeed, and the agent needs to reach for convention 28
    /// Exception 4 (`use_upstream_wheel`) instead if its own `%build`
    /// genuinely requires one of these.
    pub unbuildable_deps: Vec<String>,
    /// Current CLI invocation's run_id, used to scope the per-package
    /// cumulative-spend tracking in state.json. A fresh CLI invocation
    /// gets a different run_id; `run_package` sees the mismatch and
    /// resets `cumulative_spent_usd` to 0 for that package. See
    /// `PkgState::cumulative_spent_run_id`.
    pub run_id: String,
    /// User-supplied source repositories, for packages PyPI ships as wheels
    /// with no repository in their metadata. Shared across the whole run so a
    /// dependency discovered mid-run picks up its override too.
    pub source_urls: std::sync::Arc<crate::sources::SourceMap>,
    /// Which Python flavors this work directory builds for, from
    /// `<work-dir>/flavors.toml` — see that module's doc comment. Shared the
    /// same way as `source_urls` and for the same reason: every package in
    /// the run targets the same distro, so the answer never differs between
    /// them.
    pub python_flavors: std::sync::Arc<crate::flavors::PythonFlavors>,
}

impl PipelineConfig {
    /// The CPython ABI tag to pass as `pypi::pick_wheel_from_json`'s
    /// `single_locked_flavor_abi`, derived from `python_flavors`. `None`
    /// unless this project is configured for exactly one Python flavor and
    /// that flavor's name translates to an ABI tag at all (see
    /// `flavors::abi_tag_for_flavor`).
    pub fn single_locked_flavor_abi(&self) -> Option<String> {
        self.python_flavors
            .single_flavor()
            .and_then(crate::flavors::abi_tag_for_flavor)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum AgentBackend {
    #[default]
    Cli,
}

#[derive(Debug)]
pub enum PipelineResult {
    Built { rpm_name: String },
    AlreadyBuilt { rpm_name: String },
    MissingDeps { rpm_name: String, deps: Vec<String> },
    Failed { rpm_name: String, reason: String },
}

// ── durable package state ─────────────────────────────────────────────────────

/// The policy the pipeline enforces. Bump it only for a large change, such as
/// switching a family of packages from source builds to upstream wheels: every
/// package whose recorded policy is older is checked again on its next run,
/// even if it built successfully before. Small convention changes do not bump
/// it. Still 0: no re-check has been needed yet.
pub const POLICY_VERSION: u32 = 0;

/// Distinguishes the temp files of concurrent saves within one process.

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, Default)]
pub struct PkgState {
    /// The `POLICY_VERSION` the last successful build was checked against.
    #[serde(default)]
    pub policy_version: u32,
    pub last_built_version: Option<String>,
    pub last_outcome: Option<String>,
    /// One-line reason recorded for the most recent unsuccessful outcome
    /// (`last_outcome` is `"failed"` or `"missing_deps"`), so the next run —
    /// or a human reading state.json — can see WHY without re-opening the
    /// run.jsonl stream. Preserved across runs the same way `last_outcome`
    /// is; cleared when the package next succeeds.
    ///
    /// Updated on every unsuccessful [`PipelineResult`], not just `Failed`:
    /// before this covered `MissingDeps` too, a round that reported a
    /// missing dependency left whatever `Failed` reason happened to be here
    /// from an earlier, unrelated round untouched — observed on
    /// python-accelerate, where this field kept describing a long-resolved
    /// version-mismatch bug through several later `MissingDeps` rounds and a
    /// `%check` fix attempt, misleadingly implying that old bug was still
    /// the live blocker.
    #[serde(default)]
    pub last_failure_reason: Option<String>,
    /// Version for which the pre-build Review turn was already run and
    /// accepted (not `Blocked`). Lets a resumed run — e.g. after a crash that
    /// left the package reviewed but never committed — skip straight to
    /// validate/commit instead of paying for another agent review turn on a
    /// spec that didn't change.
    #[serde(default)]
    pub reviewed_version: Option<String>,
    /// Hash of the spec file's contents at the moment `reviewed_version` was
    /// recorded. `reviewed_version` alone is not enough: the version can
    /// stay the same while the spec on disk changes underneath it (a manual
    /// edit, or a partially-applied fix from an interrupted run), and
    /// resuming must not treat that changed-but-unreviewed spec as already
    /// reviewed just because the version tag matches.
    #[serde(default)]
    pub reviewed_spec_hash: Option<u64>,
    /// Total model-call USD spent during the `cumulative_spent_run_id`
    /// run. Persisted so an outer-loop retry WITHIN that run (which
    /// recreates `run_package` from scratch for the same package)
    /// enforces `budget_usd` as a per-package cap rather than resetting
    /// to zero on every call — the Sep review called out the per-call
    /// reset as "neither a whole-closure nor a durable per-package cap".
    ///
    /// Deliberately scoped to one run (keyed by `cumulative_spent_run_id`)
    /// rather than running since the dawn of time: a fresh invocation of
    /// the CLI gets a fresh \$0 budget. Carrying balances across
    /// restarts would mean the first run accidentally spent its whole
    /// budget on a doomed package, every subsequent run would refuse to
    /// touch it with no visible way to reset short of editing state.json
    /// by hand. The user explicitly asked for the restart-reset.
    #[serde(default)]
    pub cumulative_spent_usd: f64,
    /// Which run_id's spend `cumulative_spent_usd` belongs to. When
    /// `run_package` loads state.json and this doesn't match the current
    /// run_id, `cumulative_spent_usd` is treated as 0 (and the field is
    /// overwritten with the new run_id on the next save). Serde-defaulted
    /// so pre-existing state.json files without this field read as 'from
    /// an earlier run, start fresh' automatically.
    #[serde(default)]
    pub cumulative_spent_run_id: Option<String>,
    /// Fix-attempt count within the `cumulative_spent_run_id` run, mirroring
    /// `cumulative_spent_usd`'s scoping exactly (same field gates both —
    /// there is no separate run_id for this one). Without this, a package
    /// that returns to `run_package` again later in the SAME CLI run (e.g.
    /// after a `MissingDeps` round trip once its dependencies are built)
    /// restarted its local `attempts` counter at 0: `max_attempts` reset to
    /// a fresh budget on top of whatever it already burned, and notes.md's
    /// "Attempt N" headers (append-only, never truncated) collided with
    /// attempt numbers already written by the earlier call.
    #[serde(default)]
    pub cumulative_attempts: usize,
    /// Sorted+deduped OBS-reported unresolvable-dependency set for which the
    /// one-off `TranslateUnresolvableDeps` agent turn already ran. Prevents
    /// paying for another lookup if this exact package resurfaces with the
    /// identical set of missing deps (e.g. a dependent is still being built
    /// and this package gets requeued against the same gap).
    #[serde(default)]
    pub unresolvable_review_hash: Option<u64>,
    /// Upstream names the agent returned for `unresolvable_review_hash`'s
    /// set, if any. Reused instead of OBS's raw names the next time that
    /// exact set recurs, since the agent isn't re-asked.
    #[serde(default)]
    pub unresolvable_translated: Vec<String>,
}

/// Hash spec content for [`PkgState::reviewed_spec_hash`]. Not cryptographic —
/// only used to detect "did the spec change since it was reviewed", not for
/// anything security-sensitive.
fn hash_spec(content: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    content.hash(&mut hasher);
    hasher.finish()
}

/// Hash a sorted+deduped dependency-name list for
/// [`PkgState::unresolvable_review_hash`]. `\0`-joined so e.g. `["a", "bc"]`
/// and `["ab", "c"]` don't collide.
fn hash_deps(sorted_deps: &[String]) -> u64 {
    hash_spec(&sorted_deps.join("\0"))
}

impl PkgState {
    pub fn load(ws: &Workspace) -> Self {
        let path = ws.state_json();
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Self::default(),
            Err(e) => {
                ap_core::diag!(
                    "  cannot read {} ({e}); starting from empty state",
                    path.display()
                );
                return Self::default();
            }
        };
        serde_json::from_str(&text).unwrap_or_else(|e| {
            // Keep the unreadable file: defaulting and then saving would
            // otherwise erase the package's history without a trace.
            let bad = path.with_extension("json.bad");
            ap_core::diag!(
                "  {} is corrupt ({e}); keeping it as {} and starting fresh",
                path.display(),
                bad.display()
            );
            if let Err(re) = std::fs::rename(&path, &bad) {
                ap_core::diag!(
                    "  cannot move {} aside ({re}); the next save will overwrite it",
                    path.display()
                );
            }
            Self::default()
        })
    }

    /// Written to a temporary file and renamed, so a crash mid-write leaves
    /// the previous state rather than a truncated one. Errors name the file.
    pub fn save(&self, ws: &Workspace) -> std::io::Result<()> {
        let s = serde_json::to_string_pretty(self)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        let path = ws.state_json();
        // `write_atomic` names its temp file uniquely, so two writers never
        // interleave their bytes and the rename never publishes a torn file.
        ap_core::fsx::write_atomic(&path, s)
            .map_err(|e| std::io::Error::new(e.kind(), format!("{}: {e}", path.display())))
    }

    /// Read-modify-write of state.json under an exclusive lock, so two workers
    /// (or two processes sharing a state root) cannot both start from the same
    /// version and have the later save erase the earlier one's change. Returns
    /// the state as stored.
    pub fn update(ws: &Workspace, f: impl FnOnce(&mut PkgState)) -> std::io::Result<PkgState> {
        let path = ws.state_json();
        let lock_path = path.with_extension("json.lock");
        // Held until `_lock` drops, after the save, so the whole
        // load-modify-save is one critical section.
        let _lock = ap_core::proc_lock::lock_with_wait(&lock_path, Duration::from_secs(60))
            .map_err(|e| {
                std::io::Error::other(format!("{}: {}", lock_path.display(), lock_error_text(&e)))
            })?;
        let mut state = Self::load(ws);
        f(&mut state);
        state.save(ws)?;
        Ok(state)
    }

    /// Does state.json record a GREEN build of this version?
    ///
    /// Only `success` qualifies. `wheel-only` previously counted too, but
    /// the Sep review (reproduced in the Oct follow-up) called that out:
    /// the generation branch that writes `wheel-only` ALSO returns
    /// `PipelineResult::Failed { reason: "wheel-only package - candidates: ..." }`.
    /// So the state tag and the pipeline result disagree — a package the
    /// pipeline reports as failed looks "already built" to the very next
    /// `select_target` call, which then skips it forever and anything
    /// that depends on it is unresolvable with no visible retry.
    /// `wheel-only` is a terminal failure marker, not a success.
    pub fn is_built(&self, version: &str) -> bool {
        self.last_built_version.as_deref() == Some(version)
            && self.last_outcome.as_deref() == Some("success")
            && self.policy_current()
    }

    /// Whether the last success was checked against the current policy.
    // The comparison is always true while POLICY_VERSION is 0; it becomes a
    // real check the first time the version is bumped.
    #[allow(clippy::absurd_extreme_comparisons)]
    pub fn policy_current(&self) -> bool {
        self.policy_version >= POLICY_VERSION
    }

    pub fn is_reviewed(&self, version: &str, spec_content: &str) -> bool {
        self.reviewed_version.as_deref() == Some(version)
            && self.reviewed_spec_hash == Some(hash_spec(spec_content))
    }

    /// Has the one-off dependency-name translation already run for exactly
    /// this (sorted+deduped) set of unresolvable deps?
    pub fn already_reviewed_unresolvable(&self, sorted_deps: &[String]) -> bool {
        self.unresolvable_review_hash == Some(hash_deps(sorted_deps))
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

/// Find the node `--target` names in the resolved closure.
///
/// Matches against both the node's own (PyPI) name and its derived RPM name,
/// fuzzily — `-`/`_`/`.` and case are not meaningful dependency identity (see
/// `fuzzy_key`'s doc comment), and a caller has no way to know in advance
/// which of the two spellings a given node happens to be keyed under. Getting
/// this wrong used to silently check out and commit to a brand-new,
/// wrongly-named OBS package instead of the real one (`python-backports.zstd`
/// vs. the actual `python-backports-zstd`) rather than failing loudly.
fn find_explicit_target<'a>(
    nodes: &'a [ap_ecosystem::pip::Node],
    target: &str,
) -> Result<&'a ap_ecosystem::pip::Node, String> {
    let target_key = osc::fuzzy_key(target);
    nodes
        .iter()
        .find(|n| {
            osc::fuzzy_key(&n.name) == target_key
                || osc::fuzzy_key(&osc::rpm_name(&n.name)) == target_key
        })
        .ok_or_else(|| {
            let mut names: Vec<&str> = nodes.iter().map(|n| n.name.as_str()).collect();
            names.sort_unstable();
            format!(
                "package '{target}' not in dependency closure — available: {}",
                names.join(", ")
            )
        })
}

/// Does OBS reporting this status mean a dependent package will actually find
/// a built binary when it resolves against this one?
///
/// Only `Succeeded` is a hard yes; `Blocked`/`Building`/`Scheduled` are "will
/// be, soon" and left in to avoid duplicate scheduling of work OBS is already
/// doing. Everything else — `Failed`, `Unresolvable`, `Broken`, `Disabled`,
/// `Excluded`, `Unknown`, or `None` (no entry at all for this distro/arch) —
/// is a no.
///
/// This used to allow every non-{Failed, Unresolvable, Broken} status through
/// (and `None` fell into "non-failed" too), which treated a Disabled source
/// and a never-built-for-this-arch package as if their binaries were sitting
/// in the repo waiting to be installed. The mocked disabled-source case the
/// Oct 6 review probed with (`demo==999.0` returning Ok with no RPM produced)
/// was exactly this: a `python-demo` source existed in the project, OBS
/// reported it Disabled, `select_target` saw "not Failed/Unresolvable/Broken",
/// called the package resolvable, found nothing left to build, and the whole
/// run exited successful.
pub fn status_is_resolvable(status: Option<&osc::BuildStatus>) -> bool {
    matches!(
        status,
        Some(
            osc::BuildStatus::Succeeded
                | osc::BuildStatus::Blocked
                | osc::BuildStatus::Building
                | osc::BuildStatus::Scheduled
        )
    ) || matches!(status, Some(osc::BuildStatus::Unknown(code)) if code == "finished")
}

#[cfg(test)]
mod resolvable_tests {
    use super::*;

    #[test]
    fn a_finished_build_without_a_result_is_still_in_flight() {
        let finished = osc::BuildStatus::Unknown("finished".to_string());
        assert!(status_is_resolvable(Some(&finished)));
        assert!(!status_is_resolvable(Some(&osc::BuildStatus::Failed)));
        assert!(!status_is_resolvable(Some(&osc::BuildStatus::Unknown(
            "weird".into()
        ))));
    }
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
/// `permanently_failed` holds RPM names the caller has given up retrying
/// (see [`crate::pipeline`] retry-count tracking in the CLI build loop) —
/// they and anything depending on them are excluded from the frontier so a
/// package that can never succeed doesn't get re-selected forever.
///
/// `force` tells the gate the user asked explicitly to rebuild (via `--force`,
/// or by invoking `update`, which always passes it): the root (requested)
/// node is then never treated as resolvable, even if it is already green in
/// OBS and locally. Without this, `--force` is unenforceable from here —
/// `run_package`'s own force check only fires if the package reaches it in the
/// first place, and `select_target` would otherwise skip it before it ever
/// does. This was the mechanical path the Oct 6 review's `update --force
/// --regen` reproducer took to return Ok without ever running a build.
///
/// Returns `Ok(vec![])` when every package in the closure is already
/// resolvable, or when nothing further can be built because everything left
/// is blocked by a `permanently_failed` package.
pub fn select_target(
    name: &str,
    explicit_target: Option<&str>,
    // Pin the root package to this version, if the user asked for one.
    pinned_version: Option<&str>,
    config: &PipelineConfig,
    permanently_failed: &std::collections::HashSet<String>,
    force: bool,
    reporter: &dyn Reporter,
) -> Result<Vec<TargetInfo>, String> {
    reporter.report(&Event::ResolveStarted {
        root: name.to_string(),
    });

    // Kick off the package availability fetch concurrently with pip resolve.
    // Checked separately, not merged: whether a build-status check applies
    // depends on which side found the package (see the `resolvable` comment
    // below).
    let obs_project = config.project.clone();
    let (nodes, own_packages, linked_packages, build_statuses) = std::thread::scope(|scope| {
        let own_handle = scope.spawn(|| {
            osc::list_project_packages(&obs_project, Emit::run(reporter)).unwrap_or_default()
        });
        let linked_handle = scope.spawn(|| {
            let mut linked = std::collections::HashSet::new();
            for (path_project, _repo) in
                osc::get_project_repo_paths(&obs_project, Emit::run(reporter))
            {
                // `list_project_packages` already warned on failure; a project
                // we could not list contributes nothing.
                linked.extend(
                    osc::list_project_packages(&path_project, Emit::run(reporter))
                        .unwrap_or_default(),
                );
            }
            linked
        });
        let statuses_handle = scope.spawn(|| {
            osc::get_project_build_statuses(
                &obs_project,
                &config.distro,
                &config.arch,
                Emit::run(reporter),
            )
        });
        let nodes = ap_ecosystem::pip::resolve(name, pinned_version);
        // Join the fetches (likely already done while pip was resolving).
        (
            nodes,
            join_or_warn(own_handle, "own"),
            join_or_warn(linked_handle, "linked"),
            join_or_warn(statuses_handle, "statuses"),
        )
    });
    let nodes = nodes.map_err(|e| format!("resolve failed for {name}: {e}"))?;

    let total = nodes.len();

    // A node already present in the user's own project is resolvable unless
    // OBS reports its own build as actually failing there, in which case it
    // stays unresolved so the frontier picks it back up and `run_package`'s
    // existing build-log-driven fix path can retry it — instead of a broken
    // OBS package silently blocking everything above it forever.
    //
    // A node present only in a linked/build project (e.g. openSUSE:Factory,
    // reached via `osc meta prj`'s `<path>` entries) is resolvable on mere
    // presence, with no build-status check: we don't own or rebuild those
    // packages, so there is nothing to retry, and querying that project's
    // build status per its own repo/arch is unreliable (naming and listing
    // quirks in a project we don't control) — requiring it previously caused
    // already-available packages like `python-setuptools` to be queued for
    // packaging again. Presence in the linked project's sources is exactly
    // the signal OBS itself uses to consider a package "available".
    //
    // It's also resolvable if its local state.json records a prior
    // "success" outcome. (`wheel-only` state is a terminal FAILURE marker,
    // not a success — see `PkgState::is_built`'s doc comment — so it does
    // not count here. A wheel-only package stays unresolved and
    // re-selected until it either becomes genuinely buildable from source
    // or the top-level retry gives up on it.)
    // `osc::rpm_name` is only a guess — openSUSE has no single deterministic
    // rule for how a PyPI name's separators map to the RPM name (see its doc
    // comment: `pydantic_core` is packaged as `python-pydantic-core`, but
    // `typing_extensions` keeps its underscore). Comparing the guess for
    // exact string equality against the real project listings previously
    // made already-available packages like `python-pydantic-core` look
    // unresolvable and get queued as duplicates. Index both listings by a
    // separator-blind `fuzzy_key` instead, and use the *real* listed name
    // (not the guess) for the build-status lookup once a match is found.
    let own_fuzzy: std::collections::HashMap<String, &String> = own_packages
        .iter()
        .map(|p| (osc::fuzzy_key(p), p))
        .collect();
    let linked_fuzzy: std::collections::HashSet<String> =
        linked_packages.iter().map(|p| osc::fuzzy_key(p)).collect();

    // How a node came to be resolvable, so the two very different reasons —
    // "already in the distro, nothing to build" vs. "this project built it"
    // — can be reported as separate counts instead of one combined number
    // that made the distro's own contribution look like this run's work.
    #[derive(Clone, Copy, PartialEq)]
    enum Resolution {
        No,
        /// Present in a linked/path project, or a system-tool alias
        /// ([`SYSTEM_TOOL_ALIASES`]) — the distro already has it.
        Distro,
        /// Present in this project's own OBS listing, or already built in
        /// the local workspace.
        Built,
    }

    let resolution: Vec<Resolution> = nodes
        .iter()
        .map(|n| {
            // `--force` on the root was previously only observed inside
            // `run_package`, which never ran if `select_target` had already
            // skipped the package: so a `--force` or `update --force --regen`
            // against a package OBS already showed green silently exited
            // successful without building anything (Oct 6 review finding #1).
            // Mark the user's own requested root as unresolved up front so it
            // reaches the frontier; the rest of the closure still goes
            // through the ordinary resolvability check below so a forced
            // rebuild does not stampede every already-built dependency with
            // it.
            if force && n.is_requested {
                return Resolution::No;
            }
            // Some PyPI packages exist purely to redistribute a binary that
            // openSUSE already ships as its own identically-named system
            // package (`ninja` is the case that prompted this: it's a
            // pip-installable copy of the `ninja` build tool, pulled in by
            // build backends like scikit-build-core/meson-python that just
            // shell out to it). Packaging a `python-<name>` RPM for one of
            // these would only duplicate the system package, so treat them
            // as already resolved rather than ever selecting them as a
            // build target — see `SYSTEM_TOOL_ALIASES`.
            if is_system_tool_alias(&n.name) {
                return Resolution::Distro;
            }
            let rpm = osc::rpm_name(&n.name);
            let key = osc::fuzzy_key(&rpm);
            if let Some(&real) = own_fuzzy.get(&key) {
                return if status_is_resolvable(build_statuses.get(real)) {
                    Resolution::Built
                } else {
                    Resolution::No
                };
            }
            if linked_fuzzy.contains(&key) {
                return Resolution::Distro;
            }
            // Check local workspace state.
            let ws_name = ap_core::run::sanitize_name(&rpm);
            let state_path = config.state_root.join(ws_name).join("state.json");
            let built = match std::fs::read_to_string(&state_path) {
                Ok(s) => match serde_json::from_str::<PkgState>(&s) {
                    Ok(st) => st.is_built(&n.version),
                    Err(e) => {
                        ap_core::diag!(
                            "  {} is unreadable ({e}); treating {} as not built",
                            state_path.display(),
                            n.name
                        );
                        false
                    }
                },
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => false,
                Err(e) => {
                    ap_core::diag!(
                        "  cannot read {} ({e}); treating {} as not built",
                        state_path.display(),
                        n.name
                    );
                    false
                }
            };
            if built {
                Resolution::Built
            } else {
                Resolution::No
            }
        })
        .collect();

    let resolvable: Vec<bool> = resolution.iter().map(|r| *r != Resolution::No).collect();
    let in_distro_count = resolution
        .iter()
        .filter(|r| **r == Resolution::Distro)
        .count();
    let built_count = resolution
        .iter()
        .filter(|r| **r == Resolution::Built)
        .count();
    let remaining = total - in_distro_count - built_count;

    reporter.report(&Event::ResolveFinished {
        total,
        built: built_count,
        in_distro: in_distro_count,
        remaining,
    });

    // Explicit target override — skip the resolvability gate.
    if let Some(target) = explicit_target {
        let node = find_explicit_target(&nodes, target)?.clone();
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
        // Given up on this one already — leave it (and anything that needs
        // it) out of the frontier instead of re-selecting it forever.
        if permanently_failed.contains(&osc::rpm_name(&node.name)) {
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
        // Nothing buildable but packages remain. If EVERY still-unresolved
        // package is explained by a `permanently_failed` package — it failed
        // itself, or it depends (transitively, within the closure) on one
        // that did — that's an expected terminal state: the caller reports
        // it and stops rather than treating it as a resolver bug. A
        // `permanently_failed` set that doesn't cover everything left
        // unresolved means something else is also stuck (e.g. an unrelated
        // dependency cycle), which must still surface as an error instead of
        // being silently swallowed just because *some* package gave up.
        let mut explained = vec![false; nodes.len()];
        for (i, node) in nodes.iter().enumerate() {
            if !resolvable[i] && permanently_failed.contains(&osc::rpm_name(&node.name)) {
                explained[i] = true;
            }
        }
        loop {
            let mut changed = false;
            for (i, node) in nodes.iter().enumerate() {
                if resolvable[i] || explained[i] {
                    continue;
                }
                let deps = dep_names_from_requires_dist(&node.requires_dist);
                let any_explained_dep = deps
                    .iter()
                    .any(|dep| name_to_idx.get(dep).map(|&j| explained[j]).unwrap_or(false));
                let all_deps_settled = deps.iter().all(|dep| {
                    name_to_idx
                        .get(dep)
                        .map(|&j| resolvable[j] || explained[j])
                        .unwrap_or(true)
                });
                if any_explained_dep && all_deps_settled {
                    explained[i] = true;
                    changed = true;
                }
            }
            if !changed {
                break;
            }
        }

        let blocked: Vec<String> = nodes
            .iter()
            .enumerate()
            .filter(|(i, _)| !resolvable[*i])
            .map(|(_, n)| n.name.clone())
            .collect();

        let all_explained = nodes
            .iter()
            .enumerate()
            .all(|(i, _)| resolvable[i] || explained[i]);
        if all_explained && !permanently_failed.is_empty() {
            return Ok(vec![]);
        }

        return Err(format!(
            "cannot build {name}: stuck — unresolvable packages: {}",
            blocked.join(", ")
        ));
    }

    Ok(frontier)
}

/// PyPI names (normalized, see [`normalize_pkg_name`]) that are pure
/// redistributions of a tool openSUSE already ships as a system package of
/// the same name. Callers depending on one of these want the executable on
/// PATH, not an importable Python module — see the `resolvable` computation
/// in [`select_target`] and convention 27's "PyPI packages that just
/// redistribute a system tool" in the agent prompt.
///
/// Deliberately a single, hardcoded, hand-verified entry rather than a
/// user-extensible config file: unlike `flavors.toml` (which genuinely
/// varies per work directory/distro), whether a PyPI package just
/// redistributes a system binary is a fixed fact about that package, not
/// something that differs between work directories — and confirming one
/// needs checking that the actual consumer shells out to the tool rather
/// than importing its Python module (`ninja` is correct because that one
/// case was spot-checked, not because every same-shaped PyPI package works
/// this way). That verification belongs in reviewed source control, not a
/// `<work-dir>/`-local file that `--cleanup` can wipe and that never
/// benefits any other work directory in the first place.
const SYSTEM_TOOL_ALIASES: &[&str] = &["ninja"];

/// Whether `name` (a bare PyPI/import name, not an RPM name) is one of
/// [`SYSTEM_TOOL_ALIASES`] — a dependency that should resolve against the
/// existing system package instead of ever being built as its own
/// `python-<name>` RPM. Exposed so callers outside this module (the CLI's
/// agent-discovered-dependency queue) can skip these the same way
/// `select_target` does for pip-closure nodes.
pub fn is_system_tool_alias(name: &str) -> bool {
    SYSTEM_TOOL_ALIASES.contains(&normalize_pkg_name(name).as_str())
}

/// Normalize a PyPI package name to a consistent lookup key.
///
/// Delegates to [`osc::fuzzy_key`] rather than keeping a second, weaker
/// definition here: this used to only lowercase and fold `-` into `_`,
/// which still let a dotted name (`ruamel.yaml`) fail to match a
/// `requires_dist` entry spelling it with a hyphen or underscore instead —
/// the exact same class of bug `fuzzy_key`'s doc comment documents for RPM
/// names, just on the PyPI-graph side of this module instead of the OBS
/// side.
fn normalize_pkg_name(name: &str) -> String {
    osc::fuzzy_key(name)
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
                    c == '('
                        || c == '['
                        || c == ';'
                        || c == '>'
                        || c == '<'
                        || c == '='
                        || c == '!'
                        || c == ' '
                })
                .next()?
                .trim();
            if name.is_empty() {
                None
            } else {
                Some(normalize_pkg_name(name))
            }
        })
        .collect()
}

/// How many times one turn is simply re-issued, with no pause, when the API
/// answers 429. A rejected call costs nothing and the quota is a moving
/// window, so retrying straight away is the fastest way through; past this
/// the failure goes to the round loop, which retries the package next round.
const MAX_IMMEDIATE_RATE_LIMIT_RETRIES: u32 = 5;

/// Agent turns running at once, process-wide. Unlimited by default so packages
/// get built as fast as the API allows; set `AUTO_PACKAGE_AGENT_JOBS=<n>` to cap
/// it if the model quota (429s) is the bottleneck.
static AGENT_ACTIVE: (std::sync::Mutex<usize>, std::sync::Condvar) =
    (std::sync::Mutex::new(0), std::sync::Condvar::new());

struct AgentSlot;

impl AgentSlot {
    /// `None` = no cap.
    fn limit() -> Option<usize> {
        std::env::var("AUTO_PACKAGE_AGENT_JOBS")
            .ok()
            .and_then(|v| v.parse::<usize>().ok())
            .filter(|n| *n > 0)
    }

    fn acquire() -> Self {
        let (lock, cvar) = &AGENT_ACTIVE;
        let max = Self::limit();
        let mut active = lock.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(max) = max {
            while *active >= max {
                active = cvar.wait(active).unwrap_or_else(|e| e.into_inner());
            }
        }
        *active += 1;
        AgentSlot
    }
}

impl Drop for AgentSlot {
    fn drop(&mut self) {
        let (lock, cvar) = &AGENT_ACTIVE;
        *lock.lock().unwrap_or_else(|e| e.into_inner()) -= 1;
        cvar.notify_one();
    }
}

/// Join a resolve-phase worker, reporting a panic instead of treating it as
/// an empty (and therefore believable) answer.
fn join_or_warn<T: Default>(h: std::thread::ScopedJoinHandle<'_, T>, what: &str) -> T {
    h.join().unwrap_or_else(|_| {
        ap_core::diag!("  the {what} lookup thread panicked; treating it as empty");
        T::default()
    })
}

// ── main pipeline ─────────────────────────────────────────────────────────────

/// RPM names currently inside `run_package` anywhere in this process.
///
/// Several root builds can share one process (the TUI lets the user add more
/// at runtime), and two of them can discover the same dependency. Both would
/// then drive the same checkout, state dir and OBS package at once. Holding
/// this guard serialises them: the second waits, then its own `is_built`
/// check turns a success into `AlreadyBuilt` instead of a duplicate build.
static IN_FLIGHT: (
    std::sync::Mutex<std::collections::BTreeSet<String>>,
    std::sync::Condvar,
) = (
    std::sync::Mutex::new(std::collections::BTreeSet::new()),
    std::sync::Condvar::new(),
);

struct InFlightGuard(String);

impl InFlightGuard {
    fn acquire(rpm_name: &str) -> Self {
        let (lock, cvar) = &IN_FLIGHT;
        let mut busy = lock.lock().unwrap_or_else(|e| e.into_inner());
        while busy.contains(rpm_name) {
            busy = cvar.wait(busy).unwrap_or_else(|e| e.into_inner());
        }
        busy.insert(rpm_name.to_string());
        Self(rpm_name.to_string())
    }
}

impl Drop for InFlightGuard {
    fn drop(&mut self) {
        let (lock, cvar) = &IN_FLIGHT;
        lock.lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&self.0);
        cvar.notify_all();
    }
}

/// A build can run for a long time, so a second process waits this long for
/// the first to finish before giving up.
const BUILD_LOCK_WAIT: Duration = Duration::from_secs(30 * 60);

fn lock_error_text(e: &ap_core::proc_lock::LockError) -> String {
    match e {
        ap_core::proc_lock::LockError::Held(Some(pid)) => format!("held by pid {pid}"),
        ap_core::proc_lock::LockError::Held(None) => "held by another process".to_string(),
        ap_core::proc_lock::LockError::Io(e) => e.to_string(),
    }
}

pub fn run_package(
    config: &PipelineConfig,
    pkg_run: &PkgRunDir,
    reporter: &dyn Reporter,
) -> PipelineResult {
    let _in_flight = InFlightGuard::acquire(&config.rpm_name);
    // The in-flight guard is per process; this lock keeps a second `ap` on the
    // same state root from building the same package at once. Held for the
    // whole run. A failed open is left for `run_package_inner` to report.
    let _build_lock = match Workspace::open(&config.state_root, &config.rpm_name) {
        Ok(ws) => {
            let lock_path = ws.dir().join("build.lock");
            match ap_core::proc_lock::lock_with_wait(&lock_path, BUILD_LOCK_WAIT) {
                Ok(f) => Some(f),
                Err(e) => {
                    return PipelineResult::Failed {
                        rpm_name: config.rpm_name.clone(),
                        reason: format!(
                            "could not take {}: {}",
                            lock_path.display(),
                            lock_error_text(&e)
                        ),
                    };
                }
            }
        }
        Err(_) => None,
    };
    let result = run_package_inner(config, pkg_run, reporter);
    // Persist last-attempt status in the durable workspace so a resumed run
    // (and the TUI showing per-package state across runs) can see WHY the
    // last attempt ended, instead of only "last_outcome: null" on anything
    // that didn't successfully build. Success paths already set
    // `last_outcome` to "success"/"wheel-only" inline — this covers every
    // unsuccessful path, which otherwise left state.json untouched and made
    // a mid-run crash indistinguishable from "never attempted".
    //
    // `MissingDeps` is covered here too, not just `Failed` — see
    // `PkgState::last_failure_reason`'s doc comment for the stale-reason bug
    // this closes (a `MissingDeps` round used to leave an older, unrelated
    // `Failed` reason sitting in state.json untouched).
    let outcome = match &result {
        PipelineResult::Failed { reason, .. } => Some(("failed", reason.clone())),
        PipelineResult::MissingDeps { deps, .. } => Some((
            "missing_deps",
            format!("waiting on dependencies: {}", deps.join(", ")),
        )),
        PipelineResult::Built { .. } | PipelineResult::AlreadyBuilt { .. } => None,
    };
    let ws = Workspace::open(&config.state_root, &config.rpm_name)
        .inspect_err(|e| {
            reporter.report(&Event::Warning {
                step: None,
                message: format!("opening the package workspace failed: {e}"),
            });
        })
        .ok();
    if let (PipelineResult::Built { .. }, Some(ws)) = (&result, &ws) {
        package_notes_step(config, pkg_run, ws, reporter);
    }
    if let Some((outcome, reason)) = outcome
        && let Some(ws) = &ws
    {
        update_state(ws, reporter, |state| {
            state.last_built_version = Some(config.version.clone());
            state.last_outcome = Some(outcome.to_string());
            state.last_failure_reason = Some(reason);
        });
    }
    result
}

/// Refresh the package's notes as a visible step. It runs after the build has
/// succeeded but before `PackageFinished`, so the TUI must show it: otherwise
/// the package reads as "running" with no step for as long as the model takes.
/// A failure only warns; the package stays built.
fn package_notes_step(
    config: &PipelineConfig,
    pkg_run: &PkgRunDir,
    ws: &Workspace,
    reporter: &dyn Reporter,
) {
    let Ok(step) = StepGuard::begin(
        pkg_run,
        reporter,
        StepKind::PackageNotes,
        None,
        &mut Attempts::default(),
    ) else {
        return;
    };
    let run = crate::package_notes::update_after_build(
        ws,
        &config.rpm_name,
        &config.version,
        &config.project,
        &config.distro,
        &config.arch,
    );
    // Charged before the result is looked at: a failed notes turn still cost
    // money, and that spend must reach the run total and the package's state.
    if run.usage.cost_usd > 0.0 || run.usage.input_tokens + run.usage.output_tokens > 0 {
        reporter.report(&Event::NotesUsage {
            step: step.id,
            usage: run.usage.clone(),
        });
        add_run_spend(ws, &config.run_id, run.usage.cost_usd);
    }
    let outcome = match run.result {
        Ok(()) => StepOutcome::Ok,
        Err(e) => {
            reporter.report(&Event::Warning {
                step: Some(step.id),
                message: format!("package notes not updated: {e}"),
            });
            StepOutcome::Skipped
        }
    };
    step.finish(outcome);
}

/// Reject a name that is about to be joined into a filesystem path as a single
/// component: it must be non-empty, must not contain `/` or `..`, and must not
/// start with `.` (which covers `.` and hidden directories).
fn validate_path_component(label: &str, value: &str) -> Result<(), String> {
    if value.is_empty() {
        return Err(format!("{label} is empty"));
    }
    if value.contains('/') || value.contains("..") || value.starts_with('.') {
        return Err(format!(
            "{label} {value:?} is not a safe path component (no '/', '..', or leading '.')"
        ));
    }
    Ok(())
}

/// What every stage of one package run reads: configuration, the run
/// directory, the reporter, the workspace and the paths derived from the name.
struct RunCtx<'a> {
    config: &'a PipelineConfig,
    pkg_run: &'a PkgRunDir,
    reporter: &'a dyn Reporter,
    ws: Workspace,
    rpm_name: String,
    checkout_dir: PathBuf,
    spec_path: PathBuf,
}

impl RunCtx<'_> {
    fn failed(&self, reason: String) -> PipelineResult {
        PipelineResult::Failed {
            rpm_name: self.rpm_name.clone(),
            reason,
        }
    }

    fn turn_env(&self) -> TurnEnv<'_> {
        TurnEnv {
            config: self.config,
            ws: &self.ws,
            pkg_run: self.pkg_run,
            reporter: self.reporter,
        }
    }
}

/// The state that stages mutate as the run proceeds.
struct RunState {
    step_attempts: Attempts,
    sdist_top_level: String,
    from_repo_archive: bool,
    /// Fix attempts used so far (persisted for outer-loop retries).
    attempts: usize,
    spent_usd: f64,
    session_id: Option<String>,
}

/// What `generate_spec_stage` learned about the package.
struct SpecInfo {
    need_spec: bool,
    is_maturin: bool,
    is_rust: bool,
}

/// Whether a round of the validate/build loop moves on or goes around again.
enum Flow {
    Proceed,
    Retry,
}

/// How a build round ended: another round is needed, or the package is done.
enum RoundEnd {
    Retry,
    Finished(PipelineResult),
}

fn run_package_inner(
    config: &PipelineConfig,
    pkg_run: &PkgRunDir,
    reporter: &dyn Reporter,
) -> PipelineResult {
    match run_stages(config, pkg_run, reporter) {
        Ok(result) | Err(result) => result,
    }
}

fn run_stages(
    config: &PipelineConfig,
    pkg_run: &PkgRunDir,
    reporter: &dyn Reporter,
) -> Result<PipelineResult, PipelineResult> {
    let ctx = preflight(config, pkg_run, reporter)?;
    let mut st = RunState {
        step_attempts: Attempts::default(),
        sdist_top_level: String::new(),
        from_repo_archive: false,
        attempts: 0,
        spent_usd: 0.0,
        session_id: None,
    };

    let obs_status = checkout_stage(&ctx, &mut st)?;
    let spec = generate_spec_stage(&ctx, &mut st)?;
    osc_service_stage(&ctx, &mut st, &spec)?;
    fetch_sdist_stage(&ctx, &mut st)?;
    resume_budget(&ctx, &mut st);

    let build_log_nonempty = ctx
        .ws
        .build_log()
        .metadata()
        .map(|m| m.len() > 0)
        .unwrap_or(false);
    review_stage(&ctx, &mut st, &spec, build_log_nonempty)?;
    prefix_stage(&ctx, &mut st, &spec, &obs_status, build_log_nonempty)?;

    loop {
        st.step_attempts.round += 1;
        if let Flow::Retry = validate_round(&ctx, &mut st)? {
            continue;
        }
        commit_step(&ctx, &mut st)?;
        match build_round(&ctx, &mut st)? {
            RoundEnd::Retry => continue,
            RoundEnd::Finished(result) => return Ok(result),
        }
    }
}

/// Name checks and the local-state fast path; opens the workspace.
fn preflight<'a>(
    config: &'a PipelineConfig,
    pkg_run: &'a PkgRunDir,
    reporter: &'a dyn Reporter,
) -> Result<RunCtx<'a>, PipelineResult> {
    let rpm_name = config.rpm_name.clone();
    if let Err(reason) = validate_path_component("package name", &rpm_name)
        .and_then(|()| validate_path_component("project", &config.project))
    {
        return Err(PipelineResult::Failed { rpm_name, reason });
    }
    let ws = match Workspace::open(&config.state_root, &rpm_name) {
        Ok(w) => w,
        Err(e) => {
            return Err(PipelineResult::Failed {
                rpm_name,
                reason: e.to_string(),
            });
        }
    };

    let prior = PkgState::load(&ws);
    if !config.force && prior.is_built(&config.version) {
        return Err(PipelineResult::AlreadyBuilt { rpm_name });
    }
    if prior.last_outcome.as_deref() == Some("success") && !prior.policy_current() {
        update_state(&ws, reporter, |stale| {
            stale.reviewed_version = None;
            stale.reviewed_spec_hash = None;
        });
    }

    let checkout_dir = config.work_dir.join(&config.project).join(&rpm_name);
    let spec_name = format!("{rpm_name}.spec");
    let spec_path = checkout_dir.join(&spec_name);
    Ok(RunCtx {
        config,
        pkg_run,
        reporter,
        ws,
        rpm_name,
        checkout_dir,
        spec_path,
    })
}

/// Step: Checkout, then the OBS-already-built fast path. Returns the package's
/// OBS build status for the later stages.
fn checkout_stage(
    ctx: &RunCtx,
    st: &mut RunState,
) -> Result<Option<osc::BuildStatus>, PipelineResult> {
    let RunCtx {
        config,
        pkg_run,
        reporter,
        ws,
        rpm_name,
        checkout_dir,
        spec_path,
    } = ctx;
    let reporter = *reporter;
    let step_attempts = &mut st.step_attempts;

    let obs_status: Option<osc::BuildStatus>;
    let has_local_changes: bool;
    {
        let step = begin_or_fail!(
            pkg_run,
            reporter,
            StepKind::Checkout,
            None,
            step_attempts,
            rpm_name.clone()
        );

        let emit = Emit::new(reporter, step.id);
        // `osc::checkout` already runs `osc up` when the checkout dir
        // exists (and freshly `osc co`s when it doesn't), so this one
        // call brings the local tree up to date in both cases. The
        // previous flow also ran `osc::pull` right after, which is a
        // second `osc up` on the same directory — the server round-trip
        // was doubled on every single build. Removed.
        let result = osc::ensure_package(&config.project, rpm_name, &config.work_dir, emit)
            .and_then(|()| {
                osc::checkout(&config.project, rpm_name, &config.work_dir, emit).map(|_| ())
            });

        if let Err(e) = result {
            let id = step.id;
            step.finish(StepOutcome::Failed);
            reporter.report(&Event::Warning {
                step: Some(id),
                message: e.to_string(),
            });
            return Err(ctx.failed(e.to_string()));
        }
        step.finish(StepOutcome::Ok);
        // Keep spec symlink fresh.
        let abs = spec_path
            .canonicalize()
            .unwrap_or_else(|_| spec_path.clone());
        warn_if_err(reporter, "updating the spec link", ws.link_spec(&abs));

        // A checkout dir is reused across runs (see `osc::checkout`), and
        // `osc up` never discards local uncommitted edits — so a spec that a
        // previous, interrupted run already bumped to a new version but
        // never `osc commit`-ed would otherwise make the AlreadyBuilt check
        // below believe OBS built a version it never actually saw.
        has_local_changes = osc::has_local_changes(checkout_dir, emit);

        // Fetch this package's current OBS build status once; reused below both
        // to grab the build log on failure and to skip the rest of the
        // pipeline when it already succeeded and nothing would change.
        obs_status = osc::get_build_status(
            &config.project,
            rpm_name,
            &config.distro,
            &config.arch,
            emit,
        )
        .ok();

        // Fetch the previous build log if OBS reports a failed or
        // unresolvable build. Non-fatal — PreFix uses ws.build_log() if
        // present. No separate `osc results --verbose` fetch for the
        // Unresolvable case: the agent already inspects the spec and reports
        // what's missing via `unresolvable_packages`, so the OBS round-trip
        // would only duplicate work the agent will do anyway.
        if matches!(
            obs_status,
            Some(osc::BuildStatus::Failed | osc::BuildStatus::Unresolvable)
        ) {
            let log_path = ws.build_log();
            warn_if_err(
                reporter,
                "fetching the failed build log",
                osc::fetch_buildlog(
                    &config.project,
                    rpm_name,
                    &config.distro,
                    &config.arch,
                    &log_path,
                    emit,
                ),
            );
        }
    }

    // OBS already has this exact version built successfully and nothing local
    // would change it — this covers the case local state.json (checked above)
    // doesn't know about, e.g. a different machine or a cleared state dir.
    // Re-checking-out and re-diffing a package that's already green wastes an
    // OBS build slot for no reason.
    if !config.force
        && !config.regen
        && spec_path.exists()
        && !has_local_changes
        && !spec_version_differs(spec_path, &config.version)
        && matches!(obs_status, Some(osc::BuildStatus::Succeeded))
        && PkgState::load(ws).policy_current()
    {
        update_state(ws, reporter, |state| {
            state.policy_version = POLICY_VERSION;
            state.last_built_version = Some(config.version.clone());
            state.last_outcome = Some("success".to_string());
            state.last_failure_reason = None;
        });
        // This process may have cached `config.project`'s package listing
        // before learning (just now, via the fresh — not cached —
        // `get_build_status` call above) that OBS already has this one.
        osc::invalidate_project_packages_cache(&config.project);
        return Err(PipelineResult::AlreadyBuilt {
            rpm_name: rpm_name.clone(),
        });
    }
    Ok(obs_status)
}

/// Step: GenerateSpec (if needed). Sets `sdist_top_level` and
/// `from_repo_archive` on the state.
fn generate_spec_stage(ctx: &RunCtx, st: &mut RunState) -> Result<SpecInfo, PipelineResult> {
    let RunCtx {
        config,
        pkg_run,
        reporter,
        ws,
        rpm_name,
        checkout_dir,
        spec_path,
    } = ctx;
    let reporter = *reporter;
    let step_attempts = &mut st.step_attempts;

    // Captured before generation can overwrite the spec: "did this package
    // already have a spec" must reflect what was checked out, not what we
    // are about to write.
    let spec_existed_before_generation = spec_path.exists();
    let need_spec = config.regen
        || !spec_existed_before_generation
        || spec_version_differs(spec_path, &config.version);

    let (sdist_top_level, is_maturin, from_repo_archive, is_rust) = if need_spec {
        let step = begin_or_fail!(
            pkg_run,
            reporter,
            StepKind::GenerateSpec,
            None,
            step_attempts,
            rpm_name.clone()
        );

        match generate_spec(config, checkout_dir, spec_path, &step) {
            Ok(res) => {
                let step = step.finish(StepOutcome::Ok);
                // "Initial packaging" is only true for a package that had no
                // spec before this run generated one — a version bump or
                // --regen on an already-packaged package must not be
                // (mis)recorded as an initial changelog entry. `create_initial_changes`
                // itself also skips if a `.changes` file already exists, so
                // either signal of "not actually new" is enough to skip.
                if !spec_existed_before_generation {
                    warn_if_err(
                        reporter,
                        "creating the initial .changes entry",
                        osc::create_initial_changes(
                            checkout_dir,
                            rpm_name,
                            Emit::new(reporter, step.id),
                        ),
                    );
                }
                // Link spec.generated into the workspace so the review agent can compare.
                #[cfg(unix)]
                {
                    let src = step.file("spec.generated");
                    let link = ws.dir().join("spec.generated");
                    warn_if_err(
                        reporter,
                        "linking spec.generated",
                        ap_core::fsx::replace_symlink(&src, &link),
                    );
                }
                (
                    res.sdist_top_level,
                    res.is_maturin,
                    res.from_repo_archive,
                    res.is_rust,
                )
            }
            Err(GenSpecError::WheelOnly {
                pkg_name,
                version,
                candidates,
            }) => {
                // The pipeline stops here, so the step did not produce a spec.
                let step = step.finish(StepOutcome::Failed);

                let mut message = format!(
                    "{pkg_name} {version} is published on PyPI as wheels only and its \
                     metadata names no source repository."
                );

                if !candidates.is_empty() {
                    message.push_str(&format!(
                        "\n\nAuto-discovered {} candidate repositories:\n{}",
                        candidates.len(),
                        candidates
                            .iter()
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
                         the source repository.",
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
                update_state(ws, reporter, |state| {
                    state.last_built_version = Some(version);
                    state.last_outcome = Some("wheel-only".to_string());
                    state.last_failure_reason = None;
                });
                return Err(ctx.failed(format!(
                    "wheel-only package - candidates: {}",
                    if !candidates.is_empty() {
                        candidates.join(", ")
                    } else {
                        "none found, needs WebSearch".to_string()
                    }
                )));
            }
            Err(GenSpecError::Other(e) | GenSpecError::NoReleaseArchive(e)) => {
                let step = step.finish(StepOutcome::Failed);
                reporter.report(&Event::Warning {
                    step: Some(step.id),
                    message: e.clone(),
                });
                return Err(ctx.failed(e));
            }
        }
    } else {
        // Sdist may already be extracted; find top-level dir name from the tarball
        // in the checkout dir (or fall back to a reasonable guess).
        let top = find_sdist_top_level(checkout_dir, &config.pkg_name, &config.version);
        step_attempts.skip(reporter, StepKind::GenerateSpec, SkipReason::SpecExists);
        let spec_text = std::fs::read_to_string(spec_path).unwrap_or_default();
        let is_maturin = spec_text.contains("maturin");
        let is_rust = spec_text.contains("cargo_build");
        (top, is_maturin, false, is_rust)
    };
    st.sdist_top_level = sdist_top_level;
    st.from_repo_archive = from_repo_archive;
    Ok(SpecInfo {
        need_spec,
        is_maturin,
        is_rust,
    })
}

/// Steps: OscService for Rust crates and for maturin packages.
fn osc_service_stage(
    ctx: &RunCtx,
    st: &mut RunState,
    spec: &SpecInfo,
) -> Result<(), PipelineResult> {
    let RunCtx {
        config,
        pkg_run,
        reporter,
        rpm_name,
        checkout_dir,
        ..
    } = ctx;
    let reporter = *reporter;
    let step_attempts = &mut st.step_attempts;
    let SpecInfo {
        need_spec,
        is_maturin,
        is_rust,
    } = *spec;

    // ── Step: OscService (Rust crates) ───────────────────────────────────────
    // `tar_scm` fetches the tag and `cargo_vendor` produces vendor.tar.zst, the
    // two sources the Rust spec template references. It has to run before the
    // commit, not before spec generation — the spec is a template either way.
    if is_rust && let Some(ref repo_url) = config.source_repo {
        let need_service = need_spec || !checkout_dir.join("_service").exists();
        if need_service {
            let step = begin_or_fail!(
                pkg_run,
                reporter,
                StepKind::OscService,
                None,
                step_attempts,
                rpm_name.clone()
            );

            let service_xml = render_service_file(repo_url, &config.version, &config.pkg_name);
            let result = ap_core::fsx::write_atomic(&checkout_dir.join("_service"), &service_xml)
                .map_err(|e| format!("writing _service failed for {}: {e}", config.pkg_name))
                .and_then(|()| {
                    osc::run_service(checkout_dir, Emit::new(reporter, step.id))
                        .map_err(|e| e.to_string())
                });

            if let Err(reason) = result {
                return Err(ctx.failed(reason));
            }
            step.finish(StepOutcome::Ok);
        } else {
            step_attempts.skip(reporter, StepKind::OscService, SkipReason::ServiceUpToDate);
        }
    } else if is_rust && !is_maturin {
        // Without a repository there is nothing for `tar_scm` to fetch; say so
        // rather than leave the step silently absent.
        step_attempts.skip(
            reporter,
            StepKind::OscService,
            SkipReason::Other("package has no source repository".to_string()),
        );
    }

    // ── Step: OscService (maturin packages) ──────────────────────────────────
    if is_maturin {
        // The crate maturin actually builds is `[tool.maturin] manifest-path`,
        // which is often NOT the root Cargo.toml (hf_xet: the root is a
        // workspace that EXCLUDES hf_xet/, whose own Cargo.lock holds `pprof`).
        // Vendoring only the root manifest leaves the registry short of the
        // build's crates: "no matching package named `pprof` found".
        let extra_manifests = find_tarball(checkout_dir)
            .and_then(|t| maturin_manifest_path(&t))
            .into_iter()
            .collect::<Vec<_>>();
        let service_lacks_manifest = std::fs::read_to_string(checkout_dir.join("_service"))
            .map(|svc| extra_manifests.iter().any(|m| !svc.contains(m.as_str())))
            .unwrap_or(false);
        let need_service =
            need_spec || !checkout_dir.join("registry.tar.zst").exists() || service_lacks_manifest;
        if need_service {
            let step = begin_or_fail!(
                pkg_run,
                reporter,
                StepKind::OscService,
                None,
                step_attempts,
                rpm_name.clone()
            );

            // Fix any broken workspace definitions in the tarball before vendoring
            if let Some(tarball) = find_tarball(checkout_dir) {
                warn_if_err(
                    reporter,
                    "fixing the cargo workspace in the tarball",
                    fix_cargo_workspace_in_tarball(&tarball, &config.pkg_name),
                );
            }

            let normalized = config.pkg_name.replace('-', "_").to_lowercase();
            let service_xml = render_maturin_service_file(&normalized, &extra_manifests);
            let result = ap_core::fsx::write_atomic(&checkout_dir.join("_service"), &service_xml)
                .map_err(|e| format!("writing _service failed for {}: {e}", config.pkg_name))
                .and_then(|()| {
                    osc::run_service(checkout_dir, Emit::new(reporter, step.id))
                        .map_err(|e| e.to_string())
                });

            if let Err(reason) = result {
                return Err(ctx.failed(reason));
            }
            step.finish(StepOutcome::Ok);
        } else {
            step_attempts.skip(reporter, StepKind::OscService, SkipReason::ServiceUpToDate);
        }
    }
    if !is_rust && !is_maturin {
        step_attempts.skip(reporter, StepKind::OscService, SkipReason::NotRust);
    }
    Ok(())
}

/// Step: FetchSdist, then the quilt `source/` symlink.
fn fetch_sdist_stage(ctx: &RunCtx, st: &mut RunState) -> Result<(), PipelineResult> {
    let RunCtx {
        pkg_run,
        reporter,
        ws,
        rpm_name,
        checkout_dir,
        ..
    } = ctx;
    let reporter = *reporter;
    let step_attempts = &mut st.step_attempts;

    // ── Step: FetchSdist (extract for log analysis, if not done) ─────────────
    if !ws.sdist_extracted() {
        let sdist_step = begin_or_fail!(
            pkg_run,
            reporter,
            StepKind::FetchSdist,
            None,
            step_attempts,
            rpm_name.clone()
        );

        // Find tarball in checkout and extract to ws.sdist_dir()
        let mut outcome = StepOutcome::Ok;
        if let Some(tarball) = find_tarball(checkout_dir) {
            let sdist_dir = ws.sdist_dir();
            let _ = std::fs::create_dir_all(&sdist_dir);
            if let Err(e) = ap_ecosystem::sdist::extract(&tarball, &sdist_dir) {
                // The agent reads `sdist/` to review the package; an empty one
                // looks like "no sources" with no hint why.
                reporter.report(&Event::Warning {
                    step: Some(sdist_step.id),
                    message: format!("extracting {} failed: {e}", tarball.display()),
                });
                outcome = StepOutcome::Failed;
            }
        } else {
            reporter.report(&Event::Warning {
                step: Some(sdist_step.id),
                message: "no source tarball found in the checkout".to_string(),
            });
            outcome = StepOutcome::Failed;
        }

        sdist_step.finish(outcome);
    } else {
        step_attempts.skip(
            reporter,
            StepKind::FetchSdist,
            SkipReason::Other("sdist already extracted".to_string()),
        );
    }

    // ── Quilt setup: create source/ symlink ───────────────────────────────────
    // Keep this fresh every run so the symlink is correct even when the sdist
    // top-level dir changes (e.g. after --regen with a new version).
    #[cfg(unix)]
    {
        let source_link = ws.dir().join("source");
        let source_target = ws.sdist_dir().join(&st.sdist_top_level);
        if source_target.exists() {
            warn_if_err(
                reporter,
                "linking source/",
                ap_core::fsx::replace_symlink(&source_target, &source_link),
            );
        } else if source_link.symlink_metadata().is_ok() {
            // A link to a directory that is gone would mislead quilt.
            let _ = std::fs::remove_file(&source_link);
        }
    }
    Ok(())
}

/// Resume cumulative spend AND the fix-attempt count from state.json ONLY
/// for an outer-loop retry inside the current CLI run (same run_id), so
/// both the dollar budget and `max_attempts` are enforced across the
/// retries of one package during one invocation — a package that comes
/// back through `run_package` again later in the same run (e.g. after a
/// `MissingDeps` round trip) must not get a completely fresh attempt
/// budget on top of what it already burned. A fresh CLI invocation
/// (different run_id) starts the package at $0/0 attempts — the user
/// asked for the restart-reset explicitly, and it matches the mental
/// model that a new command is a new budget. See
/// `PkgState::cumulative_spent_run_id`.
fn resume_budget(ctx: &RunCtx, st: &mut RunState) {
    let config = ctx.config;
    let prior = PkgState::load(&ctx.ws);
    let resuming_this_run =
        prior.cumulative_spent_run_id.as_deref() == Some(config.run_id.as_str());
    st.attempts = if resuming_this_run {
        prior.cumulative_attempts
    } else {
        0
    };
    st.spent_usd = if resuming_this_run {
        prior.cumulative_spent_usd
    } else {
        0.0
    };
    let (spent_usd, attempts, rpm_name) = (st.spent_usd, st.attempts, &ctx.rpm_name);
    if spent_usd > 0.0 || attempts > 0 {
        ctx.reporter.report(&Event::Progress {
            step: None,
            message: format!(
                "{rpm_name}: resuming with ${spent_usd:.4} spent and {attempts} fix \
                 attempt(s) already used on this package in this CLI run (budget cap \
                 ${:.4}, max {} attempts)",
                config.budget_usd, config.max_attempts
            ),
        });
    }
}

/// Step: Review (freshly generated specs, or existing spec with no prior build).
fn review_stage(
    ctx: &RunCtx,
    st: &mut RunState,
    spec: &SpecInfo,
    build_log_nonempty: bool,
) -> Result<(), PipelineResult> {
    let RunCtx {
        config,
        reporter,
        ws,
        rpm_name,
        checkout_dir,
        spec_path,
        ..
    } = ctx;
    let reporter = *reporter;
    let need_spec = spec.need_spec;

    // A prior run may have reviewed-and-accepted this exact spec version and
    // then died (crash, kill) before ever attempting a build — e.g. mid
    // `osc ci` on an unrelated package in the same batch. Resuming that case
    // should go straight to validate/commit, not pay for a second review
    // turn on a spec that hasn't changed since the first one accepted it.
    let already_reviewed = !need_spec
        && PkgState::load(ws).is_reviewed(
            &config.version,
            &std::fs::read_to_string(spec_path).unwrap_or_default(),
        );

    if config.no_agent {
        st.step_attempts
            .skip(reporter, StepKind::AgentTurn, SkipReason::NoAgent);
    }
    if !config.no_agent && (need_spec || !build_log_nonempty) && !already_reviewed {
        let spec_before = std::fs::read_to_string(spec_path).unwrap_or_default();
        // Full-checkout snapshot taken BEFORE the agent runs so a reject /
        // blocked / no-change outcome can roll back every top-level
        // checkout edit, not just the spec. See `CheckoutSnapshot` and
        // `apply_turn_outcome`'s Blocked / NoChange arms.
        let checkout_snapshot = CheckoutSnapshot::take(checkout_dir);

        let turn = agent_turn(
            TurnCtx {
                label: "review",
                task: crate::agent::Task::Review,
                event_task: ap_core::event::Task::Review,
                attempt: 0,
                hint: None,
                unresolvable_deps: &[],
                unresolvable_detail: &[],
                checkout_dir: Some(checkout_dir),
                from_repo_archive: st.from_repo_archive,
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
            ctx.turn_env(),
            &mut st.spent_usd,
            &mut st.session_id,
            &mut st.step_attempts,
        );

        if let Ok((review_step, outcome)) = turn {
            // A repo-based build that turned out wrong-shaped for this
            // specific PyPI release (convention 24 Exception 3's caveat
            // about verified-but-mismatched repos) is exactly the kind of
            // "cannot be compiled here" call `UseUpstreamWheel` covers.
            if let Err(reason) = apply_turn_and_reset(
                ApplyTurnCtx {
                    config,
                    checkout_dir,
                    spec_path,
                    step: &review_step,
                    before: &spec_before,
                    rpm_name,
                    label: "Review — pre-build",
                    attempt: 0,
                    notes_path: ws.notes(),
                    snapshot: Some(&checkout_snapshot),
                },
                &outcome,
                reporter,
                &mut st.sdist_top_level,
                &mut st.from_repo_archive,
            ) {
                return Err(ctx.failed(reason));
            }
            let spec_hash = hash_spec(&std::fs::read_to_string(spec_path).unwrap_or_default());
            update_state(ws, reporter, |state| {
                state.reviewed_version = Some(config.version.clone());
                state.reviewed_spec_hash = Some(spec_hash);
            });
        }
    }
    Ok(())
}

/// Step: PreFix (restart with previous build log).
/// When this is a restart (spec was not regenerated) and a previous non-empty
/// build log exists, run an agent turn to fix the spec before a new OBS build.
/// Also runs when OBS already reports the package Unresolvable — that failure
/// mode often never produces a real build log at all, but the agent can
/// still inspect the spec and report what's missing via `unresolvable_packages`.
fn prefix_stage(
    ctx: &RunCtx,
    st: &mut RunState,
    spec: &SpecInfo,
    obs_status: &Option<osc::BuildStatus>,
    build_log_nonempty: bool,
) -> Result<(), PipelineResult> {
    let RunCtx {
        config,
        reporter,
        ws,
        rpm_name,
        checkout_dir,
        spec_path,
        ..
    } = ctx;
    let reporter = *reporter;
    // Gated like every other fix turn: this path also bumps the attempt count,
    // so without the limit a package could run past --max-attempts (13/10).
    if !spec.need_spec
        && !config.no_agent
        && st.attempts < config.max_attempts
        && st.spent_usd < config.budget_usd
        && (build_log_nonempty || matches!(obs_status, Some(osc::BuildStatus::Unresolvable)))
    {
        let attempts = st.attempts;
        let clean = strip_and_rewrite(spec_path);
        let extract = extract_build_log(&ws.build_log(), &st.sdist_top_level, reporter, None);
        let checkout_snapshot = CheckoutSnapshot::take(checkout_dir);

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
                unresolvable_detail: &[],
                checkout_dir: Some(checkout_dir),
                from_repo_archive: st.from_repo_archive,
                resume: false,
                context: if !build_log_nonempty {
                    // Same situation the Review turn's context covers above:
                    // an empty build log here is not "nothing to fix", it's
                    // OBS rejecting the build during dependency resolution
                    // before compilation starts. Without this, the pyarrow
                    // case showed the turn reading the empty log, seeing
                    // nothing wrong, and returning `NoChange` instead of
                    // checking BuildRequires names/casing and reporting the
                    // real gap via `unresolvable_packages`.
                    Some(
                        "The build log is empty — OBS rejected the build during \
                         dependency resolution before any compilation started. \
                         The spec may reference a package that does not exist in \
                         OBS yet, or a BuildRequires name may be wrong (wrong \
                         case, or a devel-package name upstream doesn't use — \
                         see convention 2 and convention 27). Do not return \
                         `no_change` without checking every BuildRequires name \
                         against its actual OBS spelling first.",
                    )
                } else {
                    None
                },
            },
            ctx.turn_env(),
            &mut st.spent_usd,
            &mut st.session_id,
            &mut st.step_attempts,
        );

        if let Ok((prefix_step, outcome)) = turn {
            if !outcome.unresolvable_packages.is_empty() {
                reporter.report(&Event::MissingPackages {
                    step: prefix_step.id,
                    names: outcome.unresolvable_packages.clone(),
                    source: ap_core::event::MissingSource::Agent,
                });
                let mut to_build: Vec<String> = outcome
                    .unresolvable_packages
                    .iter()
                    .map(|d| osc::strip_devel_wrapping(osc::strip_rpm_prefix(d)).to_string())
                    .collect();
                to_build.sort_unstable();
                to_build.dedup();
                return Err(PipelineResult::MissingDeps {
                    rpm_name: rpm_name.clone(),
                    deps: to_build,
                });
            }

            let effect = match apply_turn_and_reset(
                ApplyTurnCtx {
                    config,
                    checkout_dir,
                    spec_path,
                    step: &prefix_step,
                    before: &clean,
                    rpm_name,
                    label: "PreFix — restart from previous log",
                    attempt: attempts + 1,
                    notes_path: ws.notes(),
                    snapshot: Some(&checkout_snapshot),
                },
                &outcome,
                reporter,
                &mut st.sdist_top_level,
                &mut st.from_repo_archive,
            ) {
                Ok(effect) => effect,
                Err(reason) => return Err(ctx.failed(reason)),
            };
            if matches!(effect, TurnEffect::Edited | TurnEffect::SwitchedToWheel) {
                st.attempts += 1;
                persist_attempts(ws, &config.run_id, st.attempts);
            }
        }
    }
    Ok(())
}

/// One fix-after-validation agent turn: counts the attempt, runs the turn and
/// applies its outcome. `apply_label` names the apply step and
/// `agent_failed_prefix` starts the error text when the turn itself fails.
fn fix_after_validation(
    ctx: &RunCtx,
    st: &mut RunState,
    context: Option<&str>,
    apply_label: &str,
    agent_failed_prefix: &str,
) -> Result<(), PipelineResult> {
    let RunCtx {
        config,
        reporter,
        ws,
        rpm_name,
        checkout_dir,
        spec_path,
        ..
    } = ctx;
    // Never commit a spec that is not sound — OBS would spend a whole remote
    // build to tell us what the local check already said.
    st.attempts += 1;
    persist_attempts(ws, &config.run_id, st.attempts);
    let attempts = st.attempts;
    let clean = strip_and_rewrite(spec_path);
    let checkout_snapshot = CheckoutSnapshot::take(checkout_dir);
    let turn = agent_turn(
        TurnCtx {
            label: "fix-after-validation",
            task: crate::agent::Task::FixAfterValidation,
            event_task: ap_core::event::Task::FixAfterBuild {
                attempt: attempts as u32,
                max_attempts: config.max_attempts as u32,
            },
            attempt: attempts,
            hint: None,
            unresolvable_deps: &[],
            unresolvable_detail: &[],
            checkout_dir: Some(checkout_dir),
            from_repo_archive: st.from_repo_archive,
            resume: true,
            context,
        },
        ctx.turn_env(),
        &mut st.spent_usd,
        &mut st.session_id,
        &mut st.step_attempts,
    );

    match turn {
        Ok((agent_step, outcome)) => {
            if let Err(reason) = apply_turn_and_reset(
                ApplyTurnCtx {
                    config,
                    checkout_dir,
                    spec_path,
                    step: &agent_step,
                    before: &clean,
                    rpm_name,
                    label: apply_label,
                    attempt: attempts,
                    notes_path: ws.notes(),
                    snapshot: Some(&checkout_snapshot),
                },
                &outcome,
                *reporter,
                &mut st.sdist_top_level,
                &mut st.from_repo_archive,
            ) {
                return Err(ctx.failed(reason));
            }
            Ok(())
        }
        Err(e) => Err(ctx.failed(format!("{agent_failed_prefix}{e}"))),
    }
}

/// Step: ValidateSpec. A rejected spec runs a fix turn and asks for another
/// round.
fn validate_round(ctx: &RunCtx, st: &mut RunState) -> Result<Flow, PipelineResult> {
    let RunCtx {
        config,
        pkg_run,
        reporter,
        ws,
        rpm_name,
        checkout_dir,
        spec_path,
    } = ctx;
    let reporter = *reporter;

    let validate_step = begin_or_fail!(
        pkg_run,
        reporter,
        StepKind::ValidateSpec,
        None,
        &mut st.step_attempts,
        rpm_name.clone()
    );

    // Deterministic and idempotent, so safe to re-apply on every visit —
    // including a spec that already existed on disk and was never run
    // through `post_process` this invocation (`need_spec` was false).
    // Without this, each of these mistakes persists on every future run
    // (nothing else ever revisits an unchanged spec to fix it) and either
    // looks like a genuinely missing dependency (mixed-case PyPI name
    // written lowercase — `%{python_module cython >= 3}` RPM-expands to
    // the nonexistent `python313-cython`) or only surfaces as a runtime
    // failure well after a wasted build (a bare `%{$python_sitelib}`
    // outside `%python_expand`; `%python_install_alternative` without
    // its `Requires(post/postun): update-alternatives`).
    if let Ok(current) = std::fs::read_to_string(spec_path) {
        let fixed = crate::spec::normalize_python_module_versions(&current);
        let fixed = crate::spec::fix_stray_dollar_macros(&fixed);
        let fixed = crate::spec::fix_missing_alternatives_requires(&fixed);
        if fixed != current {
            write_or_warn(spec_path, &fixed);
        }
    }

    match osc::rpmspec_check(spec_path, Emit::new(reporter, validate_step.id)) {
        Ok(expanded) => {
            // The fully expanded spec is the single most useful artifact
            // for diagnosing a spec problem — keep it rather than drop it.
            let artifact = validate_step.file("rpmspec.stdout");
            write_or_warn(&artifact, &expanded);
            reporter.report(&Event::Artifact {
                step: validate_step.id,
                name: "rpmspec.stdout".to_string(),
                path: artifact,
            });

            sync_quilt_series(&expanded, checkout_dir);

            let mut missing = missing_source_files(&expanded, checkout_dir);
            // A `_service` (`download_files`, the one installed here) is how a Source
            // that is a URL gets onto disk — and the agent cannot run it.
            // Run it before calling the files missing, and keep its error
            // so the agent sees why a download did not happen.
            let mut service_error = None;
            if !missing.is_empty() && checkout_dir.join("_service").is_file() {
                match osc::run_service(checkout_dir, Emit::new(reporter, validate_step.id)) {
                    Ok(()) => missing = missing_source_files(&expanded, checkout_dir),
                    Err(e) => service_error = Some(e.to_string()),
                }
            }
            if !missing.is_empty() {
                let validate_step = validate_step.finish(StepOutcome::Failed);
                reporter.report(&Event::SpecRejected {
                    step: validate_step.id,
                    reason: ap_core::event::SpecRejection::MissingSourceFiles {
                        names: missing.clone(),
                    },
                    restored_from: spec_path.clone(),
                });

                if config.no_agent
                    || st.attempts >= config.max_attempts
                    || st.spent_usd >= config.budget_usd
                {
                    skip_agent(
                        reporter,
                        &mut st.step_attempts,
                        config,
                        st.attempts,
                        st.spent_usd,
                    );
                    return Err(ctx.failed(format!(
                        "spec references source file(s) not present in the \
                         checkout: {}",
                        missing.join(", ")
                    )));
                }

                // Never commit a spec whose Source/Patch files don't exist — OBS
                // would spend a whole remote build just to report `File ...
                // SOURCES/...: No such file or directory` (convention 13).
                let context = format!(
                    "`rpmspec --parse` succeeded, but the spec references source \
                     file(s) that do not exist in the checkout directory: {}. Either \
                     the file was never fetched/written, or a Source/Patch line's \
                     name doesn't match what's actually on disk (typo, wrong \
                     extension, stale reference after a rename). Fix the spec to \
                     reference the real file, or create/restore the missing file if \
                     it should exist — see \"Source Files and Cleanup\" below.{}",
                    missing.join(", "),
                    service_error
                        .map(|e| format!(
                            " The checkout's `_service` was run (`osc service ra`) to \
                             fetch it and failed: {e}"
                        ))
                        .unwrap_or_default()
                );
                fix_after_validation(
                    ctx,
                    st,
                    Some(&context),
                    "missing source files",
                    "missing source files and agent failed: ",
                )?;
                return Ok(Flow::Retry);
            }

            validate_step.finish(StepOutcome::Ok);
            Ok(Flow::Proceed)
        }
        Err(e) => {
            let validate_step = validate_step.finish(StepOutcome::Failed);
            write_or_warn(validate_step.file("rpmspec.stderr"), e.to_string());
            write_or_warn(ws.rpmspec_log(), e.to_string());
            reporter.report(&Event::SpecRejected {
                step: validate_step.id,
                reason: ap_core::event::SpecRejection::RpmspecParseFailed {
                    stderr: e.to_string(),
                },
                restored_from: spec_path.clone(),
            });
            if config.no_agent
                || st.attempts >= config.max_attempts
                || st.spent_usd >= config.budget_usd
            {
                skip_agent(
                    reporter,
                    &mut st.step_attempts,
                    config,
                    st.attempts,
                    st.spent_usd,
                );
                return Err(ctx.failed(format!("spec invalid: {e}")));
            }

            // Never commit a spec rpm cannot parse — OBS would spend a
            // whole remote build to tell us what rpmspec just said. Fix it
            // here and re-validate on the next pass of the loop.
            fix_after_validation(
                ctx,
                st,
                None,
                "FixAfterValidation",
                "spec invalid and agent failed: ",
            )?;
            Ok(Flow::Retry)
        }
    }
}

/// Step: Commit.
fn commit_step(ctx: &RunCtx, st: &mut RunState) -> Result<(), PipelineResult> {
    let commit_step = begin_or_fail!(
        ctx.pkg_run,
        ctx.reporter,
        StepKind::Commit,
        None,
        &mut st.step_attempts,
        ctx.rpm_name.clone()
    );
    if let Err(e) = osc::commit(
        &ctx.checkout_dir,
        "Automated packaging",
        Emit::new(ctx.reporter, commit_step.id),
    ) {
        return Err(ctx.failed(e.to_string()));
    }
    commit_step.finish(StepOutcome::Ok);
    Ok(())
}

/// Step: Build (wait for OBS remote build, fetch log, download RPMs), then
/// route its outcome: success finishes the package, unresolvable dependencies
/// end it with `MissingDeps`, and an ordinary failure runs a fix turn.
fn build_round(ctx: &RunCtx, st: &mut RunState) -> Result<RoundEnd, PipelineResult> {
    let RunCtx {
        config,
        pkg_run,
        reporter,
        rpm_name,
        ..
    } = ctx;
    let reporter = *reporter;
    let build_step = begin_or_fail!(
        pkg_run,
        reporter,
        StepKind::Build,
        None,
        &mut st.step_attempts,
        rpm_name.clone()
    );
    let build_log = build_step.file("build.log");

    let build_result = osc::obs_build(
        &config.project,
        rpm_name,
        &config.distro,
        &config.arch,
        &build_log,
        &config.rpms_dir,
        Emit::new(reporter, build_step.id),
    );

    match build_result {
        Ok(()) => Ok(RoundEnd::Finished(build_succeeded(ctx, st, build_step))),
        Err(osc::Error::UnresolvableDeps {
            names: deps,
            detail,
        }) => {
            let build_step = build_step.finish(StepOutcome::Failed);
            unresolvable_deps_round(ctx, st, &build_step, &build_log, deps, detail)
        }
        Err(osc::Error::NotActionable(msg)) => {
            build_step.finish(StepOutcome::Failed);
            Err(ctx.failed(msg))
        }
        Err(_) => {
            let build_step = build_step.finish(StepOutcome::Failed);
            fix_after_build(ctx, st, &build_step, &build_log)
        }
    }
}

/// A successful OBS build: record it and report the package as built.
fn build_succeeded(ctx: &RunCtx, st: &mut RunState, build_step: StepGuard<'_>) -> PipelineResult {
    let RunCtx {
        config,
        reporter,
        ws,
        rpm_name,
        ..
    } = ctx;
    let reporter = *reporter;
    let build_step = build_step.finish(StepOutcome::Ok);
    st.step_attempts
        .skip(reporter, StepKind::ExtractFailure, SkipReason::NoFailure);
    update_state(ws, reporter, |state| {
        state.policy_version = POLICY_VERSION;
        state.last_built_version = Some(config.version.clone());
        state.last_outcome = Some("success".to_string());
        state.last_failure_reason = None;
    });
    warn_if_err(
        reporter,
        "updating the latest-build link",
        ws.link_latest(&build_step.path),
    );
    // `config.project` now has one more package than whatever
    // this process last cached — see `list_project_packages`'s
    // doc comment. Without this, every package built from here
    // on stays invisible to every dependent package's
    // availability check for the rest of the run.
    osc::invalidate_project_packages_cache(&config.project);
    PipelineResult::Built {
        rpm_name: rpm_name.clone(),
    }
}

/// OBS reported unresolvable dependencies for the build.
fn unresolvable_deps_round(
    ctx: &RunCtx,
    st: &mut RunState,
    build_step: &StepDir,
    build_log: &Path,
    deps: Vec<String>,
    detail: Vec<String>,
) -> Result<RoundEnd, PipelineResult> {
    let RunCtx {
        config,
        reporter,
        ws,
        rpm_name,
        spec_path,
        ..
    } = ctx;
    let reporter = *reporter;
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
        st.attempts += 1;
        persist_attempts(ws, &config.run_id, st.attempts);
        if st.attempts > config.max_attempts {
            return Err(ctx.failed(format!(
                "OBS keeps reporting unresolvable dependencies after \
                 {} attempt(s) even though every one has a local \
                 RPM; log: {}",
                st.attempts,
                build_log.display()
            )));
        }
        return Ok(RoundEnd::Retry);
    }

    // Unresolvable deps cannot be fixed by spec edits — they
    // name a package that genuinely doesn't exist yet, not a
    // wrong constraint in this spec. Skip the FixAfterBuild
    // spec-fix loop and queue them for packaging directly, but
    // give the agent one single, read-only, cheap-model lookup
    // first to translate OBS's RPM-wrapped names
    // (`libtrantor-devel`) into the real upstream project name
    // (`trantor`) — mechanical lib/-devel stripping alone misses
    // cases like that one. "Once" is enforced by
    // `unresolvable_review_hash`: a later invocation of this
    // package that hits the exact same dep set reuses the cached
    // translation instead of asking again.
    reporter.report(&Event::BuildFailed {
        step: build_step.id,
        classification: ap_core::event::BuildFailure::UnresolvableDeps {
            packages: unresolvable_deps.clone(),
        },
        log: build_log.to_path_buf(),
    });

    let mut sorted_deps = unresolvable_deps.clone();
    sorted_deps.sort_unstable();
    sorted_deps.dedup();

    let prior_state = PkgState::load(ws);
    let already_reviewed = prior_state.already_reviewed_unresolvable(&sorted_deps);
    let mut translated = if already_reviewed {
        prior_state.unresolvable_translated.clone()
    } else {
        Vec::new()
    };

    if !already_reviewed && !config.no_agent && st.spent_usd < config.budget_usd {
        let turn = agent_turn(
            TurnCtx {
                label: "translate-deps",
                task: crate::agent::Task::TranslateUnresolvableDeps,
                event_task: ap_core::event::Task::TranslateUnresolvableDeps,
                attempt: 0,
                hint: None,
                unresolvable_deps: &sorted_deps,
                unresolvable_detail: &detail,
                checkout_dir: None,
                from_repo_archive: st.from_repo_archive,
                resume: false,
                context: None,
            },
            ctx.turn_env(),
            &mut st.spent_usd,
            &mut st.session_id,
            &mut st.step_attempts,
        );
        if let Ok((_, outcome)) = turn {
            translated = outcome.unresolvable_packages;
        }

        // A translated name that merely respells a bare name
        // already in this spec (`huggingface_hub` ->
        // `huggingface-hub` — same project, OBS's literal
        // requires-dist spelling vs. the dash form the
        // dependency actually gets packaged and published
        // under) leaves this package's own
        // `%{python_module ...}` line asking for a name
        // nothing will ever provide, even once the translated
        // dependency is built. Rewrite it now — see
        // `spec::rewrite_python_module_name`'s doc comment for
        // why nothing else ever revisits this.
        if !translated.is_empty() {
            let bare_names: Vec<String> = sorted_deps
                .iter()
                .map(|d| osc::strip_devel_wrapping(osc::strip_rpm_prefix(d)).to_string())
                .collect();
            if let Ok(current) = std::fs::read_to_string(spec_path) {
                let mut rewritten = current.clone();
                for t in &translated {
                    if let Some(bare) = bare_names
                        .iter()
                        .find(|b| osc::fuzzy_key(b) == osc::fuzzy_key(t) && *b != t)
                    {
                        rewritten = crate::spec::rewrite_python_module_name(&rewritten, bare, t);
                    }
                }
                if rewritten != current {
                    write_or_warn(spec_path, &rewritten);
                }
            }
        }

        update_state(ws, reporter, |state| {
            state.unresolvable_review_hash = Some(hash_deps(&sorted_deps));
            state.unresolvable_translated = translated.clone();
        });
    }

    if !translated.is_empty() {
        reporter.report(&Event::MissingPackages {
            step: build_step.id,
            names: translated.clone(),
            source: ap_core::event::MissingSource::Agent,
        });
    }
    let source_list = if translated.is_empty() {
        &unresolvable_deps
    } else {
        &translated
    };
    let mut to_build: Vec<String> = source_list
        .iter()
        .map(|d| osc::strip_devel_wrapping(osc::strip_rpm_prefix(d)).to_string())
        .collect();
    to_build.sort_unstable();
    to_build.dedup();
    Err(PipelineResult::MissingDeps {
        rpm_name: rpm_name.clone(),
        deps: to_build,
    })
}

/// An ordinary build failure: check the attempt/budget limits, then run a
/// fix-after-build turn and apply it.
fn fix_after_build(
    ctx: &RunCtx,
    st: &mut RunState,
    build_step: &StepDir,
    build_log: &Path,
) -> Result<RoundEnd, PipelineResult> {
    let RunCtx {
        config,
        pkg_run,
        reporter,
        ws,
        rpm_name,
        checkout_dir,
        spec_path,
    } = ctx;
    let reporter = *reporter;

    if config.no_agent {
        skip_agent(
            reporter,
            &mut st.step_attempts,
            config,
            st.attempts,
            st.spent_usd,
        );
        return Err(ctx.failed(format!("build failed; log: {}", build_log.display())));
    }

    st.attempts += 1;
    persist_attempts(ws, &config.run_id, st.attempts);
    let attempts = st.attempts;
    if attempts > config.max_attempts || st.spent_usd >= config.budget_usd {
        skip_agent(
            reporter,
            &mut st.step_attempts,
            config,
            attempts,
            st.spent_usd,
        );
        let why = if st.spent_usd >= config.budget_usd {
            format!(
                "budget exhausted (${:.2} spent of ${:.2})",
                st.spent_usd, config.budget_usd
            )
        } else {
            format!("attempt limit {} reached", config.max_attempts)
        };
        return Err(ctx.failed(format!(
            "gave up after {} attempt(s): {}; log: {}",
            attempts,
            why,
            build_log.display()
        )));
    }

    link_build_log(ws, build_log);

    let extract = extract_failure_step(
        pkg_run,
        reporter,
        &mut st.step_attempts,
        build_log,
        &st.sdist_top_level,
        build_step.id,
    );
    reporter.report(&Event::BuildFailed {
        step: build_step.id,
        classification: classify_failure(&extract),
        log: build_log.to_path_buf(),
    });

    let clean = strip_and_rewrite(spec_path);
    let checkout_snapshot = CheckoutSnapshot::take(checkout_dir);
    let turn = agent_turn(
        TurnCtx {
            label: "fix-after-build",
            task: crate::agent::Task::FixAfterBuild,
            event_task: ap_core::event::Task::FixAfterBuild {
                attempt: attempts as u32,
                max_attempts: config.max_attempts as u32,
            },
            attempt: attempts,
            hint: Some(&extract),
            unresolvable_deps: &[],
            unresolvable_detail: &[],
            checkout_dir: Some(checkout_dir),
            from_repo_archive: st.from_repo_archive,
            resume: true,
            context: None,
        },
        ctx.turn_env(),
        &mut st.spent_usd,
        &mut st.session_id,
        &mut st.step_attempts,
    );

    let (agent_step, outcome) = match turn {
        Ok(v) => v,
        // `agent_turn` already routed the Error through
        // `friendly_agent_error`, so this is a user-ready message
        // (budget-exhausted hint included); pass it through verbatim.
        Err(e) => return Err(ctx.failed(e)),
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
            .map(|d| osc::strip_devel_wrapping(osc::strip_rpm_prefix(d)).to_string())
            .collect();
        to_build.sort_unstable();
        to_build.dedup();
        return Err(PipelineResult::MissingDeps {
            rpm_name: rpm_name.clone(),
            deps: to_build,
        });
    }

    if let Err(reason) = apply_turn_and_reset(
        ApplyTurnCtx {
            config,
            checkout_dir,
            spec_path,
            step: &agent_step,
            before: &clean,
            rpm_name,
            label: "FixAfterBuild",
            attempt: attempts,
            notes_path: ws.notes(),
            snapshot: Some(&checkout_snapshot),
        },
        &outcome,
        reporter,
        &mut st.sdist_top_level,
        &mut st.from_repo_archive,
    ) {
        return Err(ctx.failed(reason));
    }
    Ok(RoundEnd::Retry)
}

// ── helpers ───────────────────────────────────────────────────────────────────

/// `StepGuard::begin` for the pipeline stages: a step directory that cannot be
/// created fails the package (an `Err(PipelineResult)` from the stage).
macro_rules! begin_or_fail {
    ($pkg_run:expr, $reporter:expr, $kind:expr, $detail:expr, $attempts:expr, $rpm_name:expr) => {
        match StepGuard::begin($pkg_run, $reporter, $kind, $detail, $attempts) {
            Ok(s) => s,
            Err(e) => {
                return Err(PipelineResult::Failed {
                    rpm_name: $rpm_name,
                    reason: e.to_string(),
                });
            }
        }
    };
}
use begin_or_fail;

/// The shared run environment an agent turn works in.
struct TurnEnv<'a> {
    config: &'a PipelineConfig,
    ws: &'a Workspace,
    pkg_run: &'a PkgRunDir,
    reporter: &'a dyn Reporter,
}

/// What one agent turn needs beyond [`PipelineConfig`].
struct TurnCtx<'a> {
    /// Names the turn: the step directory is `NN-agent-<label>` and the label
    /// is the `StepStarted` detail, e.g. `"review"` or `"fix-after-build"`.
    label: &'a str,
    task: crate::agent::Task,
    event_task: ap_core::event::Task,
    attempt: usize,
    hint: Option<&'a crate::log::ExtractedLog>,
    unresolvable_deps: &'a [String],
    unresolvable_detail: &'a [String],
    checkout_dir: Option<&'a Path>,
    from_repo_archive: bool,
    /// Resume the previous `claude` session rather than starting a fresh one.
    resume: bool,
    /// Optional short note injected into the prompt before the hint section.
    context: Option<&'a str>,
}

/// Reshape an agent turn's `Error` into the one-line reason the pipeline
/// propagates to the user via `PipelineResult::Failed`.
///
/// The default `Display` for `Error::Failed`/`Error::EndedEarly` is useful
/// for debugging (full stderr dump) but doesn't tell the user how to fix
/// the common cases. In particular, the drogon-build crash the Oct 2026
/// user hit was `claude exited 1` with empty stderr because
/// `--max-budget-usd` ran out mid-turn — nothing in the opaque message
/// pointed at the one knob (`--budget-usd`) they needed to raise. The
/// claude CLI DOES emit the diagnosis in the stream (`subtype:
/// "error_max_budget_usd"`, `errors: ["Reached maximum budget (...)"]`)
/// and `Error::EndedEarly` carries both; translate them into something
/// the user can act on.
///
/// Pull the inner CLI message out via `ended_early_message()` rather than
/// embedding `{e}`'s `Display` — `Display` prepends
/// `"claude ended turn with <subtype>: "` AND repeats the subtype, so
/// wrapping it in the budget sentence used to produce
/// `"model budget of $2.00 (--budget-usd) exhausted mid-turn (claude
/// ended turn with error_max_budget_usd: Reached maximum budget ($0.03))"`
/// — two different dollar figures with no indication that $2.00 is the
/// per-package cap and $0.03 is the per-turn remainder.
fn friendly_agent_error(e: &crate::agent::Error, budget_usd: f64, pkg_name: &str) -> String {
    if e.is_budget_exhausted() {
        let detail = e.ended_early_message().unwrap_or("budget exhausted");
        return format!(
            "model budget of ${budget_usd:.2} (--budget-usd) exhausted \
             mid-turn: {detail}. Raise --budget-usd for this package \
             and retry. The agent's prompt + partial output are \
             preserved in the step dir (crash-prompt.md, \
             crash-stdout.log) if you need to see how far it got."
        );
    }
    if e.is_rate_limited() {
        let detail = e.ended_early_message().unwrap_or("rate limited");
        return format!(
            "the model API rejected the turn with a quota/rate limit (429): \
             {detail}\nThis is a provider-side limit, not a package problem: \
             wait a minute and re-run (state is kept), lower --jobs, or raise \
             the quota. Partial output is in the step dir (crash-stdout.log)."
        );
    }
    if let (Some(subtype), Some(message)) = (e.ended_early_subtype(), e.ended_early_message()) {
        return format!(
            "claude ended turn early ({subtype}): {message}. The \
             agent's prompt + partial output are preserved in the \
             step dir (crash-prompt.md, crash-stdout.log)."
        );
    }
    format!("agent error for {pkg_name}: {e}")
}

/// Report why no further agent turn will run for this package.
fn skip_agent(
    reporter: &dyn Reporter,
    step_attempts: &mut Attempts,
    config: &PipelineConfig,
    attempts: usize,
    spent_usd: f64,
) {
    let reason = if config.no_agent {
        SkipReason::NoAgent
    } else if spent_usd >= config.budget_usd {
        SkipReason::BudgetExhausted
    } else {
        SkipReason::Other(format!(
            "attempt limit reached ({attempts} of {})",
            config.max_attempts
        ))
    };
    step_attempts.skip(reporter, StepKind::AgentTurn, reason);
}

/// Charge a failed model call's cost to the package and the run as soon as the
/// call returns. A retry overwrites the error it follows, so the cost cannot
/// wait for the final result: that would drop the first call's spend.
fn charge_failed_call(
    ws: &Workspace,
    run_id: &str,
    spent_usd: &mut f64,
    result: &Result<crate::agent::TurnOutcome, crate::agent::Error>,
) {
    let Err(e) = result else {
        return;
    };
    if let Some(usage) = e.usage() {
        *spent_usd += usage.cost_usd();
        // Saved now, not with the next successful turn: a package that
        // stops here would otherwise lose the spend in state.json.
        add_run_spend(ws, run_id, usage.cost_usd());
    }
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
    env: TurnEnv<'_>,
    spent_usd: &mut f64,
    session_id: &mut Option<String>,
    step_attempts: &mut Attempts,
) -> Result<(StepDir, crate::agent::TurnOutcome), String> {
    let TurnEnv {
        config,
        ws,
        pkg_run,
        reporter,
    } = env;
    let step = StepGuard::begin(
        pkg_run,
        reporter,
        StepKind::AgentTurn,
        Some(ctx.label),
        step_attempts,
    )
    .map_err(|e| e.to_string())?;

    if let Some(hint) = ctx.hint {
        let extract = hint.to_event_extract();
        let artifact = step.file("extract.json");
        if let Ok(json) = serde_json::to_string_pretty(&extract) {
            write_or_warn(&artifact, json);
        }
        reporter.report(&Event::ExtractResult {
            step: step.id,
            extract,
            artifact,
        });
    }

    let notes = tail_for_prompt(&std::fs::read_to_string(ws.notes()).unwrap_or_default());
    let package_notes = tail_for_prompt(&crate::package_notes::read(&config.rpm_name));
    let on_tool_use = |tool: &str, input: &str| {
        reporter.report(&Event::AgentToolUse {
            step: step.id,
            tool: tool.to_string(),
            input: input.to_string(),
        });
    };
    // After a prose-only reply the retry runs in a fresh session with this
    // nudge added to the prompt context (see below).
    let fresh_session = std::cell::Cell::new(false);
    // Moved along after each 429 so the retry runs on a different model (each
    // model has its own quota).
    let model_rotation = std::cell::Cell::new(0usize);
    let nudge: std::cell::OnceCell<String> = std::cell::OnceCell::new();
    // `spent` is what the package has spent when this call starts; it is passed
    // in so the closure does not hold `spent_usd` while the loop charges it.
    let make_req = |spent: f64| crate::agent::TurnRequest {
        workspace: ws.dir(),
        pkg_name: &config.pkg_name,
        version: &config.version,
        // The `python-` prefix is set by `Ecosystem::rpm_name` (see
        // `build/src/sources.rs`) once the probe has decided the ecosystem,
        // so it is the single stable "this is a Python package" signal the
        // pipeline can hand the agent prompt.
        is_python: config.rpm_name.starts_with("python-"),
        task: ctx.task,
        attempt: ctx.attempt,
        max_attempts: config.max_attempts,
        spent_usd: spent,
        budget_usd: config.budget_usd,
        hint: ctx.hint,
        notes: &notes,
        package_notes: &package_notes,
        budget: crate::agent::TurnBudget::default(),
        from_repo_archive: ctx.from_repo_archive,
        subprojects: &config.subprojects,
        unresolvable_deps: ctx.unresolvable_deps,
        unresolvable_detail: ctx.unresolvable_detail,
        failed_deps: &config.failed_deps,
        unbuildable_deps: &config.unbuildable_deps,
        session_id: if ctx.resume && !fresh_session.get() {
            session_id.as_deref()
        } else {
            None
        },
        checkout_dir: ctx.checkout_dir,
        context: nudge.get().map(String::as_str).or(ctx.context),
        model_rotation: model_rotation.get(),
        on_tool_use: Some(&on_tool_use),
    };

    // A 429 is the provider's quota, not this package's fault. Re-issue the
    // turn on the next model in the rotation (each model has its own quota)
    // before spending one of the package's attempts, so a burst of parallel
    // packages does not all give up at once.
    let mut rate_limit_retries = 0u32;
    let turn_result = loop {
        // The quota is shared by every package running in parallel: when any
        // turn hits a 429, all of them pause instead of each discovering it
        // separately and retrying in lockstep.
        let slot = AgentSlot::acquire();
        let mut result = crate::agent::turn(make_req(*spent_usd));
        charge_failed_call(ws, &config.run_id, spent_usd, &result);
        // A resumed session can answer in prose ("I already called
        // StructuredOutput in my previous message") without making the call in
        // THIS turn, which leaves nothing to act on (python-brotlicffi's last
        // failure). Ask once more in a fresh session, saying what happened.
        if matches!(&result, Err(crate::agent::Error::Parse { message, .. })
            if message.contains("no usable turn response"))
        {
            reporter.report(&Event::Warning {
                step: Some(step.id),
                message: "the agent ended its turn without calling StructuredOutput; \
                          retrying once in a fresh session"
                    .to_string(),
            });
            let _ = nudge.set(format!(
                "{}Your previous reply did not call the StructuredOutput tool, so \
                 nothing you did was recorded. The checkout and spec on disk are in \
                 whatever state you left them: re-read them, finish the task, and END \
                 by calling StructuredOutput with your result.",
                ctx.context.map(|c| format!("{c}\n\n")).unwrap_or_default()
            ));
            fresh_session.set(true);
            result = crate::agent::turn(make_req(*spent_usd));
            charge_failed_call(ws, &config.run_id, spent_usd, &result);
        }
        drop(slot);
        match result {
            Err(e)
                if e.is_rate_limited() && rate_limit_retries < MAX_IMMEDIATE_RATE_LIMIT_RETRIES =>
            {
                rate_limit_retries += 1;
                model_rotation.set(model_rotation.get() + 1);
                reporter.report(&Event::Warning {
                    step: Some(step.id),
                    message: format!(
                        "model API rate-limited (429) on {}; retrying now on {} ({rate_limit_retries}/{MAX_IMMEDIATE_RATE_LIMIT_RETRIES})",
                        crate::agent::model_for_attempt(ctx.attempt, model_rotation.get() - 1),
                        crate::agent::model_for_attempt(ctx.attempt, model_rotation.get()),
                    ),
                });
            }
            other => break other,
        }
    };
    let outcome = match turn_result {
        Ok(o) => o,
        Err(e) => {
            // Any of these failing is otherwise a dead end: nothing below
            // this point runs, so without this the prompt, stdout and stderr
            // all vanish the moment this process exits and the failure
            // becomes undiagnosable — exactly what happened to the
            // `python-torchcodec`/`python-backports-zstd` "claude exited 1"
            // crashes that prompted adding this (see `agent::Error`'s
            // accessors for which variants carry what).
            for (name, content) in [
                ("crash-prompt.md", e.prompt()),
                ("crash-stdout.log", e.stdout()),
                ("crash-stderr.log", e.stderr()),
            ] {
                let Some(content) = content.filter(|c| !c.is_empty()) else {
                    continue;
                };
                let path = step.file(name);
                if ap_core::fsx::write_atomic(&path, content).is_ok() {
                    reporter.report(&Event::Artifact {
                        step: step.id,
                        name: name.to_string(),
                        path,
                    });
                }
            }
            let step = step.finish(StepOutcome::Failed);
            let message = friendly_agent_error(&e, config.budget_usd, &config.pkg_name);
            reporter.report(&Event::Warning {
                step: Some(step.id),
                message: message.clone(),
            });
            return Err(message);
        }
    };

    *session_id = outcome.session_id.clone();
    *spent_usd += outcome.usage.cost_usd();

    // If the claude CLI's final `result` event carried a structured
    // end-early signal but `pick_turn_response` still rescued a usable
    // `StructuredOutput` from the stream, we return Ok but the turn
    // was NOT clean — surface it as a Warning so a budget-cut-but-
    // rescued turn is distinguishable from a clean one in the event
    // stream (otherwise the per-package budget runs out one turn
    // later with no visible warning to the user). The budget-cap case
    // additionally gets the --budget-usd hint, since that is the one
    // knob the user can actually turn.
    if let Some(signal) = &outcome.ended_early {
        let hint = if signal.subtype.starts_with("error_max_budget") {
            format!(
                " Raise --budget-usd (currently ${:.2}) if the next \
                 turn needs more headroom.",
                config.budget_usd
            )
        } else {
            String::new()
        };
        reporter.report(&Event::Warning {
            step: Some(step.id),
            message: format!(
                "claude ended turn early ({}): {} — a StructuredOutput \
                 was rescued from the stream so the turn is being \
                 applied, but the model was cut off before it could \
                 finish.{hint}",
                signal.subtype, signal.message,
            ),
        });
    }

    // Persist the running total so an outer-loop retry (or a crash
    // mid-pipeline) within the SAME CLI invocation resumes with the
    // correct cumulative spend instead of a reset-to-zero cap. Tagged
    // with the current run_id so a fresh CLI invocation reads a
    // mismatch and starts the package at $0 — the user asked for that
    // restart-reset. See `PkgState::cumulative_spent_run_id`.
    if let Ok(ws) = Workspace::open(&config.state_root, &config.rpm_name) {
        // Only the spend fields: the in-memory total is the truth for them,
        // while everything else in the file may have moved under another writer.
        update_state(&ws, reporter, |st| {
            st.cumulative_spent_usd = *spent_usd;
            st.cumulative_spent_run_id = Some(config.run_id.clone());
        });
    }

    let prompt_path = step.file("prompt.md");
    write_or_warn(&prompt_path, &outcome.prompt);
    write_or_warn(step.file("output.md"), &outcome.output);

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
        write_or_warn(&response_path, json);
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

    let step = step.finish(StepOutcome::Ok);

    Ok((step, outcome))
}

/// Drop stale `# ap: fix` comments from the spec on disk and return the result.
///
/// The returned text is the roll-back point for the edit the agent is about to
/// make, so the agent's own comment lines never accumulate across attempts.
fn strip_and_rewrite(spec_path: &Path) -> String {
    let before = std::fs::read_to_string(spec_path).unwrap_or_else(|e| {
        ap_core::diag!("  cannot read {}: {e}", spec_path.display());
        String::new()
    });
    let clean = crate::spec::strip_ap_fix_comments(&before);
    if clean != before {
        write_or_warn(spec_path, &clean);
    }
    clean
}

/// Put `before` back as the spec after a rejected agent edit.
///
/// An empty `before` means the roll-back point could not be read (see
/// [`strip_and_rewrite`]); no real spec is empty, so writing it would
/// destroy the file the agent edited instead of restoring it. A failed
/// write is reported rather than dropped.
fn restore_spec(spec_path: &Path, before: &str, step: &StepDir, reporter: &dyn Reporter) {
    let problem = if before.trim().is_empty() {
        Some("no readable roll-back point; leaving the spec as the agent left it".to_string())
    } else {
        ap_core::fsx::write_atomic(spec_path, before)
            .err()
            .map(|e| format!("writing the rolled-back spec failed: {e}"))
    };
    if let Some(message) = problem {
        reporter.report(&Event::Warning {
            step: Some(step.id),
            message: format!("{}: {message}", spec_path.display()),
        });
    }
}

/// Point the durable workspace's `build.log` at this run's build log.
///
/// Uses a hard link rather than a symlink so the workspace entry stays a
/// plain file for the agent. The claude CLI's `--add-dir` sandbox resolves
/// a symlink to its real target before admitting the read, and the real
/// target lives under `runs/<run-id>/<pkg>/05-build/` — a sibling of the
/// workspace, outside anything the agent is granted. A symlink there surfaces
/// to the agent as `"the symlink target is outside the allowed working
/// directories"` and blocks the entire turn on an otherwise readable file.
/// A hard link appears to the sandbox as the workspace path itself and is
/// admitted; `fetch_buildlog`'s `std::fs::write(log_path, ...)` truncates
/// the inode in place, so both entries continue to reflect the latest log.
///
/// `hard_link` fails with `EXDEV` across filesystems; the copy fallback
/// covers that at the cost of duplicating the bytes.
fn link_build_log(ws: &Workspace, build_log: &Path) {
    let link = ws.build_log();
    // Link under a temp name and rename over `build.log`, so the agent never
    // finds the workspace without a log between the remove and the create.
    let tmp = link.with_extension(format!("log.{}.tmp", std::process::id()));
    let _ = std::fs::remove_file(&tmp);
    let linked = std::fs::hard_link(build_log, &tmp)
        .or_else(|_| std::fs::copy(build_log, &tmp).map(|_| ()))
        .and_then(|()| std::fs::rename(&tmp, &link));
    if let Err(e) = linked {
        let _ = std::fs::remove_file(&tmp);
        ap_core::diag!(
            "  cannot link {} to {}: {e}",
            link.display(),
            build_log.display()
        );
    }
}

/// Basenames of every `Source`/`Patch` file the spec references but that do
/// not exist in `checkout_dir`.
///
/// Takes `rpmspec --parse`'s output rather than the raw spec text so macros
/// (`%{name}-%{version}.tar.gz`) and conditionals are already resolved —
/// checking the raw spec would false-positive on any unexpanded macro. A URL
/// value (the common `Source:` shape for a plain PyPI tarball) is reduced to
/// its basename, the same name `osc::checkout`/`fetch_release_archive` save
/// it under locally.
fn missing_source_files(expanded_spec: &str, checkout_dir: &Path) -> Vec<String> {
    let mut missing = Vec::new();
    for line in expanded_spec.lines() {
        let line = line.trim();
        let Some(tag) = ["Source", "Patch"].iter().find(|t| line.starts_with(**t)) else {
            continue;
        };
        let Some(rest) = line[tag.len()..]
            .trim_start_matches(|c: char| c.is_ascii_digit())
            .strip_prefix(':')
        else {
            continue;
        };
        let value = rest.trim();
        // Macro that failed to expand (shouldn't happen once `rpmspec --parse`
        // has already succeeded) — nothing reliable to check against disk.
        if value.is_empty() || value.contains('%') {
            continue;
        }
        let name = value.rsplit('/').next().unwrap_or(value);
        if !checkout_dir.join(name).exists() {
            missing.push(name.to_string());
        }
    }
    missing
}

/// Keep `checkout_dir/series` in sync with the spec's current `PatchN:`
/// lines. `QUILT_PATCHES` points `quilt new`/`quilt add`/`quilt refresh`
/// (see `agent::TurnCtx::checkout_dir`'s doc comment) at `checkout_dir`
/// directly, so a patch removed from the spec — by hand, by `--regen`, or
/// by an earlier agent turn — otherwise leaves its filename behind in
/// `series` forever: `%autosetup -p1`'s quilt backend chokes trying to
/// apply a patch the current spec no longer declares, which looks like a
/// spurious, unexplained `%prep` failure on a spec that presently declares
/// zero patches (observed on python-python-docx). Idempotent and run on
/// every `ValidateSpec` visit, like `missing_source_files`.
fn sync_quilt_series(expanded_spec: &str, checkout_dir: &Path) {
    let mut patches = Vec::new();
    for line in expanded_spec.lines() {
        let line = line.trim();
        if !line.starts_with("Patch") {
            continue;
        }
        let Some(rest) = line["Patch".len()..]
            .trim_start_matches(|c: char| c.is_ascii_digit())
            .strip_prefix(':')
        else {
            continue;
        };
        let value = rest.trim();
        if value.is_empty() || value.contains('%') {
            continue;
        }
        patches.push(value.rsplit('/').next().unwrap_or(value).to_string());
    }

    let wanted = if patches.is_empty() {
        String::new()
    } else {
        patches.join("\n") + "\n"
    };
    let series_path = checkout_dir.join("series");
    let current = std::fs::read_to_string(&series_path).unwrap_or_default();
    if current == wanted {
        return;
    }
    if wanted.is_empty() {
        if let Err(e) = std::fs::remove_file(&series_path)
            && e.kind() != std::io::ErrorKind::NotFound
        {
            ap_core::diag!("  cannot remove {}: {e}", series_path.display());
        }
    } else {
        write_or_warn(&series_path, &wanted);
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
    // Note: We used to warn about missing osc timestamp prefixes, but this is
    // harmless and not actionable — the extractor still works, just with
    // slightly less accurate anchoring. Removed to reduce noise.

    extracted
}

/// Analyse a failed build log inside its own `ExtractFailure` step.
///
/// A step directory that cannot be created only loses the step's report, not
/// the analysis the agent needs, so it falls back to extracting unreported.
fn extract_failure_step(
    pkg_run: &PkgRunDir,
    reporter: &dyn Reporter,
    step_attempts: &mut Attempts,
    build_log: &Path,
    sdist_top_level: &str,
    build_step: ap_core::event::StepId,
) -> crate::log::ExtractedLog {
    match StepGuard::begin(
        pkg_run,
        reporter,
        StepKind::ExtractFailure,
        None,
        step_attempts,
    ) {
        Ok(step) => {
            let extract = extract_build_log(build_log, sdist_top_level, reporter, Some(step.id));
            step.finish(StepOutcome::Ok);
            extract
        }
        Err(e) => {
            reporter.report(&Event::Warning {
                step: Some(build_step),
                message: format!("could not report the failure analysis step: {e}"),
            });
            extract_build_log(build_log, sdist_top_level, reporter, Some(build_step))
        }
    }
}

/// Validate an agent's spec edit, keep it or restore `before`, and report.
///
/// Returns true when the edit was kept. `rpmspec --parse` is the judge: a
/// macro reference inside a comment is only warned about, because RPM expands
/// comments but the great majority of such lines still parse fine, and
/// throwing away an otherwise good fix costs an attempt.
/// Snapshot of every top-level file in a checkout directory before an agent
/// turn runs, so a reject can roll back ALL of the agent's changes — not
/// just the spec, which was the only file the pre-fix `accept_or_restore`
/// touched. Patches, `_service`, `.changes`, and anything else at the
/// checkout root are now part of the atomic unit.
///
/// Scoped to top-level files (`checkout_dir/*`): the agent's expected
/// write surface is just the OSC-committed artifacts. The `.osc/` metadata
/// directory and the extracted source tree (potentially millions of files)
/// are deliberately NOT snapshotted — editing those would be the agent
/// stepping well outside the recipe, and copying them around on every
/// turn would be prohibitively expensive.
struct CheckoutSnapshot {
    checkout_dir: PathBuf,
    /// filename → contents at snapshot time
    files: std::collections::HashMap<std::ffi::OsString, Vec<u8>>,
}

impl CheckoutSnapshot {
    fn take(checkout_dir: &Path) -> Self {
        let mut files = std::collections::HashMap::new();
        if let Ok(iter) = std::fs::read_dir(checkout_dir) {
            for entry in iter.filter_map(|e| e.ok()) {
                let ty = match entry.file_type() {
                    Ok(t) => t,
                    Err(_) => continue,
                };
                if !ty.is_file() {
                    continue;
                }
                let name = entry.file_name();
                if let Ok(bytes) = std::fs::read(entry.path()) {
                    files.insert(name, bytes);
                }
            }
        }
        Self {
            checkout_dir: checkout_dir.to_path_buf(),
            files,
        }
    }

    /// Restore every top-level file to its snapshot state: restore
    /// modified files, re-create deleted ones, and delete any top-level
    /// files the agent added. Idempotent — called on reject only.
    fn restore(&self, reporter: &dyn Reporter, step: &StepDir) {
        // 1) Re-write every snapshotted file (handles both edited and
        //    deleted ones).
        for (name, bytes) in &self.files {
            let path = self.checkout_dir.join(name);
            if let Err(e) = ap_core::fsx::write_atomic(&path, bytes) {
                reporter.report(&Event::Warning {
                    step: Some(step.id),
                    message: format!("could not restore {}: {e}", path.display()),
                });
            }
        }
        // 2) Delete any top-level files that didn't exist in the snapshot
        //    (i.e. new files the agent added).
        if let Ok(iter) = std::fs::read_dir(&self.checkout_dir) {
            for entry in iter.filter_map(|e| e.ok()) {
                let ty = match entry.file_type() {
                    Ok(t) => t,
                    Err(_) => continue,
                };
                if !ty.is_file() {
                    continue;
                }
                let name = entry.file_name();
                if !self.files.contains_key(&name) {
                    let path = entry.path();
                    if let Err(e) = std::fs::remove_file(&path) {
                        reporter.report(&Event::Warning {
                            step: Some(step.id),
                            message: format!(
                                "could not delete agent-added file {}: {e}",
                                path.display()
                            ),
                        });
                    }
                }
            }
        }
    }
}

fn accept_or_restore(
    spec_path: &Path,
    before: &str,
    step: &StepDir,
    reporter: &dyn Reporter,
    notes_entry: &str,
    notes_path: PathBuf,
) -> bool {
    accept_or_restore_with_snapshot(
        spec_path,
        before,
        None,
        step,
        reporter,
        notes_entry,
        notes_path,
    )
}

/// `accept_or_restore` extended with a full-checkout snapshot taken BEFORE
/// the agent turn ran. On reject (rpmspec --parse fails on the agent's
/// new spec), both the spec AND every other top-level checkout file the
/// agent may have edited are rolled back from the snapshot.
///
/// The two-function split exists because `accept_or_restore` is also
/// called from the automatic retry path inside `generate_from_repo` where
/// there is no "before this turn" snapshot to take — a `None` snapshot
/// there means "spec-only restore", preserving pre-fix behavior for that
/// one caller.
///
/// Oct 6 review (reproducing Sep): the pre-fix function restored only the
/// spec text, so a `.changes`/patch/`_service` edit the agent made in the
/// same turn survived even when the spec itself was rejected. The next
/// turn then saw a half-reverted state and could commit it.
fn accept_or_restore_with_snapshot(
    spec_path: &Path,
    before: &str,
    snapshot: Option<&CheckoutSnapshot>,
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
        restore_spec(spec_path, before, step, reporter);
        // When we have a full-checkout snapshot, roll back patches /
        // `.changes` / `_service` / etc. too — the agent's edits are an
        // atomic unit, and a half-reverted checkout is a worse state
        // than any version this run has actually validated.
        if let Some(snap) = snapshot {
            snap.restore(reporter, step);
        }
        return false;
    }

    // `rpmspec --parse` cannot see a patch that does not apply; that would only
    // surface as a `%prep` failure after a full OBS cycle. Hand-written patches
    // with wrong hunk counts ("malformed patch") are the usual culprit.
    if let Err(problem) = check_patches_apply(spec_path, &after) {
        reporter.report(&Event::SpecRejected {
            step: step.id,
            reason: ap_core::event::SpecRejection::RpmspecParseFailed {
                stderr: problem.clone(),
            },
            restored_from: spec_path.to_path_buf(),
        });
        restore_spec(spec_path, before, step, reporter);
        if let Some(snap) = snapshot {
            snap.restore(reporter, step);
        }
        append_notes(
            notes_path,
            &format!(
                "\n## Edit rejected — a patch does not apply\n{problem}\n\
                 Create or change patches ONLY with quilt (`cd source && quilt new/add/refresh`), \
                 never by writing a .patch file by hand: hand-written hunk headers are wrong. \
                 The edit was rolled back.\n"
            ),
        );
        return false;
    }

    let diff = unified_diff(before, &after);
    let (added, removed) = diff_counts(&diff);
    let diff_path = step.file("spec.diff");
    write_or_warn(&diff_path, &diff);
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

/// Everything [`apply_turn_outcome`] needs besides the outcome itself and the
/// reporter — bundled so the function doesn't trip clippy's argument-count
/// lint the way its five call sites' worth of parameters would inline.
struct ApplyTurnCtx<'a> {
    config: &'a PipelineConfig,
    checkout_dir: &'a Path,
    spec_path: &'a Path,
    step: &'a StepDir,
    /// Spec content before this turn ran, for `accept_or_restore`'s diff.
    before: &'a str,
    rpm_name: &'a str,
    /// Short description of this turn used in both the notes.md heading and
    /// any failure reason (`"FixAfterBuild"`, `"Review — pre-build"`, ...).
    label: &'a str,
    attempt: usize,
    notes_path: PathBuf,
    /// Full-checkout snapshot taken BEFORE the agent turn ran. When `Some`,
    /// a rejected / blocked / no-change outcome rolls back every top-level
    /// checkout file the agent may have edited (patches, `.changes`,
    /// `_service`, …), not just the spec. The Sep review called out spec-
    /// only rollback as insufficient; this field is the plumbing. Each
    /// call site opts in by capturing a snapshot right before `agent_turn`
    /// and passing `Some(&snap)` here. Sites that don't opt in get the
    /// pre-fix spec-only restore (safe default).
    snapshot: Option<&'a CheckoutSnapshot>,
}

/// Apply one agent turn's outcome the same way at every call site that runs
/// a fix turn (Review, PreFix, FixAfterBuild — both the unresolvable-deps and
/// generic-failure shapes — and FixAfterValidation): accept an edit, switch
/// to the upstream wheel and accept that instead, or report nothing changed.
///
/// Centralizing this is what makes `Action::UseUpstreamWheel` (convention 28
/// Exception 4) actually available at *every* turn instead of only the ones
/// whose call site happened to be updated — that gap is exactly how it went
/// missing at three separate sites the first time each was patched by hand.
/// `Blocked` and a failed wheel fallback are returned as `Err` for the
/// caller to fold into its own `PipelineResult::Failed`.
///
/// Returns [`TurnEffect::SwitchedToWheel`] as a distinct case from
/// `Edited` — not just `Ok(true)` — because switching replaces the entire
/// spec with a wheel skeleton: `sdist_top_level` and `from_repo_archive`,
/// both computed once from the *original* source at the top of
/// `run_package`, describe a source tree that no longer has anything to do
/// with the spec on disk (a wheel unpacks flat, and it isn't a repository
/// archive). The caller resets them; folding a wheel switch into the same
/// variant as an ordinary edit would leave those stale — a wrong
/// `sdist_top_level` mainly degrades traceback-file matching in
/// `extract_build_log`, not silently corrupts the package, but there is no
/// reason to leave it wrong once we know better.
fn apply_turn_outcome(
    ctx: ApplyTurnCtx<'_>,
    outcome: &crate::agent::TurnOutcome,
    reporter: &dyn Reporter,
) -> Result<TurnEffect, String> {
    let ApplyTurnCtx {
        config,
        checkout_dir,
        spec_path,
        step,
        before,
        rpm_name,
        label,
        attempt,
        notes_path,
        snapshot,
    } = ctx;
    let effect = match &outcome.action {
        crate::agent::Action::Blocked { why } => {
            // Oct 6 finding (sibling of #7): on Blocked, restore the full
            // checkout too — the agent may have written speculative
            // patches/_service files before deciding it's blocked, and
            // those should not survive the rejection.
            if let Some(snap) = snapshot {
                snap.restore(reporter, step);
            }
            restore_spec(spec_path, before, step, reporter);
            Err(format!("{label}: agent blocked: {why}"))
        }
        crate::agent::Action::NoChange => {
            // On NoChange, also restore — the agent's "no change needed"
            // verdict shouldn't paper over an unreported partial edit.
            if let Some(snap) = snapshot {
                snap.restore(reporter, step);
            }
            Ok(TurnEffect::NoChange)
        }
        crate::agent::Action::Edited => {
            let changed = accept_or_restore_with_snapshot(
                spec_path,
                before,
                snapshot,
                step,
                reporter,
                &format!(
                    "\n## Attempt {attempt} — {label}\nReasoning: {}\nConfidence: {:?}\n",
                    outcome.reasoning, outcome.confidence
                ),
                notes_path,
            );
            Ok(if changed {
                TurnEffect::Edited
            } else {
                TurnEffect::NoChange
            })
        }
        crate::agent::Action::UseUpstreamWheel { why } => {
            // Minimum-shape gate on the reason: too short to be real
            // evidence rejects the switch outright instead of silently
            // swapping the recipe based on an empty `why`.
            if let Err(e) = validate_wheel_switch_why(why) {
                return Err(format!("{label}: {e}"));
            }
            reporter.report(&Event::Warning {
                step: Some(step.id),
                message: format!(
                    "{rpm_name} reported unbuildable from source in this environment \
                     ({why}); trying to repackage the upstream PyPI wheel instead"
                ),
            });
            match try_wheel_fallback(config, checkout_dir, spec_path, step) {
                Ok(wheel) => {
                    // Durable, reviewable audit record of what got swapped
                    // and why. See `write_wheel_switch_provenance` for the
                    // rationale — the OBS commit's reviewers do not see
                    // the run's JSONL, so the switch has to leave a trail
                    // inside the checkout itself.
                    if let Err(e) = write_wheel_switch_provenance(
                        checkout_dir,
                        rpm_name,
                        config,
                        &wheel,
                        attempt,
                        label,
                        why,
                        before,
                    ) {
                        reporter.report(&Event::Warning {
                            step: Some(step.id),
                            message: format!(
                                "{rpm_name}: wheel switch succeeded but writing \
                                 wheel-origin.txt / spec.pre-wheel-switch failed \
                                 ({e}); add them by hand before submitting the \
                                 OBS request — the switch is otherwise invisible \
                                 to the recipe reviewer"
                            ),
                        });
                    }
                    let changed = accept_or_restore(
                        spec_path,
                        before,
                        step,
                        reporter,
                        &format!(
                            "\n## {label} — switched to upstream wheel\n\
                             Wheel: {}\n\
                             SHA256: {}\n\
                             Reasoning: {why}\n",
                            wheel.filename, wheel.sha256,
                        ),
                        notes_path,
                    );
                    Ok(if changed {
                        TurnEffect::SwitchedToWheel
                    } else {
                        TurnEffect::NoChange
                    })
                }
                // A per-CPython-locked wheel set (Triton) is refused by the
                // automatic switch but IS packageable by hand (convention
                // 49). Failing the package here threw that away even when
                // the agent had correctly diagnosed it; hand the agent the
                // redirect through the notes it reads next turn and carry on.
                Err(e) if e.contains("no usable upstream wheel either") => {
                    append_notes(
                        notes_path,
                        &format!(
                            "\n## {label} — use_upstream_wheel was refused\n\
                             {e}\n\
                             The automatic wheel switch cannot serve every flavor from one \
                             wheel. Do NOT return `use_upstream_wheel` again: if PyPI has a \
                             wheel for each flavor's CPython ABI, follow convention 49 and \
                             write the per-flavor wheel spec yourself with action `edited`; \
                             otherwise report `blocked` naming the flavor that has no wheel.\n"
                        ),
                    );
                    reporter.report(&Event::Warning {
                        step: Some(step.id),
                        message: format!(
                            "{rpm_name}: use_upstream_wheel refused ({e}); asking the \
                             agent to repackage per-flavor wheels by hand (convention 49)"
                        ),
                    });
                    Ok(TurnEffect::NoChange)
                }
                Err(e) => Err(format!("{label}: unbuildable from source ({why}), and {e}")),
            }
        }
    };

    // After any accepted agent edit, re-run OBS services so a newly-added or
    // renamed Source the agent fetches via a service — one it may have just
    // written a `_service` file for in this very turn, including on a
    // checkout that had none before — is actually materialised on disk
    // before the next validation sees it. Convention 13's "the checkout
    // does not gain a file just because the spec now names it correctly"
    // trip-up (drogon-shape case) costs an entire extra attempt without
    // this. Called unconditionally rather than only when `_service` already
    // existed, because that gate would silently miss exactly the case this
    // was added for; `osc` itself is the authority on what to do when none
    // is present. Non-fatal: a service failure here surfaces as a warning
    // and lets the existing validation gate catch any actual breakage,
    // instead of masking whatever the agent was trying to fix behind an
    // unrelated service error.
    if matches!(
        &effect,
        Ok(TurnEffect::Edited | TurnEffect::SwitchedToWheel)
    ) && let Err(e) = osc::run_service(checkout_dir, Emit::new(reporter, step.id))
    {
        reporter.report(&Event::Warning {
            step: Some(step.id),
            message: format!("osc service ra after {label} failed: {e}; proceeding anyway"),
        });
    }

    effect
}

/// [`apply_turn_outcome`] plus the one side effect every one of its 6 call
/// sites in `run_package` must perform identically on `SwitchedToWheel`
/// (reset `sdist_top_level`/`from_repo_archive` — see that doc comment for
/// why). Four of the six needed nothing beyond that and were byte-identical;
/// the other two layer one extra statement (bumping `attempts`, or setting
/// `agent_edited`) on the returned `TurnEffect` — this is that shared base,
/// not a sixth copy of the same reset.
fn apply_turn_and_reset(
    ctx: ApplyTurnCtx<'_>,
    outcome: &crate::agent::TurnOutcome,
    reporter: &dyn Reporter,
    sdist_top_level: &mut String,
    from_repo_archive: &mut bool,
) -> Result<TurnEffect, String> {
    let effect = apply_turn_outcome(ctx, outcome, reporter)?;
    if effect == TurnEffect::SwitchedToWheel {
        *sdist_top_level = String::new();
        *from_repo_archive = false;
    }
    Ok(effect)
}

/// What happened to the spec as a result of [`apply_turn_outcome`]. See its
/// doc comment for why `SwitchedToWheel` needs to be distinguishable from
/// `Edited` rather than folded into one "changed" boolean.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TurnEffect {
    NoChange,
    Edited,
    SwitchedToWheel,
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
/// `dep` is a raw RPM dependency string like `python313-Cython>=3.1`. The
/// name and the (optional) version operator + required version are parsed
/// via [`parse_rpm_dep_constraint`], then each matching RPM filename is
/// unpacked by [`extract_rpm_filename_version`] and compared using
/// [`rpm_version_satisfies`].
///
/// The pre-review fix only compared by name-prefix and ignored the version
/// operator entirely — so e.g. a fake `python-foo-0.1-1.1.x86_64.rpm`
/// satisfied `python-foo>=99`, and the dep got silently filtered out of the
/// "ask the agent to look at this" list even though no actually-usable RPM
/// existed. OBS would then keep reporting the same dep as unresolvable until
/// the attempt counter ran out.
fn rpm_satisfies_dep(rpms_dir: &Path, dep: &str) -> bool {
    let (pkg_name, constraint) = parse_rpm_dep_constraint(dep);
    let prefix = format!("{pkg_name}-");
    std::fs::read_dir(rpms_dir)
        .into_iter()
        .flatten()
        .filter_map(|e| e.ok())
        .any(|entry| {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if !name.starts_with(prefix.as_str()) {
                return false;
            }
            let Some(ver) = extract_rpm_filename_version(pkg_name, &name) else {
                return false;
            };
            match &constraint {
                Some((op, required)) => rpm_version_satisfies(ver, op, required),
                None => true,
            }
        })
}

/// Split an RPM dependency string `python313-Cython>=3.1` into
/// `("python313-Cython", Some((">=", "3.1")))`. Operators considered, in
/// precedence order to avoid matching `>=` as `>` first: `>=`, `<=`, `==`,
/// `!=`, `>`, `<`, `=`. A bare `name` (no operator) returns `(name, None)`
/// — the caller treats "any version will do" as satisfied by presence of
/// the name alone.
fn parse_rpm_dep_constraint(dep: &str) -> (&str, Option<(&str, &str)>) {
    const OPS: &[&str] = &[">=", "<=", "==", "!=", ">", "<", "="];
    for op in OPS {
        if let Some(i) = dep.find(op) {
            let (name, rest) = dep.split_at(i);
            let version = rest[op.len()..].trim();
            return (name.trim(), Some((*op, version)));
        }
    }
    (dep.trim(), None)
}

/// Pull `<version>` out of an RPM filename of the form
/// `<name>-<version>-<release>.<arch>.rpm`.
///
/// `name` is passed in so no name/version ambiguity exists (name may itself
/// contain `-`). Returns `None` for a filename whose shape doesn't match
/// (not stripped of `.rpm`, missing a `-<release>` or `.<arch>` segment).
fn extract_rpm_filename_version<'a>(name: &str, filename: &'a str) -> Option<&'a str> {
    let stem = filename.strip_suffix(".rpm")?;
    // stem = <name>-<version>-<release>.<arch>
    let after_name = stem.strip_prefix(name)?.strip_prefix('-')?;
    // after_name = <version>-<release>.<arch>
    // Strip ".<arch>" from the right.
    let (ver_rel, _arch) = after_name.rsplit_once('.')?;
    // Strip "-<release>" from the right (release doesn't contain '-' in
    // practice, so a single rsplit suffices).
    let (version, _release) = ver_rel.rsplit_once('-')?;
    Some(version)
}

/// Evaluate `candidate <op> required` using numeric-aware segment compare.
///
/// Both strings are split on `.`/`-`; each segment compares numerically when
/// both sides parse as integers (so `20 > 2`, not `"20" < "2"`
/// lexicographically) and lexicographically otherwise. Missing trailing
/// segments default to "0" so `1.2 == 1.2.0` and `1.2 < 1.2.3`.
///
/// Not a full RPM EVR implementation — doesn't understand `~`, `^`, or an
/// explicit epoch. It is correct for every Python-RPM case the project has
/// in practice, and strictly tighter than the pre-review substring-only
/// behavior; a true full-fidelity EVR comparator belongs in `osc`/`rpm`
/// crate wrappers, not here.
fn rpm_version_satisfies(candidate: &str, op: &str, required: &str) -> bool {
    use std::cmp::Ordering;
    let a: Vec<&str> = candidate.split(['.', '-']).collect();
    let b: Vec<&str> = required.split(['.', '-']).collect();
    let n = a.len().max(b.len());
    let mut ord = Ordering::Equal;
    for i in 0..n {
        let x = a.get(i).copied().unwrap_or("0");
        let y = b.get(i).copied().unwrap_or("0");
        let segment_ord = match (x.parse::<u64>(), y.parse::<u64>()) {
            (Ok(xn), Ok(yn)) => xn.cmp(&yn),
            _ => x.cmp(y),
        };
        if segment_ord != Ordering::Equal {
            ord = segment_ord;
            break;
        }
    }
    match op {
        ">=" => !matches!(ord, Ordering::Less),
        "<=" => !matches!(ord, Ordering::Greater),
        ">" => matches!(ord, Ordering::Greater),
        "<" => matches!(ord, Ordering::Less),
        "==" | "=" => matches!(ord, Ordering::Equal),
        "!=" => !matches!(ord, Ordering::Equal),
        _ => true,
    }
}

/// True iff the spec on disk declares a `Version:` other than `version`.
///
/// Compares the parsed `Version:` tag value for exact string equality, not
/// a substring check. The previous `contains("Version: 1.2")` matched inside
/// `Version: 1.20`, so a request for `1.2` wrongly reported "spec matches"
/// against a spec actually declaring `1.20`, and the pipeline would short-
/// circuit as `AlreadyBuilt` for the wrong version.
///
/// `true` on an unreadable spec or one with no `Version:` at all so the
/// caller re-generates rather than skipping work it has no evidence it
/// already did.
fn spec_version_differs(spec_path: &Path, version: &str) -> bool {
    let Ok(text) = std::fs::read_to_string(spec_path) else {
        return true;
    };
    let Some(header) = crate::spec::parse_header(&text) else {
        return true;
    };
    header.version != version
}

/// Apply the spec's `PatchN:` files, in order, to a scratch copy of the source
/// tarball the way `%autosetup -p1` will. `Ok` when there is nothing to check
/// (no patches, no tarball, not a `-p1` autosetup) or all of them apply.
fn check_patches_apply(spec_path: &Path, spec: &str) -> Result<(), String> {
    let patches: Vec<String> = spec
        .lines()
        .filter_map(|l| {
            let (tag, value) = l.split_once(':')?;
            let tag = tag.trim();
            (tag.len() > 5
                && tag[..5].eq_ignore_ascii_case("patch")
                && tag[5..].chars().all(|c| c.is_ascii_digit()))
            .then(|| value.trim().to_string())
        })
        .collect();
    if patches.is_empty() || !spec.contains("%autosetup") || !spec.contains("-p1") {
        return Ok(());
    }
    let Some(checkout) = spec_path.parent() else {
        return Ok(());
    };
    let Some(tarball) = find_tarball(checkout) else {
        return Ok(());
    };
    let scratch = tempfile::tempdir().map_err(|e| e.to_string())?;
    if ap_ecosystem::sdist::extract(&tarball, scratch.path()).is_err() {
        return Ok(());
    }
    // A single top-level directory is what `%autosetup -n` enters.
    let entries: Vec<_> = std::fs::read_dir(scratch.path())
        .map_err(|e| e.to_string())?
        .filter_map(|e| e.ok())
        .collect();
    let root = match entries.as_slice() {
        [only] if only.path().is_dir() => only.path(),
        _ => scratch.path().to_path_buf(),
    };
    for name in patches {
        let name = name.replace("%{version}", "");
        // Patch names come from spec text. Only a bare file name is a patch in
        // this checkout; `/etc/x` or `../x` would read outside it.
        if Path::new(&name).file_name() != Some(name.as_ref()) {
            continue;
        }
        let patch_file = checkout.join(&name);
        if !patch_file.is_file() {
            // A missing patch file is reported by the missing-source check.
            continue;
        }
        let out = std::process::Command::new("patch")
            .args([
                "-p1",
                "--fuzz=0",
                "-f",
                "-s",
                "--no-backup-if-mismatch",
                "-i",
            ])
            .arg(&patch_file)
            .current_dir(&root)
            .stdin(std::process::Stdio::null())
            .output()
            .map_err(|e| {
                let spec = spec_path.file_name().unwrap_or_default().to_string_lossy();
                format!("could not run patch for {name} ({spec}): {e}")
            })?;
        if !out.status.success() {
            let detail = String::from_utf8_lossy(&out.stdout).to_string()
                + &String::from_utf8_lossy(&out.stderr);
            return Err(format!(
                "{name} does not apply to the pristine source with `patch -p1 --fuzz=0`: {}",
                detail.trim().chars().take(600).collect::<String>()
            ));
        }
    }
    Ok(())
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
            || n.ends_with(".whl")
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
        // A wheel unpacks flat (no wrapping directory) — see
        // `ap_ecosystem::sdist::extract`'s `.whl` branch.
        if fname.ends_with(".whl") {
            return String::new();
        }
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
    /// The repository exists but has no release archive for the wanted
    /// version (a guessed repo for a wheel-only vendor package, or a tag
    /// scheme the archive-URL guesses don't cover). Carries the full message.
    NoReleaseArchive(String),
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
        match generate_from_repo(config, repo_url, checkout_dir, spec_path, step) {
            // A repository with no archive for this version cannot be the
            // source of this release (a guessed repo for a wheel-only vendor
            // package, or an unusual tag scheme): use PyPI's own artifacts
            // below rather than failing the package outright.
            Err(GenSpecError::NoReleaseArchive(msg)) => {
                ap_core::diag!("  {msg}\n  falling back to PyPI's own release artifacts");
            }
            result => return result,
        }
    }

    use ap_ecosystem::pypi;
    use ap_ecosystem::sdist;

    let pypi_url = pypi_json_url(&config.pkg_name, &config.version);
    let json = crate::http::read_text(crate::http::get(&pypi_url).call().map_err(|e| {
        GenSpecError::Other(format!(
            "PyPI fetch failed for {} {}: {e}",
            config.pkg_name, config.version
        ))
    })?)
    .map_err(|e| {
        GenSpecError::Other(format!(
            "PyPI read failed for {} {}: {e}",
            config.pkg_name, config.version
        ))
    })?;
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
                    // Auto-discover a source repository. Only a *verified*
                    // (confirmed to exist) candidate is safe to build from
                    // automatically — an unverified guess just burns an
                    // attempt on a 404. See `RepoCandidates`.
                    let repos = find_source_repo_candidates(&config.pkg_name);
                    match repos.verified.first() {
                        Some(url) => {
                            ap_core::diag!(
                                "info: auto-discovered and verified source repository for {}: {url}",
                                config.pkg_name,
                            );
                            url.clone()
                        }
                        None => {
                            // No source anywhere. This is the normal shape of a
                            // closed-source binary redistribution (NVIDIA's
                            // individual CUDA toolkit component wheels are the
                            // running example) — repackage the wheel itself
                            // rather than getting permanently stuck. See
                            // convention 24 Exception 3.
                            return wheel_or_wheel_only(
                                config,
                                &json,
                                checkout_dir,
                                spec_path,
                                step,
                                repos.guessed,
                            );
                        }
                    }
                }
            };

            let tarball = checkout_dir.join(format!(
                "{}-{}.tar.gz",
                safe_file_part(&config.pkg_name.to_lowercase()),
                safe_file_part(&config.version)
            ));
            let (bytes, url) = match fetch_release_archive(&source_url, &config.version) {
                Ok(v) => v,
                // The repository exists but has no archive for THIS version (a
                // guessed repo for a vendor wheel such as nvidia-nccl-cu13, or an
                // unusual tag scheme): it cannot be this release's source. Use
                // PyPI's own artifact instead of failing the package.
                Err(GenSpecError::NoReleaseArchive(msg)) => {
                    ap_core::diag!("  {msg}\n  falling back to PyPI's wheel");
                    return wheel_or_wheel_only(
                        config,
                        &json,
                        checkout_dir,
                        spec_path,
                        step,
                        vec![source_url.clone()],
                    );
                }
                Err(e) => return Err(e),
            };
            ap_core::fsx::write_atomic(&tarball, &bytes)
                .map_err(|e| GenSpecError::Other(format!("writing {}: {e}", tarball.display())))?;
            let info = ap_ecosystem::sdist::inspect_preferring(&tarball, Some(&config.pkg_name))
                .map_err(|e| GenSpecError::Other(format!("inspecting {url}: {e}")))?;

            return generate_repo_python_spec(config, &tarball, &info, &json, spec_path, step);
        }
        Err(e) => return Err(GenSpecError::Other(format!("PyPI parse failed: {e}"))),
    };

    // Download tarball to checkout dir.
    let tarball = release.sdist.fetch(checkout_dir).map_err(|e| {
        GenSpecError::Other(format!(
            "tarball download failed for {}: {e}",
            release.sdist.url
        ))
    })?;

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
        ap_core::fsx::write_atomic(spec_path, &spec_text)
            .map_err(|e| GenSpecError::Other(format!("writing spec failed: {e}")))?;
        write_or_warn(step.file("spec.generated"), &spec_text);
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
            top_level_tests_dir: sdist_info.top_level_tests_dir.clone(),
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
    ap_core::fsx::write_atomic(spec_path, &spec_text)
        .map_err(|e| GenSpecError::Other(format!("writing spec failed: {e}")))?;

    // Save artifact copy.
    write_or_warn(step.file("spec.generated"), &spec_text);

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
    let tarball = checkout_dir.join(format!(
        "{}-{}.tar.gz",
        safe_file_part(&name),
        safe_file_part(&config.version)
    ));

    let (bytes, url) = fetch_release_archive(repo_url, &config.version)?;
    ap_core::fsx::write_atomic(&tarball, &bytes)
        .map_err(|e| GenSpecError::Other(format!("writing {}: {e}", tarball.display())))?;

    // Same preference the probe used, so generation and naming agree on which
    // subproject of a monorepo is being built.
    let info = ap_ecosystem::sdist::inspect_preferring(&tarball, Some(&config.pkg_name))
        .map_err(|e| GenSpecError::Other(format!("inspecting {url}: {e}")))?;

    // Mirror `sources::probe`: a CMake/autotools root with a non-matching
    // pyproject.toml beside it is a C/C++ project with a Python-helper
    // sidecar (llama.cpp's root `pyproject` declares `llama-cpp-scripts`
    // for shipping helper scripts), not a Python project. Without this
    // check `info.python_packaging` wins the ecosystem decision below
    // and the pipeline either falls into the mismatch-block's
    // `try_wheel_fallback` (no PyPI wheel exists for a C++ library like
    // llama.cpp, so it dies with "no usable upstream wheel either") or
    // — when the pyproject name happens to match the wanted name —
    // calls py2pack on a C/C++ project. Probe already decided PlainC
    // via `is_python_hijack`; align the generator with that decision.
    let is_python_hijack = crate::sources::is_python_hijack(&info, &config.pkg_name);

    // A monorepo may hold several projects, and `inspect_preferring` falls
    // back to the shallowest manifest when none of them declares the wanted
    // name. That fallback is fine for browsing, but generating a spec from it
    // silently ships the wrong project (e.g. building a compiled-artifact-only
    // wheel's monorepo checkout, which builds its unrelated top-level
    // package). Refuse instead of guessing, so a mismatch surfaces here
    // rather than several minutes later in review.
    let normalize = |n: &str| n.to_lowercase().replace(['-', '.'], "_");
    let wanted = normalize(&config.pkg_name);
    let declared_names: Vec<&str> = info
        .project_name
        .iter()
        .map(String::as_str)
        .chain(info.subprojects.iter().filter_map(|s| s.name.as_deref()))
        .collect();
    // Skip the mismatch guard when the archive is a Python-hijacked
    // C/C++ project: the "declared name doesn't match wanted" condition
    // is true BY CONSTRUCTION in that case (it's the hijack's own
    // trigger), and letting it fall through sends a C/C++ project to
    // `try_wheel_fallback` which cannot possibly help.
    if !is_python_hijack
        && (info.python_packaging || info.cargo_manifest)
        && !declared_names.iter().any(|n| normalize(n) == wanted)
    {
        let found = if declared_names.is_empty() {
            "no named projects at all".to_string()
        } else {
            declared_names.join(", ")
        };
        let mismatch_reason = format!(
            "{url} does not declare a project named {wanted:?} (found: {found}). \
             This usually means {} is binary-only upstream and this repository \
             is not its real source.",
            config.pkg_name
        );
        // Don't just die here: a repo-guessing heuristic that "verified" (the
        // repo exists) but picked the wrong artifact for this specific PyPI
        // release is exactly what convention 28 Exception 4's second
        // paragraph covers when the Review agent finds it — the fix is the
        // same either way (repackage the upstream wheel), so do it now
        // instead of hard-failing and waiting for a human to notice the
        // error message's own advice ("check whether ... a wheel"). The
        // wrongly-fetched archive is no longer needed either way.
        let _ = std::fs::remove_file(&tarball);
        // Discard the picked wheel info here: the provenance file only
        // makes sense when the switch is the result of a deliberate agent
        // `UseUpstreamWheel`, where the audit trail has a specific `why`
        // to record. This call site is the generation path's automatic
        // retry on a wrongly-guessed repo (no agent decision was made),
        // so there is nothing to attest beyond "guess was wrong, we fell
        // back" and the warning above the caller already covers it.
        return match try_wheel_fallback(config, checkout_dir, spec_path, step) {
            Ok(_wheel) => Ok(GenSpecResult {
                sdist_top_level: String::new(),
                is_maturin: false,
                is_rust: false,
                from_repo_archive: false,
            }),
            Err(e) => Err(GenSpecError::Other(format!(
                "{mismatch_reason} Tried repackaging the upstream wheel instead, but {e} \
                 (retry with a different --source-url if one exists)."
            ))),
        };
    }

    // Treat a Python-hijacked archive as C/C++ before any Python/Rust
    // branch has a chance to claim it — the pyproject.toml here is a
    // scripts sidecar, not the primary build system.
    if info.python_packaging && !is_python_hijack {
        // py2pack needs PyPI metadata for the summary, license and deps. When
        // the project is not on PyPI at all there is nothing to fetch, and the
        // agent fills those in from the source during the review turn.
        let pypi_json = match fetch_pypi_json(&config.pkg_name, &config.version) {
            Ok(json) => json.unwrap_or_default(),
            Err(e) => {
                // Not "not on PyPI": the spec would silently lack its license,
                // summary and dependencies. Say so; the review turn still fills
                // them in from the source.
                ap_core::diag!("  warning: {e}; generating the spec without PyPI metadata");
                String::new()
            }
        };
        return generate_repo_python_spec(config, &tarball, &info, &pypi_json, spec_path, step);
    }

    if info.cargo_manifest {
        // The Rust path re-fetches through `osc service ra`, which is what
        // produces the vendored sources OBS builds from.
        let _ = std::fs::remove_file(&tarball);
        return generate_rust_spec(config, repo_url, spec_path, step);
    }

    if info.cmake_at_root || info.autotools_at_root {
        // Unlike Rust, a plain C/C++ project vendors nothing through OBS
        // services — the archive already downloaded is the real Source0.
        let build = if info.cmake_at_root {
            crate::spec::CBuildSystem::Cmake
        } else {
            crate::spec::CBuildSystem::Autotools
        };
        let top_level = info
            .top_level_dir
            .clone()
            .unwrap_or_else(|| format!("{name}-{}", config.version));
        let spec_text = crate::spec::c_project_skeleton(
            &config.pkg_name,
            &config.version,
            repo_url,
            &top_level,
            build,
        );
        let spec_text = crate::spec::strip_ap_fix_comments(&spec_text);
        ap_core::fsx::write_atomic(spec_path, &spec_text)
            .map_err(|e| GenSpecError::Other(format!("writing spec failed: {e}")))?;
        write_or_warn(step.file("spec.generated"), &spec_text);
        return Ok(GenSpecResult {
            sdist_top_level: top_level,
            is_maturin: false,
            is_rust: false,
            // Not the PyPI-specific repo-archive case (convention 23) — a
            // plain C/C++ project has no PyPI angle at all. Its own guidance
            // is baked into the skeleton's convention 29 comment instead.
            from_repo_archive: false,
        });
    }

    Err(GenSpecError::Other(format!(
        "{url} contains no pyproject.toml, setup.py, Cargo.toml, CMakeLists.txt \
         or configure.ac, so auto-package cannot tell how to build it"
    )))
}

/// Download the first release archive that exists, returning its bytes and URL.
fn fetch_release_archive(repo_url: &str, version: &str) -> Result<(Vec<u8>, String), GenSpecError> {
    let candidates = crate::sources::archive_urls(repo_url, version);
    for url in &candidates {
        let Ok(resp) = crate::http::get_download(url).call() else {
            continue;
        };
        if let Ok(buf) = crate::http::read_download(resp)
            && !buf.is_empty()
        {
            return Ok((buf, url.clone()));
        }
    }
    Err(GenSpecError::NoReleaseArchive(format!(
        "no release archive for version {version} at {repo_url}. Tried:\n{}",
        candidates
            .iter()
            .map(|u| format!("  {u}"))
            .collect::<Vec<_>>()
            .join("\n"),
    )))
}

/// Build a minimal `Release` for a Python project that isn't on PyPI at
/// all (an upstream stub that was never published, a vendor-internal
/// package, a monorepo subproject). `name`/`version`/`tarball_filename`
/// are all we know; everything else (summary, license, homepage,
/// `requires_python`) is left blank for the Review agent to fill in
/// from the pyproject.toml during the first fix turn — the same path
/// the monorepo skeleton already takes.
///
/// `sdist.sha256`/`url`/`size` stay empty for the same reason
/// `parse_release_with_tarball` leaves them so: the tarball has
/// already been downloaded locally, and the generator uses only the
/// filename.
fn synthesize_release_without_pypi(
    name: String,
    version: String,
    tarball_filename: String,
) -> ap_ecosystem::pypi::Release {
    ap_ecosystem::pypi::Release {
        name,
        version,
        summary: None,
        homepage: None,
        license: ap_ecosystem::pypi::License::Unknown,
        requires_python: None,
        yanked: false,
        sdist: ap_ecosystem::pypi::Sdist {
            filename: tarball_filename,
            url: String::new(),
            sha256: String::new(),
            size: 0,
        },
    }
}

/// `https://pypi.org/pypi/<name>/<version>/json` with both parts URL-encoded.
fn pypi_json_url(name: &str, version: &str) -> String {
    format!(
        "https://pypi.org/pypi/{}/{}/json",
        urlencoding::encode(name),
        urlencoding::encode(version)
    )
}

/// PyPI's JSON for a release: `Ok(None)` when the project/version is simply not
/// on PyPI (404), `Err` for anything else (network, 5xx, oversized body) so the
/// caller can tell "not published" from "could not ask".
fn fetch_pypi_json(name: &str, version: &str) -> Result<Option<String>, String> {
    match crate::http::get(&pypi_json_url(name, version)).call() {
        Ok(resp) => crate::http::read_text(resp)
            .map(Some)
            .map_err(|e| format!("reading PyPI JSON for {name} {version}: {e}")),
        Err(ureq::Error::StatusCode(404)) => Ok(None),
        Err(e) => Err(format!("PyPI JSON for {name} {version}: {e}")),
    }
}

/// A component of a file name built from untrusted text (package name, tag):
/// anything but `[A-Za-z0-9._+-]` becomes `_`, and `..` cannot survive.
fn update_state(ws: &Workspace, reporter: &dyn Reporter, f: impl FnOnce(&mut PkgState)) {
    warn_if_err(reporter, "saving state.json", PkgState::update(ws, f));
}

/// Add a turn's cost to the package's spend for this run, so state.json keeps
/// it even when the pipeline stops before the next successful turn. Spend from
/// an earlier run is not carried over (see `PkgState::cumulative_spent_run_id`).
fn add_run_spend(ws: &Workspace, run_id: &str, cost_usd: f64) {
    // A read-modify-write under the state lock: `base` must come from the
    // file as it is now, not from a copy another writer has since replaced.
    let saved = PkgState::update(ws, |st| {
        let base = if st.cumulative_spent_run_id.as_deref() == Some(run_id) {
            st.cumulative_spent_usd
        } else {
            0.0
        };
        st.cumulative_spent_usd = base + cost_usd;
        st.cumulative_spent_run_id = Some(run_id.to_string());
    });
    if let Err(e) = saved {
        ap_core::diag!("  cannot save the notes cost: {e}");
    }
}

/// Most notes text inlined into one prompt. Older entries are dropped first, so
/// a long history costs context but never blows up every turn.
const MAX_PROMPT_NOTES_BYTES: usize = 16 * 1024;

/// The last `MAX_PROMPT_NOTES_BYTES` of `text`, cut at a line boundary.
fn tail_for_prompt(text: &str) -> String {
    if text.len() <= MAX_PROMPT_NOTES_BYTES {
        return text.to_string();
    }
    let mut start = text.len() - MAX_PROMPT_NOTES_BYTES;
    while !text.is_char_boundary(start) {
        start += 1;
    }
    let rest = match text[start..].find('\n') {
        Some(i) => &text[start + i + 1..],
        None => "",
    };
    format!("(earlier entries omitted)\n{rest}")
}

/// Persist the fix-attempt count after every increment, the same way
/// `agent_turn` persists `spent_usd` after every turn — so a crash or a
/// `MissingDeps` round trip mid-pipeline resumes with the right count
/// instead of restarting `attempts` at 0 (see `PkgState::cumulative_attempts`).
fn persist_attempts(ws: &Workspace, run_id: &str, attempts: usize) {
    let saved = PkgState::update(ws, |st| {
        st.cumulative_attempts = attempts;
        // Stamping this run's id onto a spend recorded under another run would
        // make that run's dollars count against this one. Reset it first.
        if st.cumulative_spent_run_id.as_deref() != Some(run_id) {
            st.cumulative_spent_usd = 0.0;
        }
        st.cumulative_spent_run_id = Some(run_id.to_string());
    });
    if let Err(e) = saved {
        ap_core::diag!("  cannot save the attempt count: {e}");
    }
}

/// Write an artifact or spec copy, warning with the path when it fails so the
/// run does not go on as if the file had been written.
fn write_or_warn(path: impl AsRef<Path>, contents: impl AsRef<[u8]>) {
    let path = path.as_ref();
    if let Err(e) = ap_core::fsx::write_atomic(path, contents) {
        ap_core::diag!("  cannot write {}: {e}", path.display());
    }
}

/// Report a failed bookkeeping step (state file, convenience symlinks the
/// agent reads) as a warning instead of dropping it.
fn warn_if_err<T, E: std::fmt::Display>(reporter: &dyn Reporter, what: &str, result: Result<T, E>) {
    if let Err(e) = result {
        reporter.report(&Event::Warning {
            step: None,
            message: format!("{what} failed: {e}"),
        });
    }
}

fn safe_file_part(s: &str) -> String {
    let cleaned: String = s
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '+' | '-') {
                c
            } else {
                '_'
            }
        })
        .collect();
    cleaned.replace("..", "_")
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

    ap_core::fsx::write_atomic(spec_path, &spec_text)
        .map_err(|e| GenSpecError::Other(format!("writing spec failed: {e}")))?;
    write_or_warn(step.file("spec.generated"), &spec_text);

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
        ap_core::fsx::write_atomic(spec_path, &spec_text)
            .map_err(|e| GenSpecError::Other(format!("writing spec failed: {e}")))?;
        write_or_warn(step.file("spec.generated"), &spec_text);
        return Ok(GenSpecResult {
            sdist_top_level,
            is_maturin: sdist_info.build_system.is_maturin(),
            is_rust: false,
            from_repo_archive: true,
        });
    }

    // Build a Release from the PyPI JSON metadata, substituting the forge
    // tarball name so py2pack writes the correct Source0.
    //
    // When pypi_json is empty, the project simply isn't on PyPI — this
    // function's own doc comment on `pypi_json` says that's expected
    // ("py2pack fills in what it can and the review turn supplies the
    // rest from the source"), and the design intent is to still produce
    // a usable skeleton. Previously the code tried `parse_release_with_tarball("")`
    // anyway, got `Error::Malformed`, and surfaced a hard
    // "project is not on PyPI, so py2pack has nothing to describe it
    // with" error that killed the whole pipeline run — exactly the
    // crash the user hit on `llama-cpp-scripts` (llama.cpp's root
    // pyproject stub that upstream never published). Synthesize a
    // minimal `Release` directly in that case so py2pack produces a
    // skeleton the Review agent can fill in from source.
    let release = if pypi_json.is_empty() {
        synthesize_release_without_pypi(name.clone(), version.clone(), tarball_name.clone())
    } else {
        ap_ecosystem::pypi::parse_release_with_tarball(pypi_json, &tarball_name).map_err(|e| {
            GenSpecError::Other(format!(
                "PyPI metadata for {name} {version} was returned but could not be \
                     parsed ({e}). This is distinct from the no-PyPI case (which uses \
                     the synthetic-release fallback) — a non-empty-but-malformed body \
                     usually means the index returned an error page where JSON was \
                     expected; inspect the request manually."
            ))
        })?
    };

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
            top_level_tests_dir: sdist_info.top_level_tests_dir.clone(),
        },
    );

    let spec_text =
        crate::spec::fix_autosetup_dir(&spec_text, &sdist_top_level).unwrap_or(spec_text);

    // A repository snapshot of a monorepo keeps the project one or more levels
    // down; %build and %install have to change into it.
    let spec_text = match &sdist_info.project_dir {
        Some(dir) => crate::spec::build_in_subdir(&spec_text, dir),
        None => spec_text,
    };

    let spec_text = crate::spec::strip_ap_fix_comments(&spec_text);

    ap_core::fsx::write_atomic(spec_path, &spec_text)
        .map_err(|e| GenSpecError::Other(format!("writing spec failed: {e}")))?;
    write_or_warn(step.file("spec.generated"), &spec_text);

    Ok(GenSpecResult {
        sdist_top_level,
        is_maturin: sdist_info.build_system.is_maturin(),
        is_rust: false,
        from_repo_archive: true,
    })
}

/// Convention 28 Exception 4: the agent decided THIS package's own source
/// cannot be compiled in this environment (e.g. it needs NVIDIA's `nvcc` to
/// build `.cu` device code, which OBS cannot provide) even though, unlike
/// [`generate_wheel_binary_spec`]'s callers, a real sdist exists. Re-fetches
/// the PyPI release JSON and, if it also publishes a wheel, overwrites
/// `spec_path` with the same wheel-binary skeleton the no-sdist path uses —
/// this is an independent check, not trust in the agent's claim: if PyPI has
/// no wheel either, the caller still fails the package.
///
/// Returns the picked [`ap_ecosystem::pypi::WheelFile`] so the caller can
/// write a provenance record alongside the spec. The Oct 6 review called
/// out that previously the only check was "a wheel exists" — the agent's
/// claim that the source genuinely cannot be built was never independently
/// verifiable after the fact. We cannot verify "source builds are
/// impossible" from here (that is a factual claim about an environment OBS
/// controls), but we can at least pin the exact artifact that got
/// substituted and the reasoning that justified it, so a human reviewer
/// has something to audit before the recipe is submitted.
fn try_wheel_fallback(
    config: &PipelineConfig,
    checkout_dir: &Path,
    spec_path: &Path,
    step: &StepDir,
) -> Result<ap_ecosystem::pypi::WheelFile, String> {
    let pypi_url = pypi_json_url(&config.pkg_name, &config.version);
    let json = crate::http::read_text(crate::http::get(&pypi_url).call().map_err(|e| {
        format!(
            "PyPI fetch failed for {} {}: {e}",
            config.pkg_name, config.version
        )
    })?)
    .map_err(|e| {
        format!(
            "PyPI read failed for {} {}: {e}",
            config.pkg_name, config.version
        )
    })?;
    let wheel = match ap_ecosystem::pypi::pick_wheel_from_json(
        &json,
        &config.arch,
        config.single_locked_flavor_abi().as_deref(),
    ) {
        Ok(w) => w,
        Err(e) => {
            // No single wheel serves every flavor; one per flavor may (Triton).
            return match generate_multi_flavor_wheel_spec(
                config,
                &json,
                checkout_dir,
                spec_path,
                step,
            ) {
                Some(Ok((_, first))) => Ok(first),
                Some(Err(GenSpecError::Other(m) | GenSpecError::NoReleaseArchive(m))) => {
                    Err(format!("per-flavor wheel spec generation failed: {m}"))
                }
                Some(Err(GenSpecError::WheelOnly { .. })) | None => {
                    Err(format!("no usable upstream wheel either: {e}"))
                }
            };
        }
    };
    generate_wheel_binary_spec(config, &wheel, &json, checkout_dir, spec_path, step).map_err(
        |e| match e {
            GenSpecError::Other(msg) | GenSpecError::NoReleaseArchive(msg) => {
                format!("wheel spec generation failed: {msg}")
            }
            GenSpecError::WheelOnly { .. } => {
                "wheel spec generation failed unexpectedly".to_string()
            }
        },
    )?;
    Ok(wheel)
}

/// Enforce the minimum shape of a `UseUpstreamWheel`'s `why`: non-empty, long
/// enough to carry an actual quoted build-log line or missing-toolchain
/// name, not a bare "idk" or whitespace.
///
/// Not evidence verification — the agent could still fabricate a plausible
/// string — but a cheap gate against the obvious trivial-reason case, and
/// paired with the mandatory provenance file ([`write_wheel_switch_provenance`])
/// it makes the switch auditable rather than a silent recipe swap. Returns
/// `Err` for the caller to fold into its own failure path.
///
/// The 20-char floor is the length of the shortest realistic
/// evidence line the Exception 4 wording asks for (`"nvcc: not found"` is 15,
/// `"find_package(CUDAToolkit"` is 23); it rejects "" / "no reason" / the
/// placeholder shapes without rejecting any genuine single-line citation.
fn validate_wheel_switch_why(why: &str) -> Result<(), String> {
    let trimmed = why.trim();
    if trimmed.len() < 20 {
        return Err(format!(
            "use_upstream_wheel's `why` ({trimmed:?}) is too short to be \
             audit evidence — convention 28 Exception 4 asks for the exact \
             missing compiler/SDK and a quoted build.log line that proves it"
        ));
    }
    Ok(())
}

/// Record WHAT got swapped and WHY, so a human reviewing the OBS commit
/// after the pipeline has moved on can see the switch happened at all.
///
/// Writes `wheel-origin.txt` into the checkout and preserves the original
/// spec as `<spec>.pre-wheel-switch` before the fallback overwrites it.
/// Both files travel with the OBS commit and show up in the review diff,
/// instead of the switch being discoverable only from the run's JSONL
/// stream (which the OBS submit request's reviewers never see).
///
/// Non-fatal — a provenance-write failure surfaces as a warning rather
/// than reverting an otherwise-successful wheel switch; a wheel spec that
/// built the package correctly is still better than the source spec that
/// couldn't, with or without the audit file. The warning tells the
/// operator to add the file manually.
#[allow(clippy::too_many_arguments)]
fn write_wheel_switch_provenance(
    checkout_dir: &Path,
    rpm_name: &str,
    config: &PipelineConfig,
    wheel: &ap_ecosystem::pypi::WheelFile,
    attempt: usize,
    label: &str,
    why: &str,
    original_spec: &str,
) -> std::io::Result<()> {
    let provenance = format!(
        "Switched from source build to upstream wheel.\n\
         \n\
         Package:      {rpm_name}\n\
         PyPI name:    {pkg_name}\n\
         Version:      {version}\n\
         Arch:         {arch}\n\
         Switch at:    {label}, attempt {attempt}\n\
         Pipeline:     auto-package convention 28 Exception 4\n\
         \n\
         Upstream wheel:\n\
         \x20 filename: {wheel_filename}\n\
         \x20 url:      {wheel_url}\n\
         \x20 sha256:   {wheel_sha256}\n\
         \x20 size:     {wheel_size} bytes\n\
         \n\
         Agent-reported reason (why the source cannot be compiled here):\n\
         {why}\n\
         \n\
         The pre-switch source-build spec is preserved as\n\
         `{rpm_name}.spec.pre-wheel-switch` in this checkout. Review both\n\
         before accepting the recipe; this file is the only record that\n\
         the recipe was swapped from source to binary.\n",
        pkg_name = config.pkg_name,
        version = config.version,
        arch = config.arch,
        wheel_filename = wheel.filename,
        wheel_url = wheel.url,
        wheel_sha256 = wheel.sha256,
        wheel_size = wheel.size,
    );
    ap_core::fsx::write_atomic(&checkout_dir.join("wheel-origin.txt"), provenance)?;
    ap_core::fsx::write_atomic(
        &checkout_dir.join(format!("{rpm_name}.spec.pre-wheel-switch")),
        original_spec,
    )?;
    Ok(())
}

/// Repackage a prebuilt wheel as-is, for a release with no sdist and no
/// discoverable source repository — a closed-source binary redistribution
/// (NVIDIA's individual CUDA toolkit component wheels are the running
/// example: nvcc, cupti, nvrtc, cudart have no public source at all). See
/// convention 24 Exception 3 in the agent prompt.
/// Repackage the release's upstream wheel when one is usable for every flavor;
/// otherwise report it wheel-only (with `candidates` for the operator).
fn wheel_or_wheel_only(
    config: &PipelineConfig,
    json: &str,
    checkout_dir: &Path,
    spec_path: &Path,
    step: &StepDir,
    candidates: Vec<String>,
) -> Result<GenSpecResult, GenSpecError> {
    match ap_ecosystem::pypi::pick_wheel_from_json(
        json,
        &config.arch,
        config.single_locked_flavor_abi().as_deref(),
    ) {
        Ok(wheel) => {
            generate_wheel_binary_spec(config, &wheel, json, checkout_dir, spec_path, step)
        }
        Err(_) => {
            match generate_multi_flavor_wheel_spec(config, json, checkout_dir, spec_path, step) {
                Some(result) => result.map(|(r, _)| r),
                None => Err(GenSpecError::WheelOnly {
                    pkg_name: config.pkg_name.clone(),
                    version: config.version.clone(),
                    candidates,
                }),
            }
        }
    }
}

/// Wheels each locked to one CPython (Triton, onnxruntime): when EVERY flavor
/// the project builds has its own wheel for this arch, download them all and
/// write the per-flavor spec (convention 49) instead of leaving the agent to do
/// it by hand without being able to fetch the files. `None` when it does not
/// apply (fewer than two flavors, or a flavor with no wheel).
fn generate_multi_flavor_wheel_spec(
    config: &PipelineConfig,
    json: &str,
    checkout_dir: &Path,
    spec_path: &Path,
    step: &StepDir,
) -> Option<Result<(GenSpecResult, ap_ecosystem::pypi::WheelFile), GenSpecError>> {
    let flavors = match config.python_flavors.try_effective() {
        Ok(flavors) => flavors,
        Err(e) => return Some(Err(GenSpecError::Other(e))),
    };
    if flavors.len() < 2 {
        return None;
    }
    let abis: Vec<String> = flavors
        .iter()
        .filter_map(|f| crate::flavors::abi_tag_for_flavor(f))
        .collect();
    let picked = match ap_ecosystem::pypi::pick_wheels_per_flavor(json, &config.arch, &abis) {
        Ok(picked) => picked,
        Err(e) => {
            ap_core::diag!(
                "  cannot pick per-flavor wheels for {}: {e}",
                config.pkg_name
            );
            return None;
        }
    };
    ap_core::diag!(
        "info: {} {}'s wheels are each locked to one CPython — repackaging one wheel per flavor ({})",
        config.pkg_name,
        config.version,
        flavors.join(", ")
    );
    Some((|| {
        let mut wheels: Vec<(String, String)> = Vec::new();
        let mut first_path = None;
        for (flavor, (_, wheel)) in flavors.iter().zip(&picked) {
            let path = wheel
                .fetch(checkout_dir)
                .map_err(|e| GenSpecError::Other(format!("wheel download failed: {e}")))?;
            first_path.get_or_insert(path);
            wheels.push((flavor.clone(), wheel.filename.clone()));
        }
        let release = ap_ecosystem::pypi::parse_release_with_tarball(json, &wheels[0].1)
            .map_err(|e| GenSpecError::Other(format!("PyPI parse failed: {e}")))?;
        let root_is_purelib = first_path
            .as_deref()
            .and_then(ap_ecosystem::sdist::wheel_root_is_purelib);
        let layout = first_path
            .as_deref()
            .and_then(ap_ecosystem::sdist::wheel_layout);
        let spec_text =
            crate::spec::strip_ap_fix_comments(&crate::spec::wheel_multi_flavor_skeleton(
                &config.pkg_name,
                &config.version,
                &wheels,
                &release,
                root_is_purelib,
                layout.as_ref(),
            ));
        ap_core::fsx::write_atomic(spec_path, &spec_text)
            .map_err(|e| GenSpecError::Other(format!("writing spec failed: {e}")))?;
        write_or_warn(step.file("spec.generated"), &spec_text);
        if crate::spec::is_prebuilt_binary_wheel(&wheels[0].1, layout.as_ref()) {
            let rpmlintrc_path = checkout_dir.join(format!(
                "python-{}.rpmlintrc",
                config.pkg_name.to_lowercase()
            ));
            let header_dirs = layout
                .as_ref()
                .map(|l| l.header_dirs.as_slice())
                .unwrap_or(&[]);
            write_or_warn(
                &rpmlintrc_path,
                crate::spec::prebuilt_wheel_rpmlintrc(header_dirs),
            );
        }
        let first_wheel = picked.into_iter().next().map(|(_, w)| w).ok_or_else(|| {
            GenSpecError::Other(format!(
                "no wheel picked for any flavor of {}",
                config.pkg_name
            ))
        })?;
        Ok((
            GenSpecResult {
                sdist_top_level: String::new(),
                is_maturin: false,
                is_rust: false,
                from_repo_archive: false,
            },
            first_wheel,
        ))
    })())
}

fn generate_wheel_binary_spec(
    config: &PipelineConfig,
    wheel: &ap_ecosystem::pypi::WheelFile,
    pypi_json: &str,
    checkout_dir: &Path,
    spec_path: &Path,
    step: &StepDir,
) -> Result<GenSpecResult, GenSpecError> {
    ap_core::diag!(
        "info: {} {} has no sdist and no source repository — repackaging the upstream wheel {}",
        config.pkg_name,
        config.version,
        wheel.filename,
    );

    let wheel_path = wheel
        .fetch(checkout_dir)
        .map_err(|e| GenSpecError::Other(format!("wheel download failed: {e}")))?;
    let root_is_purelib = ap_ecosystem::sdist::wheel_root_is_purelib(&wheel_path);
    let layout = ap_ecosystem::sdist::wheel_layout(&wheel_path);

    let release = ap_ecosystem::pypi::parse_release_with_tarball(pypi_json, &wheel.filename)
        .map_err(|e| GenSpecError::Other(format!("PyPI parse failed: {e}")))?;

    let spec_text = crate::spec::wheel_binary_skeleton(
        &config.pkg_name,
        &config.version,
        &wheel.filename,
        &release,
        root_is_purelib,
        layout.as_ref(),
    );
    let spec_text = crate::spec::strip_ap_fix_comments(&spec_text);
    ap_core::fsx::write_atomic(spec_path, &spec_text)
        .map_err(|e| GenSpecError::Other(format!("writing spec failed: {e}")))?;
    write_or_warn(step.file("spec.generated"), &spec_text);
    if crate::spec::is_prebuilt_binary_wheel(&wheel.filename, layout.as_ref()) {
        let rpmlintrc_path = checkout_dir.join(format!(
            "python-{}.rpmlintrc",
            config.pkg_name.to_lowercase()
        ));
        let header_dirs = layout
            .as_ref()
            .map(|l| l.header_dirs.as_slice())
            .unwrap_or(&[]);
        write_or_warn(
            &rpmlintrc_path,
            crate::spec::prebuilt_wheel_rpmlintrc(header_dirs),
        );
    }

    Ok(GenSpecResult {
        // A wheel unpacks flat — no wrapping top-level directory.
        sdist_top_level: String::new(),
        is_maturin: false,
        is_rust: false,
        from_repo_archive: false,
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

/// `BuildFailure`'s variants (`PrepFailed`/`BuildFailed`/`InstallFailed`/
/// `CheckFailed`) name *phases*, not failure *shapes* — so this has to switch
/// on `failure.phase`, not `failure.genre`. The previous version switched on
/// genre (`ShellError` always became `PrepFailed`, `PythonTraceback` always
/// became `BuildFailed`, regardless of which section actually failed) and
/// genres it didn't recognize a mapping for — `Genre::Unknown`, which is
/// exactly what a plain GCC/Clang compiler diagnostic or a linker error looks
/// like, since neither is a Python traceback or a `script: line N:` shell
/// error — fell all the way to `Unclassified` even when the phase was known
/// perfectly well (a real `%build` compiler failure in `triton`,
/// `python-hf-xet` and `python-torchcodec` all did this). Phase is already
/// the more reliable signal here: `rpm_reported_failure` gets it from rpm's
/// own `(%section)` in "Bad exit status from ... (%section)", so use it
/// directly and only fall back to genre for phases that aren't one of the
/// four rpm sections at all (rpm's own post-install file-list check often
/// gets attributed to whichever section ran last, e.g. `%license`).
fn classify_failure(extracted: &crate::log::ExtractedLog) -> ap_core::event::BuildFailure {
    use crate::log::{Genre, Phase};
    let Some(failure) = &extracted.failure else {
        return ap_core::event::BuildFailure::Unclassified {
            raw: unclassified_raw(extracted),
        };
    };
    match &failure.phase {
        Phase::Section(s) if s == "%prep" => ap_core::event::BuildFailure::PrepFailed,
        Phase::Section(s) if s == "%build" => ap_core::event::BuildFailure::BuildFailed,
        Phase::Section(s) if s == "%install" => ap_core::event::BuildFailure::InstallFailed,
        Phase::Section(s) if s == "%check" => ap_core::event::BuildFailure::CheckFailed,
        _ if failure.genre == Genre::PostBuildCheck || failure.phase == Phase::PostBuild => {
            ap_core::event::BuildFailure::InstallFailed
        }
        // Any other named rpm section — `%generate_buildrequires` (a real
        // case for pyproject-rpm-macros' dynamic BuildRequires resolution),
        // `%conf`, `%mkbuilddir`, etc. These all run before `%build` as part
        // of setting up the build, so `PrepFailed` is the closest existing
        // bucket rather than falling all the way through to `Unclassified`
        // and losing the phase signal `failure.phase` already captured.
        Phase::Section(_) => ap_core::event::BuildFailure::PrepFailed,
        // Phase::Setup: the failure landed before any spec section began —
        // buildroot resolution, workflow aborted mid-setup, or OBS never
        // produced a real log at all. Not a code-shaped failure, but still
        // worth reporting more than an empty string (which is what we used
        // to do when `error_lines` was empty — common for a build that
        // never progressed far enough to emit an rpm `error:` line, which
        // in turn looked like an extraction bug to users and buried the
        // real problem).
        _ => ap_core::event::BuildFailure::Unclassified {
            raw: unclassified_raw(extracted),
        },
    }
}

/// What to show for an `Unclassified` failure.
///
/// The old code just took `error_lines[0]`, which is empty for a log with no
/// rpm `error:` line or Python exception at all — common for OBS failures
/// that never produced a real build log (status `Broken`, `Excluded`, a
/// buildroot resolution that aborted before scriptlets started, or an osc
/// invocation that got no bytes back at all). Fall back through progressively
/// weaker signals so the TUI/CLI shows SOMETHING actionable instead of an
/// empty string that reads like a bug in extraction itself.
fn unclassified_raw(extracted: &crate::log::ExtractedLog) -> String {
    if let Some(first) = extracted.error_lines.iter().find(|l| !l.trim().is_empty()) {
        return first.clone();
    }
    // The excerpt is already filtered to interesting content when the
    // extractor found a failure signal of ANY kind, so prefer it to the raw
    // tail.
    if let Some(f) = &extracted.failure {
        let trimmed = f.excerpt.trim();
        if !trimmed.is_empty() {
            return tail_lines(trimmed, 5);
        }
    }
    let tail = extracted.log_tail.trim();
    if tail.is_empty() {
        return "(no build log — OBS likely rejected the build before any \
                scriptlet ran; check `osc results` for the status)"
            .to_string();
    }
    tail_lines(tail, 5)
}

/// Keep the last `n` non-empty lines of `text`.
fn tail_lines(text: &str, n: usize) -> String {
    let lines: Vec<&str> = text
        .lines()
        .filter(|l| !l.trim().is_empty())
        .rev()
        .take(n)
        .collect();
    lines.into_iter().rev().collect::<Vec<_>>().join("\n")
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
/// `extra_manifests` are further Cargo manifests (relative to the archive
/// root) to vendor alongside the root one — see [`maturin_manifest_path`].
fn render_maturin_service_file(normalized_name: &str, extra_manifests: &[String]) -> String {
    let extra: String = extra_manifests
        .iter()
        .filter(|m| m.as_str() != "Cargo.toml")
        .map(|m| format!("         \x20    <param name=\"cargotoml\">{m}</param>\n"))
        .collect();
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
         {extra}\
         \x20 </service>\n\
         </services>\n"
    )
}

/// The `[tool.maturin] manifest-path` of an sdist's root `pyproject.toml`,
/// relative to the archive root, when it names a manifest other than the root
/// `Cargo.toml`. `None` when absent, default, the archive cannot be read, or
/// the manifest is itself a member of the root `Cargo.toml`'s workspace: the
/// root manifest's own `cargotoml` param already vendors every workspace
/// member through their one shared lockfile, and `obs-service-cargo` errors
/// out (bare `NotFound`, os error 2) when told to additionally vendor a
/// member path, since it looks for a lockfile next to that manifest instead
/// of at the workspace root.
fn maturin_manifest_path(tarball: &Path) -> Option<String> {
    let scratch = tempfile::tempdir().ok()?;
    ap_ecosystem::sdist::extract(tarball, scratch.path()).ok()?;
    let entries: Vec<_> = std::fs::read_dir(scratch.path())
        .ok()?
        .filter_map(|e| e.ok())
        .collect();
    let root = match entries.as_slice() {
        [only] if only.path().is_dir() => only.path(),
        _ => scratch.path().to_path_buf(),
    };
    let text = std::fs::read_to_string(root.join("pyproject.toml")).ok()?;
    let doc: toml::Value = toml::from_str(&text).ok()?;
    let path = doc
        .get("tool")?
        .get("maturin")?
        .get("manifest-path")?
        .as_str()?
        .trim_start_matches("./")
        .to_string();
    if path == "Cargo.toml" || path.starts_with('/') || path.contains("..") {
        return None;
    }
    if is_cargo_workspace_member(&root.join("Cargo.toml"), &path) {
        return None;
    }
    Some(path)
}

/// Whether `manifest_rel` (relative to the archive root) names a path listed
/// in `root_cargo_toml`'s `[workspace] members` (plain entries and simple
/// `dir/*` globs — the forms Cargo itself supports for member lists).
fn is_cargo_workspace_member(root_cargo_toml: &Path, manifest_rel: &str) -> bool {
    let Some(member_dir) = Path::new(manifest_rel).parent() else {
        return false;
    };
    let Ok(text) = std::fs::read_to_string(root_cargo_toml) else {
        return false;
    };
    let Ok(doc) = toml::from_str::<toml::Value>(&text) else {
        return false;
    };
    let Some(workspace) = doc.get("workspace") else {
        return false;
    };
    let Some(members) = workspace.get("members").and_then(|m| m.as_array()) else {
        return false;
    };
    let excluded = workspace
        .get("exclude")
        .and_then(|e| e.as_array())
        .is_some_and(|a| {
            a.iter()
                .filter_map(|v| v.as_str())
                .any(|p| Path::new(p) == member_dir)
        });
    if excluded {
        return false;
    }
    members
        .iter()
        .filter_map(|m| m.as_str())
        .any(|pattern| match pattern.strip_suffix("/*") {
            Some(prefix) => member_dir.parent() == Some(Path::new(prefix)),
            None => Path::new(pattern) == member_dir,
        })
}

fn append_notes(notes_path: PathBuf, section: &str) {
    // Append, never read-modify-write: a failed read used to replace the
    // whole journal with just this section.
    use std::io::Write;
    let result = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&notes_path)
        .and_then(|mut f| f.write_all(section.as_bytes()));
    if let Err(e) = result {
        ap_core::diag!("  cannot append to {}: {e}", notes_path.display());
    }
}

/// Fix broken workspace definitions in a Cargo.toml inside a tarball.
///
/// Some packages reference workspace members (often git submodules) that
/// aren't included in GitHub's archive downloads. This removes
/// genuinely-missing members from the workspace definition so cargo vendor
/// doesn't fail.
///
/// Correctness notes from the earlier review:
///
/// * **Glob-aware membership**. Cargo's `members = ["crates/*"]` syntax is
///   a glob; literal filesystem existence of a path named `crates/*`
///   previously said "not there, remove it", collapsing a perfectly valid
///   workspace to `members = []`. `member_resolves` now expands single-
///   segment wildcards and keeps the member when any filesystem entry
///   matches the pattern.
///
/// * **Original tarball preserved**. The repack previously overwrote the
///   downloaded archive in place, destroying the pristine SHA256 identity
///   the caller had just verified. The original is now kept as
///   `<name>.orig.tar.gz` next to the modified one so provenance survives.
///
/// * **Audit trail**. When any member is removed, a `cargo-workspace-fix.
///   txt` is written alongside the tarball naming each removed member;
///   that file lands in the OBS commit so the reviewer sees what changed.
fn fix_cargo_workspace_in_tarball(tarball: &std::path::Path, pkg_name: &str) -> Result<(), String> {
    // Extract to temp directory
    let temp_dir = tempfile::tempdir().map_err(|e| e.to_string())?;
    let extract_dir = temp_dir.path();

    ap_core::diag!("  checking for broken Cargo workspace in tarball");

    // Extract tarball
    let archive_file =
        std::fs::File::open(tarball).map_err(|e| format!("opening tarball: {}", e))?;
    let mut archive = tar::Archive::new(flate2::read::GzDecoder::new(archive_file));
    archive
        .unpack(extract_dir)
        .map_err(|e| format!("extracting tarball: {}", e))?;

    // Find Cargo.toml files
    let mut fixed = false;
    let mut audit_log: Vec<(PathBuf, Vec<String>)> = Vec::new();
    for entry in walkdir::WalkDir::new(extract_dir)
        .max_depth(3)
        .into_iter()
        .filter_map(|e| e.ok())
    {
        if entry.file_name() != "Cargo.toml" {
            continue;
        }

        let cargo_toml_path = entry.path();
        let content = match std::fs::read_to_string(cargo_toml_path) {
            Ok(c) => c,
            Err(_) => continue,
        };

        // Parse as TOML
        let mut doc: toml::Table = match toml::from_str(&content) {
            Ok(d) => d,
            Err(_) => continue,
        };

        // Check if it has a workspace section
        let workspace = match doc.get_mut("workspace") {
            Some(toml::Value::Table(w)) => w,
            _ => continue,
        };

        // Get members array
        let members = match workspace.get_mut("members") {
            Some(toml::Value::Array(m)) => m,
            _ => continue,
        };

        // Check which members actually resolve. A member can be a literal
        // path (`crates/foo`) OR a single-segment glob (`crates/*`,
        // `vendor/foo-*`); `member_resolves` understands both so a glob
        // whose parent dir has at least one match stays in instead of
        // being stripped as if the literal path `crates/*` were missing.
        let base_dir = cargo_toml_path.parent().ok_or_else(|| {
            format!(
                "Cargo.toml in the {pkg_name} tarball has no parent directory: {}",
                cargo_toml_path.display()
            )
        })?;
        let mut existing_members = Vec::new();
        let mut removed_members = Vec::new();

        for member in members.iter() {
            if let Some(member_str) = member.as_str() {
                if member_resolves(base_dir, member_str) {
                    existing_members.push(member.clone());
                } else {
                    ap_core::diag!("  ⚠ removing missing workspace member: {}", member_str);
                    removed_members.push(member_str.to_string());
                    fixed = true;
                }
            }
        }

        if !removed_members.is_empty() {
            // Update the members array
            *members = existing_members;

            // Write back the modified Cargo.toml
            let new_content = toml::to_string_pretty(&doc)
                .map_err(|e| format!("serializing Cargo.toml: {}", e))?;
            ap_core::fsx::write_atomic(cargo_toml_path, new_content)
                .map_err(|e| format!("writing Cargo.toml: {}", e))?;

            ap_core::diag!(
                "  ✓ fixed Cargo.toml: removed {} missing workspace members",
                removed_members.len()
            );

            audit_log.push((
                cargo_toml_path
                    .strip_prefix(extract_dir)
                    .unwrap_or(cargo_toml_path)
                    .to_path_buf(),
                removed_members,
            ));
        }
    }

    if !fixed {
        ap_core::diag!("  ✓ no workspace issues found");
        return Ok(());
    }

    // Preserve the pristine downloaded archive before replacing it, so the
    // original SHA256-verified bytes are still auditable after the fact.
    // Non-fatal — a save failure logs a warning rather than aborting a
    // successful workspace fix.
    let orig_backup = tarball.with_file_name(format!(
        "{}.orig",
        tarball
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default()
    ));
    if let Err(e) = std::fs::copy(tarball, &orig_backup) {
        ap_core::diag!(
            "  warning: could not back up original tarball to {}: {e}",
            orig_backup.display()
        );
    }

    // Audit record of what members were stripped and from which
    // Cargo.toml. Written next to the tarball so it travels with the OBS
    // commit; without this the modification is invisible to the recipe
    // reviewer.
    let audit = {
        let mut s = String::from(
            "cargo workspace fix applied by auto-package: the following \
             workspace members were removed from in-tarball Cargo.toml \
             files because nothing in the archive resolved them (likely \
             omitted git submodules).\n\
             \n\
             Original tarball preserved next to this one as the `.orig`\n\
             file; compare for the exact byte-level diff.\n\n",
        );
        for (path, members) in &audit_log {
            s.push_str(&format!("{}:\n", path.display()));
            for m in members {
                s.push_str(&format!("  - {m}\n"));
            }
        }
        s
    };
    let audit_path = tarball.with_file_name("cargo-workspace-fix.txt");
    write_or_warn(&audit_path, audit);

    // Re-pack the tarball
    ap_core::diag!("  re-packing fixed tarball");
    let temp_tarball = tarball.with_extension("tar.gz.tmp");
    let tar_file = std::fs::File::create(&temp_tarball)
        .map_err(|e| format!("creating temp tarball: {}", e))?;
    let enc = flate2::write::GzEncoder::new(tar_file, flate2::Compression::default());
    let mut tar_builder = tar::Builder::new(enc);

    // Add all files from extract_dir
    for entry in walkdir::WalkDir::new(extract_dir) {
        // An unreadable entry must fail the rebuild: skipping it would ship a
        // tarball that silently lacks files.
        let entry = entry.map_err(|e| format!("walking {}: {e}", extract_dir.display()))?;
        let path = entry.path();
        if path == extract_dir {
            continue;
        }

        let relative = path
            .strip_prefix(extract_dir)
            .map_err(|e| format!("{}: {e}", path.display()))?;

        if path.is_file() {
            tar_builder
                .append_path_with_name(path, relative)
                .map_err(|e| format!("adding file to tarball: {}", e))?;
        } else if path.is_dir() {
            tar_builder
                .append_dir(relative, path)
                .map_err(|e| format!("adding dir to tarball: {}", e))?;
        }
    }

    tar_builder
        .finish()
        .map_err(|e| format!("finalizing tarball: {}", e))?;

    // Replace original tarball
    std::fs::rename(&temp_tarball, tarball).map_err(|e| format!("replacing tarball: {}", e))?;

    ap_core::diag!("  ✓ tarball updated with fixed workspace");
    Ok(())
}

/// Does `member` under `base_dir` resolve to anything on disk?
///
/// Cargo workspace members are either literal paths (`crates/foo`) or
/// single-segment globs (`crates/*`, `vendor/foo-*`). The literal case is
/// a direct filesystem check; the glob case reads the parent directory
/// and matches each entry name against the pattern. Returns `true` as
/// soon as any match is found.
///
/// Not a complete Cargo workspace resolver — doesn't handle `**` recursive
/// globs or Cargo's `exclude = [...]` filter — but every workspace shape
/// the running packaging corpus hits uses single-segment `*` patterns.
fn member_resolves(base_dir: &Path, member: &str) -> bool {
    let (parent_rel, pattern) = match member.rsplit_once('/') {
        Some((p, n)) => (p, n),
        None => ("", member),
    };
    let parent = if parent_rel.is_empty() {
        base_dir.to_path_buf()
    } else {
        base_dir.join(parent_rel)
    };
    if !parent.is_dir() {
        return false;
    }
    if !pattern.contains('*') {
        return parent.join(pattern).exists();
    }
    let Ok(iter) = std::fs::read_dir(&parent) else {
        return false;
    };
    iter.filter_map(|e| e.ok())
        .any(|e| glob_matches_single_segment(pattern, &e.file_name().to_string_lossy()))
}

/// Match a single-segment glob (one with `*` wildcards, no `/`) against a
/// candidate filename.
///
/// A single `*` matches any run of characters (including empty); multiple
/// `*`s split the pattern into literal pieces that must appear in order.
/// Does not implement `?`, `[...]` character classes, or `**`.
fn glob_matches_single_segment(pattern: &str, candidate: &str) -> bool {
    if !pattern.contains('*') {
        return pattern == candidate;
    }
    let mut parts = pattern.split('*');
    let first = parts.next().unwrap_or("");
    if !candidate.starts_with(first) {
        return false;
    }
    let mut rest = &candidate[first.len()..];
    let mut peekable = parts.peekable();
    while let Some(part) = peekable.next() {
        if part.is_empty() && peekable.peek().is_none() {
            return true; // trailing *
        }
        match rest.find(part) {
            Some(idx) => rest = &rest[idx + part.len()..],
            None => return false,
        }
    }
    true
}

/// Source repository candidates for a package with no (or no usable) PyPI
/// entry.
pub struct RepoCandidates {
    /// Confirmed to exist via `verify_repo_exists` — safe to build from
    /// automatically without a human/agent confirming it first.
    pub verified: Vec<String>,
    /// Unverified guesses, surfaced only for a human/agent to check manually.
    pub guessed: Vec<String>,
}

/// Find candidate source repositories for a package.
///
/// Uses GitHub API search, known package mappings, and common patterns. This
/// is the same lookup `generate_spec` uses for a PyPI wheel-only release with
/// no sdist; it is also `pub` so the CLI's dependency-discovery loop can fall
/// back to it for a name that PyPI has no record of at all (not a Python
/// package, or a Python package not published to PyPI) — see convention 27's
/// "handle any package, not just PyPI" note.
pub fn find_source_repo_candidates(pkg_name: &str) -> RepoCandidates {
    ap_core::diag!(
        "info: searching for source repository candidates for: {}",
        pkg_name
    );

    let lower = pkg_name.to_lowercase();
    let mut candidates = Vec::new();
    let mut verified_candidates = Vec::new();
    let mut distro_evidence: Option<crate::discovery::DistroInfo> = None;

    // Known package mappings for special cases
    // NOTE: nvidia-cuda-nvrtc, nvidia-cuda-runtime, nvidia-cuda-cupti and
    // nvidia-cuda-nvcc used to map here too. They don't: NVIDIA/cuda-python
    // is the *bindings* monorepo (cuda.bindings/cuda.core/cuda.pathfinder) —
    // it does not contain the closed-source nvcc compiler, cupti profiler,
    // nvrtc or cudart binaries those wheels actually redistribute. There is
    // no public source for them at all; they fall through to the
    // wheel-binary-repackaging path in `generate_spec` instead.
    let known_mappings: &[(&str, &str)] = &[
        // NVIDIA CUDA packages
        ("cuda-python", "https://github.com/NVIDIA/cuda-python"),
        ("nvmath-python", "https://github.com/NVIDIA/nvmath-python"),
        ("nvidia-ml-py", "https://github.com/NVIDIA/nvidia-ml-py"),
        ("nvidia-nvshmem-cu", "https://github.com/NVIDIA/nvshmem"),
        ("nvshmem4py", "https://github.com/NVIDIA/nvshmem"),
        ("nvidia-cuda-cccl", "https://github.com/NVIDIA/cccl"),
        (
            "nvidia-nvjitlink",
            "https://github.com/NVIDIA/nvjitlink-python",
        ),
    ];

    // Check known mappings first
    for (pattern, repo_url) in known_mappings {
        if lower.starts_with(pattern) {
            ap_core::diag!("  checking known mapping: {}", repo_url);
            if verify_repo_exists(repo_url) {
                ap_core::diag!("  ✓ verified: {}", repo_url);
                verified_candidates.push(repo_url.to_string());
            } else {
                candidates.push(repo_url.to_string());
            }
        }
    }

    // Generate candidate URLs based on package name patterns
    let normalized_name = pkg_name.replace("_", "-").to_lowercase();

    // For NVIDIA packages, try NVIDIA org URLs FIRST and with high priority
    // This ensures we prefer official NVIDIA repos over any random GitHub repos
    let is_nvidia_package =
        lower.starts_with("nvidia-") || lower.starts_with("cuda-") || lower.starts_with("nv");

    if is_nvidia_package {
        ap_core::diag!("  detected NVIDIA package, preferring github.com/NVIDIA organization");

        let base = lower
            .strip_prefix("nvidia-")
            .or_else(|| lower.strip_prefix("cuda-"))
            .unwrap_or(&lower);

        let clean = base
            .replace("-cu11", "")
            .replace("-cu12", "")
            .replace("-cu13", "")
            .replace("_", "-");

        // Try NVIDIA org with the cleaned package name
        let nvidia_url = format!("https://github.com/NVIDIA/{}", clean);
        ap_core::diag!("  checking NVIDIA org: {}", nvidia_url);
        if verify_repo_exists(&nvidia_url) {
            ap_core::diag!("  ✓ verified NVIDIA repo: {}", nvidia_url);
            verified_candidates.insert(0, nvidia_url.clone());
        } else {
            candidates.insert(0, nvidia_url);
        }

        // Also try without cuda- prefix for packages like cuda-python
        if let Some(after_cuda) = clean.strip_prefix("cuda-") {
            let nvidia_url2 = format!("https://github.com/NVIDIA/{}", after_cuda);
            ap_core::diag!(
                "  checking NVIDIA org (without cuda- prefix): {}",
                nvidia_url2
            );
            if verify_repo_exists(&nvidia_url2) {
                ap_core::diag!("  ✓ verified NVIDIA repo: {}", nvidia_url2);
                verified_candidates.insert(0, nvidia_url2.clone());
            } else {
                candidates.insert(0, nvidia_url2);
            }
        }

        // Try the exact package name under NVIDIA org
        let nvidia_exact = format!("https://github.com/NVIDIA/{}", pkg_name);
        if nvidia_exact != format!("https://github.com/NVIDIA/{}", clean) {
            ap_core::diag!("  checking NVIDIA org (exact name): {}", nvidia_exact);
            if verify_repo_exists(&nvidia_exact) {
                ap_core::diag!("  ✓ verified NVIDIA repo: {}", nvidia_exact);
                verified_candidates.insert(0, nvidia_exact.clone());
            } else {
                candidates.insert(0, nvidia_exact);
            }
        }

        // For NVIDIA packages, skip GitHub API search to avoid non-official repos
        ap_core::diag!("  skipping GitHub API search for NVIDIA package (prefer official repos)");
    } else {
        // For non-NVIDIA packages, use GitHub API search
        if let Some(urls) = search_github_repos(pkg_name) {
            for url in urls {
                if !verified_candidates.contains(&url) && !candidates.contains(&url) {
                    ap_core::diag!("  found via GitHub search: {}", url);
                    verified_candidates.push(url);
                }
            }
        }
        // Not everything lives on GitHub: ask Repology (the upstream other
        // distributions record) and GitLab/Codeberg search too. Their hits
        // are real search results, but a homepage can be stale, so they still
        // have to answer a reachability probe below.
        let (others, distro) = crate::discovery::search_other_forges(pkg_name);
        // openSUSE's own spec points at a non-forge release directory
        // (Savannah, ftp.gnu.org): if it lists real releases it is the
        // distro-confirmed upstream, ahead of anything a name search found.
        for dir in &distro.index_candidates {
            if let Ok(url) = crate::discovery::validate_found_url(dir)
                && !verified_candidates.contains(&url)
            {
                ap_core::diag!("  ✓ openSUSE's spec records release directory: {url}");
                verified_candidates.insert(0, url);
            }
        }
        for url in &others {
            if !verified_candidates.contains(url) && !candidates.contains(url) {
                ap_core::diag!("  found via forge search: {}", url);
                candidates.push(url.clone());
            }
        }
        distro_evidence = Some(distro);
    }

    // Try common organization patterns for other vendor-prefixed packages
    if lower.starts_with("nvidia-") || lower.starts_with("cuda-") {
        // Already handled above with higher priority
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
    } else if lower.starts_with("azure-")
        && let Some(after) = lower.strip_prefix("azure-")
    {
        candidates.push(format!("https://github.com/Azure/{}", after));
    }

    // Try the exact package name with common Python project organizations
    for org in &["python", "pypa", "psf"] {
        candidates.push(format!("https://github.com/{}/{}", org, normalized_name));
    }

    // Try just the package name under various popular organizations
    candidates.push(format!(
        "https://github.com/{}/{}",
        normalized_name, normalized_name
    ));

    // Also try common name variations
    let name_parts: Vec<&str> = normalized_name.split('-').collect();
    if name_parts.len() > 1 {
        // Try first part as org, rest as repo (e.g. "foo-bar" -> foo/bar)
        candidates.push(format!(
            "https://github.com/{}/{}",
            name_parts[0],
            name_parts[1..].join("-")
        ));
    }

    // Try the package name directly
    candidates.push(format!("https://github.com/{}", normalized_name));

    // Check all generated candidates
    for url in &candidates {
        if !verified_candidates.contains(url) {
            ap_core::diag!("  checking: {}", url);
            // An owner/org/user page (`github.com/oatpp`) is reachable but is
            // not a repository: there is nothing in it to package.
            if crate::discovery::is_repository_url(url) && verify_repo_exists(url) {
                ap_core::diag!("  ✓ verified: {}", url);
                verified_candidates.push(url.clone());
            }
        }
    }

    // Repology-confirmed upstreams outrank search hits that merely share the
    // name; the sort is stable, so everything else keeps its order.
    if let Some(distro) = &distro_evidence {
        verified_candidates
            .sort_by_key(|c| !distro.forge_repos.iter().any(|r| c.eq_ignore_ascii_case(r)));
    }

    if verified_candidates.is_empty() {
        ap_core::diag!(
            "  ✗ no verified repositories found after trying {} URLs",
            candidates.len()
        );
    }

    // Last resort, after every deterministic source: let the agent search the
    // web (Savannah, SourceForge, project sites, renamed projects) when there
    // is nothing, or when what there is contradicts what other distributions
    // record (a name collision — see `candidates_unconfirmed`). Its answer is
    // validated before it is used, and goes first.
    let unconfirmed = distro_evidence
        .as_ref()
        .is_some_and(|r| crate::discovery::candidates_unconfirmed(&verified_candidates, r));
    if verified_candidates.is_empty() || unconfirmed {
        let hints = crate::discovery::Hints {
            homepages: distro_evidence
                .as_ref()
                .map(|r| r.homepages.clone())
                .unwrap_or_default(),
            candidates: verified_candidates.clone(),
        };
        if let Some(url) = crate::discovery::agent_find_source(pkg_name, &hints) {
            verified_candidates.retain(|c| c != &url);
            verified_candidates.insert(0, url);
        }
    }
    if !verified_candidates.is_empty() {
        ap_core::diag!(
            "  ✓ found {} verified candidates",
            verified_candidates.len()
        );
    }
    RepoCandidates {
        verified: verified_candidates,
        guessed: candidates,
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

    ap_core::diag!("  searching GitHub API for: {}", normalized);

    // Route through `sources::get_json`, which prefers the `gh` CLI (so
    // the user's `gh auth login` auth and gh's own rate-limit backoff
    // both apply) and only falls back to raw ureq + GITHUB_TOKEN when
    // `gh` isn't installed. See `sources::try_gh_api`'s doc comment.
    let json = match crate::sources::get_json_for_search(&search_url) {
        Ok(json) => json,
        Err(e) => {
            ap_core::diag!("  GitHub search for {normalized} failed: {e}");
            return None;
        }
    };

    let items = json.get("items")?.as_array()?;

    // GitHub returns the items sorted by stars DESC thanks to the
    // `sort=stars&order=desc` query. Preserve that order by appending to
    // two separate buckets — exact matches first, close matches after —
    // and concatenating at the end. The pre-fix code did `matches.insert
    // (0, ...)` for exact matches, which REVERSED their order: three
    // repos exactly named "trantor" (an-tao/trantor, trantor-library
    // /trantor, mrc-pop/trantor) came in from gh in star order (an-tao
    // first), and the insert-at-zero flipped them so mrc-pop ended up
    // first in the returned list. Downstream picked that first one, it
    // had zero tags/releases, and the whole dep got marked unbuildable.
    let mut exact_matches = Vec::new();
    let mut close_matches = Vec::new();

    for item in items.iter().take(10) {
        let repo_name = item.get("name")?.as_str()?;
        let html_url = item.get("html_url")?.as_str()?;

        let repo_normalized = repo_name.replace("_", "-").to_lowercase();
        let pkg_normalized = normalized.to_lowercase();

        if repo_normalized == pkg_normalized {
            exact_matches.push(html_url.to_string());
        } else if repo_normalized == pkg_normalized.replace("-", "")
            || repo_normalized.ends_with(&format!("-{}", pkg_normalized))
            || repo_normalized.starts_with(&format!("{}-", pkg_normalized))
        {
            close_matches.push(html_url.to_string());
        }
    }

    let mut matches = exact_matches;
    matches.extend(close_matches);

    if !matches.is_empty() {
        ap_core::diag!("  found {} matching repos via GitHub API", matches.len());
        Some(matches)
    } else {
        None
    }
}

/// Verify that a repository URL exists by making a HEAD request.
fn verify_repo_exists(url: &str) -> bool {
    crate::http::head(url)
        .call()
        .map(|r| r.status() == 200)
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_path_component_rejects_unsafe_names() {
        for bad in ["", ".", "..", "a/b", "../etc", ".hidden", "x..y", "/abs"] {
            assert!(
                validate_path_component("name", bad).is_err(),
                "{bad:?} should be rejected"
            );
        }
        for good in ["python-requests", "openSUSE:Factory", "home:user_1.2"] {
            assert!(
                validate_path_component("name", good).is_ok(),
                "{good:?} should be accepted"
            );
        }
    }

    #[test]
    fn pkg_state_survives_a_round_trip_and_leaves_no_temp_file() {
        let root = tempfile::tempdir().unwrap();
        let ws = Workspace::open(root.path(), "python-foo").unwrap();
        let st = PkgState {
            last_built_version: Some("1.2.3".into()),
            last_outcome: Some("success".into()),
            ..Default::default()
        };
        st.save(&ws).unwrap();
        let loaded = PkgState::load(&ws);
        assert_eq!(loaded.last_built_version.as_deref(), Some("1.2.3"));
        assert!(!ws.state_json().with_extension("json.tmp").exists());
    }

    /// Two writers each doing 50 read-modify-writes must not lose any: without
    /// the lock, both load the same count and the later save erases the other.
    #[test]
    fn concurrent_updates_do_not_lose_writes() {
        let root = tempfile::tempdir().unwrap();
        let handles: Vec<_> = (0..2)
            .map(|_| {
                let path = root.path().to_path_buf();
                std::thread::spawn(move || {
                    let ws = Workspace::open(&path, "python-foo").unwrap();
                    for _ in 0..50 {
                        PkgState::update(&ws, |st| st.cumulative_attempts += 1).unwrap();
                    }
                })
            })
            .collect();
        for h in handles {
            h.join().unwrap();
        }
        let ws = Workspace::open(root.path(), "python-foo").unwrap();
        assert_eq!(PkgState::load(&ws).cumulative_attempts, 100);
    }

    /// A truncated/corrupt state.json used to be silently replaced by the
    /// default on the next save, erasing the package's history. It must be
    /// kept aside instead.
    #[test]
    fn a_corrupt_state_file_is_preserved_not_overwritten() {
        let root = tempfile::tempdir().unwrap();
        let ws = Workspace::open(root.path(), "python-foo").unwrap();
        std::fs::write(ws.state_json(), "{ \"last_built_version\": \"1.0\", ").unwrap();
        let loaded = PkgState::load(&ws);
        assert!(loaded.last_built_version.is_none());
        let kept = ws.state_json().with_extension("json.bad");
        assert!(std::fs::read_to_string(kept).unwrap().contains("1.0"));
    }

    /// A state.json that cannot be read (here: it is a directory) is not a
    /// corrupt file. It must not be moved aside as if it were, and the load
    /// still falls back to the default instead of failing the run.
    #[test]
    fn an_unreadable_state_file_is_left_in_place() {
        let root = tempfile::tempdir().unwrap();
        let ws = Workspace::open(root.path(), "python-foo").unwrap();
        std::fs::create_dir(ws.state_json()).unwrap();
        let loaded = PkgState::load(&ws);
        assert!(loaded.last_built_version.is_none());
        assert!(ws.state_json().is_dir());
        assert!(!ws.state_json().with_extension("json.bad").exists());
    }

    /// A failed save must say which file it could not write.
    #[test]
    fn a_failed_state_save_names_the_file() {
        let root = tempfile::tempdir().unwrap();
        let ws = Workspace::open(root.path(), "python-foo").unwrap();
        std::fs::remove_dir_all(ws.dir()).unwrap();
        let err = PkgState::default().save(&ws).unwrap_err();
        assert!(err.to_string().contains("state.json"), "{err}");
    }

    #[test]
    fn a_missing_state_file_is_just_the_default() {
        let root = tempfile::tempdir().unwrap();
        let ws = Workspace::open(root.path(), "python-foo").unwrap();
        assert!(PkgState::load(&ws).last_built_version.is_none());
        assert!(!ws.state_json().with_extension("json.bad").exists());
    }

    #[test]
    fn agent_turns_are_uncapped_unless_asked() {
        // SAFETY: tests in this module that touch this variable run serially
        // through the lock below.
        static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
        unsafe { std::env::remove_var("AUTO_PACKAGE_AGENT_JOBS") };
        assert_eq!(AgentSlot::limit(), None);
        for junk in ["0", "-1", "many", ""] {
            unsafe { std::env::set_var("AUTO_PACKAGE_AGENT_JOBS", junk) };
            assert_eq!(AgentSlot::limit(), None, "{junk:?}");
        }
        unsafe { std::env::set_var("AUTO_PACKAGE_AGENT_JOBS", "7") };
        assert_eq!(AgentSlot::limit(), Some(7));
        unsafe { std::env::remove_var("AUTO_PACKAGE_AGENT_JOBS") };
    }

    /// The service-run gate trusts `missing_source_files`: a URL Source is judged
    /// by the file name it would download to.
    #[test]
    fn a_url_source_is_present_once_its_file_is_on_disk() {
        let dir = tempfile::tempdir().unwrap();
        let spec = "Source0: https://files.pythonhosted.org/packages/ab/cd/x-1.0-cp313-cp313-manylinux_2_27_x86_64.whl\n";
        assert_eq!(
            missing_source_files(spec, dir.path()),
            vec!["x-1.0-cp313-cp313-manylinux_2_27_x86_64.whl"]
        );
        std::fs::write(
            dir.path()
                .join("x-1.0-cp313-cp313-manylinux_2_27_x86_64.whl"),
            b"",
        )
        .unwrap();
        assert!(missing_source_files(spec, dir.path()).is_empty());
    }

    #[test]
    fn maturin_service_vendors_the_crate_maturin_builds() {
        let plain = render_maturin_service_file("outlines_core", &[]);
        assert_eq!(plain.matches("name=\"cargotoml\"").count(), 1);
        let xml = render_maturin_service_file("hf_xet", &["hf_xet/Cargo.toml".to_string()]);
        assert!(
            xml.contains("<param name=\"cargotoml\">Cargo.toml</param>"),
            "{xml}"
        );
        assert!(
            xml.contains("<param name=\"cargotoml\">hf_xet/Cargo.toml</param>"),
            "{xml}"
        );
    }

    #[test]
    fn maturin_manifest_path_is_read_from_pyproject() {
        let dir = tempfile::tempdir().unwrap();
        let tarball = dir.path().join("hf_xet-1.7.0.tar.gz");
        {
            let enc = flate2::write::GzEncoder::new(
                std::fs::File::create(&tarball).unwrap(),
                flate2::Compression::default(),
            );
            let mut tb = tar::Builder::new(enc);
            let data = b"[build-system]\nrequires=[\"maturin\"]\nbuild-backend=\"maturin\"\n[tool.maturin]\nmanifest-path = \"hf_xet/Cargo.toml\"\n";
            let mut h = tar::Header::new_gnu();
            h.set_size(data.len() as u64);
            h.set_mode(0o644);
            h.set_cksum();
            tb.append_data(&mut h, "hf_xet-1.7.0/pyproject.toml", &data[..])
                .unwrap();
            tb.into_inner().unwrap().finish().unwrap();
        }
        assert_eq!(
            maturin_manifest_path(&tarball).as_deref(),
            Some("hf_xet/Cargo.toml")
        );
    }

    /// `chromadb`'s `pyproject.toml` points maturin at `rust/python_bindings/Cargo.toml`,
    /// but that crate is a member of the workspace rooted at the archive's own
    /// `Cargo.toml` — vendoring it as a second, separate `cargotoml` param makes
    /// `obs-service-cargo` fail with a bare `NotFound` looking for a lockfile
    /// next to the member instead of at the workspace root it actually lives in.
    #[test]
    fn maturin_manifest_path_skips_a_root_workspace_member() {
        let dir = tempfile::tempdir().unwrap();
        let tarball = dir.path().join("chromadb-1.5.9.tar.gz");
        {
            let enc = flate2::write::GzEncoder::new(
                std::fs::File::create(&tarball).unwrap(),
                flate2::Compression::default(),
            );
            let mut tb = tar::Builder::new(enc);
            let mut add = |name: &str, data: &[u8]| {
                let mut h = tar::Header::new_gnu();
                h.set_size(data.len() as u64);
                h.set_mode(0o644);
                h.set_cksum();
                tb.append_data(&mut h, name, data).unwrap();
            };
            add(
                "chromadb-1.5.9/pyproject.toml",
                b"[build-system]\nrequires=[\"maturin\"]\nbuild-backend=\"maturin\"\n[tool.maturin]\nmanifest-path = \"rust/python_bindings/Cargo.toml\"\n",
            );
            add(
                "chromadb-1.5.9/Cargo.toml",
                b"[workspace]\nmembers = [\"rust/python_bindings\", \"rust/types\"]\n",
            );
            tb.into_inner().unwrap().finish().unwrap();
        }
        assert_eq!(maturin_manifest_path(&tarball), None);
    }

    #[test]
    fn a_malformed_hand_written_patch_is_caught_before_the_build() {
        use std::io::Write;
        let dir = tempfile::tempdir().unwrap();
        // pristine tarball with one file
        let tarball = dir.path().join("pkg-1.0.tar.gz");
        {
            let enc = flate2::write::GzEncoder::new(
                std::fs::File::create(&tarball).unwrap(),
                flate2::Compression::default(),
            );
            let mut tb = tar::Builder::new(enc);
            let data = b"a\nb\nc\n";
            let mut h = tar::Header::new_gnu();
            h.set_size(data.len() as u64);
            h.set_mode(0o644);
            h.set_cksum();
            tb.append_data(&mut h, "pkg-1.0/f.txt", &data[..]).unwrap();
            tb.into_inner().unwrap().finish().unwrap();
        }
        let spec = "%autosetup -p1 -n pkg-%{version}\nPatch0:         fix.patch\n";
        let spec_path = dir.path().join("pkg.spec");
        std::fs::write(&spec_path, spec).unwrap();

        // hunk header says 5 lines, body has 3: the shape hand-written patches get wrong
        std::fs::write(
            dir.path().join("fix.patch"),
            "--- a/f.txt\n+++ b/f.txt\n@@ -1,5 +1,5 @@\n a\n-b\n+B\n c\n",
        )
        .unwrap();
        assert!(check_patches_apply(&spec_path, spec).is_err());

        let mut good = std::fs::File::create(dir.path().join("fix.patch")).unwrap();
        write!(
            good,
            "--- a/f.txt\n+++ b/f.txt\n@@ -1,3 +1,3 @@\n a\n-b\n+B\n c\n"
        )
        .unwrap();
        drop(good);
        assert!(check_patches_apply(&spec_path, spec).is_ok());

        // nothing to check without patches
        assert!(check_patches_apply(&spec_path, "%autosetup -p1\n").is_ok());
    }

    #[test]
    fn safe_file_part_cannot_escape_a_directory() {
        for bad in ["a/b", "../x", "/abs", "a b", "v1\n"] {
            let out = safe_file_part(bad);
            assert!(
                !out.contains('/') && !out.contains(".."),
                "{bad:?} -> {out:?}"
            );
        }
        assert_eq!(safe_file_part("Foo_bar-1.2+git"), "Foo_bar-1.2+git");
    }

    #[test]
    fn pypi_json_url_encodes_both_parts() {
        assert_eq!(
            pypi_json_url("a/b", "1 0"),
            "https://pypi.org/pypi/a%2Fb/1%200/json"
        );
    }

    #[test]
    fn append_notes_appends_and_never_truncates() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("notes.md");
        append_notes(p.clone(), "one\n");
        append_notes(p.clone(), "two\n");
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "one\ntwo\n");
    }

    /// The synthetic-release fallback for "project genuinely not on
    /// PyPI": fills in only what we know (name, version, local tarball
    /// filename) and leaves summary/license/homepage/requires_python
    /// blank for the Review agent to fill in from the pyproject.toml
    /// during the first fix turn. If any of these fields silently gets
    /// a non-empty default — a placeholder "FIXME" summary or a bogus
    /// "SPDX-NOASSERTION" license — the generated spec would ship
    /// garbage upstream, so this test locks each field explicitly.
    #[test]
    fn synthesize_release_without_pypi_leaves_every_unknown_field_blank() {
        let r = synthesize_release_without_pypi(
            "llama-cpp-scripts".to_string(),
            "0.6.0".to_string(),
            "llama-cpp-scripts-0.6.0.tar.gz".to_string(),
        );
        assert_eq!(r.name, "llama-cpp-scripts");
        assert_eq!(r.version, "0.6.0");
        assert_eq!(r.sdist.filename, "llama-cpp-scripts-0.6.0.tar.gz");
        assert_eq!(r.summary, None);
        assert_eq!(r.homepage, None);
        assert_eq!(r.license, ap_ecosystem::pypi::License::Unknown);
        assert_eq!(r.requires_python, None);
        assert!(!r.yanked);
        assert!(r.sdist.url.is_empty());
        assert!(r.sdist.sha256.is_empty());
        assert_eq!(r.sdist.size, 0);
    }

    fn fake_node(name: &str) -> ap_ecosystem::pip::Node {
        ap_ecosystem::pip::Node {
            name: name.to_string(),
            version: "1.0.0".to_string(),
            sha256: String::new(),
            requires_dist: vec![],
            requires_python: None,
            is_requested: false,
            is_yanked: false,
        }
    }

    /// `last_failure_reason` is a new field: old state.json files on disk
    /// won't have it, and must still deserialize cleanly (otherwise anyone
    /// upgrading past this change would lose their whole package state).
    /// It must also round-trip, so the next run reads back exactly what the
    /// crashing run wrote.
    /// User explicitly asked: when restarting (new CLI invocation),
    /// per-package spend resets to \$0. The field is scoped by
    /// `cumulative_spent_run_id`; a mismatch between the stored run_id
    /// and the current one means the stored spend belonged to a
    /// previous invocation and must be ignored.
    #[test]
    fn cumulative_spend_only_resumes_within_the_same_run_id() {
        let old = PkgState {
            last_built_version: Some("1.2.3".into()),
            last_outcome: Some("failed".into()),
            last_failure_reason: None,
            reviewed_version: None,
            reviewed_spec_hash: None,
            cumulative_spent_usd: 1.75,
            cumulative_spent_run_id: Some("20260107-120000-abcdef".into()),
            ..Default::default()
        };
        // Serde round-trip to confirm persistence.
        let json = serde_json::to_string(&old).unwrap();
        let back: PkgState = serde_json::from_str(&json).unwrap();

        // Same run_id → spend resumes.
        let same_run = Some("20260107-120000-abcdef");
        let resume = if back.cumulative_spent_run_id.as_deref() == same_run {
            back.cumulative_spent_usd
        } else {
            0.0
        };
        assert_eq!(resume, 1.75, "same-run retry resumes the running total");

        // New CLI invocation → different run_id → reset to 0.
        let fresh_run = Some("20260108-000000-ff00ff");
        let resume_after_restart = if back.cumulative_spent_run_id.as_deref() == fresh_run {
            back.cumulative_spent_usd
        } else {
            0.0
        };
        assert_eq!(
            resume_after_restart, 0.0,
            "restart (different run_id) starts the package at \\$0 — \
             what the user asked for explicitly"
        );
    }

    /// Pre-fix state.json files from before this change don't have the
    /// `cumulative_spent_run_id` field at all. Serde-default to None, and
    /// a None stored run_id vs any current run_id is a mismatch — so
    /// those packages also start at \$0 on the first run that reads them.
    #[test]
    fn pre_restart_fix_state_defaults_to_zero_spend_on_next_run() {
        let legacy = r#"{"last_built_version":"1.0","last_outcome":"failed",
                        "cumulative_spent_usd": 2.50}"#;
        let st: PkgState = serde_json::from_str(legacy).expect("legacy state must parse");
        assert_eq!(st.cumulative_spent_usd, 2.50);
        assert!(st.cumulative_spent_run_id.is_none());
        // The resume check: None vs Some(run_id) ⇒ mismatch ⇒ 0.
        let resume = if st.cumulative_spent_run_id.as_deref() == Some("any-run") {
            st.cumulative_spent_usd
        } else {
            0.0
        };
        assert_eq!(resume, 0.0);
    }

    /// Earlier review probe: `wheel-only` state counted as built. But the
    /// generation branch that writes `wheel-only` also returns
    /// `PipelineResult::Failed` — treating the recorded tag as success
    /// made the pipeline-failed package look resolved to the next
    /// `select_target` call, silently blocking every dependent build
    /// forever.
    #[test]
    fn wheel_only_outcome_is_not_a_built_state() {
        let state = PkgState {
            last_built_version: Some("1.2.3".into()),
            last_outcome: Some("wheel-only".into()),
            last_failure_reason: None,
            reviewed_version: None,
            reviewed_spec_hash: None,
            cumulative_spent_usd: 0.0,
            cumulative_spent_run_id: None,
            ..Default::default()
        };
        assert!(
            !state.is_built("1.2.3"),
            "wheel-only is a terminal failure marker, not a success"
        );
    }

    #[test]
    fn only_success_is_a_built_state() {
        let make = |outcome: &str| PkgState {
            policy_version: POLICY_VERSION,
            last_built_version: Some("1.0".into()),
            last_outcome: Some(outcome.into()),
            last_failure_reason: None,
            reviewed_version: None,
            reviewed_spec_hash: None,
            cumulative_spent_usd: 0.0,
            cumulative_spent_run_id: None,
            ..Default::default()
        };
        assert!(make("success").is_built("1.0"));
        assert!(!make("failed").is_built("1.0"));
        assert!(!make("wheel-only").is_built("1.0"));
        assert!(!make("something-else").is_built("1.0"));
    }

    #[test]
    fn pkg_state_round_trips_and_tolerates_the_missing_failure_reason_field() {
        let legacy = r#"{"last_built_version":"1.2.3","last_outcome":"success"}"#;
        let s: PkgState = serde_json::from_str(legacy)
            .expect("legacy state.json without last_failure_reason must still parse");
        assert_eq!(s.last_built_version.as_deref(), Some("1.2.3"));
        assert_eq!(s.last_outcome.as_deref(), Some("success"));
        assert!(s.last_failure_reason.is_none());

        let fresh = PkgState {
            last_built_version: Some("1.2.3".into()),
            last_outcome: Some("failed".into()),
            last_failure_reason: Some("agent error: claude exited 1".into()),
            reviewed_version: None,
            reviewed_spec_hash: None,
            cumulative_spent_usd: 0.0,
            cumulative_spent_run_id: None,
            ..Default::default()
        };
        let json = serde_json::to_string(&fresh).unwrap();
        let back: PkgState = serde_json::from_str(&json).unwrap();
        assert_eq!(
            back.last_failure_reason.as_deref(),
            Some("agent error: claude exited 1")
        );
        assert!(
            !back.is_built("1.2.3"),
            "`failed` must never satisfy is_built"
        );
    }

    /// `Succeeded` is the only hard "available right now" status; the three
    /// transient OBS states (`Blocked`/`Building`/`Scheduled`) are kept in to
    /// avoid stampeding work OBS is already doing. Everything else — the
    /// explicit terminal failures, user-disabled/excluded sources, and the
    /// catch-all `Unknown` — is not available for a dependent build to link
    /// against and must not be treated as resolvable.
    #[test]
    fn only_succeeded_and_in_flight_statuses_count_as_resolvable() {
        for ok in [
            crate::osc::BuildStatus::Succeeded,
            crate::osc::BuildStatus::Blocked,
            crate::osc::BuildStatus::Building,
            crate::osc::BuildStatus::Scheduled,
        ] {
            assert!(
                status_is_resolvable(Some(&ok)),
                "{ok:?} should still count as resolvable"
            );
        }
        for not_ok in [
            crate::osc::BuildStatus::Failed,
            crate::osc::BuildStatus::Unresolvable,
            crate::osc::BuildStatus::Broken,
            crate::osc::BuildStatus::Disabled,
            crate::osc::BuildStatus::Excluded,
            crate::osc::BuildStatus::Unknown("x".into()),
        ] {
            assert!(
                !status_is_resolvable(Some(&not_ok)),
                "{not_ok:?} must not count as resolvable \
                 (Oct 6 finding #1: this was silently green)"
            );
        }
    }

    /// A package that is in the OBS project source listing but has no build
    /// status entry at all for the requested distro/arch previously counted
    /// as resolvable (the `!matches!(... Some(Failed|Unresolvable|Broken))`
    /// returned true on `None`), making every "I added a source but haven't
    /// ever built it for this arch" package look available to its
    /// dependents. Must now be treated as unresolved.
    #[test]
    fn a_package_with_no_build_status_entry_is_not_resolvable() {
        assert!(!status_is_resolvable(None));
    }

    /// Oct 6 review finding #4: a `UseUpstreamWheel` with a bare / empty
    /// `why` previously swapped the recipe silently. Enforce a minimum
    /// shape so the audit record has at least a usable sentence in it.
    #[test]
    fn use_upstream_wheel_rejects_a_trivially_short_why() {
        for empty in ["", " ", "idk", "no reason", "needs nvcc"] {
            assert!(
                validate_wheel_switch_why(empty).is_err(),
                "{empty:?} must be rejected as audit evidence"
            );
        }
    }

    /// A real-shape `why` matching the convention 28 Exception 4 example
    /// (quoted build.log line + named toolchain) must pass.
    #[test]
    fn use_upstream_wheel_accepts_a_real_shape_why() {
        for ok in [
            "nvcc: command not found at build.log:847, needed by find_package(CUDAToolkit REQUIRED)",
            "The source needs NVIDIA's proprietary nvcc toolchain; find_package(CUDAToolkit REQUIRED) failed",
        ] {
            assert!(
                validate_wheel_switch_why(ok).is_ok(),
                "{ok:?} should be accepted — this is the exact evidence shape \
                 convention 28 Exception 4 asks for"
            );
        }
    }

    #[test]
    fn explicit_target_matches_regardless_of_dot_vs_hyphen_spelling() {
        // The real incident this guards against: the closure holds the node
        // under its PyPI name `backports.zstd`, but the already-committed
        // OBS package is `python-backports-zstd` (hyphenated per convention
        // 2). A caller passing either `backports-zstd` or
        // `python-backports.zstd` must land on the same node instead of
        // `find_explicit_target` reporting no match and the caller going on
        // to check out a brand-new, wrongly-named package.
        let nodes = vec![fake_node("backports.zstd"), fake_node("other-pkg")];

        for spelling in [
            "backports.zstd",
            "backports-zstd",
            "python-backports-zstd",
            "BACKPORTS_ZSTD",
        ] {
            let found = find_explicit_target(&nodes, spelling)
                .unwrap_or_else(|e| panic!("expected a match for {spelling:?}: {e}"));
            assert_eq!(found.name, "backports.zstd");
        }
    }

    #[test]
    fn explicit_target_error_lists_available_names() {
        let nodes = vec![fake_node("torchcodec"), fake_node("backports.zstd")];
        let err = find_explicit_target(&nodes, "nonexistent").unwrap_err();
        assert!(
            err.contains("torchcodec"),
            "error should list available names: {err}"
        );
        assert!(
            err.contains("backports.zstd"),
            "error should list available names: {err}"
        );
    }

    /// Real shape from `triton`/`python-hf-xet`/`python-torchcodec`: a plain
    /// GCC compiler diagnostic in `%build` is neither a Python traceback nor
    /// a `script: line N:` shell error, so its genre is `Unknown` — but the
    /// phase is unambiguous (`error: Bad exit status from ... (%build)`
    /// names it), and this used to get thrown away by switching on genre
    /// instead of phase, reporting `Unclassified` for a perfectly ordinary
    /// `%build` failure.
    #[test]
    fn a_compiler_error_in_build_is_build_failed_not_unclassified() {
        let raw = "\
[    1s] Executing(%build): /usr/bin/bash -e /var/tmp/rpm-tmp.abc123
[    2s] + cmake --build .
[   64s] /build/triton-0.9/src/utils.cpp:39:34: error: 'PyLong_AsUnsignedLongLongAndOverflow' was not declared in this scope
[   64s] make[1]: *** [Makefile:149: all] Error 2
[   64s] error: Bad exit status from /var/tmp/rpm-tmp.abc123 (%build)
";
        let mapper = crate::log::PathMapper::with_sdist_root("triton-0.9");
        let extracted = crate::log::extract(raw, &mapper);
        assert_eq!(
            extracted.failure.as_ref().map(|f| f.genre),
            Some(crate::log::Genre::Unknown),
            "fixture must reproduce the Unknown-genre shape this test guards against"
        );
        match classify_failure(&extracted) {
            ap_core::event::BuildFailure::BuildFailed => {}
            other => panic!("expected BuildFailed, got {other:?}"),
        }
    }

    /// Real shape from `drogon`: OBS reported the build as Failed but no
    /// build.log was ever produced (the worker aborted during buildroot
    /// resolution). `error_lines` is empty, so the old `error_lines.first()`
    /// pick returned an empty `raw` — the TUI then just showed
    /// "classification: Unclassified { raw: '' }" with no clue what happened.
    /// Now we fall back to the log tail (or an explicit "no build log"
    /// placeholder) so the operator sees something they can act on.
    #[test]
    fn unclassified_falls_back_to_log_tail_when_no_error_lines() {
        // Shaped like an aborted buildroot resolution: lots of chatter, no
        // `error:` line, no Python traceback, no `Bad exit status`.
        let raw = "\
[    0s] expanding package dependencies for openSUSE_Tumbleweed/x86_64 ...
[    1s] cannot find libtrantor-devel in any configured repository
[    1s] worker aborted
";
        let mapper = crate::log::PathMapper::with_sdist_root("drogon-1.9.13");
        let extracted = crate::log::extract(raw, &mapper);
        assert!(
            extracted.error_lines.is_empty(),
            "fixture must reproduce the no-error-lines shape"
        );
        match classify_failure(&extracted) {
            ap_core::event::BuildFailure::Unclassified { raw } => {
                assert!(!raw.is_empty(), "raw should not fall back to empty");
                assert!(
                    raw.contains("libtrantor-devel") || raw.contains("worker aborted"),
                    "raw should carry a line from the log tail; got {raw:?}"
                );
            }
            other => panic!("expected Unclassified, got {other:?}"),
        }
    }

    #[test]
    fn unclassified_reports_the_missing_log_explicitly_when_log_is_empty() {
        let mapper = crate::log::PathMapper::with_sdist_root("foo-1.0");
        let extracted = crate::log::extract("", &mapper);
        match classify_failure(&extracted) {
            ap_core::event::BuildFailure::Unclassified { raw } => {
                assert!(
                    raw.contains("no build log"),
                    "empty-log raw should name the condition, got {raw:?}"
                );
            }
            other => panic!("expected Unclassified, got {other:?}"),
        }
    }

    /// A phase other than the 4 explicitly named ones (`%generate_buildrequires`
    /// is a real case for pyproject-rpm-macros' dynamic BuildRequires
    /// resolution) used to fall all the way through to `Unclassified`,
    /// discarding the phase `failure.phase` already captured.
    #[test]
    fn an_unnamed_section_failure_is_prep_failed_not_unclassified() {
        let raw = "\
[    1s] Executing(%generate_buildrequires): /usr/bin/bash -e /var/tmp/rpm-tmp.abc123
[    2s] + pyproject_buildrequires
[    3s] ModuleNotFoundError: No module named 'setuptools_scm'
[    3s] error: Bad exit status from /var/tmp/rpm-tmp.abc123 (%generate_buildrequires)
";
        let mapper = crate::log::PathMapper::with_sdist_root("foo-1.0");
        let extracted = crate::log::extract(raw, &mapper);
        match classify_failure(&extracted) {
            ap_core::event::BuildFailure::PrepFailed => {}
            other => panic!("expected PrepFailed, got {other:?}"),
        }
    }

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
        // `normalize_pkg_name` (== `osc::fuzzy_key`) strips separators
        // entirely rather than picking one canonical spelling — it's a
        // comparison-only key, never displayed or built from.
        assert_eq!(
            dep_names_from_requires_dist(&reqs),
            vec!["requests", "typingextensions"]
        );
    }

    #[test]
    fn dep_names_match_regardless_of_the_original_dependencys_separator_style() {
        // A `ruamel.yaml`-style dotted node name and a `requires_dist` entry
        // that spells the same project with a hyphen or underscore instead
        // must normalize to the same key, or the dependency-graph edge
        // between them silently fails to resolve.
        let reqs = vec!["ruamel-yaml".to_string(), "ruamel_yaml".to_string()];
        assert_eq!(
            normalize_pkg_name("ruamel.yaml"),
            normalize_pkg_name("ruamel-yaml")
        );
        for name in dep_names_from_requires_dist(&reqs) {
            assert_eq!(name, normalize_pkg_name("ruamel.yaml"));
        }
    }

    #[test]
    fn missing_source_files_flags_a_source_or_patch_not_on_disk() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("foo-1.0.tar.gz"), b"").unwrap();

        let spec = "\
Name:           foo
Source:         https://example.com/foo-1.0.tar.gz
Source1:        registry.tar.zst
Patch0:         0001-fix.patch
";
        let mut missing = missing_source_files(spec, dir.path());
        missing.sort();
        assert_eq!(missing, vec!["0001-fix.patch", "registry.tar.zst"]);
    }

    #[test]
    fn missing_source_files_is_empty_when_everything_is_present() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("foo-1.0.tar.gz"), b"").unwrap();

        let spec = "Source: https://example.com/foo-1.0.tar.gz\n";
        assert!(missing_source_files(spec, dir.path()).is_empty());
    }

    /// Reproduces the python-python-docx bug: a `series` file left over from
    /// an earlier quilt session names a patch the current spec no longer
    /// declares. `%autosetup -p1` then chokes trying to apply it during
    /// `%prep` even though the spec currently has zero `Patch:` lines.
    #[test]
    fn sync_quilt_series_drops_a_patch_the_spec_no_longer_declares() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("series"), b"0001-orphaned.patch\n").unwrap();

        sync_quilt_series("Name: foo\n", dir.path());

        assert!(
            !dir.path().join("series").exists(),
            "stale series file must be removed when the spec declares no patches"
        );
    }

    #[test]
    fn sync_quilt_series_rewrites_to_match_the_specs_current_patches() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("series"),
            b"0001-old.patch\n0002-old.patch\n",
        )
        .unwrap();

        let spec = "Name: foo\nPatch0: 0001-new.patch\n";
        sync_quilt_series(spec, dir.path());

        assert_eq!(
            std::fs::read_to_string(dir.path().join("series")).unwrap(),
            "0001-new.patch\n"
        );
    }

    #[test]
    fn sync_quilt_series_is_a_no_op_when_already_in_sync() {
        let dir = tempfile::tempdir().unwrap();
        // No series file, no patches declared — must not create one.
        sync_quilt_series("Name: foo\n", dir.path());
        assert!(!dir.path().join("series").exists());
    }

    /// Earlier review probe: a fake `python-foo-0.1-1.1.x86_64.rpm`
    /// satisfied `python-foo>=99` because the pre-fix check only matched
    /// the name prefix. Must now actually compare versions.
    #[test]
    fn rpm_dep_version_constraint_is_enforced_not_just_the_name_prefix() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("python-foo-0.1-1.1.x86_64.rpm"), b"").unwrap();

        assert!(
            !rpm_satisfies_dep(tmp.path(), "python-foo>=99"),
            "0.1 does NOT satisfy >=99 — pre-fix regression"
        );
        assert!(rpm_satisfies_dep(tmp.path(), "python-foo>=0.1"));
        assert!(rpm_satisfies_dep(tmp.path(), "python-foo"));
    }

    /// 1.20 is NOT less than 1.2 — numeric segments must compare as numbers.
    #[test]
    fn rpm_version_compare_treats_decimal_segments_as_numbers() {
        assert!(rpm_version_satisfies("1.20", ">=", "1.2"));
        assert!(!rpm_version_satisfies("1.2", ">=", "1.20"));
        assert!(rpm_version_satisfies("1.20", ">", "1.2"));
        assert!(rpm_version_satisfies("1.2.0", "==", "1.2"));
        assert!(rpm_version_satisfies("1.2", "<", "1.2.3"));
    }

    #[test]
    fn parse_rpm_dep_prefers_two_char_operators() {
        assert_eq!(
            parse_rpm_dep_constraint("foo>=1.2"),
            ("foo", Some((">=", "1.2")))
        );
        assert_eq!(
            parse_rpm_dep_constraint("foo <= 2"),
            ("foo", Some(("<=", "2")))
        );
        assert_eq!(parse_rpm_dep_constraint("foo"), ("foo", None));
    }

    #[test]
    fn rpm_filename_version_extraction_handles_hyphenated_names() {
        assert_eq!(
            extract_rpm_filename_version(
                "python313-Cython",
                "python313-Cython-3.2.9-1.1.x86_64.rpm"
            ),
            Some("3.2.9")
        );
        assert_eq!(
            extract_rpm_filename_version("python-foo", "python-foo-0.1-1.1.x86_64.rpm"),
            Some("0.1")
        );
    }

    /// Earlier review probe: a spec declaring `Version: 1.20` matched a
    /// request for version `1.2` because the pre-fix check was a bare
    /// substring search. Must now compare the actual parsed tag value.
    /// Earlier review probe: a valid `members = ["crates/*"]` with a real
    /// member in `crates/foo` became `members = []` because the pre-fix
    /// check did literal `.exists()` on the path "crates/*".
    #[test]
    fn cargo_workspace_glob_matches_existing_members() {
        let tmp = tempfile::tempdir().unwrap();
        let base = tmp.path();
        std::fs::create_dir_all(base.join("crates/foo")).unwrap();
        std::fs::create_dir_all(base.join("crates/bar")).unwrap();

        assert!(
            member_resolves(base, "crates/*"),
            "crates/* matches crates/foo"
        );
        assert!(
            member_resolves(base, "crates/foo"),
            "literal path still works"
        );
        assert!(member_resolves(base, "crates/f*"), "prefix glob works");
    }

    #[test]
    fn cargo_workspace_glob_with_no_match_is_still_considered_missing() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join("crates")).unwrap();
        assert!(
            !member_resolves(tmp.path(), "crates/*"),
            "empty crates/ dir means the glob genuinely resolves nothing"
        );
        assert!(!member_resolves(tmp.path(), "nonexistent/*"));
        assert!(!member_resolves(tmp.path(), "nope"));
    }

    #[test]
    fn glob_single_segment_matcher_handles_the_common_shapes() {
        assert!(glob_matches_single_segment("*", "anything"));
        assert!(glob_matches_single_segment("foo-*", "foo-bar"));
        assert!(glob_matches_single_segment("foo-*", "foo-"));
        assert!(!glob_matches_single_segment("foo-*", "bar"));
        assert!(glob_matches_single_segment("*-core", "ripgrep-core"));
        assert!(glob_matches_single_segment("a*b*c", "aXXbYYc"));
        assert!(!glob_matches_single_segment("a*b*c", "aXXbYY"));
        assert!(glob_matches_single_segment("literal", "literal"));
        assert!(!glob_matches_single_segment("literal", "other"));
    }

    #[test]
    fn spec_version_differs_rejects_substring_matches() {
        let tmp = tempfile::tempdir().unwrap();
        let spec = tmp.path().join("foo.spec");
        std::fs::write(
            &spec,
            "Name: foo\nVersion: 1.20\nSource: foo-%{version}.tar.gz\n",
        )
        .unwrap();

        assert!(
            spec_version_differs(&spec, "1.2"),
            "1.2 is NOT 1.20 — pre-fix substring check wrongly said they matched"
        );
        assert!(!spec_version_differs(&spec, "1.20"));
    }

    #[test]
    fn checkout_snapshot_restores_edited_deleted_and_added_top_level_files() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        // Pre-snapshot state: three top-level files.
        std::fs::write(dir.join("foo.spec"), "Name: foo\nVersion: 1.0\n").unwrap();
        std::fs::write(dir.join("fix.patch"), "--- orig\n+++ new\n").unwrap();
        std::fs::write(dir.join("foo.changes"), "entry\n").unwrap();
        // A nested dir that must survive snapshot/restore untouched (the
        // snapshot is top-level only).
        std::fs::create_dir_all(dir.join(".osc")).unwrap();
        std::fs::write(dir.join(".osc").join("_meta"), "osc internal\n").unwrap();

        let snap = CheckoutSnapshot::take(dir);

        // Simulate agent edits: modify one, delete one, add a new one.
        std::fs::write(dir.join("foo.spec"), "Name: foo\nVersion: 2.0\n").unwrap();
        std::fs::remove_file(dir.join("fix.patch")).unwrap();
        std::fs::write(dir.join("new-patch.patch"), "agent-added\n").unwrap();

        // Reporter stub.
        struct Null;
        impl Reporter for Null {
            fn report(&self, _: &Event) {}
        }
        let step = StepDir {
            path: dir.to_path_buf(),
            id: ap_core::event::StepId(0),
        };
        snap.restore(&Null, &step);

        // Edited file restored.
        assert_eq!(
            std::fs::read_to_string(dir.join("foo.spec")).unwrap(),
            "Name: foo\nVersion: 1.0\n",
            "spec must be rolled back"
        );
        // Deleted file re-created.
        assert!(
            dir.join("fix.patch").exists(),
            "deleted patch must be restored"
        );
        assert_eq!(
            std::fs::read_to_string(dir.join("fix.patch")).unwrap(),
            "--- orig\n+++ new\n",
        );
        // Agent-added file removed.
        assert!(
            !dir.join("new-patch.patch").exists(),
            "agent-added top-level file must be deleted on restore"
        );
        // Nested .osc untouched.
        assert!(dir.join(".osc").join("_meta").exists());
    }

    #[test]
    fn missing_source_files_ignores_unexpanded_macros_and_lookalike_lines() {
        let dir = tempfile::tempdir().unwrap();
        let spec = "\
Sourceforge is not a tag\n\
Source:         %{undefined_macro}\n";
        assert!(missing_source_files(spec, dir.path()).is_empty());
    }

    /// Records every event so a test can pair `StepStarted` with `StepFinished`.
    #[derive(Default)]
    struct Recorder(std::sync::Mutex<Vec<Event>>);

    impl Reporter for Recorder {
        fn report(&self, event: &Event) {
            self.0.lock().unwrap().push(event.clone());
        }
    }

    /// Every started step must end, whichever path leaves the pipeline.
    fn assert_steps_balanced(events: &[Event]) {
        let mut open = std::collections::HashSet::new();
        for event in events {
            match event {
                Event::StepStarted { step, .. } => assert!(open.insert(step.0)),
                Event::StepFinished { step, .. } => assert!(open.remove(&step.0)),
                _ => {}
            }
        }
        assert!(open.is_empty(), "steps never finished: {open:?}");
    }

    #[test]
    fn a_failed_checkout_still_finishes_its_step() {
        let tmp = tempfile::tempdir().unwrap();
        let work_dir = tmp.path().to_path_buf();
        // A space is not a valid OBS name, so `osc::ensure_package` refuses it
        // before running anything.
        let config = PipelineConfig {
            pkg_name: "foo".into(),
            rpm_name: "python-foo".into(),
            version: "1.0".into(),
            project: "bad project".into(),
            work_dir: work_dir.clone(),
            state_root: work_dir.join("state"),
            rpms_dir: work_dir.join("rpms"),
            distro: "openSUSE_Tumbleweed".into(),
            arch: "x86_64".into(),
            max_attempts: 1,
            budget_usd: 1.0,
            agent_backend: AgentBackend::Cli,
            no_agent: true,
            force: false,
            regen: false,
            source_repo: None,
            subprojects: Vec::new(),
            failed_deps: Vec::new(),
            unbuildable_deps: Vec::new(),
            run_id: "test".into(),
            source_urls: Default::default(),
            python_flavors: Default::default(),
        };
        let run = ap_core::run::RunDir::allocate(&work_dir).unwrap();
        let pkg_run = run.pkg("python-foo");
        let recorder = Recorder::default();

        let result = run_package_inner(&config, &pkg_run, &recorder);

        assert!(matches!(result, PipelineResult::Failed { .. }));
        let events = recorder.0.lock().unwrap();
        assert!(events.iter().any(|e| matches!(
            e,
            Event::StepStarted {
                kind: StepKind::Checkout,
                attempt: 1,
                ..
            }
        )));
        assert_steps_balanced(&events);
    }

    #[test]
    fn no_agent_reports_the_agent_skip_only_once() {
        let mut config = PipelineConfig {
            pkg_name: "foo".into(),
            rpm_name: "python-foo".into(),
            version: "1.0".into(),
            project: "p".into(),
            work_dir: PathBuf::new(),
            state_root: PathBuf::new(),
            rpms_dir: PathBuf::new(),
            distro: "openSUSE_Tumbleweed".into(),
            arch: "x86_64".into(),
            max_attempts: 1,
            budget_usd: 1.0,
            agent_backend: AgentBackend::Cli,
            no_agent: true,
            force: false,
            regen: false,
            source_repo: None,
            subprojects: Vec::new(),
            failed_deps: Vec::new(),
            unbuildable_deps: Vec::new(),
            run_id: "test".into(),
            source_urls: Default::default(),
            python_flavors: Default::default(),
        };
        config.no_agent = true;
        let recorder = Recorder::default();
        let mut step_attempts = Attempts::default();

        // The review gate announces the skip up front, and a later failure
        // path reports it again through `skip_agent`.
        step_attempts.skip(&recorder, StepKind::AgentTurn, SkipReason::NoAgent);
        skip_agent(&recorder, &mut step_attempts, &config, 0, 0.0);

        let events = recorder.0.lock().unwrap();
        let skips = events
            .iter()
            .filter(|e| matches!(e, Event::StepSkipped { .. }))
            .count();
        assert_eq!(skips, 1);
    }
}

#[cfg(test)]
mod prompt_notes_tests {
    use super::*;

    #[test]
    fn short_notes_pass_through_and_long_notes_keep_the_recent_end() {
        assert_eq!(tail_for_prompt("short\n"), "short\n");
        let long: String = (0..4000).map(|i| format!("line {i}\n")).collect();
        let cut = tail_for_prompt(&long);
        assert!(cut.len() <= MAX_PROMPT_NOTES_BYTES + 64);
        assert!(cut.starts_with("(earlier entries omitted)\n"));
        assert!(cut.trim_end().ends_with("line 3999"));
        assert!(!cut.contains("line 0\n"));
    }
}

#[cfg(test)]
mod policy_tests {
    use super::*;

    #[test]
    fn a_success_under_the_current_policy_is_built() {
        let st = PkgState {
            last_built_version: Some("1.0".into()),
            last_outcome: Some("success".into()),
            policy_version: POLICY_VERSION,
            ..Default::default()
        };
        assert!(st.policy_current());
        assert!(st.is_built("1.0"));
    }
}
