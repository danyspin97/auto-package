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

use crate::{distro_bounds, osc, workspace::Workspace};

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
    /// Set by `lint` and `update`: check the upstream state of the carried
    /// patches that are upstream requests, and act on it (`patch_provenance`).
    pub patch_provenance: bool,
    /// The package's existing recipe is carried forward to a new version: a
    /// bumped dependency, or the package of an update (`update`). Set for a
    /// package whose spec existed before the run. Unlike `update`, a dependency
    /// has it: the origin's spec is read for the problems the origin already
    /// has, which are reported and not fixed (see `carried_origin`).
    pub carried_recipe: bool,
    /// For an update of the package (`update`): the `Version:` the OBS branch
    /// was at when the update was planned. The checkout's spec must be at it
    /// before the first agent turn. `None` for every other run.
    pub branch_version: Option<String>,
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
    /// Set by `auto-package fix`: the bug to fix in the package at the version
    /// it has. The pre-build turn then adds the fix as a patch instead of
    /// reviewing the recipe.
    pub bug_fix: Option<std::sync::Arc<crate::bugfix::FixTask>>,
    /// The release archive is built by a build system auto-package does not
    /// know (Go, Zig, a plain Makefile): see `sources::probe`. Only the
    /// recipe the checkout holds can build it, so no spec is generated for it,
    /// and it is not a Python package whatever its RPM name says.
    pub unknown_build_system: bool,
    /// The request as the user typed it after `auto-package build` (`foo`,
    /// `pypi:foo`, a repository URL, `foo --url <url>`), for the rerun command
    /// the failure texts suggest. `None` for a dependency, which nobody
    /// requested: its rerun is named by its own name.
    pub request_text: Option<String>,
    /// Set for a build in a branch project (an update, an improvement or a
    /// backport): a plain `build` of the package does not reproduce it, so the
    /// failure suggests no rerun command.
    pub branch_build: bool,
}
mod state;
pub use state::*;
mod different_retry;
mod failure_key;
#[cfg(test)]
mod failure_key_tests;
pub use different_retry::*;
pub use failure_key::same_failure;

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

mod failure_kind;
pub use failure_kind::*;
#[cfg(test)]
mod failed_osc_tests;
#[cfg(test)]
mod failure_kind_tests;

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
        /// What must be done before a rerun can succeed, when there is one.
        remedy: Option<ap_core::remedy::Remedy>,
        /// The full detail and the one action beyond `reason`, when known.
        note: Option<ap_core::failure_note::FailureNote>,
    },
}
mod target;
pub use target::*;

#[cfg(test)]
mod resolvable_tests;
mod select;
pub use select::*;
mod closure_gate;
use closure_gate::*;
mod blocked;
mod denied_note;
pub use blocked::*;
mod slots;
use slots::*;
mod notes_step;
mod run;
pub use run::*;
mod obs_names;
mod osc_failure;
use obs_names::*;
mod checkout;
use checkout::*;
use update_mode::*;
mod carried_origin;
mod generate_stage;
#[cfg(test)]
mod no_agent_fix_tests;
mod recap_entry;
mod unknown_build;
mod unowned_fix;
mod unowned_fix_apply;
#[cfg(test)]
mod unowned_fix_tests;

mod update_mode;

