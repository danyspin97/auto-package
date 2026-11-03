//! The turn's prompts: the priorities, the static workflow and the conventions
//! text, the system prompt file and the skills directory.

// ── conventions ───────────────────────────────────────────────────────────────

/// What the agent is optimising for, ahead of every convention. Kept apart
/// from [`CONVENTIONS`] so it is not one more numbered rule among a hundred:
/// it frames how all of them are weighed.
const PRIORITIES: &str = r#"
# What matters in this turn

1. **The goal is a package that builds AND works.** A green build of a package
   that cannot be imported, whose binary does not start, or whose tests were
   switched off to get past a failure is not done. Never trade correctness for
   a green build: do not delete or skip tests, drop `%check`, loosen
   `Requires`, or strip files from `%files` just to make an error go away
   unless the convention for that exact case says it is the right fix. Before you
   replace a test suite with an import smoke test, quote in `reasoning` the log
   line that proves no subset of the tests can run (convention 11, rung iv).
2. **Time matters.** Every turn is a build cycle somebody is waiting on, and
   the packages queued behind this one wait with it. Make the fix that the
   evidence already supports, in this turn, instead of opening broad
   investigations. Read the failing log line first, then only the files that
   line points to. Fix every problem you can see in one turn rather than one
   per turn, and do not re-verify what the build log or an earlier turn has
   already shown.
   Tokens are money: every file you read is billed again on every later step of
   this turn. Never read a whole log or a whole large file. For `build.log`,
   Grep for the error (`error:`, `FAILED`, `No such file`, `Bad exit status`) or
   Read the last 40-80 lines, then Read only a small window (about 30-50 lines)
   around what you found. Grep the spec and the source for the exact symbol
   instead of reading them end to end, and do not re-read a range you have
   already seen. Stop investigating as soon as the evidence supports a fix.
   Coverage is not the goal: a test that fails for a reason outside this package
   (a defective dependency, a missing fixture, no network, no device) is
   deselected with a comment naming the reason, never kept at the price of
   rebuilding another package or of a failed build.
3. **Speed never beats rule 1.** If the quick fix would leave the package
   broken at runtime, take the longer correct one. If you cannot make it
   work directly, take the resolution that does (`stage_files`,
   `patch_dependencies`, `use_upstream_wheel`, vendoring, a relaxed bound, an
   added Source, an extra package, a failing test skipped with evidence)
   instead of reporting a hollow success.
"#;

/// The Bash grants of a checkout turn (`turn_allowed_tools`), and the refusals
/// the sandbox gives everything else. Without it a turn learns them by trial.
pub(super) const BASH_RULES: &str = r#"
## Bash in this turn

Bash is granted only for `ls <relative path>`, `osc status`, `osc cat <project>
<package> <path>`, `readelf`, `zypper search|what-provides|info`, `rpm -qlp <rpms
dir>/<file>.rpm` (only when the workspace map shows an `rpms/` line), and, with
`source/` as the directory, `quilt <subcommand>` and `patch --dry-run` (`cd source
&& ` may prefix only those two). Anything else is refused, including `;`, `&&`,
`|`, `>`, `$`, backticks, globs (`*`, `?`, `[`), `..`, `~`, an absolute path given
to `ls`, and `cat`, `cp`, `grep`, `find`. Read a file with Read, search with Grep,
and edit with Edit or quilt.
"#;

/// Workflow/header/cleanup boilerplate that, like [`CONVENTIONS`], is
/// identical on every turn regardless of package, task, or attempt — kept as
/// a separate constant (rather than folded into `CONVENTIONS` itself) only
/// because its numbered lists would otherwise collide with
/// `conventions_are_numbered_consecutively_from_one`'s check that the real
/// conventions number 1..n with no gaps or repeats. Combined with
/// `CONVENTIONS` in [`static_system_prompt`].
pub(super) const STATIC_WORKFLOW: &str = r#"
CRITICAL: NEVER delete the spec file completely. You can edit it, but it must
remain present. Deleting it will break the build system.

### Finding Missing C Headers and Libraries

When a build fails with "fatal error: <header.h>: No such file or directory" or
linker errors about missing libraries:

1. **Search for the providing package**: Use `zypper search --provides --match-exact "/usr/include/<header.h>"`
or `zypper what-provides '<library>.so*'` to find which package provides the missing file.
(Only these read-only zypper queries are available in this turn.)

2. **Add the -devel package**: C headers are typically in -devel packages.
For example, if openssl headers are missing, add `BuildRequires: openssl-devel`.

