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
    /// Set for a distro package branched into the project to be patched
    /// (convention 62): what is wrong with it, as the package that needs the
    /// change described it. The pre-build turn then patches the package
    /// instead of reviewing a generated spec.
    pub patch_request: Option<String>,
    /// The machine-checkable part of `patch_request`, when the dependent gave one.
    pub patch_condition: Option<crate::ship_condition::FilesCondition>,
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
mod blocked;
pub use blocked::*;
mod slots;
pub(super) use slots::*;
mod run;
pub use run::*;
mod obs_names;
pub(super) use obs_names::*;
mod checkout;
use checkout::*;
mod generate_stage;
mod patch_stage;
use patch_stage::*;
mod patch_loop;
use patch_loop::*;
mod review;
use review::*;
mod deps_turn;
use deps_turn::*;
mod dep_edit;
use dep_edit::*;
mod linked;
use linked::*;
mod fix_after;
use fix_after::*;
mod commit;
mod validate;
pub(super) use commit::*;
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
pub(super) use agent::*;
mod ladder;
pub use ladder::*;
mod logs;
mod sibling;
mod spec_edits;
use spec_edits::*;
mod snapshot;
use snapshot::*;
mod rejection;
use rejection::*;
mod apply_turn;
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
use carry::*;
mod prompt_notes;
pub(crate) use prompt_notes::*;

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
    // a usable skeleton. The code tries `parse_release_with_tarball("")`
    // first, gets `Error::Malformed`, and surfaces a hard
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
            console_scripts: sdist_info.console_scripts.clone(),
            install_entries: crate::spec::sdist_files_entries(
                &sdist_info.files,
                &sdist_top_level,
                sdist_info.project_dir.as_deref(),
            ),
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
/// write a provenance record alongside the spec. The only check is that a wheel
/// exists; the agent's claim that the source cannot be built is not verified
/// here. We cannot verify "source builds are
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
    try_wheel_fallback_with(config, checkout_dir, spec_path, step, &fetch_release_json)
}

/// How the wheel switch learns what PyPI has for a release: the JSON of
/// `https://pypi.org/pypi/<name>/<version>/json`. Injected so a test drives the
/// switch without the network.
type PypiLookup<'a> = &'a dyn Fn(&PipelineConfig) -> Result<String, String>;

/// The release's PyPI JSON, from the network.
fn fetch_release_json(config: &PipelineConfig) -> Result<String, String> {
    let pypi_url = pypi_json_url(&config.pkg_name, &config.version);
    crate::http::read_text(crate::http::get_with_retries(&pypi_url).map_err(|e| {
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
    })
}

fn try_wheel_fallback_with(
    config: &PipelineConfig,
    checkout_dir: &Path,
    spec_path: &Path,
    step: &StepDir,
    lookup: PypiLookup<'_>,
) -> Result<ap_ecosystem::pypi::WheelFile, String> {
    let json = lookup(config)?;
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
                false,
            ) {
                Some(Ok((_, first))) => Ok(first),
                Some(Err(
                    GenSpecError::Other(m)
                    | GenSpecError::NoReleaseArchive(m)
                    | GenSpecError::DiskFull(m),
                )) => Err(format!("per-flavor wheel spec generation failed: {m}")),
                Some(Err(GenSpecError::WheelOnly { .. })) | None => {
                    Err(format!("no usable upstream wheel either: {e}"))
                }
            };
        }
    };
    generate_wheel_binary_spec(config, &wheel, &json, checkout_dir, spec_path, step, false)
        .map_err(|e| match e {
            GenSpecError::Other(msg)
            | GenSpecError::NoReleaseArchive(msg)
            | GenSpecError::DiskFull(msg) => {
                format!("wheel spec generation failed: {msg}")
            }
            GenSpecError::WheelOnly { .. } => {
                "wheel spec generation failed unexpectedly".to_string()
            }
        })?;
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
/// Marker written to the notes when the wheel switch was deferred in favour
/// of npm vendoring, so the second request goes through.
const NPM_VENDOR_NOTE_MARKER: &str = "[npm-vendor-route-suggested]";

/// Whether a `.tar.gz` sdist ships a `Cargo.toml` at its top level.
fn tarball_has_root_cargo_toml(tarball: &Path) -> bool {
    let Ok(file) = std::fs::File::open(tarball) else {
        return false;
    };
    let mut ar = ap_ecosystem::sdist::bounded_tar_gz(file);
    let Ok(entries) = ar.entries() else {
        return false;
    };
    entries
        .take(ap_ecosystem::sdist::SCAN_MAX_ENTRIES)
        .flatten()
        .any(|e| {
            e.path()
                .is_ok_and(|p| p.components().count() == 2 && p.ends_with("Cargo.toml"))
        })
}

/// Whether a `.tar.gz` sdist ships a `Cargo.lock` at its top level, or next to
/// one of `manifests` (paths relative to the archive root).
fn tarball_has_cargo_lock(tarball: &Path, manifests: &[String]) -> bool {
    let Ok(file) = std::fs::File::open(tarball) else {
        return false;
    };
    let mut ar = ap_ecosystem::sdist::bounded_tar_gz(file);
    let Ok(entries) = ar.entries() else {
        return false;
    };
    entries
        .take(ap_ecosystem::sdist::SCAN_MAX_ENTRIES)
        .flatten()
        .any(|e| {
            e.path().is_ok_and(|p| {
                if !p.ends_with("Cargo.lock") {
                    return false;
                }
                let rel: PathBuf = p.components().skip(1).collect();
                rel.parent().is_some_and(|dir| {
                    dir.as_os_str().is_empty()
                        || manifests.iter().any(|m| Path::new(m).parent() == Some(dir))
                })
            })
        })
}

/// Whether a `.tar.gz` sdist ships a `package-lock.json` (the input the
/// `node_modules` service reads; it cannot use pnpm-lock.yaml or yarn.lock).
fn tarball_has_npm_lockfile(tarball: &Path) -> bool {
    let Ok(file) = std::fs::File::open(tarball) else {
        return false;
    };
    let mut ar = ap_ecosystem::sdist::bounded_tar_gz(file);
    let Ok(entries) = ar.entries() else {
        return false;
    };
    entries
        .take(ap_ecosystem::sdist::SCAN_MAX_ENTRIES)
        .flatten()
        .any(|e| {
            e.path().is_ok_and(|p| {
                p.file_name().is_some_and(|n| n == "package-lock.json")
                    && !p.components().any(|c| c.as_os_str() == "node_modules")
            })
        })
}

