//! The durable state of a package: the settled marks, PkgState and its impls, and
//! the reconciliation of the state with OBS and with the checkout.

use super::*;
use crate::pipeline::blocked::*;
use crate::pipeline::build_round::*;
use crate::pipeline::slots::*;

/// `last_outcome` of a package `--local` built on this machine. It is not a
/// success: OBS never built it, so the readers that mean "OBS has it" (see
/// [`PkgState::is_built`]) do not accept it.
pub const LOCAL_SUCCESS: &str = "local-success";

/// Packages the pipeline itself reported built (or already built) in this
/// process, by `fuzzy_key`. The resolver reads OBS statuses, which can keep
/// calling a package unbuilt for a reason it cannot fix by building it again (a
/// failing flavor): without this a package that finishes `AlreadyBuilt` is
/// selected again the next round, and the packages that wait for it, the
/// requested root among them, are never reached.
static SETTLED: std::sync::Mutex<Option<std::collections::HashSet<String>>> =
    std::sync::Mutex::new(None);

pub(super) fn mark_settled(rpm_name: &str) {
    SETTLED
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get_or_insert_with(Default::default)
        .insert(osc::fuzzy_key(rpm_name));
}

pub(super) fn is_settled(rpm_name: &str) -> bool {
    SETTLED
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_ref()
        .is_some_and(|s| s.contains(&osc::fuzzy_key(rpm_name)))
}

/// The requested root packages one build invocation has finished (built or
/// already built), by `fuzzy_key`. Owned by that invocation: a package that
/// another invocation, or a dependency closure, settled does not count here.
#[derive(Default)]
pub struct FinishedRoots(std::collections::HashSet<String>);

impl FinishedRoots {
    /// Notes the result of the run of `rpm_name` when it is the requested
    /// root `root_rpm` and the run built it or found it built.
    pub fn note(&mut self, root_rpm: &str, rpm_name: &str, result: &PipelineResult) {
        if osc::fuzzy_key(root_rpm) == osc::fuzzy_key(rpm_name)
            && matches!(
                result,
                PipelineResult::Built { .. } | PipelineResult::AlreadyBuilt { .. }
            )
        {
            self.0.insert(osc::fuzzy_key(rpm_name));
        }
    }

    pub fn contains(&self, rpm_name: &str) -> bool {
        self.0.contains(&osc::fuzzy_key(rpm_name))
    }
}