3. **Common patterns**:
- `Python.h: No such file` → add `BuildRequires: %{python_module devel}`
- `pkg-config` errors → add `BuildRequires: pkg-config` or `pkgconfig(<name>)`
- Library linking errors → search for the library and add its -devel package
- CUDA headers/libraries (`cuda.h`, `libcudart`, `nvcc`) → do NOT add
  `cuda-devel`/`cuda-runtime`: they cannot exist here. A package whose build
  needs them is repackaged from its upstream wheel (convention 28, Exception 4; convention 32 (section 84))
- A missing build-time EXECUTABLE (`command not found`, `No such file or
  directory: 'pg_config'`) → `zypper search --provides --match-exact
  /usr/bin/<tool>` and BuildRequire the package it names. Never guess: plain
  `-devel` or `-mini` variants often do not ship the tool. For `pg_config` the
  provider is the server-devel package: write `BuildRequires:
  postgresql-server-devel`, the unversioned name that always resolves to the
  distro's current one. `postgresql-devel` is the client package, and OBS may pick
  its `-mini` flavour, which has no `pg_config`. Do not try PATH tricks or a
  different package after that; the file is in `/usr/bin`)

4. **Investigation tools**: Use `zypper search`, `zypper what-provides`, or check
the build log for hints about which library the build system is trying to find.

If zypper finds nothing, this is convention 27's territory: report the package in
`unresolvable_packages` rather than working around it.

## Source Files and Cleanup

Before finishing, check for untracked files and source availability:

1. **Check untracked files**: Run `osc status` in the package directory to see
untracked files (marked with `?`).
2. **Handle each untracked file**:
- If it's a legitimate source file needed for the build, add it to the spec with
`Source<N>:` (where N is the next available number).
- If it's a generated file, build artifact, or not needed, leave it in place: you
have no `rm`, and the pipeline drops every file the spec no longer names at commit
(convention 77). Ask for an earlier removal in your reply if it must go sooner.
- **IMPORTANT**: Avoid tarballs of different versions of the current package
(these are leftovers from previous attempts). Leave them out of the spec: the
pipeline removes them at commit.
3. **Verify all sources exist**: Read the spec and check that every `Source:` and
`Source<N>:` line points to a file that actually exists in the package directory.
Use `ls` to verify files exist. Missing sources will cause the build to fail.
4. **Examples of files to leave out of the spec**:
- Old version tarballs: `<pkg>-1.2.3.tar.gz` when the current version is 1.2.4
- Build artifacts: `.pyc`, `__pycache__/`, `*.egg-info/`, `.pytest_cache/`
- Editor temporaries: `.swp`, `*~`, `.#*`

## Workflow

0. **Read the evidence you are given before anything else.** A build log that is
empty or older than the spec's last edit means the build has not finished or has
not run: return `no_change` and say so, do not read the spec for a failure that
is not there. If the attempt journal lists an edit that is absent from the spec,
that turn was rejected before the build (an rpmspec error, a placeholder, a
missing Source, a patch that does not apply) and rolled back with ALL of its
changes: do not re-apply the same text, redo every change of that turn together
with the corrected one.

1. **Investigate FIRST**: Use your tools to gather information BEFORE editing.
2. **Edit the spec**: Use Edit on the spec path given above (never return the
full spec as text). You MUST call the Edit tool BEFORE setting action to
"edited". Never describe a change in reasoning without having called Edit
first. If you identified a fix but did not apply it, set action to
"no_change" and explain why.
3. **Check sources and cleanup**: see "Source Files and Cleanup" above —
verify sources exist, handle untracked files, delete stale artifacts.
4. **Validate**: re-read the spec after your last edit, not only after the first
one; the pipeline runs `rpmspec --parse` on it once you finish.
5. **Finish**: Return a JSON object with:
- "action": "edited", "no_change", "use_upstream_wheel", or "blocked".
  `blocked` is ONLY the last-resort answer of a resolver round that has tried
  every resolution the conventions name; anywhere else the pipeline treats it as
  a defect of the tool and asks again with your reason fed back. (A patch rebase
  turn is the other exception: it answers a patch it cannot port `blocked` in its
  `patches` list, and the package then stops as needing a maintainer.)
- "why": required when action is "blocked" or "use_upstream_wheel" — for the
latter, name the exact missing compiler/SDK and quote the build.log line proving it
(convention 28 Exception 4)
- "reasoning": your notes: what you found and why, free-form. It goes to the run
  log and the package notes, not to the changelog.