/// The contents of the shallowest `package-lock.json` of a `.tar.gz` sdist that
/// is not inside a `node_modules` directory.
fn tarball_npm_lockfile(tarball: &Path) -> Option<Vec<u8>> {
    use std::io::Read;
    let file = std::fs::File::open(tarball).ok()?;
    let mut ar = ap_ecosystem::sdist::bounded_tar_gz(file);
    let mut best: Option<(usize, Vec<u8>)> = None;
    for mut entry in ar
        .entries()
        .ok()?
        .take(ap_ecosystem::sdist::SCAN_MAX_ENTRIES)
        .flatten()
    {
        let Ok(path) = entry.path().map(|p| p.into_owned()) else {
            continue;
        };
        if path.file_name().is_none_or(|n| n != "package-lock.json")
            || path.components().any(|c| c.as_os_str() == "node_modules")
            || !entry.header().entry_type().is_file()
        {
            continue;
        }
        let depth = path.components().count();
        if best.as_ref().is_some_and(|(d, _)| *d <= depth) {
            continue;
        }
        let mut bytes = Vec::new();
        if entry
            .by_ref()
            .take(crate::stage::MAX_STAGE_BYTES + 1)
            .read_to_end(&mut bytes)
            .is_ok()
            && bytes.len() as u64 <= crate::stage::MAX_STAGE_BYTES
        {
            best = Some((depth, bytes));
        }
    }
    best.map(|(_, b)| b)
}

/// The `node_modules` service entry of the npm vendoring route.
const NODE_MODULES_SERVICE_ENTRY: &str = "  <service name=\"node_modules\" mode=\"manual\">\n\
     \x20   <param name=\"input\">package-lock.json</param>\n\
     \x20 </service>\n";

/// `_service` text with the `node_modules` service added; `None` when it
/// already has one or is not a `<services>` document.
fn with_node_modules_service(existing: Option<&str>) -> Option<String> {
    match existing {
        None => Some(format!(
            "<services>\n{NODE_MODULES_SERVICE_ENTRY}</services>\n"
        )),
        Some(xml) if xml.contains("name=\"node_modules\"") => None,
        Some(xml) => {
            let at = xml.rfind("</services>")?;
            Some(format!(
                "{}{NODE_MODULES_SERVICE_ENTRY}{}",
                &xml[..at],
                &xml[at..]
            ))
        }
    }
}

/// The checkout files the npm vendoring route writes (see
/// [`stage_npm_vendor_route`]).
const NPM_VENDOR_FILES: [&str; 2] = ["package-lock.json", "_service"];

/// The file work of the npm vendoring route, done by the pipeline: copy the
/// sdist's `package-lock.json` into the checkout and add the `node_modules`
/// service to `_service`. Returns what was done, for the notes.
fn stage_npm_vendor_route(checkout_dir: &Path, version: &str) -> Result<Vec<String>, String> {
    let tarball = find_tarball(checkout_dir, version)
        .ok_or_else(|| "the checkout has no source archive".to_string())?;
    let lock = tarball_npm_lockfile(&tarball)
        .ok_or_else(|| format!("{} has no usable package-lock.json", tarball.display()))?;
    let mut done = Vec::new();
    let dest = checkout_dir.join("package-lock.json");
    if std::fs::read(&dest).ok().as_deref() != Some(lock.as_slice()) {
        ap_core::fsx::write_atomic(&dest, &lock)
            .map_err(|e| format!("writing package-lock.json: {e}"))?;
    }
    done.push("copied package-lock.json from the sdist into the checkout".to_string());
    let service = checkout_dir.join("_service");
    let existing = std::fs::read_to_string(&service).ok();
    match with_node_modules_service(existing.as_deref()) {
        Some(updated) => {
            ap_core::fsx::write_atomic(&service, updated)
                .map_err(|e| format!("writing _service: {e}"))?;
            done.push("added the `node_modules` service to `_service`".to_string());
        }
        None if existing
            .as_deref()
            .is_some_and(|x| x.contains("name=\"node_modules\"")) =>
        {
            done.push("found the `node_modules` service already in `_service`".to_string());
        }
        None => return Err("the existing _service is not a <services> document".to_string()),
    }
    Ok(done)
}

/// Whether the notes record an npm vendoring deferral (the marker
/// `npm_vendor_route_pending` reads).
fn notes_record_npm_deferral(notes_path: &Path) -> bool {
    std::fs::read_to_string(notes_path).is_ok_and(|n| n.contains(NPM_VENDOR_NOTE_MARKER))
}

/// The ladder round the mechanical wheel rung runs in: round 2, or two rounds
/// after a round that deferred to the npm vendoring route. The round in between
/// is an agent turn, so the npm route gets the turn it asked for before the
/// wheel switch is applied (the marker already in the notes would otherwise
/// turn the wheel switch through, with no npm attempt).
fn wheel_rung_round(npm_deferred_round: Option<usize>) -> usize {
    npm_deferred_round.map_or(2, |r| r + 2)
}

/// True when a `use_upstream_wheel` should first be turned into an npm
/// vendoring attempt: the sdist has a package-lock.json outside `node_modules`
/// and that redirect has not been given yet. Read from the sdist and the
/// notes, not from the agent's wording, so the route is taken once per package.
fn npm_vendor_route_pending(checkout_dir: &Path, notes_path: &Path, version: &str) -> bool {
    if std::fs::read_to_string(notes_path).is_ok_and(|n| n.contains(NPM_VENDOR_NOTE_MARKER)) {
        return false;
    }
    find_tarball(checkout_dir, version).is_some_and(|t| tarball_has_npm_lockfile(&t))
}

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
/// Writes `wheel-origin.txt` into `state/<pkg>/` and preserves the original
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
         `{rpm_name}.spec.pre-wheel-switch` in the package's state directory. Review both\n\
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
    // Kept in the package's state directory: files in the checkout are
    // "not committing" noise on every Commit step.
    // Through the workspace, which sanitizes the name: a raw `join` would let a
    // package name with `/` or `..` write outside the state root.
    let dir = Workspace::open(&config.state_root, rpm_name)?
        .dir()
        .to_path_buf();
    ap_core::fsx::write_atomic(&dir.join("wheel-origin.txt"), provenance)?;
    ap_core::fsx::write_atomic(
        &dir.join(format!(
            "{}.spec.pre-wheel-switch",
            ap_core::run::sanitize_name(rpm_name)
        )),
        original_spec,
    )?;
    Ok(())
}