// ── durable package state ─────────────────────────────────────────────────────

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, Default)]
pub struct PkgState {
    pub last_built_version: Option<String>,
    pub last_outcome: Option<String>,
    /// One-line reason recorded for the most recent unsuccessful outcome
    /// (`last_outcome` is `"failed"` or `"missing_deps"`), so the next run —
    /// or a human reading state.json — can see WHY without re-opening the
    /// run.jsonl stream. Preserved across runs the same way `last_outcome`
    /// is; cleared when the package next succeeds.
    ///
    /// Updated on every unsuccessful [`PipelineResult`], not just `Failed`, so
    /// a `MissingDeps` round replaces the reason too and it never describes an
    /// older, unrelated failure.
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
    /// Hash of the spec for which every rung of the resolution ladder was
    /// tried and failed, with the full ladder history. The same ladder is not
    /// run again on the same spec (it would only fail again, at the price of
    /// several turns); a changed spec, or `--force`, tries afresh.
    #[serde(default)]
    pub ladder_spec_hash: Option<u64>,
    #[serde(default)]
    pub ladder_reason: Option<String>,
    /// The spent ladder was refused its wheel for a security patch: a rerun on
    /// the same spec ends as the first run did, needing a maintainer. Read only
    /// with `ladder_spec_hash`, which every write of the memo sets with it.
    #[serde(default)]
    pub ladder_needs_maintainer: bool,
    /// The memo's short recap (see `ladder_fields`); same JSON object.
    #[serde(flatten)]
    pub ladder_recap: crate::pipeline::ladder_fields::LadderRecapFields,
    /// Hash of the build.log the last PreFix agent turn read. The same log is
    /// not handed to a turn twice (it would find nothing new, at a turn's cost).
    #[serde(default)]
    pub prefix_log_hash: Option<u64>,
    /// Total model-call USD spent during the `cumulative_spent_run_id`
    /// run. Persisted so an outer-loop retry WITHIN that run (which
    /// recreates `run_package` from scratch for the same package)
    /// enforces `budget_usd` as a per-package cap rather than resetting
    /// to zero on every call.
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
    /// Fix-after-build rounds given in the `cumulative_spent_run_id` run, scoped
    /// like `cumulative_attempts` (see `build_rounds::BuildRounds`).
    #[serde(default)]
    pub cumulative_build_rounds: usize,
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
    /// Dependencies a package of the distro serves at a version newer than the
    /// bound this package's spec carries, as `(name, shipped version)`: the
    /// bound is dropped from the spec (convention 75 (section 59)) and the build tried, not
    /// the dependency queued or the package given up on. Applied on every
    /// validation, so a bound the agent writes back is dropped again.
    #[serde(default)]
    pub relaxed_dependencies: Vec<(String, String)>,
    /// Hashes of the problems patched into this package for another package
    /// (`patch_request`): a request with the same text is not made again, but a
    /// new one, from a dependent that still fails, is.
    #[serde(default)]
    pub patched_problems: Vec<u64>,
    /// Hashes of the patch requests a patch turn answered without a patch
    /// (nothing upstream to restore). A dependent that asks again is not sent
    /// back here: it gets the newest release its bound allows instead.
    #[serde(default)]
    pub declined_patches: Vec<u64>,
    /// Hashes of the machine-checkable conditions of patch requests that were
    /// found true of this package (already, or after a patch turn).
    #[serde(default)]
    pub satisfied_conditions: Vec<u64>,
    /// Patch turns that did not make a condition true, by the condition's hash.
    #[serde(default)]
    pub condition_failures: Vec<(u64, u32)>,
    /// The OBS package name this package is built and checked out under, when
    /// it differs from the workspace's `rpm_name` only by case (a package the
    /// project already lists as `python-APScheduler`). Resolved from the
    /// project's listing at the start of each run and recorded here; `None`
    /// in a state.json written before the field existed, which means
    /// `rpm_name` itself.
    #[serde(default)]
    pub obs_name: Option<String>,
    /// What the fix loop changed that a reader must be told (see
    /// [`crate::fix_flags`]), for the run `fix_flags_run_id`.
    #[serde(default)]
    pub fix_flags: Vec<crate::fix_flags::FixFlag>,
    #[serde(default)]
    pub fix_flags_run_id: Option<String>,
    /// What the change adds that a reviewer must look at (see
    /// [`crate::risk_flags`]), for the run `risk_flags_run_id`.
    #[serde(default)]
    pub risk_flags: Vec<crate::risk_flags::RiskFlag>,
    #[serde(default)]
    pub risk_flags_run_id: Option<String>,
    /// The version the fix-loop and risk flags were recorded for. A run for
    /// another version starts with none (see `run_package`).
    #[serde(default)]
    pub flags_version: Option<String>,
    /// The rpmlint `E:` keys a fix turn was given, or the agent left, for the
    /// package version `rpmlint_shown_version` (see `rpmlint_fix`).
    #[serde(default)]
    pub rpmlint_shown: Vec<String>,
    #[serde(default)]
    pub rpmlint_shown_version: Option<String>,
    /// The failures this package repeated, and whether its next attempt is
    /// told to try a different approach (see [`super::different_retry`]).
    #[serde(default)]
    pub retry: super::different_retry::RetryRecord,
}

