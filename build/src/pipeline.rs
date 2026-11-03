//! Single-package build pipeline.
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

use crate::distro_bounds;
use crate::osc;
use crate::workspace::Workspace;

mod baseline;
use baseline::*;
mod flags;
use flags::*;

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
    pub no_agent: bool,
    /// Rebuild even if state.json says already built.
    pub force: bool,
    /// Regenerate spec from scratch.
    pub regen: bool,
    /// This run is an `update` of the package: its recipe is kept, and the
    /// update is checked against the origin it was branched from. Set for the
    /// package the update command builds only, never for a dependency, a
    /// `build`, or a `lint`.
    pub update: bool,
    /// Build from this source repository instead of a PyPI sdist. Whether it
    /// holds a Rust crate or a Python project is decided by looking inside the
    /// release archive, not by which flag was used.
    pub source_repo: Option<String>,
    /// The upstream tag `version` stands for, when it is a dashed tag mapped to
    /// an RPM version (`v2.9.0-1` for `2.9.0.1`), for the spec editor.
    pub upstream_tag: Option<String>,
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
    /// Set for a distro package branched into the project to be patched
    /// (convention 62): what is wrong with it, as the package that needs the
    /// change described it. The pre-build turn then patches the package
    /// instead of reviewing a generated spec.
    pub patch_request: Option<String>,
    /// The machine-checkable part of `patch_request`, when the dependent gave one.
    pub patch_condition: Option<crate::ship_condition::FilesCondition>,
    /// The release archive is built by a build system auto-package does not
    /// know (meson, Go, Zig, a plain Makefile): see `sources::probe`. Only the
    /// recipe the checkout holds can build it, so no spec is generated for it,
    /// and it is not a Python package whatever its RPM name says.
    pub unknown_build_system: bool,
}
mod state;
pub use state::*;

/// The reserved `--source-url <package>=wheel` value: package this one from the
/// upstream wheel, not from source (torch, whose Factory recipe is a multi-hour
/// source build and whose PyPI release is the CUDA wheel).
pub const WHEEL_SOURCE: &str = "wheel";

impl PipelineConfig {
    /// Whether this package is to be repackaged from its upstream wheel.
    pub fn prefers_wheel(&self) -> bool {
        self.source_urls.get(&self.pkg_name) == Some(WHEEL_SOURCE)
    }

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

/// Why a package failed. Callers decide what to retry, count and give up on
/// from this, never from the wording of `reason`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FailureKind {
    #[default]
    Other,
    /// The API answered 429 and the shared pause did not clear it.
    RateLimited,
    /// The package used up its budget or its attempt allowance.
    Exhausted,
    /// Another process held the package's build lock past the wait. Nothing
    /// was tried, so the package is deferred rather than failed.
    Busy,
    /// The run was aborted because the agent cannot work at all (claude is not
    /// logged in). Not a package failure: nothing is retried or counted.
    AgentUnavailable,
    /// The network, OBS or this machine failed (a dropped connection, an OBS
    /// 5xx, a timeout), not the package. Recorded as `interrupted`, so the
    /// package reads as re-runnable; the caller retries it within its usual
    /// bound.
    Transient,
    /// The disk is full (`ENOSPC`). Handled like `Transient`, and told apart
    /// so the user can free space and retry the package.
    DiskFull,
    /// A patch turn ran but did not make the dependent's shipping condition
    /// true. The package's own condition counters bound the repeats (a second
    /// patch turn, then the upstream wheel), so the caller re-queues the
    /// request within the same run whatever its round count.
    ConditionUnmet,
    /// The agent's turn itself failed: the process crashed or timed out, or its
    /// answer could not be read. Not a refusal of the package's edit (the apply
    /// gate's refusals are `Other`), and a style-only failure downgrades it like
    /// the other agent-side kinds.
    AgentFailed,
    /// The agent's turn changed the checkout's osc or git metadata, or left a
    /// link that leaves the tree (see `agent::integrity`). Its own kind: no
    /// caller downgrades it to a warning or retries it, because the checkout
    /// may not be as the pipeline left it. A change that could not be put back
    /// is named in the reason.
    Tampered,
    /// The new release needs a decision a maintainer makes, such as a signing
    /// key the package's keyring lacks. Nothing was sent to OBS, and nothing is
    /// retried: the same refusal would come back.
    NeedsMaintainer,
    /// A source the package needs is gone upstream: its URL answers 404 or
    /// 410, so the release moved or was removed outside the tool (class 4 of
    /// D-06). Nothing is retried: the same answer would come back.
    BrokenOutside,
}

