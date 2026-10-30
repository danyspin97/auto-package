# Plan: packages hosted in git (src.opensuse.org)

Status: plan only, written 2026-10-30; the open questions are decided in
section 9. Nothing here is implemented. The goal is full parity for
packages whose sources live in git on src.opensuse.org: `update`, `lint`
and `build` work on them, and `--submit` opens a pull request instead of an
OBS submit request. OBS still builds them. The first scope is the devel
organisations (section 9, decision 1); product branches in `pool` stay
refused.

Every claim about the workflow below is marked **[verified]** (checked on
2026-10-30 against the public Gitea API, read-only `osc`, or the installed
`git-obs`/`osc` 1.28.0 help) or **[assumed]** (read in a secondary source or
inferred; check it before relying on it).

## 1. How the git workflow works

### Where things live

- Every package is a git repo `pool/<pkg>` on src.opensuse.org, one branch
  per product: `factory`, `leap-16.0`, `leap-16.1`, ... **[verified]**
  (`pool/python-requests` has `factory`; `pool/perl-Moose` has `factory`,
  `leap-16.0`, `leap-16.1`). Nobody pushes to `pool` directly; changes go in
  by pull request from a fork. **[assumed]** (wiki summary; the PRs seen are
  all from forks).
- A project is a git repo too, the "project git" (`_ObsPrj` for devel
  projects, e.g. `perl/_ObsPrj`; `products/PackageHub`, `openSUSE/Leap` for
  products). It holds one submodule per package, `_config`,
  `workflow.config` and `_maintainership.json`. **[verified]**
  (`perl/_ObsPrj#master` has 3216 entries, mostly submodules;
  `products/PackageHub#leap-16.0` `.gitmodules` entries read
  `url = ../../pool/0ad`, `branch = leap-16.0`; `perl/_ObsPrj` entries read
  `url = ../FAQ-O-Matic` with no branch).
- A devel organisation (e.g. `perl`) holds forks of the pool repos and its
  own project git. **[verified]** (`perl/perl-Moose` is a fork of
  `pool/perl-Moose`, default branch `main`).
- `workflow.config` is "non-standard JSON": it has trailing commas.
  **[verified]** (`perl/_ObsPrj`: `"ManualMergeOnly": true,` before `}`). It
  names the workflow (`pr`, `direct`), the package `Organization` and the
  `Branch` (PackageHub: `"Organization": "pool", "Branch": "leap-16.0"`).

### How OBS sees it

- An OBS project tracks its project git with a project-level `<scmsync>` in
  its meta. **[verified]**: `devel:languages:perl` ->
  `https://src.opensuse.org/perl/_ObsPrj#master`;
  `openSUSE:Backports:SLE-16.0` -> `.../products/PackageHub#leap-16.0`;
  `openSUSE:Leap:16.0` -> `.../openSUSE/Leap#leap-16.0`.
  `openSUSE:Factory` and `devel:languages:python` have no `<scmsync>`: they
  are still plain OBS projects. **[verified]**
- Each package of such a project has a package-level `<scmsync>` pinned to a
  commit (`.../perl/perl-Moose#a4a4cf...`) and an OBS-generated
  `_scmsync.obsinfo` (`commit:`, `url:`, `revision:`, `projectscmsync:`)
  plus `build.specials.obscpio`. **[verified]** (`osc meta pkg`, `osc ls`,
  `osc cat` on `devel:languages:perl perl-Moose`).
- Reading still works through OBS: `osc ls <project>` lists the packages and
  `osc cat` returns the spec. **[verified]** (3210 packages listed for
  `devel:languages:perl`). So the sweep and the audit read git-hosted
  projects as they read OBS ones today.
- A push reaches OBS through a webhook (`gitea_obs_bridge`) that calls
  `POST /source?cmd=triggerscmsync`; OBS then runs `obs-scm-bridge` to sync.
  **[verified]** as documented (`git-workflow-documentation`,
  `architecture/hld/scmsync_flow.md`); whether a user's own fork has that
  webhook is **[assumed]** (not documented).
- `osc branch`, `osc bco` and `osc ci` are turned off for scmsync packages;
  `osc co` of one clones the git repo (`--native-obs-package` opts out).
  **[assumed]** for branch/ci (wiki summary); the `co` behaviour is
  **[verified]** from `osc co --help`.

### Contributing