impl PkgState {
    /// Remember that `dependency`'s bound is to be dropped for `shipped`.
    pub(super) fn relax_dependency(&mut self, dependency: &str, shipped: &str) {
        let key = osc::fuzzy_key(dependency);
        match self
            .relaxed_dependencies
            .iter_mut()
            .find(|(d, _)| osc::fuzzy_key(d) == key)
        {
            Some(entry) => entry.1 = shipped.to_string(),
            None => self
                .relaxed_dependencies
                .push((dependency.to_string(), shipped.to_string())),
        }
    }
}

/// Bring every package's recorded outcome in line with reality at startup.
///
/// - `running` left by a process that died (nothing holds the package's
///   `build.lock`) becomes `interrupted`, so the package reads as re-runnable.
/// - `missing_deps` whose waited-on dependencies are all built (their own
///   state says `success`) becomes `interrupted` too: the wait is over.
///
/// Returns the packages changed, as `(name, new outcome)`.
pub fn reconcile_state(state_root: &Path) -> Vec<(String, &'static str)> {
    let mut changed = Vec::new();
    let Ok(rd) = std::fs::read_dir(state_root) else {
        return changed;
    };
    let mut names: Vec<String> = rd
        .flatten()
        .filter(|e| e.path().join("state.json").is_file())
        .filter_map(|e| e.file_name().into_string().ok())
        .collect();
    names.sort();
    let built = |dep: &str| {
        [dep.to_string(), osc::rpm_name(dep)].iter().any(|n| {
            Workspace::find_existing(state_root, &[n.as_str()])
                .is_some_and(|w| PkgState::load(&w).last_outcome.as_deref() == Some("success"))
        })
    };
    for name in names {
        let Some(ws) = Workspace::find_existing(state_root, &[&name]) else {
            continue;
        };
        let state = PkgState::load(&ws);
        let (new, reason) = match state.last_outcome.as_deref() {
            Some("running") => {
                // Only a lock we can take proves the owner is gone.
                if ap_core::proc_lock::try_lock(&ws.dir().join("build.lock")).is_err() {
                    continue;
                }
                (
                    "interrupted",
                    "the run that was building this package ended without a result",
                )
            }
            Some("missing_deps") => {
                let waiting: Vec<String> = state
                    .last_failure_reason
                    .as_deref()
                    .and_then(|r| r.strip_prefix("waiting on dependencies: "))
                    .map(|d| d.split(',').map(|x| x.trim().to_string()).collect())
                    .unwrap_or_default();
                if waiting.is_empty() || !waiting.iter().all(|d| built(d)) {
                    continue;
                }
                (
                    "interrupted",
                    "the dependencies it waited on are built; ready to run again",
                )
            }
            _ => continue,
        };
        if PkgState::update(&ws, |st| {
            st.last_outcome = Some(new.to_string());
            st.last_failure_reason = Some(reason.to_string());
        })
        .is_ok()
        {
            changed.push((name, new));
        }
    }
    // Dependents that wait on a blocked package, and those whose blocker is
    // no longer blocked.
    let (more, _errors) = sync_blocked_waiters(state_root, &[]);
    for (name, outcome) in more {
        changed.retain(|(n, _)| *n != name);
        changed.push((name, outcome));
    }
    changed
}