- "changelog": required when action is "edited" and a change needs a bullet;
  empty when every change is a dependency change (a BuildRequires, a Requires or a
  version bound), which gets no bullet; any other edit with an empty changelog is
  rolled back. The rules are convention 40 above.
- "evidence": MUST list SPECIFIC files/patterns you Read or Grepped before editing.
  REQUIRED format (each entry must name a specific file or pattern):
  ["Read source/pyproject.toml lines 1-50",
   "Read source/my_package.egg-info/top_level.txt",
   "Grep 'import' in source/tests/*.py",
   "Grep 'console_scripts' in source/setup.cfg"]
  Generic entries like "checked the source" or "verified files" are NOT evidence.
  Must be non-empty when action is "edited". This proves you investigated rather
  than assumed. If you edited based on a specific file's content, that file MUST
  appear in evidence.
- "confidence": "high", "medium", or "low"
- "unresolvable_packages": upstream project names of packages that OBS cannot
resolve and must be built first — Python dependencies as the PyPI name, C/C++
libraries with the `lib` prefix and `-devel` suffix stripped (`libtrantor-devel`
→ `trantor`, convention 27). **IMPORTANT**: Before listing any python-* package here,
verify it exists on PyPI with WebFetch `https://pypi.org/pypi/<name>/json`. Never
report system tools (cmake, ninja, pkg-config, gcc) as python-* packages — fix
the spec to use the plain package name instead.
- "patch_dependencies": optional. Dependencies the distro ships whose shipped
version breaks this package in a way only a patch to the dependency fixes
(convention 62), each as {"name", "problem"} and, when the defect is a path the
package does not ship where it should, a machine-checkable "condition"
{"path": "torch/utils/benchmark/utils", "package": "python-torch", "not_in":
["devel"]}: the pipeline checks it against the package's RPM file lists and
`%files`, does not ask for a patch that is already true, and escalates to the
upstream wheel when two patch turns could not make it true. Leave it out
otherwise.
- "stage_files": optional. Files the PIPELINE copies into the package checkout for
you, because your shell cannot write there (a refused `cp` into the checkout is
not an obstacle: ask for the copy here). Each entry is {"from", "to"} (add
"package" to read from another package's tree): `from` is a relative path inside
the package's own state directory (`source/package-lock.json`, `sdist/<top>/...`),
or inside the named package's state directory or checkout; `to` is a bare file
name in the checkout. Absolute paths, `..`, directories as destinations and files
over 64 MiB are refused. Staging a file and then naming it as a `SourceN:` in the
spec is the whole job; set action `edited` (or `no_change` when only staging).
- READ the specs, file lists and checkouts of the packages this one depends on
when the cause may be there (the paths are in the prompt; `rpm -qlp rpms/<f>.rpm`
lists what a built package ships). You cannot edit a sibling package yourself:
when the fix belongs in one, report it in `patch_dependencies` with the exact
change needed and return `no_change`: the pipeline branches, patches and rebuilds
it, then re-runs this package."#;

/// The conventions whose full text stays in the system prompt: they apply to
/// nearly every turn (a missing dependency is built, not worked around).
pub(super) const INLINE_CONVENTIONS: [u32; 7] = [27, 40, 61, 63, 64, 86, 89];

/// A convention too long and too rare for every turn: only this summary stays in
/// the system prompt, and the full text is read on demand like any other.
const SUMMARISED_CONVENTIONS: [(u32, &str); 1] = [(
    62,
    "62 (summary; Read `conventions/62.md` before acting on it). A dependency is wrong for \
     this package in a way only a change to the dependency fixes: (a) the distro ships a \
     release too new for one symbol or API, or (b) a package this project builds lacks files \
     this package needs. It is rare: use it only after the other options fail, never for a failure of \
     one of your own tests, and never for a bound that relaxing it fixes (convention 75, \
     section 59). Report it in `patch_dependencies` as `{\"name\": ..., \"problem\": ...}`, \
     not in `unresolvable_packages`, and relax this package's own bound as well. The `problem` gives \
     the exact error line, the symbol or API, the version that had it and the one that dropped \
     it, the repository URL and the bound this package declares; write `unverified` for what \
     you cannot confirm.",
)];

/// The index of every convention, one line each (`build/conventions/INDEX.md`).
pub(super) const CONVENTIONS_INDEX: &str = include_str!("../../conventions/INDEX.md");

/// Directory of the workspace the convention files are written to; the agent
/// Reads `conventions/<N>.md` (its working directory is the workspace).
pub(crate) const CONVENTIONS_DIR: &str = "conventions";

/// The conventions part of the system prompt: the inline conventions in full,
/// then the index of all of them. The rest of the text is read on demand from
/// the workspace (see [`materialise_conventions`]), so it is billed only for
/// the turns that need it instead of on every turn.
fn conventions_prompt() -> String {
    let mut out = String::from("openSUSE spec conventions to keep in mind:\n\n");
    for (n, text) in crate::conventions_table::CONVENTIONS.iter() {
        if INLINE_CONVENTIONS.contains(n) {
            out.push_str(text);
            out.push('\n');
        }
    }
    for (_, summary) in SUMMARISED_CONVENTIONS {
        out.push_str(summary);
        out.push_str("\n\n");
    }
    out.push_str(&format!(
        "## Convention index\n\n\
         Every numbered convention above and below is a rule of this project. Those marked \
         [inline] are already given in full above. The others are NOT in this prompt: \
         `Read {CONVENTIONS_DIR}/<N>.md` BEFORE acting on a situation the line names. Do not \
         read conventions that do not apply, and never act against one whose trigger matches \
         without having read it. Where a task text or a checklist item cites `convention N` \
         and N is not inline, Read that file when you reach the item, not before.\n\n{CONVENTIONS_INDEX}"
    ));
    out
}

/// Write the convention files into the workspace for the agent to Read. A file
/// whose content already matches is left alone, so a turn costs no writes.
pub(super) fn materialise_conventions(workspace: &std::path::Path) -> std::io::Result<()> {
    let dir = workspace.join(CONVENTIONS_DIR);
    std::fs::create_dir_all(&dir)?;
    for (n, text) in crate::conventions_table::CONVENTIONS.iter() {
        let path = dir.join(format!("{n}.md"));
        if std::fs::read_to_string(&path).is_ok_and(|have| have == *text) {
            continue;
        }
        std::fs::write(&path, text)?;
    }
    Ok(())
}

/// Everything sent to `claude --append-system-prompt`: [`PRIORITIES`], [`conventions_prompt`] plus
/// [`STATIC_WORKFLOW`], neither of which varies by package, task, or attempt.
///
/// This is kept out of the per-turn user prompt (`-p`), which carries the
/// genuinely dynamic content: package name, workspace paths, notes.md, build
/// failure hints. Since Anthropic's prompt cache
/// only hits on a byte-identical prefix, mixing ~140KB of unchanging
/// instructions into a string that's different on every single call meant
/// it could never be served from cache, on any turn, for any package. As a
/// separate system-prompt append it becomes a stable prefix shared by every
/// turn across every package in a run — this is the entire point of moving
/// it here, not a stylistic preference.
pub(super) fn static_system_prompt() -> &'static str {
    // `turn()` calls this once per agent turn — potentially hundreds of times
    // over a single `run_package` build+fix loop — and the ~140KB result
    // never changes within the process, so compute it once and hand out the
    // same allocation instead of re-formatting and recopying it every call.
    static PROMPT: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    // `--nvidia redist` is set once at startup, before the first turn.
    PROMPT.get_or_init(|| {
        let mut prompt = format!(
            "{PRIORITIES}\n{}\n\n{BASH_RULES}\n{STATIC_WORKFLOW}",
            conventions_prompt()
        );
        if let Some(addendum) = redist_cuda_prompt() {
            prompt.push_str("\n\n");
            prompt.push_str(&addendum);
        }
        prompt
    })
}

