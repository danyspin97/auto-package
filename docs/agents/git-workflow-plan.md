# Plan: packages hosted in git (src.opensuse.org)

Status: plan only, written 2026-10-30; the open questions are decided in
section 9. Nothing here is implemented. The goal is full parity for
packages whose sources live in git on src.opensuse.org: `update`, `lint`
and `build` work on them, and `--submit` opens a pull request instead of an
OBS submit request. OBS still builds them. The canonical repository of a
package is `pool/<pkg>`. The first scope is a Factory devel package whose
OBS package scmsyncs a fork of `pool/<pkg>` (first target:
`server:database/mariadb-connector-c`); its PR goes to
`pool/<pkg>#factory` (section 9, decisions 1 and 8). Devel organisations
with their own project git come second; product branches in `pool` stay
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
- `workflow.config` is "non-standard JSON": it has trailing commas and
  `//` line comments. **[verified]** (`perl/_ObsPrj`: `"ManualMergeOnly":
  true,` before `}`; `openSUSE/Factory#main`: `// Require "merge ok" ack
  before merging`, read with
  `GET /repos/openSUSE/Factory/raw/workflow.config?ref=main`). It names
  the workflow (`pr`, `direct`), the package `Organization` and the
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
  **[verified]** from `osc co --help`. These refusals are osc's
  client-side `git_is_unsupported` checks (`osc/commandline.py`); the tool
  does not go through them: it branches with `osc api -X POST
  ...?cmd=branch` (`osc/branch/create.rs`). What the OBS server does on
  `cmd=branch` of an scmsync package is **unverified**.

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
- Factory has two routes. `osc sr devel/pkg openSUSE:Factory` still works
  and `pool/<pkg>#factory` records accepted requests **[verified]**
  (`pool/python-requests#factory`'s head is "Accepting request 1354099
  from devel:languages:python"). And Factory has a project git
  `openSUSE/Factory#main` whose `workflow.config` reads `"Workflows":
  ["pr"]`, `"Organization": "pool"`, `"Branch": "factory"`, reviewers
  `gitea_test`, `~legaldb`, `~factory_bot`, `"ManualMergeProject": true`
  **[verified]**,
  so a PR to `pool/<pkg>#factory` is a contribution route; open PRs to
  `factory` exist (`pool/stgit!9`, from `lkocman/stgit`), but none was
  seen merged, so how a merged one reaches `openSUSE:Factory` on OBS (whose
  meta has no `<scmsync>`) is **[assumed]**.
- A package can be git-hosted while its project is not: the package meta
  carries the `<scmsync>`. **[verified]**: `server:database` has no
  project `<scmsync>`; `server:database/mariadb-connector-c` has
  `<scmsync>https://src.opensuse.org/adamm/mariadb-connector-c#main</scmsync>`,
  its `osc ls` lists `_scmsync.obsinfo` (`url: .../adamm/mariadb-connector-c`,
  `revision: main`) and `build.specials.obscpio`. `adamm/mariadb-connector-c`
  is a Gitea user repo (no `/orgs/adamm`), `fork: true`, `parent:
  pool/mariadb-connector-c`: the maintainer's working fork.
  `openSUSE:Factory/mariadb-connector-c` names `server:database` as its
  devel package and has no `<scmsync>`. `pool/mariadb-connector-c` has the
  branches `factory` (default), `leap-16.1`, `slfo-main`, `slfo-1.0` to
  `slfo-1.3`, and on 2026-10-30 `pool#factory` and `adamm#main` have the
  same head `faf83b36...` (Gitea `compare/factory...adamm:main` reports 0
  commits). The cross-fork compare works both ways **[verified]**:
  `compare/slfo-1.0...adamm:main` reports 303 commits, and an unknown
  owner or ref answers 404. Commit ids are 64 hex digits: the repos use
  SHA-256 objects.
- Tarballs are stored with git LFS: `.gitattributes` maps `*.gz`, `*.xz`,
  `*.zst`, `*.obscpio`, `*.whl`, ... to `filter=lfs`, and `*.changes` to
  `merge=merge-changes`. **[verified]** (`pool/python-requests#factory`; its
  `requests-2.34.2.tar.gz` is a 131-byte pointer;
  `pool/mariadb-connector-c#factory` maps `*.gz` and `*.tar` to
  `filter=lfs` and its `mariadb-connector-c-3.4.9-src.tar.gz` is a
  132-byte pointer). The LFS objects are uploaded by git-lfs's `pre-push`
  hook, and the `filter.lfs.*` drivers come from git config: on this
  machine they are in `/etc/gitconfig` and the identity in
  `~/.config/git/config` **[verified]** (`git config --show-origin`).
- Credentials: `git-obs` reads `~/.config/tea/config.yml` (or
  `$GIT_OBS_CONFIG`), the login from `-G`/`$GIT_OBS_LOGIN`, or
  `GIT_OBS_GITEA_URL`/`_USER`/`_TOKEN`/`_SSH_KEY`. **[verified]**
  (`git-obs --help`; the config file exists on this machine and was not
  read). `git-obs login list` prints the logins without their tokens
  unless `--show-tokens` is given, `git-obs api /user` makes an
  authenticated GET, and the hidden `git-obs -G <login> login
  gitcredentials-helper` is a git credential helper that hands the token
  to `git` for https pushes **[verified]** (osc 1.28.0
  `commands_git/login_list.py`, `api.py`,
  `login_gitcredentials_helper.py`).

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
| Other branch call sites | `cmd=branch` | `--follow-devel` (`items.rs::branch_item`, OBS resolves the devel package), `--update-deps` items, dependency branches `pipeline/obs_names.rs` (`branch_package_as`) and `cli/src/build_cmd/deps.rs` (`branch_package`) |
| Second commit path | checkout, baseline, `osc vc`, `osc ci` | `cli/src/update_project/tidy.rs::tidy_item` (`lint --improve`) |
| Maintenance commits | checkout, baseline, commit | `maintenance/check.rs` (references commit), `cli/src/update_project/maintenance.rs::stage_backport`, `cli/src/update.rs` (`--in-place`) |
| Baselines | saved with the checkout path only | `osc/baseline.rs`, restored by `pipeline/run.rs::restore_stale_baseline`, `proc/shutdown.rs` (stopped run), `cli/src/baseline.rs` (`baseline discard`), `tidy.rs`, `maintenance/check.rs` |
| In-flight write guard | `proc::obs_write` | `pipeline/run.rs` (whole package run), `tidy.rs`, `maintenance/check.rs`; not taken by `create_submit_request` |
| `--revision` | OBS revision or srcmd5 only | `build/src/branch_revision.rs::is_revision` |
| Service identity, outputs | `.osc/_apiurl` -> oscrc `realname`/`email`; only `.osc` refused as an output | `service_sandbox/sandbox.rs` (`changes_identity`), `service_sandbox/run.rs::move_outputs` |
| Checkout hygiene | finds checkouts by `.osc`, dirty by `osc status` | `cli/src/cleanup.rs` (`checkouts`, `checkout_is_dirty`), `osc/checkout.rs` (`discard_unfinished_checkout`), `osc/service.rs::repair_wc` |
| Recipe files | `_link` names the origin | `osc/publish.rs::restore_dropped_recipe_files` (a `cmd=fork` package has no `_link`, so `_constraints`/`_multibuild` are not brought back) |

Each row either goes through the seam of section 3 or refuses a git
package explicitly: `--revision` (a commit id is not an OBS revision),
`--in-place`, maintenance and `stage_backport` stay refused for git;
`tidy_item` goes through `PackageSource` (phase 5a) and is refused for git
until then; `cleanup`, `discard_unfinished_checkout` and `repair_wc` must
skip a directory without `.osc` rather than treat it as broken; the
service identity for a git checkout comes from the branch project's
apiurl, not from a `.osc/_apiurl` file; `move_outputs` refuses `.git` and
`.gitattributes` as service outputs as it refuses `.osc`; and
`restore_dropped_recipe_files` takes the origin from the `Branched`
record, not from `_link`.

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
/// `https://src.opensuse.org/<owner>/<repo>#<branch-or-commit>`.
pub struct GitRef { pub base: Url, pub owner: String, pub repo: String, pub rev: String }

/// Where one package's sources live. Resolved per package: a project
/// without `<scmsync>` (`server:database`) can hold a package whose meta
/// carries one (`mariadb-connector-c`), next to plain OBS packages.
pub enum OriginHost {
    Obs,
    Git {
        /// The repo OBS builds the origin from (package `<scmsync>`, or
        /// the submodule of the project git): read, never written.
        scmsync: GitRef,
        /// Where the PR goes: `pool/<pkg>#factory` (package-level,
        /// decision 8) or the devel org repo and branch (project-level).
        target: GitRef,
    },
}

/// Where one package comes from and where its changes go back to.
pub struct Origin {
    pub obs_project: String,   // read side and the build, both hosts
    pub package: String,
    pub host: OriginHost,
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
    /// Branch (OBS) or fork + build branch + scmsync'd package (git) into the
    /// branch project. Idempotent.
    fn branch(&self, origin: &Origin, branch_project: &str, emit: Emit<'_>) -> Result<Branched, Error>;
    fn rebase(&self, branched: &Branched, emit: Emit<'_>) -> Result<Rebased, Error>;
    fn checkout(&self, branched: &Branched, work_dir: &Path, emit: Emit<'_>) -> Result<Box<dyn WorkingCopy>, Error>;
    /// Stage `files` (adds and removals) and publish them: `osc ci`, or
    /// `git commit` + `git push` to the fork's build branch. Returns the
    /// revision published.
    fn publish(&self, wc: &dyn WorkingCopy, message: &str, files: &[String], emit: Emit<'_>) -> Result<String, Error>;
    /// Record what the branch holds before the run's first write
    /// (`.osc/sources`, or the build branch head), and put it back.
    fn save_baseline(&self, wc: &dyn WorkingCopy, dir: &Path) -> Result<(), Error>;
    fn restore(&self, baseline: &Path, emit: Emit<'_>) -> Result<(), Error>;
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
- `pkgsrc::detect(project, package, emit) -> OriginHost` decides per
  package. It reads the project meta once per project (`parse_project_info`
  gains `scmsync: Option<String>`; a project `<scmsync>` makes every
  package git, through its submodule), and otherwise the package: a
  `_scmsync.obsinfo` in the package's file list (already read by
  `update/read.rs`) is the cheap signal, `osc meta pkg` confirms it. The
  PR target is resolved per package (decision 8), not per project.
- The source is per package: `pkgsrc::source_for(&Origin) ->
  Arc<dyn PackageSource>` hands out one shared `ObsSource` or `GitSource`.
  A run over `server:database` mixes both.
- `RunCtx` (pipeline) gets `origin: Origin` and `source: Arc<dyn
  PackageSource>`; both are known in `preflight` (`pipeline/checkout.rs`),
  before any checkout. `RunCtx` is built there and borrowed immutably, so
  the `Box<dyn WorkingCopy>` that `checkout_stage` creates lives in
  `RunState` (already `&mut` in every stage), and the bare
  `checkout_dir` uses that read `.osc/` go through it; `checkout_dir`
  stays as the path both agree on.
- `SubmitBackend` (`cli/src/update_project/submit.rs`) keeps each item's
  `Origin` and asks `source_for` per item; `prepare_request` asks that
  source for the revision, open requests and creation, so `--submit` and
  the TUI still share one path.

What does not get a trait: `osc::obs_build` and the results reads (same
for both hosts), `depreach` (the branch project is still an OBS project with
build paths), names (`fuzzy_key`, `rpm_name`).

## 4. Each operation in the git workflow

| Operation | OBS (today) | Git |
|---|---|---|
| Detect host | - | `osc meta prj <P>`: `<scmsync>` present -> `OriginHost::Git` for every package (through its submodule); else per package: `_scmsync.obsinfo` listed -> `osc meta pkg <P> <pkg>`, package `<scmsync>` present -> git for that package. Run on the package OBS will branch from, at every branch call site (phase 1) |
| List packages | `osc ls <P>` | same (verified to work); cross-check the submodules of the project git when the listing and `.gitmodules` disagree |
| Read spec (sweep, audit) | `osc cat` | same; OBS may lag the git branch, so the plan step records `_scmsync.obsinfo`'s `commit:` and the checkout compares it with the branch head |
| Origin of a package | the project | `_scmsync.obsinfo` `url:` gives `<owner>/<repo>` (the repo OBS builds from). Package-level scmsync (decision 8): the PR target is that repo's Gitea `parent`, which must be `pool/<pkg>`, branch `factory`. Project-level scmsync (devel org): the target is the repo itself, branch from the submodule's `branch =` in the project git's `.gitmodules`, else the repo's default branch (perl: `main`) |
| Branch project | `home:<user>:branches:<P>` | the same name (`osc::default_branch_project`); an ordinary OBS project with build paths, no project `<scmsync>` |
| Branch / fork | `osc branch` | the tool's own steps, not `osc fork` (decision 2); `<owner>/<repo>` below is the PR target (`pool/<pkg>` for package-level scmsync), never the repo the devel package scmsyncs: (0) `<org>` is `--fork-org`, else the user's Gitea login, read once per run from `git-obs api /user` (`login`; the token stays in `git-obs`); (1) unless `GET /repos/<org>/<repo>` already shows a fork whose `parent` is `<owner>/<repo>`, fork it with `git-obs repo fork <owner>/<repo>`, adding `--target-owner <org>` only for `--fork-org` (git-obs sends it to Gitea as `"organization"`, osc 1.28.0 `commands_git/repo_fork.py`, `gitea_api/fork.py`, so it cannot name a user); then re-read `GET /repos/<org>/<repo>` and require a fork with that `parent`: git-obs exits 0 on `ForkExists` when the user already has a fork under another name and says so on stderr only, so its output is not parsed and that case stops the package (`GET /repos/<owner>/<repo>/forks` names the existing fork in the message); a repo of that name that is not such a fork stops the package; (2) push a build branch `auto-package/<target-branch>-build` at the target branch head (the PR branch `auto-package/<target-branch>` is written only by `--submit`, decision 4); (3) `POST /source/<P>/<pkg>?cmd=fork&scmsync=https://src.opensuse.org/<org>/<repo>#auto-package/<target-branch>-build&target_project=<branch project>` through the `osc` chokepoint (the OBS half of `osc fork`, `osc/obs_api/package.py::cmd_fork`). One lock per `<org>/<repo>#<branch>` under the work directory root, held from this step to the end of the package's run, since two runs with different `--project` values reach the same fork branch and the project lock (`cli/src/project_lock.rs`) does not cover it |
| Rebase | `osc rebase` / link merge | `git fetch` the target branch, `git rebase` the build branch onto it; a conflict is reported like `Rebased::Conflict` today, never resolved by hand |
| Check out | `osc co` / `osc up` | `git clone` (anonymous https) of the fork into `<work>/<P>/<pkg>/`, git dir kept outside the tree (section 6), `git lfs pull`; reuse with `git fetch` + fast-forward |
| Pristine | `.osc/sources/<f>` | `git show <base>:<f>`, where `<base>` is the target branch commit the build branch started from, kept as `refs/auto-package/base` |
| Changelog | `osc vc` | `osc vc` works outside an osc working copy: it then takes the default apiurl **[verified]** (osc 1.28.0 `osc/commandline.py`, `do_vc`: `is_package_dir` else `get_api_url()`) |
| Commit | `osc addremove` + `osc ci` | `git add`/`git rm` of exactly the plan's files, with `filter.lfs.*`, `user.name`/`user.email` passed by `-c` (section 6); the build branch holds one commit on top of `<base>`: the first round commits, later rounds amend it (`git commit --amend -m <commit_message>`), decision 4 |
| Publish | (inside `osc ci`) | `git lfs push <fork-url> HEAD` (the `pre-push` hook is off, section 6), then `git push <fork-url> HEAD:refs/heads/auto-package/<target-branch>-build` with `--force-with-lease` against the last pushed commit; `<fork-url>` is the URL of the fork itself (the ssh or https URL of the user's `git-obs` login, never a configured remote name); refused for any other repo or branch, and for any branch that heads an open PR (`push_target_allowed`, section 6); a failed lease (someone else moved the branch) stops the package |
| Wait for sources | - | poll `osc cat <branch-project> <pkg> _scmsync.obsinfo` until `commit:` is the pushed id, for at most 10 minutes; then one `POST /source/<branch-project>/<pkg>?cmd=runservice` and a second 10-minute wait; still stale: the package fails with "OBS did not sync the push", and nothing is built from stale sources (decision 3) |
| Build, wait, results | `osc results`, log, binaries | unchanged, against the branch project |
| Restore after failure | `osc ci` of the baseline | push the build branch back to the commit it held before the run (`<base>` for a branch the run created), with lease against the run's last push; the baseline directory records the host, the fork URL, the build branch, that commit and the last pushed one, and every reader of baselines (`restore_stale_baseline`, the stop handler in `proc/shutdown.rs`, `baseline discard`) dispatches on the recorded host (a record without one is OBS). The PR branch is never restored, so a restore cannot empty an open PR; `pool`, the devel org and the maintainer's fork the devel package scmsyncs are never written |
| Revision pin | `srcmd5` | commit id: the PR is refused when the build branch head is not the commit OBS built; `--submit` pushes that same commit id to the PR branch, so the PR head is the built commit |
| Open requests | `/request?...` | every page of `GET /repos/<owner>/<repo>/pulls?state=open`, matched on `base.ref == <target-branch>`, `head.repo.full_name == <org>/<repo>` and `head.ref == auto-package/<target-branch>`; `GET .../pulls/<target-branch>/<org>:auto-package/<target-branch>` is cheaper, but which PR it returns when closed ones share the head is unverified, so only the list decides (Gitea's `head.label` is the bare branch name, so a filter on `<org>:<branch>` over the list never matches **[verified]**: `pool/stgit!9` has `head.label` `licensedigger`, and `.../pulls/factory/lkocman:licensedigger` answers 200 with PR 9) |
| Submit | `osc sr` | with an open PR from the PR branch: no push, no new PR, reported like an open request today; otherwise push the built commit to `auto-package/<target-branch>` (lease against its current head, if any), then `git-obs pr create --source <org>/<repo>:auto-package/<target-branch> --target <owner>/<repo>:<target-branch> --title ... --description ...` `--allow-maintainer-edit yes` (the option takes `0`, `1`, `yes`, `no`, osc 1.28.0 `pr_create.py`); title `Update <pkg> to <version>` (an update) or the commit message's first line (a lint or fix run), body `sr_description` (the new `.changes` entry first, as pool PRs do); never a direct API call, so the tool never holds the token |
| Diff (TUI `d`) | `osc rdiff` | `git diff <base>..<head>` in the checkout |
| `--in-place` | commit into the project | refused for git (decision 6): pool takes no direct pushes, and a push into a devel org's branch is an unreviewed write to a shared repo |
| Maintenance (`--maintenance`, `--backport`) | incidents | refused for git; SLE 16 maintenance is PRs to `pool/<pkg>#leap-16.x` with `NoProjectGitPR`, a separate plan |
| `build` of a new package | `ensure_package` | builds in an OBS project as today; when the package's distro origin is git, it is forked as above. A brand new package needs a new repo plus an `[ADD]` issue on the project git: printed for the user, never opened by the tool |

Where the PR goes (decision 8):

- **Package-level scmsync to a fork of pool** (`server:database/mariadb-connector-c`
  -> `adamm/mariadb-connector-c#main`): to `pool/<pkg>#factory`, the
  canonical repo, found as the scmsync repo's Gitea `parent` (anonymous
  `GET /repos/<owner>/<repo>`). The branch project package scmsyncs the
  build branch of the user's fork, so OBS builds with the devel project's
  repositories the commit that `--submit` then makes the PR head; later
  runs without `--submit` move only the build branch, never the open PR.
  The maintainer's fork is read, never written, and is not the PR target.
- **Project-level scmsync, devel org** (`devel:languages:perl`): to the
  package repo the project tracks, `perl/<pkg>#main`, as the org's own
  `workflow.config` (`"Organization": "perl"`, `pr`) defines. That is the
  same rule as today's submit request "to the project it was branched
  from"; forwarding to Factory stays the devel maintainers' job.
- **Refused**: a project whose packages resolve to a `pool` product branch
  (`openSUSE:Backports:SLE-16.0` -> `pool/<pkg>#leap-16.0`, Leap 16,
  `slfo-*`), and every package-level scmsync that fails a check of
  decision 8.

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
  refused when `git lfs` is missing, or when `git check-attr
  --source=<base> filter` says a file the plan commits should be LFS and
  the staged blob is not a pointer. The attributes are read from `<base>`,
  not from the worktree: the agent and the services (the sandbox binds the
  checkout read-write, `service_sandbox/sandbox.rs`) can both write a
  `.gitattributes`. Old archives are removed with `git rm` like `osc rm`
  today.
- **`_service`**: unchanged locally: services run in the sandbox
  (`osc::run_service` -> `service_sandbox`) and their outputs (e.g.
  `vendor.tar.zst`, LFS) are committed. `.gitattributes` and `.gitignore`
  are restored to `<base>` after each service run as after each agent turn,
  and `move_outputs` refuses `.git` and `.gitattributes` outputs. Whether
  OBS also runs the package's `_service` after `obs-scm-bridge` is
  **assumed** yes; phase 5b checks it on one package before relying on it.
- **`_scmsync.obsinfo`, `build.specials.obscpio`**: OBS-generated; never
  in the publish plan, ignored by the sweep's file reads.
- **Non-package files** (`README.md`, `.gitattributes`, `.gitignore`,
  `cpanspec.yml`, ...): `publish_plan` skips untracked ones as today, but
  `remove_tracked_strays` must not `git rm` a tracked dotfile or README. In
  git it removes only archives and patches the spec no longer names.
- **Pipeline-owned files**: `.gitattributes`, `.gitignore` and `.git`
  (file or directory, `<checkout>/.git` and `<checkout>/.git/**`) join the
  list of paths the agent may not write (`agent/tools.rs`,
  `turn_denied_tools`); the first two are restored to pristine after each
  turn like `.changes` is today, and a `.git` in the worktree fails the
  turn.
- **Conventions and skills**: one convention says the checkout may be a git
  repository the tool commits and pushes, and the agent edits files only
  (no `git`, no `osc vc`). The agent gets no git command at all on a git
  checkout, not even `git status` or `git diff *`: it holds
  `Edit`/`Write(<checkout>/**)` (`agent/tools.rs`), so it could plant a
  `.git` file pointing at a git dir whose config sets `core.fsmonitor` or
  `diff.external`, which `git status`/`git diff` would run; `git diff`
  also takes `--output=<file>` and `--no-index`; and with the git dir
  outside the worktree (section 6) those commands find no repository
  anyway. The pipeline puts `git diff <base>` of the checkout into the
  prompt instead. `osc status` is not granted on a git checkout either.
- **AGENTS.md**: the rule stays as it is ("auto-package is the only thing
  that changes packages") and gains the git verbs: no `git commit`,
  `git push`, `git-obs pr create` or `osc fork` by hand; reading (`git log`,
  `git diff`, `git-obs pr get`, the Gitea API with GET) is fine for a review.

## 6. Credentials and safety

- **Nothing is pushed anywhere but the run's fork, and no PR is opened
  without `--submit`.** `GitSource::publish` takes the remote from the
  `Branched` record, and a pure check (`push_target_allowed`) refuses a
  remote whose owner is not the run's fork owner (`--fork-org`, else the
  user's login), a repo whose `parent` is not the
  origin repo, and any branch not named `auto-package/...`. It also
  refuses, right before every push, a branch that heads an open PR
  (section 4, "Open requests"), unless the push is the `--submit` step and
  the PR branch has no open PR yet. Without `--submit`, pushes go to the
  build branch only (they are what OBS builds, like today's commits to the
  branch project) and the run summary says so; the PR branch, which is
  what a reviewer sees, is written only by `--submit`.
  `--dry-run` writes nothing, on either host. `push_target_allowed` also
  refuses `pool`, the PR target's owner and the owner of the repo the devel
  package scmsyncs (e.g. `adamm`) as a push target outright, whatever
  `--fork-org` says; the devel package itself (`server:database/<pkg>`)
  is never written either: only the branch project's copy is created.
- **Token handling**: the tool never reads the token itself. Forks
  (`git-obs repo fork`) and PRs (`git-obs pr create`) go through
  `git-obs`, which reads `~/.config/tea/config.yml`; Gitea reads use the
  anonymous API. `--gitea-login` passes `-G`. Pushes name the fork by URL:
  the ssh URL with `SSH_AUTH_SOCK` passed to the push alone, or the https
  URL with `-c credential.https://src.opensuse.org.helper="git-obs -G
  <login> login gitcredentials-helper"`, which hands the token to `git`,
  not to the tool. No direct authenticated API client is part of this
  plan.
- **Agent environment**: `agent/env.rs` is already an allowlist
  (`AGENT_ENV_ALLOW`, `AGENT_ENV_ALLOW_PREFIXES`), so no `GIT_*`, `GITEA_*`
  or `TEA_*` name reaches the agent today, and `GIT_ASKPASS`,
  `GIT_SSH_COMMAND` and `SSH_AUTH_SOCK` are on its denylist too. The
  denylist gains `GIT_OBS_*`, `GITEA_*`, `TEA_*`, `SSH_ASKPASS`, `GIT_SSH`,
  `GIT_CONFIG_*`, `GIT_DIR` and `GIT_WORK_TREE` only as the second layer,
  in case the allowlist grows. The service sandbox already binds an empty
  `HOME` (`--dir` `SANDBOX_HOME`, `service_sandbox/sandbox.rs`), so it sees
  no `~/.config/tea`, `~/.ssh` or git config; nothing to add there.
- **Git itself as an attack surface**: the agent can write the checkout,
  so a `.git/hooks/*`, `.git/config` (`core.fsmonitor`, `core.sshCommand`,
  filter drivers) or a `.git` file pointing elsewhere would run code with
  the user's credentials at the next `git commit`. Hence: the git dir lives
  outside the worktree (`<work>/<P>/.git-dirs/<pkg>.git`), every git call
  passes `--git-dir`/`--work-tree` explicitly, runs with
  `-c core.hooksPath=/dev/null -c core.fsmonitor=false`, a scrubbed env,
  `GIT_CONFIG_NOSYSTEM=1` and `GIT_CONFIG_GLOBAL=/dev/null`, and goes through one `pkgsrc::git::run`
  chokepoint (like the `osc` chokepoint in `proc/capture.rs`).
- **What the hardening takes away, given back explicitly**: without the
  system and global config the `filter.lfs.*` drivers (in `/etc/gitconfig`
  on this machine), `user.name`/`user.email` and any credential helper are
  gone, and with hooks off git-lfs's `pre-push` upload never runs. So
  `git::run` passes `-c filter.lfs.clean="git-lfs clean -- %f"`,
  `-c filter.lfs.smudge="git-lfs smudge -- %f"`,
  `-c filter.lfs.process="git-lfs filter-process"`,
  `-c filter.lfs.required=true`, and the identity, read once by the tool
  from the user's own git config (`git config --global user.name`), as
  `-c user.name=... -c user.email=...`; the credential setting of the
  previous bullet goes on the push only; and every push is preceded by
  `git lfs push <fork-url> <commit>`, whose failure stops the push.
  Phase 4 tests the filter and identity on the local bare repo; phase 5b
  tests the LFS upload and the push on the first run.
- **Rate limits**: Gitea calls get their own token bucket next to
  `ap_core::limits::obs_acquire`.

## 7. Phases

Each phase is one worker commit series that compiles, passes and is useful
on its own. Nothing before phase 5b writes to Gitea.

1. **Detect and refuse.** `parse_project_info` reads the project
   `<scmsync>`; the package meta parser reads the package `<scmsync>`;
   `pkgsrc::detect`; `update`, `lint` and `build` stop early on a
   git-hosted project or package with a message naming the scmsync URL,
   instead of reaching `osc branch`/`osc ci` (today nothing stops a
   package-level scmsync package such as
   `server:database/mariadb-connector-c` from being sent to OBS
   `cmd=branch` and `osc ci` like any other; what OBS then does is
   unverified, section 1). The check keys on the package OBS will branch
   from, not on `--project`, so it sits in the branch chokepoint
   (`osc::branch_package_at`, `branch_package_as`, `branch_package`) and
   covers every caller: `items.rs::branch_item`, `--update-deps` items,
   `pipeline/obs_names.rs` and `cli/src/build_cmd/deps.rs`. With
   `--follow-devel` it first reads the origin's `<devel>` from `osc meta
   pkg` and checks that package too (`update --project openSUSE:Factory
   --only mariadb-connector-c --follow-devel` would branch the scmsync
   devel package). `lint --improve` (`tidy.rs`) and `--revision` refuse a
   git package the same way. The refusals that
   stay after phase 7 (decision 1: product branches, and the package-level
   cases decision 8 rejects) keep this message. Tests: meta fixtures of
   `openSUSE:Backports:SLE-16.0`, `devel:languages:perl` (project
   scmsync), `perl-Moose` (package scmsync in a scmsync project),
   `server:database` plus `mariadb-connector-c` (package scmsync in a
   plain project), and an `openSUSE:Factory` package whose devel package
   scmsyncs (`--follow-devel`). Ships: safety on its own.
2. **Working-copy seam, no behaviour change.** `WorkingCopy` and
   `OscWorkingCopy`; every `.osc/sources`, `.osc/_files` and
   `osc status` reader listed in section 2 goes through it. Tests: the
   existing suite, plus `OscWorkingCopy` on a fixture `.osc/`.
3. **Gitea reads and the git plan.** `pkgsrc::gitea` (anonymous GET: repo,
   branches, raw file, pulls), parsers for `.gitmodules`, `workflow.config`
   (trailing commas and `//` line comments) and `_scmsync.obsinfo`;
   origin resolution, including the decision 8 checks (scmsync repo `parent`, `factory` branch, Factory
   devel package, `compare/factory...<owner>:<branch>`). `update
   --dry-run` and `lint --dry-run` on a git-hosted project or package
   print, per package, the fork, the build and PR branches, the branch-project
   package and the PR target, or the refusal. Tests: fixtures captured
   from the API calls in section 1 (`adamm/mariadb-connector-c`,
   `pool/mariadb-connector-c`, `openSUSE/Factory`'s `workflow.config`).
4. **Git working copy, local only.** `GitWorkingCopy` and the hardened
   `git::run`: clone (anonymous), base ref, pristine, staging, commit, LFS
   checks (attributes from `<base>`), stray rules. No remote writes. Tests:
   local bare repos created by the test under the cargo target dir,
   including a hook and a hostile `.git` file that must not run or
   redirect, a worktree `.gitattributes` that drops `filter=lfs` (still
   refused), and the explicit `filter.lfs.*` and identity `-c` settings
   with `GIT_CONFIG_NOSYSTEM=1`.
5. **`PackageSource`**, in two steps.
   (a) *No behaviour change*: `ObsSource` over today's code; the pipeline,
   `items.rs`, `tidy.rs` and the baseline readers (`run.rs`,
   `proc/shutdown.rs`, `cli/src/baseline.rs`) switch to the trait and
   `source_for`; the baseline record gains its host (absent = OBS).
   Tests: the existing suite.
   (b) *Fork and push, behind `--source-backend git`*: `GitSource::branch`
   (`git-obs repo fork`, build branch, OBS `cmd=fork`, the fork-branch
   lock), `rebase`, `publish` (amend, `git lfs push`, push with lease),
   `wait_for_sources` (bounded wait, one `runservice`), `save_baseline`
   and `restore` of the build branch; fork, push and restore steps take
   `proc::obs_write` so a stop waits for them. Before the first push
   lands, in this same phase: `run.jsonl` events for every fork, push,
   restore and (phase 6) PR, `explain` showing them, the agent rules of
   section 5 (no git grants, `git diff <base>` in the prompt, `.git`,
   `.gitattributes`, `.gitignore` denied), and the AGENTS.md git verbs.
   `update`/`lint` on a package-level scmsync package (first:
   `mariadb-connector-c`, section 9 checklist) and later on a devel-org
   project build in the branch project without `--submit`. Tests: pure
   planners (`cmd=fork` query, push refspec, `push_target_allowed`
   including the open-PR refusal, the wait/trigger/fail sequence), a fake
   runner for the command sequence, a stop during a push (the stop
   handler waits, then the restore runs), a stale git baseline restored at
   the next run start, a failed lease, and the manual run on
   `mariadb-connector-c`; that run also checks the three assumptions
   decisions 2 and 3 rest on (what `cmd=fork` creates, whether a fork push
   syncs by itself, whether `runservice` re-syncs an scmsync package) and
   that the LFS objects reached the fork.
6. **Pull requests.** `GitSource::open_requests`, `request`, `diff`;
   `prepare_request` and `SubmitBackend` through the trait; commit-id pin;
   the push of the built commit to the PR branch; the open Factory request
   check by target (decision 8); `git-obs pr create` under
   `proc::obs_write`; TUI `S`/`A`/`d`. Tests: request planning, the open-PR
   match on paginated fixtures, no push to a PR branch with an open PR,
   refusal when the head moved, `git-obs pr create --dry-run` argument
   building (with `--allow-maintainer-edit yes`).
7. **Default on, `build` parity, docs.** `--source-backend auto` becomes
   the default for the scopes of decision 1; `build` forks git-origin
   packages the same way; conventions, skills and
   `docs/agents/work-directory.md` (git dirs, the build and PR branches,
   git baselines) updated.

Later, not in this plan: product branches in `pool` (Backports SLE-16.0,
Leap 16, `leap-16.1`, `slfo-*`) together with maintenance PRs for
`leap-16.x`, package-level scmsync packages outside Factory's devel
chain, `--in-place` for
`workflow-direct` devel orgs, multi-package PRs (`PR: pool/pkgA!2` lines,
`git-obs pr --recursive`), new-package `[ADD]` issues.

## 8. Risks

- **Sync lag and triggers.** If a fork has no webhook, OBS never sees the
  push; the wait is bounded and fails the package (decision 3), but
  whether `runservice` re-syncs an scmsync package is **[assumed]** until
  the phase 5b run.
- **Own fork steps drifting.** The tool does what `osc fork` does with its
  own calls (decision 2); if OBS or `git-obs` change `cmd=fork` or the
  fork layout, the tool must follow. What `cmd=fork` creates in the branch
  project (repositories, build paths) is **[assumed]** until phase 5b.
- **Fork name clashes.** A Gitea owner holds one repo per name; a fork of
  `perl/perl-Moose` and one of `pool/perl-Moose` cannot both be
  `<user>/perl-Moose`. A package is in one scope only (decision 1), and an
  existing repo of that name whose `parent` is not the expected repo stops
  the package, so a clash is refused, never resolved by renaming; settling
  it (`--new-repo-name`) is left to when a user hits it.
  `dspinella/mariadb-connector-c` does not exist today **[verified]**.
- **The PR skips the devel project's maintainers.** A PR to
  `pool/<pkg>#factory` is reviewed by `openSUSE/Factory`'s reviewers
  (`gitea_test`, `~legaldb`, `~factory_bot`, merge by `merge ok`), not by
  the people who push to the maintainer's fork. The OBS branch project is
  still branched from the devel package, so the build is the devel build. For the first
  target the user is one of the devel package's maintainers
  (`server:database/mariadb-connector-c` lists `dspinella`)
  **[verified]**. After a merge the maintainer's fork is behind `pool`
  until its owner syncs it; the tool does not touch it.
- **Two routes into Factory.** The devel project can still submit to
  Factory by `osc sr` while a PR is open, or after it; either way one of
  the two goes stale. Before opening a PR the tool checks both sides
  (decision 8); after that, a later run rebases the build branch onto the
  new `factory` head like any rebase (section 4), and a conflict stops the
  package.
- **SHA-256 repos.** Pool repos use SHA-256 object ids; the git working
  copy and its test repos (phase 4) must use `--object-format=sha256`
  and accept 64-digit commit ids in `_scmsync.obsinfo`.
- **LFS.** A missing `git-lfs`, quota or an upload failure leaves a pushed
  commit whose archives OBS cannot fetch; the explicit `git lfs push`
  before the `git push` (section 6) stops the push on such a failure, but
  whether Gitea refuses a push whose LFS objects are missing is
  unverified.
- **Two runs on one fork branch.** Two runs with different `--project`
  values (e.g. `server:database` and `openSUSE:Factory --follow-devel`)
  reach the same `<user>/<pkg>#auto-package/factory-build`; the project
  lock does not cover that. The fork-branch lock (section 4) covers runs
  sharing a work directory; across work directories only the lease does,
  and a run that fetched after the other's push leases against it and
  overwrites it.
- **Agent writes to `.osc/` (pre-existing, not a git issue).** Nothing in
  `turn_denied_tools` (`agent/tools.rs`) denies `Edit`/`Write` on
  `<checkout>/.osc/**`, while the service sandbox binds `.osc` read-only
  because `osc` commits to the API named in `.osc/_apiurl` with the user's
  credentials (`service_sandbox/sandbox.rs`). The agent can rewrite
  `_apiurl`, `_files` or the pristine copies the pipeline trusts. To fix
  apart from this plan: deny `Edit`/`Write(<checkout>/.osc/**)`.
- **Stale reads.** The sweep reads OBS, which follows the project git with
  a delay; the base of the build branch must come from git, not from OBS.
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

1. **Scope: Factory devel packages, then devel organisations.** *Revised
   2026-10-30: the canonical repository of a package is `pool/<pkg>`.*
   Two cases get the git workflow, in this order:
   (a) a package in a project without `<scmsync>` whose package meta
   scmsyncs a fork of `pool/<pkg>` and that is Factory's devel package
   (`server:database/mariadb-connector-c` -> `adamm/mariadb-connector-c#main`,
   parent `pool/mariadb-connector-c`): the PR goes to
   `pool/<pkg>#factory` (decision 8). This is the first target.
   (b) a project with a project `<scmsync>` whose packages resolve to a
   devel org's repos (`devel:languages:perl` -> `perl/<pkg>#main`): the PR
   goes to the org repo, as before.
   Refused with the phase 1 message: product branches in `pool`
   (`openSUSE:Backports:SLE-16.0`, Leap 16, `leap-16.1`, `slfo-*`), and a
   package-level scmsync that fails a decision 8 check. `pool` is never a
   push target in any case; only a PR target, and only its `factory`
   branch. *Why*: (a) is what the user's first package needs, and
   `pool/<pkg>#factory` is where `openSUSE/Factory`'s `workflow.config`
   says PRs go; the maintainer's fork is a personal repo, not a review
   point. (b) is kept as it was because the org's own `workflow.config`
   (`"Organization": "perl"`, `pr`) declares its repos as the review
   point, and bypassing the devel project would reach Factory without the
   devel maintainers. Product branches are the maintenance side
   (`NoProjectGitPR`, release reviewers, `--maintenance`/`--backport`
   territory) and need their own plan. *Open*: (b) targets the org repo,
   not `pool`; revisit if the user wants devel orgs to PR to `pool` too.
   Product branches, in the later maintenance plan. Note: of the projects
   run so far, `X11:Wayland` and `devel:languages:c_c++` have no project
   `<scmsync>` **[verified]**; their packages are now checked one by one
   for a package-level `<scmsync>` (phase 1).
2. **Fork with the tool's own steps, not `osc fork`; forks live in the
   user's own Gitea namespace.** The steps are the row "Branch / fork" in
   section 4: `git-obs repo fork` (token stays in `git-obs`; no
   `--target-owner` for the personal namespace), a build branch
   `auto-package/<target-branch>-build`, OBS `cmd=fork` into the run's
   branch project. The fork is of the PR target (`pool/<pkg>` in case (a)),
   never of, and never into, the maintainer's fork the devel package
   scmsyncs: for `mariadb-connector-c` that is
   `dspinella/mariadb-connector-c` with parent `pool/mariadb-connector-c`,
   not a fork of `adamm/mariadb-connector-c`. `--fork-org` overrides the
   namespace; `pool`, the PR target's owner and the scmsync repo's owner
   are never accepted. *Why* the fork of `pool`: the PR head must be in
   the target's fork network, and a fork of the maintainer's fork would
   tie the user's branch to a personal repo. *Why* own steps: `osc fork` (osc 1.28.0,
   `osc/commands/fork.py`) **[verified]** (a) ignores the scmsync branch
   and uses the repo's default branch (lines 149-157, "temporary hack"),
   (b) silently switches to the devel project unless `--no-devel-project`
   (117-122), (c) clones into the current directory with the git dir
   inside the worktree (193-216), against section 6, and (d) exits when
   that directory exists (195-197), so it is not idempotent. Its two
   remote calls are what the tool makes itself. The personal namespace is
   the `git-obs repo fork` default and the equivalent of
   `home:<user>:branches`. *Open*: what `cmd=fork` creates, checked in the
   phase 5b run.
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
   is **[assumed]**; the phase 5b run checks it, and if it does not, the
   trigger is dropped and only the bounded wait remains.
4. **PR shape: one commit, the commit OBS built; build and PR branches
   apart.** The build branch `auto-package/<target-branch>-build` is
   always one commit on `<base>`, amended each round and pushed with
   lease; it is what the branch-project package scmsyncs. The PR branch
   `auto-package/<target-branch>` is written only by `--submit`, which
   pushes to it the commit id OBS built, so the PR head is the built
   commit and the pin stays a commit id (no squash at submit time). A
   push without `--submit` (a later `update`, `lint`, rebase or restore)
   moves only the build branch, so it never changes an open PR, as a
   later commit to the branch project never changes an OBS request pinned
   to its srcmd5. With `--submit` and a PR already open from the PR
   branch, nothing is pushed and no PR is made, reported like an open
   request today. Title `Update <pkg> to <version>`, or the commit
   message's first line for a lint or fix run; body `sr_description`.
   `--allow-maintainer-edit yes`. In case (a) the PR is `--source
   <user>/<pkg>:auto-package/factory --target pool/<pkg>:factory`,
   `<base>` is the `pool/<pkg>#factory` head, and the body's last line
   names the OBS project the head was built in
   (`home:<user>:branches:server:database`), whose package is created by
   `cmd=fork` from the devel package with
   `scmsync=https://src.opensuse.org/<user>/<pkg>#auto-package/factory-build`
   and so uses the devel project's repositories. *Why*: one reviewable
   change per PR, as pool PRs show; reviewers see a head that moves only
   when the user asks (`--submit`); maintainers can fix the PR in place,
   and the lease on the `--submit` push stops the package instead of
   overwriting them. *Open*: nothing.
5. **Pushes without `--submit`: yes, to the user's fork only.** They are
   the equivalent of today's commits to `home:<user>:branches:<P>`: the
   user's own namespace, needed for OBS to build. Only the build branch
   `auto-package/<target-branch>-build` of the run's fork, recorded in the
   run summary and `run.jsonl` from phase 5b on; `--dry-run` pushes
   nothing; no push to the PR branch and no PR without `--submit`.
   *Open*: nothing.
6. **`--in-place` on git: refused.** *Why*: pool takes no direct pushes,
   and pushing into a devel org's branch or a maintainer's fork (the repo a
   devel package scmsyncs) skips review on a repo others consume; in
   case (a) `--in-place` would mean writing `adamm/<pkg>#main`, which is
   someone else's repo even when the user co-maintains the OBS package.
   *Open*: `workflow-direct` orgs, later, if asked for.
7. **No direct authenticated Gitea client.** Writes go through `git-obs`
   and `git`; reads are anonymous. *Why*: the tool never reads a token.
   *Open*: nothing.
8. **Package-level scmsync: target the pool parent's `factory` branch.**
   Added 2026-10-30. For a package `<P>/<pkg>` whose meta carries
   `<scmsync>https://src.opensuse.org/<owner>/<repo>#<ref></scmsync>` in
   a project without one, the tool resolves the PR target with anonymous
   reads and refuses the package (phase 1 message, naming the failed
   check) unless every check holds:
   1. `openSUSE:Factory/<pkg>`'s meta names `<devel project="<P>"
      package="<pkg>"/>` (`osc meta pkg`), so `factory` is the right
      branch;
   2. `GET /repos/<owner>/<repo>` has `fork: true` and
      `parent.full_name == "pool/<pkg>"`; the target is that parent, not
      a repo found by name;
   3. `pool/<pkg>` has a `factory` branch; the target branch is always
      `factory`, never the scmsync `<ref>` (`main` exists only in the
      fork) and never a product branch;
   4. the scmsync `<ref>` holds no commit missing from
      `pool/<pkg>#factory` (`GET /repos/pool/<pkg>/compare/factory...<owner>:<ref>`
      reports 0 commits in the dry-run; `git merge-base --is-ancestor` in
      the clone before any push). Otherwise the maintainer has unmerged
      work, a PR from `factory` would not contain what `<P>` builds, and
      the package stops with "the devel fork has commits not in
      pool/<pkg>#factory". When `factory` is ahead of the fork the run
      goes on from `factory` (canonical) and the plan says how far ahead.
   Then: fork `pool/<pkg>` into the user's namespace (decision 2), build
   branch `auto-package/factory-build` at the `factory` head,
   branch-project package by `cmd=fork` from `<P>/<pkg>` with the build
   branch as `scmsync` (decision 4). With `--submit`, before pushing the
   PR branch, the tool also checks for an open PR on `pool/<pkg>` into
   `factory` from the user's fork (section 4, "Open requests") and for an
   open OBS submit request from any source to `openSUSE:Factory/<pkg>`;
   either one means no new PR, reported like an open request today. The
   OBS check needs a new query by target
   (`/request?view=collection&types=submit&states=new,review&roles=target&project=openSUSE:Factory&package=<pkg>`):
   `osc::open_submit_requests` (`osc/branch/submit.rs`) asks by source
   and keeps only requests from the given source project, so it would
   miss a request from anywhere but `<P>`. The run never writes `<P>/<pkg>`, the
   scmsync repo, or `pool`. *Why*: the user named `pool/<pkg>` the
   canonical repository; `openSUSE/Factory`'s `workflow.config` makes
   `pool`/`factory` the PR route **[verified]**; the Gitea `parent` field
   proves the fork relation without guessing from names; and refusing on
   any mismatch keeps a wrong PR from reaching Factory reviewers.
   *Open*: that a merged `factory` PR reaches `openSUSE:Factory` on OBS
   is **[assumed]** (section 1); it is not needed before `--submit` is
   first used.

### First run: `mariadb-connector-c`

Dry-run first, no `--submit` in any step; each step only after the phase
it needs has landed. Every command is an auto-package command; checks are
read-only (`osc meta`, `osc cat`, anonymous Gitea GET).

1. Before: `git`, `git-lfs` and `git-obs` installed, the `git-obs` login
   for src.opensuse.org works (set up by the user), `df -h /home` above
   2.5 GB. Record the heads of `pool/mariadb-connector-c#factory` and
   `adamm/mariadb-connector-c#main` and the meta of
   `server:database/mariadb-connector-c`.
2. After phase 1: `auto-package update --project server:database --only
   mariadb-connector-c --branch-project home:dspinella:branches:server:database
   --dry-run` refuses the package and names
   `https://src.opensuse.org/adamm/mariadb-connector-c#main`.
3. After phase 3: the same command and `auto-package lint --project
   server:database --only mariadb-connector-c ... --dry-run` print the
   scmsync repo `adamm/mariadb-connector-c#main`, its parent
   `pool/mariadb-connector-c`, target `pool/mariadb-connector-c#factory`
   and its head, "devel fork ahead: 0", fork
   `dspinella/mariadb-connector-c` (to be created), build branch
   `auto-package/factory-build`, PR branch `auto-package/factory`
   (written only by `--submit`), branch-project package
   `home:dspinella:branches:server:database/mariadb-connector-c` with its
   scmsync URL, and the PR title. Each line is checked against section 1.
4. After phase 5b: the same command without `--dry-run` (still no
   `--submit`); if auto-package finds no update, `lint` is the run. Check:
   the fork's `parent` is `pool/mariadb-connector-c`; the branch-project
   package scmsyncs `dspinella/mariadb-connector-c#auto-package/factory-build`
   and its `_scmsync.obsinfo` `commit:` is the pushed commit; the
   archives OBS fetched are the tarballs, not LFS pointers; no
   `auto-package/factory` branch exists; its repositories are
   `server:database`'s; the build result is the pipeline's; `run.jsonl`
   and `explain` show the fork and the pushes; the two heads and the meta
   of step 1 are unchanged. This
   run also answers the phase 5b assumptions (what `cmd=fork` creates,
   whether a fork push syncs by itself, whether `runservice` re-syncs).
5. `--submit` only after phase 6, and only when the user chooses it after
   reading the diff (TUI `d`) and the `git-obs pr create --dry-run`
   arguments.

### Needs the user

Nothing: every fact the first run needs was checked read-only, and the
Gitea login comes from the user's `git-obs` configuration (`git-obs api
/user`), as does the push transport (ssh, or https through the
`git-obs` credential helper).
