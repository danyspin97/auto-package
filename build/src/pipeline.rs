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
mod npm_vendor;
mod python_spec;
mod render;
mod wheel_spec;
mod wheel_switch;
use render::*;
mod classify;

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