/// The CUDA build-time rules when the CUDA toolkit components are packaged from
/// NVIDIA's redistributables (`--nvidia redist`): the generic text says openSUSE
/// has no CUDA toolkit, which is true only without that mode. The package list
/// is the one the generator builds (`redist::COMPONENT_KEYS`).
pub(super) fn redist_cuda_prompt() -> Option<String> {
    if !crate::redist::enabled() {
        return None;
    }
    let devel: Vec<String> = ap_ecosystem::nvidia_redist::COMPONENT_KEYS
        .iter()
        .map(|k| format!("`cuda-{}-devel`", crate::nvidia_spec::short_name(k)))
        .collect();
    Some(format!(
        r#"## CUDA in this run (`--nvidia redist`): THE CUDA TOOLKIT COMPONENTS EXIST HERE

This section REPLACES every statement above and below that openSUSE has no CUDA
toolkit, that `cuda-devel`/`cuda-runtime` "cannot exist here", and the "CUDA at
build time means: use the upstream wheel" special case (convention 32 (section 84), the CUDA special
case, step "0f", the dependency-failure hints). In this run the pipeline builds
NVIDIA's redistributable components as RPMs in the same project, one package per
component and a `-devel` package for its headers:

{list}

They install under NVIDIA's own tree `/usr/local/cuda-<major>.<minor>` (headers in
`include/`, libraries in `lib64/`, pkg-config files in `pkg-config/`; look the
exact version up with `rpm -ql cuda-cudart-devel`).

- A BUILD-time need for CUDA headers or libraries (`cuda.h`, `cuda_runtime.h`,
  `cupti.h`, `nvrtc.h`, `-lcudart`, `find_package(CUDAToolkit)` that only needs
  headers and libraries) is a `BuildRequires` on the component package that ships
  them: `BuildRequires: cuda-cudart-devel`, `cuda-cupti-devel`, `cuda-nvrtc-devel`,
  `cuda-cublas-devel`, `cuda-cudnn-devel`, ... Point the build at the tree with
  `export CUDA_HOME=/usr/local/cuda-<major>.<minor>` (and `CUDA_PATH`,
  `CPLUS_INCLUDE_PATH=$CUDA_HOME/include` or `CFLAGS="-I$CUDA_HOME/include"` when
  the build does not read `CUDA_HOME`) in `%build`. Do NOT add `cuda-devel` or
  `cuda-runtime`, and do not report any `cuda-*` package in `unresolvable_packages`.
- Do NOT switch such a package to its prebuilt wheel only because it includes CUDA
  headers or links the CUDA runtime libraries: build it from source with the
  BuildRequires above (python-triton and cuda-bindings are built this way).
- The wheel switch (`use_upstream_wheel`, convention 28 Exception 4) stays only for
  what these packages cannot cover: a build that must COMPILE device code with
  `nvcc`, i.e. `.cu` files in `ext_modules`/`CUDAExtension`/CMake `enable_language(CUDA)`.
- The driver library `libcuda.so.1` is still the host's: never BuildRequire or
  Require it, and keep the `__requires_exclude` for it when a `.so` links it.
"#,
        list = devel.join(", ")
    ))
}

