//! The configuration of one package's pipeline run: [`PipelineConfig`] and the
//! methods that read it.

use std::path::PathBuf;

use crate::version::Scheme;

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
    /// Set for a `lint` build: the post-build review diffs the spec against the
    /// one the run started with (a kept recipe has no generated spec), and runs
    /// even when that diff is empty.
    pub lint_review: bool,
    /// Set for a `lint` review of a package that carries unpublished work
    /// without `--fix`: the review's scope note says to change only what the
    /// package's own diff shows (see `build_round::post_review_scope`).
    pub lint_scoped: bool,
    /// The version scheme of the package's ecosystem: the leftover cleanup of
    /// its branch at commit compares versions in it. `Generic` unless the
    /// caller knows the ecosystem (an update of a PyPI package is `Pep440`).
    pub scheme: Scheme,
}

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