- `osc fork <project> <package>` forks the Gitea repo into the user's org
  and creates `home:<user>:branches:<project>` with the package scmsync'd to
  the fork's branch; then `git commit`, `git push`, and
  `git-obs pr create`. **[verified]** that the commands and their options
  exist (`osc fork --help`: `--target-project`, `--gitea-fork-org`,
  `--new-repo-name`, `--git-branch`; `git-obs pr create --help`:
  `--source <owner>/<repo>:<branch>`, `--target ...`, `--title`,
  `--description`, `--dry-run`, `--allow-maintainer-edit`). The exact
  objects `osc fork` creates are **[assumed]** (from `workflows.md`; not run).
- A PR on a pool repo is mirrored by the `workflow-pr` bot into a project
  PR; reviewers come from `workflow.config` and `_maintainership.json`;
  `autogits_obs_staging_bot` builds it in a dedicated OBS project and
  reports by comment, not by commit status. **[verified]** on closed PRs
  `pool/chromium!106` and `pool/perl-Mozilla-CA!1`: comments link
  `openSUSE:Backports:SLE-16.0:PullRequest:3565` and end with "Build
  successful"; the head commit has no statuses. A maintainer comments
  `merge ok` to merge.
- The PR body is customarily the new `.changes` entry. **[verified]** on the
  two PRs above.
- Factory itself is still contributed to with `osc sr devel/pkg
  openSUSE:Factory`; `pool/<pkg>#factory` follows accepted requests.
  **[verified]** that `pool/python-requests#factory`'s head commit is
  "Accepting request 1354099 from devel:languages:python"; that this is the
  only route to Factory is **[assumed]**.
- Tarballs are stored with git LFS: `.gitattributes` maps `*.gz`, `*.xz`,
  `*.zst`, `*.obscpio`, `*.whl`, ... to `filter=lfs`, and `*.changes` to
  `merge=merge-changes`. **[verified]** (`pool/python-requests#factory`; its
  `requests-2.34.2.tar.gz` is a 131-byte pointer).
- Credentials: `git-obs` reads `~/.config/tea/config.yml` (or
  `$GIT_OBS_CONFIG`), the login from `-G`/`$GIT_OBS_LOGIN`, or
  `GIT_OBS_GITEA_URL`/`_USER`/`_TOKEN`/`_SSH_KEY`. **[verified]**
  (`git-obs --help`; the config file exists on this machine and was not
  read).

Not verified at all: the wiki pages (`openSUSE:Git_Packaging_Workflow`,
`openSUSE:OBS_with_Git`, `openSUSE:OBS_to_Git`) and the Gitea web UI sit
behind a proof-of-work wall and could not be fetched; only search snippets of
them were read. No write was made to Gitea or OBS, so the effect of a fork,
a push and a PR creation is documented, not observed.

## 2. Where auto-package assumes OBS today

All OBS access goes through `osc` in `build/src/osc/` and a few callers.
What has to change, by operation:

| Operation | Today | Where |
|---|---|---|
| List packages | `osc ls` | `osc::try_list_project_packages` (`osc/project_packages.rs`), used by `update/plan.rs` and `update_project/items.rs` |
| Read a spec | `osc cat` | `update/read.rs` (`osc::list_package_files`, `osc::cat_file`), `update::spec_for_package` |
| Project kind/meta | `osc meta prj` | `osc::get_project_meta`, `osc::parse_project_info` (`osc/branch/project_meta.rs`), `cli/src/update_project/cmd.rs` |
| Branch | `POST ?cmd=branch` | `osc::branch_package_at` (`osc/branch/create.rs`), `cli/src/update_project/items.rs::branch_item`; `pipeline/obs_names.rs::branch_from_linked` |
| Rebase a branch | onto the origin's sources | `osc::rebase_package` (`osc/branch/rebase.rs`), `items.rs` |
| Delete a branch | delete a package this run created | `osc::delete_package`, `items.rs` |
| Create package | meta push | `osc::ensure_package` (`osc/checkout.rs`), `pipeline/checkout.rs::checkout_stage` |
| Check out / update | `osc co`, `osc up` | `osc::checkout`, `osc::pull` (`osc/checkout.rs`) |
| Pristine and tracked files | `.osc/sources/<f>`, `.osc/_files` | `osc/changes.rs` (`add_changes_entry`, `wrap_unpublished_changes`), `osc/publish.rs` (tracked list, `restore_named_sources`), `osc/service.rs`, `pipeline/commit.rs::commit_message`, `pipeline/patch_check.rs`, `osc/baseline.rs` (`save_baseline`, `obs_still_at_checkout`) |
| Local changes | `osc status` | `osc::has_local_changes` (`osc/service.rs`), `cli/src/main.rs`, `cli/src/cleanup.rs` |
| Changelog entry | `osc vc` | `osc::add_changes_entry` |
| Stage files | `osc addremove`, `osc rm`, `osc revert` | `osc::commit`, `remove_tracked_strays`, `restore_named_sources` (`osc/publish.rs`) |
| Commit = publish | `osc ci` | `osc::commit`, `pipeline/commit.rs::commit_step` |
| Build and wait | `osc results`, buildlog, `getbinaries` | `osc::obs_build` (`osc/build_retry.rs`, `build_once.rs`), `pipeline/build_round.rs` |
| Restore after failure | `osc ci` of the baseline | `osc::restore_baseline` (`osc/baseline.rs`) |
| Revision pin | `srcmd5` | `osc::source_srcmd5`, `verify_source_rev` (`osc/branch/submit.rs`) |
| Open requests | `/request?states=new,review` | `osc::open_submit_requests` |
| Submit | `POST /request?cmd=create` | `osc::create_submit_request`, `cli/src/update_project/request.rs::prepare_request`, `submit.rs::SubmitBackend` |
| Diff | `osc rdiff` | `cli/src/update_project/request.rs::rdiff` |
| User, branch project name | `osc whois` | `osc::current_user`, `osc::default_branch_project` (`osc/branch/api.rs`), `cli/src/update_project/run.rs` |
| Maintenance | `osc maintained`, incidents | `build/src/maintenance/` |
| Agent tools | `osc status`, `osc cat` allowed | `build/src/agent/tools.rs` |
| Credentials | `OSC_*` scrubbed, oscrc hidden | `build/src/agent/env.rs`, `build/src/service_sandbox/sandbox.rs` |

Two existing seams already point the way: `depreach::Obs` (a trait so the
resolver runs on fixtures) and `tui::modal::submit::PackageBackend` (diff,
preview, submit). Building and waiting stay on OBS in both workflows, so
`osc::obs_build`, the results cache, the build log and the RPM download do
not move.

The real coupling is narrower than the table: the pipeline needs (a) a place
where a package's files are, (b) what the published revision held
("pristine"), (c) a way to publish a set of files and know which revision OBS
then builds, and (d) at the end, a request. Everything else is OBS reads,
which keep working for git-hosted projects.

## 3. The proposed abstraction

A new module `build/src/pkgsrc/` (each file under 600 lines):

```rust
/// Which kind of host a project's sources live on.
pub enum Host {
    Obs,
    /// The project git and its branch, from the project's `<scmsync>`.
    Git { project_git: GitRef },
}

/// `https://src.opensuse.org/<owner>/<repo>#<branch-or-commit>`.
pub struct GitRef { pub base: Url, pub owner: String, pub repo: String, pub rev: String }

/// Where one package comes from and where its changes go back to.
pub struct Origin {
    pub obs_project: String,   // read side and the build, both hosts
    pub package: String,
    pub host: OriginHost,      // Obs | Git { repo: GitRef (branch, not commit) }
}