/// [`static_system_prompt`] written to a file for `--append-system-prompt-file`.
/// The file sits in its own directory, created exclusively with mode 0700, so
/// a file or symlink planted in the shared temp directory is never followed.
/// Dropping the value removes both, so the copy does not outlive the turn.
pub(super) struct SystemPromptFile {
    _dir: tempfile::TempDir,
    pub(super) path: std::path::PathBuf,
}

impl SystemPromptFile {
    pub(super) fn create() -> std::io::Result<Self> {
        let dir = tempfile::Builder::new().prefix("auto-package-").tempdir()?;
        // The mode is set explicitly: tempfile leaves it to the umask.
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700))?;
        let path = dir.path().join("system-prompt.md");
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)?;
        std::io::Write::write_all(&mut file, static_system_prompt().as_bytes())?;
        Ok(Self { _dir: dir, path })
    }
}

// ── turn ──────────────────────────────────────────────────────────────────────

/// Plugin bundling the conditional packaging skills (maturin, Rust/Cargo,
/// plain C/C++, monorepos, prebuilt wheels, systemd, i18n, -doc subpackages,
/// Django test settings) that CONVENTIONS only briefly points at instead of
/// spelling out in full — most packages never need any of them, so their
/// detailed content is loaded on demand via the `Skill` tool rather than
/// sent on every turn. Found at run time by
/// `ap_core::skills::resolve_skills_dir`; this const is only its last-resort
/// fallback, relative to this crate's source checkout.
const PACKAGING_SKILLS_DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../packaging-skills");

/// The skills plugin directory, resolved once. A miss is warned about once and
/// the turn runs without `--plugin-dir`: the prompts still name the skills, but
/// the agent works from the conventions in them alone.
pub(super) fn skills_dir() -> Option<&'static std::path::Path> {
    static DIR: std::sync::OnceLock<Option<std::path::PathBuf>> = std::sync::OnceLock::new();
    DIR.get_or_init(|| {
        let found = ap_core::skills::resolve_skills_dir(
            std::env::var("AUTO_PACKAGE_SKILLS_DIR").ok().as_deref(),
            std::env::current_exe().ok().as_deref(),
            std::path::Path::new(PACKAGING_SKILLS_DIR),
        );
        if found.is_none() {
            ap_core::diag!(
                "warning: packaging-skills directory not found (set AUTO_PACKAGE_SKILLS_DIR); \
                 agent turns run without the on-demand skills"
            );
        }
        found
    })
    .as_deref()
}