/// A package whose `state.json` records an unfinished outcome although OBS shows
/// its build succeeded is fixed here, before the run builds on it: the RPMs are
/// downloaded (`download(name)`, the number fetched) and the outcome becomes
/// `success`. The decision is [`crate::heal::obs_built_decision`]: only the
/// version the state last built, with a clean checkout, is taken. `statuses`
/// is keyed by lower-case package name. `download` gets the package's OBS name.
/// Returns `(name, Ok(rpmlint warnings))` for each package fixed and
/// `(name, Err(problem))` for each that OBS built but could not be fixed; a
/// package that is declined is not listed (its `self_check` finding says why).
pub fn reconcile_with_obs(
    state_root: &Path,
    checkout_root: &Path,
    rpms_dir: &Path,
    statuses: &std::collections::HashMap<String, osc::BuildStatus>,
    local_changes: impl Fn(&Path) -> bool,
    download: impl Fn(&str) -> Result<usize, osc::Error>,
) -> Vec<(String, Result<Vec<String>, String>)> {
    let mut out = Vec::new();
    for name in crate::heal::package_names(state_root) {
        let Some(ws) = Workspace::find_existing(state_root, &[&name]) else {
            continue;
        };
        let state = PkgState::load(&ws);
        // The checkout and the OBS download are named by the OBS name the state
        // records; a state written before that field is the name itself.
        let obs = state.obs_name.clone().unwrap_or_else(|| name.clone());
        let checkout = checkout_root.join(&obs);
        let spec_version = std::fs::read_to_string(checkout.join(format!("{obs}.spec")))
            .ok()
            .and_then(|t| crate::spec::parse_header(&t).map(|h| h.version));
        let decision = crate::heal::obs_built_decision(
            state.last_outcome.as_deref(),
            statuses.get(&name.to_lowercase()),
            spec_version.as_deref(),
            state.last_built_version.as_deref(),
            || local_changes(&checkout),
        );
        let (crate::heal::ObsDecision::Fetch, Some(version)) = (decision, spec_version) else {
            continue;
        };
        match download(&obs) {
            Ok(n) if n > 0 => {
                // The rpmlint log of what was fetched, as a normal download does.
                let lint = record_rpmlint_logs(rpms_dir, &obs, &version, &ws).warnings;
                let what = format!("recorded success: OBS built {version}, fetched {n} RPM(s)");
                let saved = PkgState::update(&ws, |st| {
                    st.last_built_version = Some(version.clone());
                    st.last_outcome = Some("success".to_string());
                    st.last_failure_reason = None;
                });
                match saved {
                    Ok(_) => {
                        crate::heal::record(&ws, &what);
                        out.push((name, Ok(lint)));
                    }
                    Err(e) => out.push((
                        name,
                        Err(format!(
                            "OBS built it; fetched {n} RPM(s), state not saved: {e}"
                        )),
                    )),
                }
            }
            Ok(_) => out.push((
                name,
                Err("OBS built it but no RPMs were downloaded".to_string()),
            )),
            Err(e) => out.push((
                name,
                Err(format!("OBS built it but fetching its RPMs failed: {e}")),
            )),
        }
    }
    out
}

/// Hash spec content for [`PkgState::reviewed_spec_hash`] and the other hashes
/// persisted in state.json. The value is the first 8 bytes of SHA-256, so it is
/// stable across Rust releases (`DefaultHasher` is not). Hashes stored by older
/// builds no longer match, which costs one extra turn per package at most.
pub(crate) fn hash_spec(content: &str) -> u64 {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(content.as_bytes());
    let mut first = [0u8; 8];
    first.copy_from_slice(&digest[..8]);
    u64::from_be_bytes(first)
}

/// Hash a sorted+deduped dependency-name list for
/// [`PkgState::unresolvable_review_hash`]. `\0`-joined so e.g. `["a", "bc"]`
/// and `["ab", "c"]` don't collide.
pub(super) fn hash_deps(sorted_deps: &[String]) -> u64 {
    hash_spec(&sorted_deps.join("\0"))
}

impl PkgState {
    /// Read-only callers may fall back to the default state (see [`Self::load`]);
    /// anything that saves must use this or [`Self::update`], so an unreadable
    /// file is never replaced by an empty state.
    pub fn load(ws: &Workspace) -> Self {
        Self::try_load(ws).unwrap_or_else(|e| {
            ap_core::diag!("  cannot read {e}; starting from empty state");
            Self::default()
        })
    }