/// Repackage a prebuilt wheel as-is, for a release with no sdist and no
/// discoverable source repository — a closed-source binary redistribution
/// (NVIDIA's individual CUDA toolkit component wheels are the running
/// example: nvcc, cupti, nvrtc, cudart have no public source at all). See
/// convention 28 Exception 3 in the agent prompt.
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
            generate_wheel_binary_spec(config, &wheel, json, checkout_dir, spec_path, step, true)
        }
        Err(_) => {
            match generate_multi_flavor_wheel_spec(
                config,
                json,
                checkout_dir,
                spec_path,
                step,
                true,
            ) {
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
    no_sdist: bool,
) -> Option<Result<(GenSpecResult, ap_ecosystem::pypi::WheelFile), GenSpecError>> {
    let flavors = match config.python_flavors.try_effective() {
        Ok(flavors) => flavors,
        Err(e) => return Some(Err(GenSpecError::Other(e))),
    };
    if flavors.len() < 2 {
        return None;
    }
    // Each flavor paired with its ABI tag. A flavor without one has no wheel to
    // pick, so the per-flavor layout does not apply.
    let mut flavor_abis: Vec<(String, String)> = Vec::with_capacity(flavors.len());
    for flavor in &flavors {
        let Some(abi) = crate::flavors::abi_tag_for_flavor(flavor) else {
            return None;
        };
        flavor_abis.push((flavor.clone(), abi));
    }
    let abis: Vec<String> = flavor_abis.iter().map(|(_, abi)| abi.clone()).collect();
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
        if picked.len() != flavor_abis.len() {
            return Err(GenSpecError::Other(format!(
                "picked {} wheel(s) for {} flavor(s)",
                picked.len(),
                flavor_abis.len()
            )));
        }
        let mut wheels: Vec<(String, String)> = Vec::new();
        let mut first_path = None;
        for ((flavor, _), (_, wheel)) in flavor_abis.iter().zip(&picked) {
            let path = wheel
                .fetch(checkout_dir)
                .map_err(|e| GenSpecError::Other(format!("wheel download failed: {e}")))?;
            first_path.get_or_insert(path);
            wheels.push((flavor.clone(), wheel.filename.clone()));
        }
        let Some((_, first_wheel)) = wheels.first().cloned() else {
            return Err(GenSpecError::Other(
                "no wheel was picked for any flavor".into(),
            ));
        };
        let release = ap_ecosystem::pypi::parse_release_with_tarball(json, &first_wheel)
            .map_err(|e| GenSpecError::Other(format!("PyPI parse failed: {e}")))?;
        let root_is_purelib = first_path
            .as_deref()
            .and_then(ap_ecosystem::sdist::wheel_root_is_purelib);
        let layout = first_path
            .as_deref()
            .and_then(ap_ecosystem::sdist::wheel_layout);
        let spec_text =
            crate::spec::strip_ap_fix_comments(&crate::spec::wheel_multi_flavor_skeleton_shared(
                &config.pkg_name,
                &config.version,
                &wheels,
                &release,
                root_is_purelib,
                layout.as_ref(),
                &crate::spec::sibling_wheels(
                    &config.rpms_dir,
                    checkout_dir.parent(),
                    &config.pkg_name,
                ),
            ));
        ap_core::fsx::write_atomic(spec_path, &spec_text)
            .map_err(|e| GenSpecError::Other(format!("writing spec failed: {e}")))?;
        let spec_text = crate::spec::with_pip_ignore_requires_python(
            &spec_text,
            release.requires_python.as_deref(),
            &flavors,
        );
        let spec_text =
            with_wheel_spec_notes(config, &spec_text, &release, layout.as_ref(), no_sdist);
        // As for the single-wheel spec: a wheel without a licence text, switched
        // to from a sdist that has one, ships the sdist's as Source1.
        let spec_text = match layout
            .as_ref()
            .filter(|_| !no_sdist)
            .and_then(|l| crate::spec::copy_sdist_license(checkout_dir, &config.version, l))
        {
            Some(file) => crate::spec::with_license_source(&spec_text, &file),
            None => spec_text,
        };
        ap_core::fsx::write_atomic(spec_path, &spec_text)
            .map_err(|e| GenSpecError::Other(format!("writing spec failed: {e}")))?;
        write_or_warn(step.file("spec.generated"), &spec_text);
        write_wheel_rpmlintrc(
            checkout_dir,
            &config.pkg_name,
            &first_wheel,
            layout.as_ref(),
        );
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

/// Write the wheel's `python-<name>.rpmlintrc` next to the spec when it is a
/// vendor-binary wheel (see [`crate::spec::wheel_rpmlintrc`]).
fn write_wheel_rpmlintrc(
    checkout_dir: &Path,
    pkg_name: &str,
    wheel_filename: &str,
    layout: Option<&ap_ecosystem::sdist::WheelLayout>,
) {
    match crate::spec::wheel_rpmlintrc(pkg_name, wheel_filename, layout) {
        Some((file, text)) => write_or_warn(checkout_dir.join(file), text),
        // A zero-byte rpmlintrc left by an earlier run is a Source99 that
        // does nothing: remove it.
        None => {
            let stale = checkout_dir.join(format!("python-{}.rpmlintrc", pkg_name.to_lowercase()));
            if std::fs::metadata(&stale).is_ok_and(|m| m.len() == 0) {
                let _ = std::fs::remove_file(stale);
            }
        }
    }
}

/// The notes a repackaged-wheel spec carries whichever generator wrote it (one
/// wheel, or one per flavor): the no-sdist note for a true no-sdist release, and
/// for a prebuilt vendor library a note that it is not a Python package, with a
/// warning when its licence is not open source.
fn with_wheel_spec_notes(
    config: &PipelineConfig,
    spec_text: &str,
    release: &ap_ecosystem::pypi::Release,
    layout: Option<&ap_ecosystem::sdist::WheelLayout>,
    no_sdist: bool,
) -> String {
    // The note about a missing sdist is only true of a release with none.
    let spec_text = if no_sdist {
        crate::spec::with_no_sdist_note(spec_text)
    } else {
        spec_text.to_string()
    };
    // A wheel that is only a vendor's shared library is a C library carried
    // through PyPI, not a Python package. It is repackaged only because a
    // Python package loads it from site-packages; say so in the description,
    // and flag a licence that is not open source.
    let library_only = layout.is_some_and(|l| l.is_library_only());
    if library_only {
        let proprietary = crate::spec::license_is_proprietary(&release.license);
        ap_core::diag!(
            "warning: {} {} is a prebuilt vendor library, not a Python package{}",
            config.pkg_name,
            config.version,
            if proprietary {
                "; its licence is not open source, so this redistributes proprietary binaries"
            } else {
                ""
            }
        );
        crate::spec::note_prebuilt_library(&spec_text, proprietary)
    } else {
        spec_text
    }
}

fn generate_wheel_binary_spec(
    config: &PipelineConfig,
    wheel: &ap_ecosystem::pypi::WheelFile,
    pypi_json: &str,
    checkout_dir: &Path,
    spec_path: &Path,
    step: &StepDir,
    no_sdist: bool,
) -> Result<GenSpecResult, GenSpecError> {
    ap_core::diag!(
        "info: {} {} {} — repackaging the upstream wheel {}",
        config.pkg_name,
        config.version,
        if no_sdist {
            "has no sdist and no source repository"
        } else {
            "is switched to its wheel"
        },
        wheel.filename,
    );

    let wheel_path = wheel
        .fetch(checkout_dir)
        .map_err(|e| GenSpecError::Other(format!("wheel download failed: {e}")))?;
    let root_is_purelib = ap_ecosystem::sdist::wheel_root_is_purelib(&wheel_path);
    let layout = ap_ecosystem::sdist::wheel_layout(&wheel_path);

    let release = ap_ecosystem::pypi::parse_release_with_tarball(pypi_json, &wheel.filename)
        .map_err(|e| GenSpecError::Other(format!("PyPI parse failed: {e}")))?;

    // Other wheels of the project already built into the same directories
    // (convention 83): co-owned, not listed whole.
    let siblings =
        crate::spec::sibling_wheels(&config.rpms_dir, checkout_dir.parent(), &config.pkg_name);
    let spec_text = crate::spec::wheel_binary_skeleton_shared(
        &config.pkg_name,
        &config.version,
        &wheel.filename,
        &release,
        root_is_purelib,
        layout.as_ref(),
        &siblings,
    );
    let spec_text = crate::spec::strip_ap_fix_comments(&spec_text);
    // A wheel whose Requires-Python is capped below a built flavor needs pip told
    // so in both sections that run it, without waiting for a review turn.
    // An unreadable flavor setup is an error, not an empty list that would
    // leave the pip ignore out of the spec without a word.
    let flavors = config
        .python_flavors
        .try_effective()
        .map_err(GenSpecError::Other)?;
    let spec_text = crate::spec::with_pip_ignore_requires_python(
        &spec_text,
        release.requires_python.as_deref(),
        &flavors,
    );
    let spec_text = with_wheel_spec_notes(config, &spec_text, &release, layout.as_ref(), no_sdist);
    // A wheel without a licence text, switched to from a sdist that has one: the
    // sdist's is shipped as Source1 and owned as %license.
    let spec_text = match layout
        .as_ref()
        .filter(|_| !no_sdist)
        .and_then(|l| crate::spec::copy_sdist_license(checkout_dir, &config.version, l))
    {
        Some(file) => crate::spec::with_license_source(&spec_text, &file),
        None => spec_text,
    };
    ap_core::fsx::write_atomic(spec_path, &spec_text)
        .map_err(|e| GenSpecError::Other(format!("writing spec failed: {e}")))?;
    write_or_warn(step.file("spec.generated"), &spec_text);
    write_wheel_rpmlintrc(
        checkout_dir,
        &config.pkg_name,
        &wheel.filename,
        layout.as_ref(),
    );

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

    // Every value lands in element text: a `&` or `<` in a URL or a tag would
    // make the file malformed or add elements.
    let (git_url, repo_name, version) = (
        crate::obs_xml::escape(&git_url),
        crate::obs_xml::escape(repo_name),
        crate::obs_xml::escape(version),
    );
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
///
/// `locked` is whether the sdist ships a `Cargo.lock`. Then the vendored registry
/// must match it: `update=true` runs `cargo update`, which re-resolves every
/// dependency, picks releases the crate was never built with (chromadb got a
/// `zip` that broke `zip-extract`) and fails outright when a requirement has
/// only yanked releases left (`zip = "^2"`).
fn render_maturin_service_file(
    normalized_name: &str,
    extra_manifests: &[String],
    locked: bool,
    root_manifest: bool,
) -> String {
    // The manifest path comes from the sdist's pyproject.toml.
    let normalized_name = crate::obs_xml::escape(normalized_name);
    let update = !locked;
    let extra: String = extra_manifests
        .iter()
        .filter(|m| m.as_str() != "Cargo.toml")
        .map(|m| {
            format!(
                "         \x20    <param name=\"cargotoml\">{}</param>\n",
                crate::obs_xml::escape(m)
            )
        })
        .collect();
    let root = if root_manifest {
        "         \x20    <param name=\"cargotoml\">Cargo.toml</param>\n"
    } else {
        ""
    };
    format!(
        "<?xml version=\"1.0\"?>\n\
         <services>\n\
         \x20 <service mode=\"manual\" name=\"download_files\" />\n\
         \x20 <service name=\"cargo_vendor\" mode=\"manual\">\n\
         \x20    <param name=\"src\">{normalized_name}-*.tar.gz</param>\n\
         \x20    <param name=\"method\">registry</param>\n\
         \x20    <param name=\"update\">{update}</param>\n\
         \x20    <param name=\"compression\">zst</param>\n\
         \x20    <param name=\"no-root-manifest\">false</param>\n\
         {root}{extra}\
         \x20 </service>\n\
         </services>\n"
    )
}

/// The source paths a unified or git diff names, as `patch -p1` resolves them:
/// the `---` and `+++` names and the `diff --git a/x b/y` names with their first
/// component dropped, and the `rename`/`copy` names (source and destination).
/// `/dev/null` (a created or removed file) names nothing.
///
/// `rename from`/`copy from` names are written without the `a/`/`b/` prefix, and
/// whether `patch` drops a component from them is not something to rely on, so
/// both the name as written and the one with its first component dropped are
/// targets. Naming one file too many only extracts it; naming the source side
/// too few fails the dry run of a valid pure rename.
pub(crate) fn patch_targets(diff: &str) -> Vec<String> {
    let mut stripped_names: Vec<&str> = Vec::new();
    let mut bare_names: Vec<&str> = Vec::new();
    for line in diff.lines() {
        if let Some(name) = line
            .strip_prefix("--- ")
            .or_else(|| line.strip_prefix("+++ "))
        {
            stripped_names.push(name.split('\t').next().unwrap_or("").trim());
        } else if let Some(rest) = line.strip_prefix("diff --git ") {
            stripped_names.extend(rest.split_whitespace());
        } else if let Some(name) = ["rename from ", "rename to ", "copy from ", "copy to "]
            .iter()
            .find_map(|p| line.strip_prefix(p))
        {
            bare_names.push(name.trim());
        }
    }
    stripped_names
        .into_iter()
        .filter(|name| *name != "/dev/null")
        .map(drop_first_component)
        .chain(
            bare_names
                .into_iter()
                .filter(|name| !name.is_empty() && *name != "/dev/null")
                .flat_map(|name| [name.to_string(), drop_first_component(name)]),
        )
        .filter(|rest| !rest.is_empty())
        .collect()
}

/// A patch name with its first directory component dropped (`-p1`). A name with
/// no directory part has none to drop, so it is its own target (`--- setup.py.orig`
/// / `+++ setup.py` patches `setup.py`).
fn drop_first_component(name: &str) -> String {
    match name.split_once('/') {
        Some((_, rest)) => rest.to_string(),
        None => name.to_string(),
    }
}

/// The `[tool.maturin] manifest-path` of an sdist's root `pyproject.toml`,
/// relative to the archive root, when it names a manifest other than the root
/// `Cargo.toml`. `Ok(None)` when absent, default, or the manifest is itself a
/// member of the root `Cargo.toml`'s workspace; `Err` when the archive cannot
/// be unpacked (a refused link, a size limit, a corrupt file). The
/// root manifest's own `cargotoml` param already vendors every workspace
/// member through their one shared lockfile, and `obs-service-cargo` errors
/// out (bare `NotFound`, os error 2) when told to additionally vendor a
/// member path, since it looks for a lockfile next to that manifest instead
/// of at the workspace root.
fn maturin_manifest_path(tarball: &Path) -> Result<Option<String>, String> {
    let scratch = tempfile::tempdir().map_err(|e| e.to_string())?;
    // Only the two root manifests are needed; unpacking the whole sdist for
    // them is what this does instead.
    ap_ecosystem::sdist::extract_where(tarball, scratch.path(), |p| {
        let depth = p
            .components()
            .filter(|c| matches!(c, std::path::Component::Normal(_)))
            .count();
        depth <= 2
            && matches!(
                p.file_name().and_then(|n| n.to_str()),
                Some("pyproject.toml" | "Cargo.toml")
            )
    })
    .map_err(|e| e.to_string())?;
    Ok(maturin_manifest_in(scratch.path()))
}

/// The manifest path named by the unpacked sdist at `scratch`, if any (see
/// [`maturin_manifest_path`]).
fn maturin_manifest_in(scratch: &Path) -> Option<String> {
    let entries: Vec<_> = std::fs::read_dir(scratch)
        .ok()?
        .filter_map(|e| e.ok())
        .collect();
    let root = match entries.as_slice() {
        [only] if only.path().is_dir() => only.path(),
        _ => scratch.to_path_buf(),
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

/// What the turn read to reach its edit (log lines, file excerpts), at most
/// three items of 200 characters each, as an `Evidence:` line for the journal.
/// The notes agent summarises the journal and otherwise has only the reasoning,
/// so its notes say "the exact error is not recorded".
fn evidence_lines(evidence: &[String]) -> String {
    let shown: Vec<String> = evidence
        .iter()
        .take(3)
        .map(|e| {
            let flat = e.split_whitespace().collect::<Vec<_>>().join(" ");
            match flat.char_indices().nth(200) {
                Some((at, _)) => format!("{}...", &flat[..at]),
                None => flat,
            }
        })
        .filter(|e| !e.is_empty())
        .collect();
    if shown.is_empty() {
        String::new()
    } else {
        format!("Evidence: {}\n", shown.join(" | "))
    }
}

/// A turn's reasoning on one line of at most 400 characters, for the notes.
fn brief(text: &str) -> String {
    let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
    match flat.char_indices().nth(400) {
        Some((at, _)) => format!("{}...", &flat[..at]),
        None => flat,
    }
}

fn append_notes(notes_path: PathBuf, section: &str) {
    // Append, never read-modify-write: a failed read would replace the whole
    // journal with just this section.
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
///   would otherwise say "not there, remove it", collapsing a perfectly valid
///   workspace to `members = []`. `member_resolves` now expands single-
///   segment wildcards and keeps the member when any filesystem entry
///   matches the pattern.
///
/// * **Original tarball preserved**. The repack must not overwrite the
///   downloaded archive in place, destroying the pristine SHA256 identity
///   the caller had just verified. The original is now kept as
///   `<name>.orig.tar.gz` next to the modified one so provenance survives.
///
/// * **Audit trail**. When any member is removed, a `cargo-workspace-fix.
///   txt` is written alongside the tarball naming each removed member;
///   that file lands in the OBS commit so the reviewer sees what changed.
fn fix_cargo_workspace_in_tarball(tarball: &std::path::Path, pkg_name: &str) -> Result<(), String> {
    // Refuse before extracting: a non-gzip archive would be rewritten as gzip.
    require_gzip_tarball(tarball)?;

    // Extract to temp directory
    let temp_dir = tempfile::tempdir().map_err(|e| e.to_string())?;
    let extract_dir = temp_dir.path();

    ap_core::diag!("  checking for broken Cargo workspace in tarball");

    // Extract tarball. The sdist extractor refuses links that could reach
    // outside `extract_dir`, so nothing below can read or write past it.
    ap_ecosystem::sdist::extract(tarball, extract_dir)
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
        // Only a real file is a manifest: a link named Cargo.toml must not
        // be read or rewritten through.
        if !entry.file_type().is_file() {
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
    // Fatal: the repack below renames over the original, so a missing backup
    // would leave the modification with no way back to the bytes it replaced.
    let orig_backup = tarball.with_file_name(format!(
        "{}.orig",
        tarball
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default()
    ));
    if let Err(e) = std::fs::copy(tarball, &orig_backup) {
        return Err(format!(
            "could not back up original tarball to {}: {e}",
            orig_backup.display()
        ));
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
    repack_tarball(extract_dir, tarball)?;

    ap_core::diag!("  ✓ tarball updated with fixed workspace");
    Ok(())
}

/// Pack `extract_dir` into `tarball`, replacing it.
///
/// Links are recorded as links, never followed: a symlink in the tree must
/// not turn into a copy of whatever it points at on this host. Anything that
/// is neither a file, a directory nor a link is left out.
fn repack_tarball(extract_dir: &Path, tarball: &Path) -> Result<(), String> {
    require_gzip_tarball(tarball)?;
    let temp_tarball = tarball.with_extension("tar.gz.tmp");
    let result = write_gzip_tarball(extract_dir, &temp_tarball).and_then(|()| {
        std::fs::rename(&temp_tarball, tarball).map_err(|e| format!("replacing tarball: {}", e))
    });
    if result.is_err() {
        // Never leave a partial temp archive behind.
        let _ = std::fs::remove_file(&temp_tarball);
    }
    result
}

/// Repacking always writes gzip, so only a `.tar.gz` original may be replaced.
/// Anything else is refused and left untouched: no bz2/zstd/xz encoder is a
/// dependency of this crate, and silently changing the format would break the
/// file name the spec refers to.
fn require_gzip_tarball(tarball: &Path) -> Result<(), String> {
    if tarball.to_string_lossy().ends_with(".tar.gz") {
        Ok(())
    } else {
        Err(format!(
            "{} is not a .tar.gz archive; not repacked, left as downloaded",
            tarball.display()
        ))
    }
}

fn write_gzip_tarball(extract_dir: &Path, temp_tarball: &Path) -> Result<(), String> {
    let tar_file =
        std::fs::File::create(temp_tarball).map_err(|e| format!("creating temp tarball: {}", e))?;
    let enc = flate2::write::GzEncoder::new(tar_file, flate2::Compression::default());
    let mut tar_builder = tar::Builder::new(enc);
    tar_builder.follow_symlinks(false);

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
        let file_type = entry.file_type();

        if file_type.is_file() || file_type.is_symlink() {
            tar_builder
                .append_path_with_name(path, relative)
                .map_err(|e| format!("adding file to tarball: {}", e))?;
        } else if file_type.is_dir() {
            tar_builder
                .append_dir(relative, path)
                .map_err(|e| format!("adding dir to tarball: {}", e))?;
        }
    }

    tar_builder
        .finish()
        .map_err(|e| format!("finalizing tarball: {}", e))?;
    // The gzip trailer is written by `finish`, not by dropping the encoder,
    // where a failure would go unseen and a truncated archive replace the
    // original.
    tar_builder
        .into_inner()
        .and_then(|enc| enc.finish())
        .and_then(|file| file.sync_all())
        .map_err(|e| format!("finalizing tarball: {}", e))?;
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
///
/// A member with a `*` in a non-final segment (`crates/*/bindings`) spans
/// several directories, which this does not expand, so it is kept as it is
/// (`true`) rather than judged missing and removed from the manifest.
fn member_resolves(base_dir: &Path, member: &str) -> bool {
    if member
        .split('/')
        .rev()
        .skip(1)
        .any(|segment| segment.contains('*'))
    {
        return true;
    }
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
    let pieces: Vec<&str> = pattern.split('*').collect();
    let (first, last) = (pieces[0], pieces[pieces.len() - 1]);
    // The ends are anchored (and must not overlap); only the pieces between
    // them float, in order.
    if candidate.len() < first.len() + last.len()
        || !candidate.starts_with(first)
        || !candidate.ends_with(last)
    {
        return false;
    }
    let mut rest = &candidate[first.len()..candidate.len() - last.len()];
    for part in &pieces[1..pieces.len() - 1] {
        match rest.find(part) {
            Some(idx) => rest = &rest[idx + part.len()..],
            None => return false,
        }
    }
    true
}

/// Whether a lowercased package name is NVIDIA's, by its name: `nvidia-`,
/// `nvidia_`, `cuda-`, `cuda_` or the bare `nvidia`. A bare `nv` prefix is not
/// a signal (`nvim`, `nvector` are unrelated).
fn is_nvidia_package_name(lower: &str) -> bool {
    ["nvidia-", "nvidia_", "cuda-", "cuda_"]
        .iter()
        .any(|p| lower.starts_with(p))
        || lower == "nvidia"
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
    // nvidia-cuda-nvcc are not mapped here: NVIDIA/cuda-python
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
            if verify_repo_exists(repo_url) == RepoProbe::Exists {
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
    // The NVIDIA-org repositories derived from the name, whatever their probe
    // said. Only these are vouched for as NVIDIA's: a search hit under the same
    // org is a search result like any other.
    let mut from_name: Vec<String> = Vec::new();
    let is_nvidia_package = is_nvidia_package_name(&lower)
        || known_mappings
            .iter()
            .any(|(p, u)| lower.starts_with(p) && u.starts_with("https://github.com/NVIDIA/"));

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
        from_name.push(nvidia_url.clone());
        ap_core::diag!("  checking NVIDIA org: {}", nvidia_url);
        if verify_repo_exists(&nvidia_url) == RepoProbe::Exists {
            ap_core::diag!("  ✓ verified NVIDIA repo: {}", nvidia_url);
            verified_candidates.insert(0, nvidia_url.clone());
        } else {
            candidates.insert(0, nvidia_url);
        }

        // Also try without cuda- prefix for packages like cuda-python
        if let Some(after_cuda) = clean.strip_prefix("cuda-") {
            let nvidia_url2 = format!("https://github.com/NVIDIA/{}", after_cuda);
            from_name.push(nvidia_url2.clone());
            ap_core::diag!(
                "  checking NVIDIA org (without cuda- prefix): {}",
                nvidia_url2
            );
            if verify_repo_exists(&nvidia_url2) == RepoProbe::Exists {
                ap_core::diag!("  ✓ verified NVIDIA repo: {}", nvidia_url2);
                verified_candidates.insert(0, nvidia_url2.clone());
            } else {
                candidates.insert(0, nvidia_url2);
            }
        }

        // Try the exact package name under NVIDIA org
        let nvidia_exact = format!("https://github.com/NVIDIA/{}", pkg_name);
        from_name.push(nvidia_exact.clone());
        if nvidia_exact != format!("https://github.com/NVIDIA/{}", clean) {
            ap_core::diag!("  checking NVIDIA org (exact name): {}", nvidia_exact);
            if verify_repo_exists(&nvidia_exact) == RepoProbe::Exists {
                ap_core::diag!("  ✓ verified NVIDIA repo: {}", nvidia_exact);
                verified_candidates.insert(0, nvidia_exact.clone());
            } else {
                candidates.insert(0, nvidia_exact);
            }
        }

        // For NVIDIA packages, skip GitHub API search to avoid non-official repos
        ap_core::diag!("  skipping GitHub API search for NVIDIA package (prefer official repos)");
    } else {
        // For non-NVIDIA packages, use GitHub API search. Not everything lives
        // on GitHub: ask Repology (the upstream other distributions record)
        // and GitLab/Codeberg search too. Their hits are real search results,
        // but a homepage can be stale, so they still have to answer a
        // reachability probe below. The searches are independent: run them
        // side by side.
        let (github, (others, distro)) = std::thread::scope(|s| {
            let github = s.spawn(|| search_github_repos(pkg_name));
            let forges = crate::discovery::search_other_forges(pkg_name);
            let github = github
                .join()
                .unwrap_or_else(|e| std::panic::resume_unwind(e));
            (github, forges)
        });
        if let Some(urls) = github {
            for url in urls {
                if !verified_candidates.contains(&url) && !candidates.contains(&url) {
                    ap_core::diag!("  found via GitHub search: {}", url);
                    verified_candidates.push(url);
                }
            }
        }
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

    // Check all generated candidates, side by side. An owner/org/user page
    // (`github.com/oatpp`) is reachable but is not a repository: there is
    // nothing in it to package, so it is not probed at all.
    let mut to_check: Vec<String> = Vec::new();
    for url in &candidates {
        if !verified_candidates.contains(url)
            && crate::discovery::is_repository_url(url)
            && !to_check.contains(url)
        {
            to_check.push(url.clone());
        }
    }
    let probes = verify_repos_exist(&to_check);
    for url in &candidates {
        if !verified_candidates.contains(url) {
            ap_core::diag!("  checking: {}", url);
            if let Some(i) = to_check.iter().position(|c| c == url)
                && probes[i] == RepoProbe::Exists
            {
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

    // A repository that exists is not thereby the project's source. Only one
    // the project or its vendor vouches for is used without confirmation: a
    // curated mapping, the repository PyPI records for this project, the
    // upstream another distribution's spec records, or an NVIDIA-org
    // repository derived from the name. Anything else (typically a name
    // search hit) is kept as a guess, and is built only when the user
    // confirms it with `--source-url`.
    let recorded = recorded_upstream(pkg_name);
    let vouched = |url: &str| {
        is_vouched_for(
            url,
            known_mappings,
            recorded.as_deref(),
            distro_evidence.as_ref(),
            &from_name,
        )
    };
    let (trusted, unvetted): (Vec<String>, Vec<String>) =
        verified_candidates.into_iter().partition(|u| vouched(u));
    let mut verified_candidates = trusted;
    for url in unvetted {
        ap_core::diag!("  not used automatically (not the project's recorded upstream): {url}");
        if !candidates.contains(&url) {
            candidates.insert(0, url);
        }
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
    // validated before it is used (`validate_found_url`: a repository or
    // release index on a known host that really exists), and goes first, but
    // it is only a model's pick from the web: it is used without confirmation
    // only when it is on a host the project itself records (a distro spec's
    // upstream addresses, or PyPI's project URLs). Otherwise it joins the
    // guesses for the user to confirm.
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
            let recorded_sites: Vec<String> = distro_evidence
                .iter()
                .flat_map(|d| d.homepages.iter())
                .chain(recorded.iter())
                .cloned()
                .collect();
            if vouched(&url) || on_recorded_site(&url, &recorded_sites) {
                verified_candidates.retain(|c| c != &url);
                verified_candidates.insert(0, url);
            } else {
                ap_core::diag!(
                    "  agent suggested {url}, which no record of the project mentions: \
                     not used automatically"
                );
                candidates.retain(|c| c != &url);
                candidates.insert(0, url);
            }
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

/// The source repository PyPI records for `pkg_name`, from its project URLs.
fn recorded_upstream(pkg_name: &str) -> Option<String> {
    let url = format!(
        "https://pypi.org/pypi/{}/json",
        urlencoding::encode(pkg_name)
    );
    let body = crate::http::read_text(crate::http::get_with_retries(&url).ok()?).ok()?;
    ap_ecosystem::pypi::source_url_from_json(&body)
}

/// Whether two repository URLs name the same repository, ignoring case, a
/// trailing slash and a `.git` suffix.
fn same_repo_url(a: &str, b: &str) -> bool {
    let norm = |u: &str| {
        u.trim()
            .trim_end_matches('/')
            .trim_end_matches(".git")
            .to_ascii_lowercase()
    };
    norm(a) == norm(b)
}

/// Hosts that serve many unrelated projects, one project page or download
/// directory per name. A host match on these says nothing about the project.
const SHARED_HOSTS: &[&str] = &[
    "sourceforge.net",
    "savannah.gnu.org",
    "savannah.nongnu.org",
    "ftp.gnu.org",
];

/// Whether `host` is one of [`SHARED_HOSTS`] or a subdomain of one.
fn is_shared_host(host: &str) -> bool {
    SHARED_HOSTS
        .iter()
        .any(|s| host == *s || host.strip_suffix(s).is_some_and(|h| h.ends_with('.')))
}

/// The project a URL on a shared host names: the first path segment that is
/// not a generic directory (`/projects/<name>`, `/p/<name>`, `/gnu/<name>`,
/// `/releases/<name>`, `/cgit/<name>.git`). `None` when the path names none.
fn shared_host_project(url: &str) -> Option<String> {
    const GENERIC: &[&str] = &["projects", "p", "releases", "gnu", "cgit", "files", "pub"];
    let uri: ureq::http::Uri = url.parse().ok()?;
    uri.path()
        .split('/')
        .filter(|s| !s.is_empty())
        .find(|s| !GENERIC.contains(&s.to_ascii_lowercase().as_str()))
        .map(|s| s.trim_end_matches(".git").to_ascii_lowercase())
}

/// Whether `url` is on a site the project itself records in `recorded`: the
/// same host, or for a shared forge (github.com, gitlab.com ...) the same
/// owner, since a host shared by everyone says nothing about who owns the
/// repository. A shared host that serves many projects (SourceForge,
/// Savannah, ftp.gnu.org) needs the same project name as well.
fn on_recorded_site(url: &str, recorded: &[String]) -> bool {
    let Some(host) = crate::http::host_of(url) else {
        return false;
    };
    let owner = |u: &str| {
        ap_ecosystem::forge::parse(u).map(|f| {
            f.project_path
                .split('/')
                .next()
                .unwrap_or_default()
                .to_ascii_lowercase()
        })
    };
    recorded.iter().any(|r| {
        if crate::http::host_of(r).as_deref() != Some(host.as_str()) {
            return false;
        }
        if ap_ecosystem::forge::is_known_host(&host) {
            matches!((owner(url), owner(r)), (Some(a), Some(b)) if a == b)
        } else if is_shared_host(&host) {
            matches!(
                (shared_host_project(url), shared_host_project(r)),
                (Some(a), Some(b)) if a == b
            )
        } else {
            true
        }
    })
}

/// Whether discovery may use `url` without confirmation: a curated mapping,
/// the repository PyPI records for the project, what another distribution's
/// spec records as its upstream, or an NVIDIA-org repository derived from the
/// package name (`from_name`). A search hit that happens to live under the
/// NVIDIA org is not vouched for.
fn is_vouched_for(
    url: &str,
    known: &[(&str, &str)],
    recorded: Option<&str>,
    distro: Option<&crate::discovery::DistroInfo>,
    from_name: &[String],
) -> bool {
    known.iter().any(|(_, k)| same_repo_url(k, url))
        || recorded.is_some_and(|r| same_repo_url(r, url))
        || distro.is_some_and(|d| {
            d.forge_repos
                .iter()
                .chain(&d.index_candidates)
                .any(|r| same_repo_url(r, url))
        })
        || from_name.iter().any(|r| same_repo_url(r, url))
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
        // One malformed hit must not discard the others.
        let (Some(repo_name), Some(html_url)) = (
            item.get("name").and_then(|v| v.as_str()),
            item.get("html_url").and_then(|v| v.as_str()),
        ) else {
            continue;
        };

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

/// What a reachability probe of a repository URL established.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RepoProbe {
    /// The host answered and the repository is there.
    Exists,
    /// The host said the repository is not there (404 or 410).
    Missing,
    /// The host could not tell: a rate limit, a 5xx, a timeout or another
    /// transport failure. Not evidence that the repository exists.
    Unknown,
}

/// Classify the final answer of a probe. Only a definite answer from the host
/// counts; anything else is `Unknown`.
fn classify_repo_probe<T>(answer: &Result<T, ureq::Error>) -> RepoProbe {
    match answer {
        Ok(_) => RepoProbe::Exists,
        Err(ureq::Error::StatusCode(404 | 410)) => RepoProbe::Missing,
        Err(_) => RepoProbe::Unknown,
    }
}

/// Verify that a repository URL exists by making a HEAD request.
///
/// A probe that cannot tell (a rate limit, a 5xx, a timeout) is retried by the
/// request wrapper; if it still cannot tell the result is `Unknown`. Only
/// `Exists` may make a candidate verified. An `Unknown` candidate is kept as an
/// unverified guess, so it is not built automatically, and it is probed again
/// when the candidate list is checked.
fn verify_repo_exists(url: &str) -> RepoProbe {
    let answer = ap_ecosystem::net::call_with_retries(|| crate::http::head(url));
    let probe = classify_repo_probe(&answer);
    if probe == RepoProbe::Unknown {
        if let Err(e) = &answer {
            ap_core::diag!("  warning: could not check {url} ({e}); not used automatically");
        }
    }
    probe
}

/// [`verify_repo_exists`] for several URLs, each probe on its own thread.
fn verify_repos_exist(urls: &[String]) -> Vec<RepoProbe> {
    std::thread::scope(|s| {
        let probes: Vec<_> = urls
            .iter()
            .map(|url| s.spawn(move || verify_repo_exists(url)))
            .collect();
        probes
            .into_iter()
            .map(|p| p.join().unwrap_or_else(|e| std::panic::resume_unwind(e)))
            .collect()
    })
}

#[cfg(test)]
mod evidence_line_tests;

#[cfg(test)]
mod version_conflict_tests;

#[cfg(test)]
mod obs_baseline_tests;

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
mod settled_tests;

#[cfg(test)]
mod hygiene_tests;

#[cfg(test)]
mod ladder_tests;

#[cfg(test)]
mod rpm_query_tests;

#[cfg(test)]
mod failure_shape_tests;

#[cfg(test)]
mod patch_stage_tests;

#[cfg(test)]
mod flag_survival_tests;

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