#[derive(Debug)]
pub enum PipelineResult {
    Built {
        rpm_name: String,
    },
    AlreadyBuilt {
        rpm_name: String,
    },
    MissingDeps {
        rpm_name: String,
        /// Everything the package waits on, the `patches` names included.
        deps: Vec<String>,
        /// The part of `deps` that is a distro package to branch and patch
        /// rather than a package to build (convention 62).
        patches: Vec<crate::agent::PatchRequest>,
        /// The part of `deps` that is a native (C/C++) library: never looked up
        /// on PyPI, whatever a translation made of its spelling.
        native: Vec<String>,
        /// The part of `deps` the repository has at a version older than a
        /// constraint wants: updated in place, not built as a new package.
        /// With the release the constraint names, when it names one.
        outdated: Vec<(String, Option<String>)>,
    },
    Failed {
        rpm_name: String,
        reason: String,
        kind: FailureKind,
    },
}
mod target;
pub use target::*;

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
mod select;
pub use select::*;
mod closure_gate;
use closure_gate::*;
mod blocked;
pub use blocked::*;
mod slots;
use slots::*;
mod run;
pub use run::*;
mod obs_names;
use obs_names::*;
mod checkout;
use checkout::*;
use update_mode::*;
mod generate_stage;
mod recap_entry;

mod update_mode;

mod patch_apply;
mod patch_stage;
mod removal_proof;
#[cfg(test)]
mod removal_proof_tests;
use patch_apply::*;
mod sdist_stage;
#[cfg(test)]
mod update_mode_tests;
use patch_stage::*;
mod patch_loop;
use patch_loop::*;
mod patch_refresh;
use patch_refresh::*;
mod refresh_series;
use refresh_series::*;
mod review;
use review::*;
mod deps_turn;
use deps_turn::*;
mod dep_edit;
use dep_edit::*;
mod linked;
use linked::*;
mod build_rounds;
mod fix_after;
use fix_after::*;
mod fix_turn;
use fix_turn::*;
mod rpmlint_fix;
use rpmlint_fix::*;
mod check_paths;
#[cfg(test)]
mod check_paths_tests;
mod commit;
mod validate;
use commit::*;
mod build_round;
mod unresolvable;

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
                    kind: FailureKind::Other,
                });
            }
        }
    };
}
use begin_or_fail;
mod agent;
mod turn_spend;
use agent::*;
mod agent_response;
use agent_response::*;
mod ladder;
pub use ladder::*;
mod logs;
mod sibling;
mod sibling_specs;
#[cfg(test)]
mod sibling_specs_tests;
mod spec_edits;
use spec_edits::*;
mod snapshot;
use snapshot::*;
mod rejection;
use rejection::*;
mod apply_turn;
mod build_switch;
#[cfg(test)]
mod build_switch_tests;
mod network_fetch;
#[cfg(test)]
mod network_fetch_tests;
mod patch_rules;
mod patch_rules_content;
#[cfg(test)]
mod patch_rules_content_tests;
mod patch_rules_spec;
#[cfg(test)]
mod patch_rules_tests;
mod service_edit;
#[cfg(test)]
mod service_edit_tests;
mod source_guard;
#[cfg(test)]
mod source_guard_tests;
#[cfg(test)]
mod turn_naming_tests;
use apply_turn::*;
mod keep_version;
mod licence;
mod patch_condition;
mod rpm_deps;
use rpm_deps::*;
mod patch_check;
pub(crate) use patch_check::*;
mod archive;
pub use archive::*;
mod gen_spec;
use gen_spec::*;
mod carry;
#[cfg(test)]
mod commit_gate_tests;
#[cfg(test)]
mod commit_message_tests;
pub mod keyring_accept;
#[cfg(test)]
mod keyring_accept_tests;
mod keyring_listing;
mod service_source;
#[cfg(test)]
mod service_source_tests;
mod source_signature;
#[cfg(test)]
mod source_signature_tests;
use crate::obs_xml::{
    mask_xml_comments, open_tag, param_value, service_block, service_elements, source_params,
};
use carry::*;
use service_source::*;
mod prompt_notes;
pub(crate) use prompt_notes::*;
mod npm_vendor;
mod python_spec;
mod render;
mod wheel_spec;
mod wheel_switch;
mod zig_stage;
#[cfg(test)]
mod zig_stage_tests;
mod zig_vendor;
use render::*;
mod classify;
mod tarball;
pub(crate) use tarball::*;
mod repo_search;
pub use repo_search::*;