    /// Like [`Self::load`], but a read failure other than "not found" is an
    /// error instead of an empty state. A corrupt file is still moved aside
    /// and yields the default state.
    pub fn try_load(ws: &Workspace) -> std::io::Result<Self> {
        let path = ws.state_json();
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(e) => {
                return Err(std::io::Error::new(
                    e.kind(),
                    format!("{}: {e}", path.display()),
                ));
            }
        };
        Ok(serde_json::from_str(&text).unwrap_or_else(|e| {
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
        }))
    }

    /// Reads the state as it is: a missing, unreadable or corrupt file is an
    /// error, never an empty state. For readers that must not act on a state
    /// they cannot see, such as the `--yes` gate. Unlike [`Self::try_load`] it
    /// does not move a corrupt file aside.
    pub fn read_strict(ws: &Workspace) -> std::io::Result<Self> {
        let path = ws.state_json();
        let text = std::fs::read_to_string(&path)
            .map_err(|e| std::io::Error::new(e.kind(), format!("{}: {e}", path.display())))?;
        serde_json::from_str(&text).map_err(|e| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("{} is corrupt: {e}", path.display()),
            )
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
        // A file that cannot be read fails the update instead of being
        // replaced by the default state (which would reset the budget counters).
        let mut state = Self::try_load(ws)?;
        f(&mut state);
        state.save(ws)?;
        Ok(state)
    }

    /// Does state.json record a GREEN build of this version?
    ///
    /// Only `success` qualifies. `wheel-only` is a terminal failure marker, not
    /// a success: the branch that writes it also returns `PipelineResult::Failed`,
    /// and a package the pipeline reports as failed must not look "already
    /// built" to `select_target`.
    pub fn is_built(&self, version: &str) -> bool {
        self.last_built_version.as_deref() == Some(version)
            && self.last_outcome.as_deref() == Some("success")
    }

    /// Does state.json record a local build (`--local`) of this version? Such a
    /// build never reached OBS, so [`Self::is_built`] does not accept it. Only
    /// a local run asks for it, to find a dependency built on this machine.
    pub fn is_built_locally(&self, version: &str) -> bool {
        self.last_built_version.as_deref() == Some(version)
            && self.last_outcome.as_deref() == Some(LOCAL_SUCCESS)
    }

    /// Record that the current spec built: `success` when OBS built it,
    /// [`LOCAL_SUCCESS`] when `--local` built it on this machine.
    pub fn record_built(&mut self, version: &str, local: bool) {
        self.last_built_version = Some(version.to_string());
        self.last_outcome = Some(if local { LOCAL_SUCCESS } else { "success" }.to_string());
        self.last_failure_reason = None;
        // A ladder spent on an earlier spec no longer holds back this build,
        // and a later rebuild of the same spec must not be refused for it.
        crate::pipeline::ladder_fields::clear_ladder_memo(self);
    }

    /// The ladder history of a failed resolution of exactly this spec content,
    /// if there was one.
    pub fn failed_ladder(&self, spec_content: &str) -> Option<&str> {
        (self.ladder_spec_hash == Some(hash_spec(spec_content)))
            .then(|| self.ladder_reason.as_deref().unwrap_or("unresolved"))
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

impl PkgState {
    pub(super) fn condition_failures_of(&self, key: u64) -> u32 {
        self.condition_failures
            .iter()
            .find(|(k, _)| *k == key)
            .map_or(0, |(_, n)| *n)
    }

    /// Count a patch turn that did not meet the condition; the new count.
    pub(super) fn record_condition_failure(&mut self, key: u64) -> u32 {
        match self.condition_failures.iter_mut().find(|(k, _)| *k == key) {
            Some((_, n)) => {
                *n += 1;
                *n
            }
            None => {
                self.condition_failures.push((key, 1));
                1
            }
        }
    }

    /// Count a patch turn on top of `unsaved` failures this process could not
    /// save earlier; the new total.
    pub(super) fn record_condition_failure_after(&mut self, key: u64, unsaved: u32) -> u32 {
        for _ in 0..unsaved {
            self.record_condition_failure(key);
        }
        self.record_condition_failure(key)
    }

    pub(super) fn record_condition_satisfied(&mut self, key: u64) {
        if !self.satisfied_conditions.contains(&key) {
            self.satisfied_conditions.push(key);
        }
        self.condition_failures.retain(|(k, _)| *k != key);
    }
}