/// A checked-out package. Replaces direct `.osc/` reads.
pub trait WorkingCopy {
    fn dir(&self) -> &Path;
    fn tracked_files(&self) -> Vec<String>;
    /// The file as the published revision holds it (`.osc/sources/<f>`, or
    /// `git show <base>:<f>`).
    fn pristine(&self, name: &str) -> Option<Vec<u8>>;
    fn has_local_changes(&self, emit: Emit<'_>) -> bool;
    /// What OBS should end up building: `srcmd5` or a commit id.
    fn revision(&self) -> Option<String>;
}

/// Where packages are branched, checked out, published and requested.
pub trait PackageSource: Send + Sync {
    fn host(&self) -> &Host;
    /// Branch (OBS) or fork + work branch + scmsync'd package (git) into the
    /// branch project. Idempotent.
    fn branch(&self, origin: &Origin, branch_project: &str, emit: Emit<'_>) -> Result<Branched, Error>;
    fn rebase(&self, branched: &Branched, emit: Emit<'_>) -> Result<Rebased, Error>;
    fn checkout(&self, branched: &Branched, work_dir: &Path, emit: Emit<'_>) -> Result<Box<dyn WorkingCopy>, Error>;
    /// Stage `files` (adds and removals) and publish them: `osc ci`, or
    /// `git commit` + `git push` to the fork's work branch. Returns the
    /// revision published.
    fn publish(&self, wc: &dyn WorkingCopy, message: &str, files: &[String], emit: Emit<'_>) -> Result<String, Error>;
    /// Block until OBS builds `revision` (no-op for OBS; polls
    /// `_scmsync.obsinfo` for git).
    fn wait_for_sources(&self, branched: &Branched, revision: &str, emit: Emit<'_>) -> Result<(), Error>;
    fn open_requests(&self, branched: &Branched, emit: Emit<'_>) -> Result<Vec<OpenRequest>, Error>;
    fn request(&self, req: &ChangeRequest, emit: Emit<'_>) -> Result<String, Error>;
    fn diff(&self, branched: &Branched, emit: Emit<'_>) -> Result<String, Error>;
}
```

- `ObsSource` wraps today's functions unchanged (`branch_package_at`,
  `rebase_package`, `checkout`, `commit`, `open_submit_requests`,
  `create_submit_request`, `rdiff`). `OscWorkingCopy` reads `.osc/`.
- `GitSource` and `GitWorkingCopy` are new (section 4).
- `ChangeRequest` generalises `osc::SubmitRequest`: source, target,
  description, title, and the pinned revision (`srcmd5` or commit id).
  `SrOutcome` and `up::sr_description` are reused as they are.
- `pkgsrc::detect(project, emit) -> Host` reads the project meta once
  (`parse_project_info` gains `scmsync: Option<String>`). A package-level
  `<scmsync>` in a non-scmsync project also yields git for that package.
- `RunCtx` (pipeline) gets `source: Arc<dyn PackageSource>` and
  `wc: Box<dyn WorkingCopy>` in place of the bare `checkout_dir` uses that
  read `.osc/`; `checkout_dir` stays as `wc.dir()`.
- `SubmitBackend` (`cli/src/update_project/submit.rs`) holds the
  `PackageSource`; `prepare_request` asks it for the revision, open
  requests and creation, so `--submit` and the TUI still share one path.

What does not get a trait: `osc::obs_build` and the results reads (same
for both hosts), `depreach` (the branch project is still an OBS project with
build paths), names (`fuzzy_key`, `rpm_name`).

## 4. Each operation in the git workflow

| Operation | OBS (today) | Git |
|---|---|---|
| Detect host | - | `osc meta prj <P>`: `<scmsync>` present -> `Host::Git` |
| List packages | `osc ls <P>` | same (verified to work); cross-check the submodules of the project git when the listing and `.gitmodules` disagree |
| Read spec (sweep, audit) | `osc cat` | same; OBS may lag the git branch, so the plan step records `_scmsync.obsinfo`'s `commit:` and the checkout compares it with the branch head |
| Origin of a package | the project | `_scmsync.obsinfo` `url:` gives `<owner>/<repo>`; the branch is the submodule's `branch =` in the project git's `.gitmodules`, else the repo's default branch (perl: `main`) |
| Branch project | `home:<user>:branches:<P>` | the same name (`osc::default_branch_project`); an ordinary OBS project with build paths, no project `<scmsync>` |
| Branch / fork | `osc branch` | the tool's own steps, not `osc fork` (decision 2): (1) unless `GET /repos/<org>/<repo>` already shows a fork whose `parent` is `<owner>/<repo>`, fork it with `git-obs repo fork <owner>/<repo> --target-owner <org>`, where `<org>` is `--fork-org` (default: the user's own Gitea namespace); a repo of that name that is not such a fork stops the package; (2) push a work branch `auto-package/<target-branch>` at the target branch head; (3) `POST /source/<P>/<pkg>?cmd=fork&scmsync=https://src.opensuse.org/<org>/<repo>#auto-package/<target-branch>&target_project=<branch project>` through the `osc` chokepoint (the OBS half of `osc fork`, `osc/obs_api/package.py::cmd_fork`) |
| Rebase | `osc rebase` / link merge | `git fetch` the target branch, `git rebase` the work branch onto it; a conflict is reported like `Rebased::Conflict` today, never resolved by hand |
| Check out | `osc co` / `osc up` | `git clone` (anonymous https) of the fork into `<work>/<P>/<pkg>/`, git dir kept outside the tree (section 6), `git lfs pull`; reuse with `git fetch` + fast-forward |
| Pristine | `.osc/sources/<f>` | `git show <base>:<f>`, where `<base>` is the target branch commit the work branch started from, kept as `refs/auto-package/base` |
| Changelog | `osc vc` | `osc vc` works on any directory with a `.changes` file (**assumed**: to test); otherwise the same entry is written by the tool's own `.changes` writer |
| Commit | `osc addremove` + `osc ci` | `git add`/`git rm` of exactly the plan's files; the work branch holds one commit on top of `<base>`: the first round commits, later rounds amend it (`git commit --amend -m <commit_message>`), decision 4 |
| Publish | (inside `osc ci`) | `git push <fork> HEAD:refs/heads/auto-package/<target-branch>` with `--force-with-lease` against the last pushed commit; refused for any remote that is not the run's fork; a failed lease (someone else moved the branch) stops the package |
| Wait for sources | - | poll `osc cat <branch-project> <pkg> _scmsync.obsinfo` until `commit:` is the pushed id, for at most 10 minutes; then one `POST /source/<branch-project>/<pkg>?cmd=runservice` and a second 10-minute wait; still stale: the package fails with "OBS did not sync the push", and nothing is built from stale sources (decision 3) |
| Build, wait, results | `osc results`, log, binaries | unchanged, against the branch project |
| Restore after failure | `osc ci` of the baseline | reset the work branch to the last good commit and push it to the fork (with lease); `pool` and the devel org are never written |
| Revision pin | `srcmd5` | commit id: the PR is refused when the work branch head is not the commit OBS built (the single-commit branch makes the built commit the PR head) |
| Open requests | `/request?...` | `GET /repos/<owner>/<repo>/pulls?state=open`, filtered on head `<fork-org>:<branch>` |
| Submit | `osc sr` | `git-obs pr create --source <org>/<repo>:<branch> --target <owner>/<repo>:<target-branch> --title ... --description ...` `--allow-maintainer-edit`; title `Update <pkg> to <version>` (an update) or the commit message's first line (a lint or fix run), body `sr_description` (the new `.changes` entry first, as pool PRs do); never a direct API call, so the tool never holds the token |
| Diff (TUI `d`) | `osc rdiff` | `git diff <base>..<head>` in the checkout |
| `--in-place` | commit into the project | refused for git (decision 6): pool takes no direct pushes, and a push into a devel org's branch is an unreviewed write to a shared repo |
| Maintenance (`--maintenance`, `--backport`) | incidents | refused for git; SLE 16 maintenance is PRs to `pool/<pkg>#leap-16.x` with `NoProjectGitPR`, a separate plan |
| `build` of a new package | `ensure_package` | builds in an OBS project as today; when the package's distro origin is git, it is forked as above. A brand new package needs a new repo plus an `[ADD]` issue on the project git: printed for the user, never opened by the tool |

The PR goes to the package repo the project tracks: `perl/<pkg>#main` for
`devel:languages:perl`. That is the same rule as today's submit request "to
the project it was branched from"; forwarding to Factory stays the devel
maintainers' job. A project whose packages resolve to a `pool` repo
(`openSUSE:Backports:SLE-16.0` -> `pool/<pkg>#leap-16.0`, Leap 16) is
refused in this plan (decision 1); `pkgsrc::detect` tells the two apart by
the origin repo's owner (`pool` or not).

## 5. Package files in a git checkout

- **Spec, patches, `series`**: unchanged. The patch stage, quilt checks
  and spec edits only see files.
- **`.changes`**: the pipeline keeps writing the entry; "unpublished entry"
  logic (`without_unpublished_update_entry`, `wrap_unpublished_changes`,
  `commit_message`) compares against `pristine()`, i.e. the target branch,
  not the last local commit, so several build rounds still read as one entry.
  `.gitattributes` names a `merge-changes` driver; the tool never merges
  `.changes` (rebase conflicts stop the package).
- **Tarballs and other archives**: committed through LFS. A commit is
  refused when `git lfs` is missing, or when `git check-attr filter` says a
  file the plan commits should be LFS and the staged blob is not a pointer.
  Old archives are removed with `git rm` like `osc rm` today.
- **`_service`**: unchanged locally: services run in the sandbox
  (`osc::run_service` -> `service_sandbox`) and their outputs (e.g.
  `vendor.tar.zst`, LFS) are committed. Whether OBS also runs the package's
  `_service` after `obs-scm-bridge` is **assumed** yes; phase 4 checks it on
  one package before relying on it.
- **`_scmsync.obsinfo`, `build.specials.obscpio`**: OBS-generated; never
  in the publish plan, ignored by the sweep's file reads.
- **Non-package files** (`README.md`, `.gitattributes`, `.gitignore`,
  `cpanspec.yml`, ...): `publish_plan` skips untracked ones as today, but
  `remove_tracked_strays` must not `git rm` a tracked dotfile or README. In
  git it removes only archives and patches the spec no longer names.
- **Pipeline-owned files**: `.gitattributes` and `.gitignore` join the list
  of files the agent may not write (`agent/tools.rs`), and are restored to
  pristine after each turn like `.changes` is today.
- **Conventions and skills**: one convention says the checkout may be a git
  repository the tool commits and pushes, and the agent edits files only
  (no `git`, no `osc vc`). The agent's `Bash(osc status)` becomes
  `Bash(git status)` / `Bash(git diff *)` for git checkouts; `git commit`,
  `git push`, `git remote`, `git config`, `git-obs`, `tea` and `osc fork`
  are denied.
- **AGENTS.md**: the rule stays as it is ("auto-package is the only thing
  that changes packages") and gains the git verbs: no `git commit`,
  `git push`, `git-obs pr create` or `osc fork` by hand; reading (`git log`,
  `git diff`, `git-obs pr get`, the Gitea API with GET) is fine for a review.

## 6. Credentials and safety

- **Nothing is pushed anywhere but the run's fork, and no PR is opened
  without `--submit`.** `GitSource::publish` takes the remote from the
  `Branched` record, and a pure check (`push_target_allowed`) refuses a
  remote whose owner is not `--fork-org`, a repo whose `parent` is not the
  origin repo, and any branch not named `auto-package/...`. Without
  `--submit`, pushes to the fork happen (they are what OBS builds, like
  today's commits to the branch project) and the run summary says so.
  `--dry-run` writes nothing, on either host. `push_target_allowed` also
  refuses `pool` and the origin's owner as a push target outright, whatever
  `--fork-org` says.
- **Token handling**: the tool never reads the token itself. Forks
  (`git-obs repo fork`), pushes and PRs (`git-obs pr create`) go through
  `git-obs` (or `git` with the user's ssh key / credential helper), which
  read `~/.config/tea/config.yml`; Gitea reads use the anonymous API.
  `--gitea-login` passes `-G`. No direct authenticated API client is part
  of this plan.
- **Agent environment**: `agent/env.rs` adds `GIT_OBS_*`, `GITEA_*`,
  `TEA_*`, `GIT_ASKPASS`, `SSH_ASKPASS`, `GIT_SSH`, `GIT_SSH_COMMAND`,
  `GIT_CONFIG_*`, `GIT_DIR`, `GIT_WORK_TREE` and `SSH_AUTH_SOCK` to the
  deny list. The service sandbox hides `~/.config/tea`, `~/.ssh`,
  `~/.git-credentials` and `~/.gitconfig` as it hides oscrc today.
- **Git itself as an attack surface**: the agent can write the checkout,
  so a `.git/hooks/*`, `.git/config` (`core.fsmonitor`, `core.sshCommand`,
  filter drivers) or a `.git` file pointing elsewhere would run code with
  the user's credentials at the next `git commit`. Hence: the git dir lives
  outside the worktree (`<work>/<P>/.git-dirs/<pkg>.git`), every git call
  passes `--git-dir`/`--work-tree` explicitly, runs with
  `-c core.hooksPath=/dev/null -c core.fsmonitor=false`, a scrubbed env and
  `GIT_CONFIG_NOSYSTEM=1`, and goes through one `pkgsrc::git::run`
  chokepoint (like the `osc` chokepoint in `proc/capture.rs`).
- **Rate limits**: Gitea calls get their own token bucket next to
  `ap_core::limits::obs_acquire`.

## 7. Phases

Each phase is one worker commit series that compiles, passes and is useful
on its own. Nothing before phase 4 writes to Gitea.

1. **Detect and refuse.** `parse_project_info` reads `<scmsync>`;
   `pkgsrc::detect`; `update`, `lint` and `build` stop early on a git-hosted
   project or package with a message naming the project git, instead of
   reaching `osc branch`/`osc ci`. The refusal of `pool`-backed projects
   (decision 1) stays after phase 7. Tests: meta fixtures of
   `openSUSE:Backports:SLE-16.0`, `devel:languages:perl` (project scmsync)
   and `perl-Moose` (package scmsync). Ships: safety on its own.
2. **Working-copy seam, no behaviour change.** `WorkingCopy` and
   `OscWorkingCopy`; every `.osc/sources`, `.osc/_files` and
   `osc status` reader listed in section 2 goes through it. Tests: the
   existing suite, plus `OscWorkingCopy` on a fixture `.osc/`.
3. **Gitea reads and the git plan.** `pkgsrc::gitea` (anonymous GET: repo,
   branches, raw file, pulls), parsers for `.gitmodules`, `workflow.config`
   (trailing commas) and `_scmsync.obsinfo`; origin resolution. `update
   --dry-run` and `lint --dry-run` on a git-hosted project print, per
   package, the fork, the work branch, the branch-project package and the
   PR target. Tests: fixtures captured from the API calls in section 1.
4. **Git working copy, local only.** `GitWorkingCopy` and the hardened
   `git::run`: clone (anonymous), base ref, pristine, staging, commit, LFS
   checks, stray rules. No remote writes. Tests: local bare repos created by
   the test under the cargo target dir, including a hook and a hostile
   `.git` file that must not run or redirect.
5. **`PackageSource`, fork and push, behind `--source-backend git`.**
   `ObsSource` over today's code (the pipeline and `items.rs` switch to the
   trait); `GitSource::branch` (`git-obs repo fork`, work branch, OBS
   `cmd=fork`), `rebase`, `publish` (amend + push with lease),
   `wait_for_sources` (bounded wait, one `runservice`), restore.
   `update`/`lint` on a devel-org project build in the branch project
   without `--submit`. Tests: pure planners (`cmd=fork` query, push
   refspec, `push_target_allowed`, the wait/trigger/fail sequence), a fake
   runner for the command sequence, and one manual run on a package the
   user picks; that run also checks the three assumptions decisions 2 and 3
   rest on (what `cmd=fork` creates, whether a fork push syncs by itself,
   whether `runservice` re-syncs an scmsync package).
6. **Pull requests.** `GitSource::open_requests`, `request`, `diff`;
   `prepare_request` and `SubmitBackend` through the trait; commit-id pin;
   TUI `S`/`A`/`d`. Tests: request planning, the open-PR filter, refusal
   when the head moved, `git-obs pr create --dry-run` argument building
   (with `--allow-maintainer-edit`).
7. **Default on, `build` parity, docs.** `--source-backend auto` becomes
   the default for devel-org projects; `build` forks git-origin packages
   whose origin is a devel org; conventions, skills,
   agent tool lists, `docs/agents/work-directory.md` (git dirs, the work
   branch) and AGENTS.md updated; `explain` shows pushes and PRs.

Later, not in this plan: product branches in `pool` (Backports SLE-16.0,
Leap 16) together with maintenance PRs for `leap-16.x`, `--in-place` for
`workflow-direct` devel orgs, multi-package PRs (`PR: pool/pkgA!2` lines,
`git-obs pr --recursive`), new-package `[ADD]` issues.

## 8. Risks

- **Sync lag and triggers.** If a fork has no webhook, OBS never sees the
  push; the wait is bounded and fails the package (decision 3), but
  whether `runservice` re-syncs an scmsync package is **[assumed]** until
  the phase 5 run.
- **Own fork steps drifting.** The tool does what `osc fork` does with its
  own calls (decision 2); if OBS or `git-obs` change `cmd=fork` or the
  fork layout, the tool must follow. What `cmd=fork` creates in the branch
  project (repositories, build paths) is **[assumed]** until phase 5.
- **Fork name clashes.** A Gitea owner holds one repo per name; a fork of
  `perl/perl-Moose` and, later, one of `pool/perl-Moose` cannot both be
  `<user>/perl-Moose`. Harmless while the scope is devel orgs only; the
  product-branch plan has to settle it (`--new-repo-name`).
- **LFS.** A missing `git-lfs`, quota or an upload failure leaves a pushed
  commit whose archives OBS cannot fetch.
- **Stale reads.** The sweep reads OBS, which follows the project git with
  a delay; the base of the work branch must come from git, not from OBS.
- **Bot reviews.** `legaldb`, `opensuse-review` and maintainer reviews are
  the PR's; an automated PR flood could annoy reviewers. `--max-packages`
  limits apply to PRs too.
- **Two hosts in one run.** A dependency closure may mix OBS-hosted and
  git-hosted packages; the trait is per package, not per run.

## 9. Decisions

Decided 2026-10-30, choosing the conservative option where the repo rules
(auto-package is the only thing that changes packages; nothing is pushed
or submitted to a shared repo without `--submit`; the tool never reads a
token) leave a choice.

1. **Scope: devel organisations only.** A project with a project
   `<scmsync>` whose packages resolve to a devel org's repos
   (`devel:languages:perl` -> `perl/<pkg>#main`) gets the git workflow.
   Product branches in `pool` (`openSUSE:Backports:SLE-16.0`, Leap 16) stay
   refused with the phase 1 message. Factory stays on the OBS path.
   *Why*: devel projects are what `update --project <devel> --submit`
   serves today (submit to the project it was branched from); `pool`
   product branches are the maintenance side (`NoProjectGitPR`, release
   reviewers, the `--maintenance`/`--backport` territory), where a wrong PR
   reaches product reviewers, and they need their own plan. Doing one
   scope also avoids the fork name clash (section 8). *Open*: product
   branches, in the later maintenance plan. Note: of the projects run so
   far, `X11:Wayland` and `devel:languages:c_c++` have no `<scmsync>`
   **[verified]**, so nothing changes for them.
2. **Fork with the tool's own steps, not `osc fork`; forks live in the
   user's own Gitea namespace.** The steps are the row "Branch / fork" in
   section 4: `git-obs repo fork` (token stays in `git-obs`), a work branch
   `auto-package/<target-branch>`, OBS `cmd=fork` into the run's branch
   project. `--fork-org` overrides the namespace; `pool` and the origin's
   owner are never accepted. *Why*: `osc fork` (osc 1.28.0,
   `osc/commands/fork.py`) **[verified]** (a) ignores the scmsync branch
   and uses the repo's default branch (lines 149-157, "temporary hack"),
   (b) silently switches to the devel project unless `--no-devel-project`
   (117-122), (c) clones into the current directory with the git dir
   inside the worktree (193-216), against section 6, and (d) exits when
   that directory exists (195-197), so it is not idempotent. Its two
   remote calls are what the tool makes itself. The personal namespace is
   the `git-obs repo fork` default and the equivalent of
   `home:<user>:branches`. *Open*: what `cmd=fork` creates, checked in the
   phase 5 run.
3. **Sync: bounded wait, then one trigger, then fail.** Wait up to 10
   minutes for `_scmsync.obsinfo` to show the pushed commit; then one
   `POST /source/<branch-project>/<pkg>?cmd=runservice` (the call
   `osc service remoterun` makes, `osc/core.py::runservice`), only ever on
   the run's own branch project; another 10 minutes; then the package
   fails with "OBS did not sync the push". *Why*: never build or submit
   stale sources, never wait forever, and the trigger writes nothing a
   commit to the branch project does not already write today. The
   project-wide `POST /source?cmd=triggerscmsync` is the bridge's call and
   is not used. *Open*: whether `runservice` re-syncs an scmsync package
   is **[assumed]**; the phase 5 run checks it, and if it does not, the
   trigger is dropped and only the bounded wait remains.
4. **PR shape: one commit, the commit OBS built.** The work branch is
   always one commit on `<base>`, amended each round and pushed with
   lease, so the PR head is the built commit and the pin stays a commit id
   (no squash at submit time). Title `Update <pkg> to <version>`, or the
   commit message's first line for a lint or fix run; body
   `sr_description`. `--allow-maintainer-edit` on. *Why*: one reviewable
   change per PR, as pool PRs show; maintainers can fix the PR in place,
   and a maintainer's push makes the next lease fail, which stops the
   package instead of overwriting them. *Open*: nothing.
5. **Pushes without `--submit`: yes, to the user's fork only.** They are
   the equivalent of today's commits to `home:<user>:branches:<P>`: the
   user's own namespace, needed for OBS to build. Only
   `auto-package/*` branches of the run's fork, recorded in the run
   summary and `run.jsonl`; `--dry-run` pushes nothing; no PR without
   `--submit`. *Open*: nothing.
6. **`--in-place` on git: refused.** *Why*: pool takes no direct pushes,
   and pushing into a devel org's branch skips review on a repo others
   consume. *Open*: `workflow-direct` orgs, later, if asked for.
7. **No direct authenticated Gitea client.** Writes go through `git-obs`
   and `git`; reads are anonymous. *Why*: the tool never reads a token.
   *Open*: nothing.