#[cfg(test)]
mod evidence_line_tests;

#[cfg(test)]
mod rpmlint_fix_tests;
#[cfg(test)]
mod version_conflict_tests;

#[cfg(test)]
mod obs_baseline_tests;

#[cfg(test)]
mod agent_tests;

#[cfg(test)]
mod tests;

#[cfg(test)]
mod prompt_notes_tests;

#[cfg(test)]
mod npm_vendor_route_tests;

#[cfg(test)]
mod workspace_link_tests;

#[cfg(test)]
mod discovery_vouching_tests;

#[cfg(test)]
mod published_defect_tests;

#[cfg(test)]
mod patch_turn_tests;

#[cfg(test)]
mod licence_fact_tests;

#[cfg(test)]
mod rollback_note_tests;

#[cfg(test)]
mod wheel_policy_tests;

#[cfg(test)]
mod redist_pipeline_tests;
#[cfg(test)]
mod unknown_build_tests;

#[cfg(test)]
mod settled_tests;

#[cfg(test)]
mod hygiene_tests;

#[cfg(test)]
mod ladder_tests;

#[cfg(test)]
mod build_rounds_tests;

#[cfg(test)]
mod rpm_query_tests;

#[cfg(test)]
mod failure_shape_tests;

#[cfg(test)]
mod patch_stage_tests;

#[cfg(test)]
mod flag_survival_tests;

mod repeat_log;

#[cfg(test)]
mod local_state_tests;

#[cfg(test)]
mod kept_baseline_tests {
    use super::hygiene_tests::config;
    use super::*;

    #[test]
    fn a_package_whose_kept_baseline_does_not_restore_does_not_start() {
        let state = tempfile::tempdir().unwrap();
        let cfg = config("foo", state.path());
        let ws = Workspace::open(state.path(), &cfg.rpm_name).unwrap();
        let kept = ws.dir().join(OBS_BASELINE_DIR);
        let checkout = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(kept.join("files")).unwrap();
        std::fs::write(kept.join("checkout"), checkout.path().display().to_string()).unwrap();
        // An attempt whose file name is not plain: the restore refuses before
        // any OBS call, so nothing is written and the baseline must stay.
        osc::note_attempt(&kept, &["../escape".to_string()]).unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let run = ap_core::run::RunDir::allocate(tmp.path())
            .unwrap()
            .pkg(&cfg.rpm_name);
        let result = run_package(&cfg, &run, &ap_core::sink::NullReporter);
        let PipelineResult::Failed { reason, .. } = result else {
            panic!("expected a refusal, got {result:?}");
        };
        assert!(reason.contains(&kept.display().to_string()), "{reason}");
        assert!(kept.join("files").exists(), "the baseline was deleted");
    }
}