mod patch_apply;
mod patch_review;
mod patch_review_edit;
use patch_review_edit::review_turn_patches;
#[cfg(test)]
mod patch_review_edit_tests;
mod patch_review_prompt;
pub(crate) mod review_marks;
#[cfg(test)]
mod review_marks_tests;
use review_marks::*;
#[cfg(test)]
mod disabled_patch_tests;
mod patch_provenance;
#[cfg(test)]
mod patch_provenance_tests;
#[cfg(test)]
mod patch_review_tests;
mod patch_rewrite;
#[cfg(test)]
mod patch_rewrite_tests;
mod patch_stage;
mod removal_proof;
#[cfg(test)]
mod removal_proof_tests;
use patch_apply::*;
#[cfg(test)]
mod carried_origin_tests;
mod sdist_stage;
#[cfg(test)]
mod sdist_stage_tests;
mod source_pass;
#[cfg(test)]
mod update_mode_tests;
use patch_stage::*;
mod patch_loop;
#[cfg(test)]
use patch_loop::*;
mod submodule_wheel;
use submodule_wheel::*;
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
mod commit_summary;
mod validate;
use commit::*;
#[cfg(test)]
use commit_summary::*;
mod build_round;
mod unresolvable;
pub use unresolvable::note_built;

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
                    remedy: None,
                    note: None,
                });
            }
        }
    };
}
use begin_or_fail;
mod agent;
mod turn_spend;
use agent::*;
pub(crate) use turn_spend::{daily_cap_check, record_daily_spend};
mod agent_response;
use agent_response::*;
mod bug_fix_check;
#[cfg(test)]
mod bug_fix_check_tests;
mod bug_fix_prompt;
#[cfg(test)]
mod bug_fix_prompt_tests;
mod bug_fix_rounds;
mod bug_fix_stage;
#[cfg(test)]
mod bug_fix_stage_tests;
mod failed_note;
mod findings_rounds;
mod findings_stage;
mod ladder;
mod ladder_end;
mod ladder_fields;
#[cfg(test)]
mod ladder_fields_tests;
mod ladder_memo;
mod ladder_recap;
#[cfg(test)]
mod ladder_recap_tests;
pub use ladder::*;
pub use ladder_fields::LadderRecapFields;
use ladder_memo::*;
mod logs;
mod sdist_name;
mod sibling;
mod sibling_specs;
#[cfg(test)]
mod sibling_specs_tests;
mod source_case;
mod spec_edits;
use spec_edits::*;
mod snapshot;
use snapshot::*;
mod comment_only;
use comment_only::*;
#[cfg(test)]
mod comment_only_tests;
mod turn_changes;
use turn_changes::*;
mod turn_error;
use turn_error::*;
mod rejection;
use rejection::*;
#[cfg(test)]
mod agent_budget_tests;
mod apply_turn;
mod build_switch;
#[cfg(test)]
mod build_switch_tests;
#[cfg(test)]
mod checkout_tests;
mod network_fetch;
#[cfg(test)]
mod network_fetch_tests;
mod patch_rules;
pub(crate) use patch_delta::patch_delta;
pub(crate) use patch_rules::{
    declared_patches, header_field, header_lines, header_problems, is_patch_comment,
    names_in_bullet,
};
pub(crate) use patch_rules_content::source_only_problems;
pub(crate) use patch_rules_spec::spec_route_problems;
mod patch_block_quilt;
#[cfg(test)]
mod patch_block_quilt_tests;
mod patch_delta;
#[cfg(test)]
mod patch_delta_tests;
mod patch_rules_content;
#[cfg(test)]
mod patch_rules_content_tests;
mod patch_rules_spec;
#[cfg(test)]
mod patch_rules_tests;
mod spend_guard;
pub(crate) use spend_guard::spend_allows_turn;
use spend_guard::*;
mod changelog_required;
mod diagnostic_edit;
#[cfg(test)]
mod refusal_callers_tests;
use changelog_required::*;
#[cfg(test)]
mod changelog_required_tests;
pub mod rpmlintrc_guard;
#[cfg(test)]
mod rpmlintrc_guard_tests;
mod sdist_wheel;
mod service_edit;
#[cfg(test)]
mod service_edit_tests;
mod source_guard;
#[cfg(test)]
mod source_guard_tests;
#[cfg(test)]
mod turn_naming_tests;
#[cfg(test)]
mod wheel_refusal_tests;
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
mod carry_error;
mod carry_fallback;
#[cfg(test)]
mod commit_gate_tests;
#[cfg(test)]
mod commit_message_tests;
#[cfg(test)]
mod commit_summary_tests;
pub mod keyring_accept;
#[cfg(test)]
mod keyring_accept_tests;
mod keyring_listing;
pub(crate) use keyring_listing::is_hidden;
mod service_source;
#[cfg(test)]
mod service_source_tests;
mod source_signature;
pub(crate) use source_signature::{
    SIGNATURE_SUFFIXES, SignatureCheck, archive_of, check_signed_sources,
};
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
mod ladder_summary_tests;
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
mod kept_baseline_tests;
