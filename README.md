# Auto-Package

Automated RPM packaging for openSUSE with AI-assisted spec repair.

## Features

- **Automated Dependency Resolution**: Automatically resolves and builds dependency chains from PyPI
- **AI-Assisted Spec Repair**: Uses Claude to fix failing builds and validate spec files
- **Interactive TUI**: Real-time build monitoring with a beautiful terminal UI
- **Event Logging**: Comprehensive JSONL event logging for analysis
- **Parallel Builds**: Concurrent package building for maximum efficiency

## Usage

### Basic Build

```bash
auto-package build <package-name> --project <obs-project>
```

### With Interactive TUI

Launch the build with a real-time terminal UI that shows every package in the
current build (the root package and any dependencies queued alongside it) as
a table, with a detail pane for whichever package is selected:
- Per-package status, current step, elapsed time, and cost
- Live event stream and agent activity for the selected package
- Aggregate progress and cost across the whole run

```bash
auto-package build <package-name> --project <obs-project> --tui
```

### Allowing a website

Services (the `_service` helpers run in the sandbox) and source fetches may
contact only the hosts on the built-in list, `AUTO_PACKAGE_SOURCE_HOSTS`, the
hosts in your `allowed-hosts` file and the hosts you allow for a run. A refusal
names the host and how to allow it. Three ways, from the narrowest:

- `--allow-host <HOST>` on `build`, `update` or `lint` (repeatable): this run
  only.
- `auto-package hosts allow <HOST> [--reason TEXT]`: permanently, written to
  `$XDG_CONFIG_HOME/auto-package/allowed-hosts` (default
  `~/.config/auto-package/allowed-hosts`, mode 0600 in a 0700 directory). Edit
  the file by hand too: one host per line, `#` for comments and reasons.
  `auto-package hosts list` shows every host, where it comes from and what it
  covers;
  `auto-package hosts remove <HOST>` removes one you allowed.
- In the TUI, on a package that was refused a host: `h` opens a prompt that
  lists up to four refused hosts, one per line, with the package and the
  request that refused each one (for example `service cargo_vendor` or
  `source lookup`). Nothing is allowed by one key. `Y` allows the hosts shown
  for this run. `p` allows a host permanently: type its exact name and press
  Enter, one host at a time. The prompt needs room for every line: if it says
  the terminal is too small, enlarge it, since `Y` and `p` refuse while a host
  would be cut off. Esc cancels; any other key does nothing. Then `r` retries
  the package.

A host is a plain name under a domain: no wildcard (`*.example.org` is not
supported), no port, address, scheme or path, no single label (`com`) and no
public suffix (`co.uk`). It covers its subdomains, for services and for source
fetches alike. Allowing a host does not open other ports or private addresses.
It is a security decision: the host receives the requests the tool makes for
sources. Only you can allow a host. The prompt offers only hosts the tool itself
refused for that package, and a host that a service asked for can be among
them. A package's service decides which hosts it asks for, so read each host
and its `refused for` line before you confirm; nothing is allowed until you
press `Y` or type the name. The agent cannot read or write the file.

### TUI Controls

- `↑`/`↓` or `j`/`k`: Select a package to view its detail pane
- `q` or `Esc`: Exit the TUI
- `Ctrl+C`: Force quit

### Examples

```bash
# Build a package with TUI enabled
auto-package build markupsafe --project home:user:python --tui

# Build with specific version
auto-package build markupsafe --version 2.1.5 --project home:user:python --tui

# Build from GitHub repository
auto-package build https://github.com/BurntSushi/ripgrep --project home:user:rust --tui
```

### Options

- `--version`: Pin a specific version (defaults to latest)
- `--project`: OBS project name (required)
- `--work-dir`: Working directory for build artifacts (default: `ap-work`)
- `--distro`: Repository to build for (default: the project's own, `openSUSE_Tumbleweed` first, then `openSUSE_Factory`, else the one that builds the architecture; a choice between several is printed)
- `--arch`: Target architecture (default: `x86_64`)
- `--allow-host <HOST>`: Allow this host for services and source fetches for this run only; repeatable. See [Allowing a website](#allowing-a-website)
- `--tui`: Enable interactive TUI mode
- `--log`: Log level when not using TUI: error, warn, info, debug, trace (default: `info`)
- `--max-attempts`: Maximum AI fix attempts per package (default: 10)
- `--budget-usd`: USD budget cap for AI calls, per package (its spend counts within one run; default: 10.0). A turn is not started when the spend so far plus the last turn's cost would reach it, and the package stops with that reason. Two `no change` answers in a row of one task kind with the same reasoning stop it too.
- `--max-total-usd <USD>`: most the AI may spend per UTC day across all runs and processes sharing the work directory, kept in `<work-dir>/ledger.json` (default: 50). A turn that would pass it is refused with a message naming the cap; an unreadable or locked ledger refuses too.
- `--max-packages <N>`: most packages one run starts, counting the dependency closure (default: 25, 0 = unlimited). Past it nothing more is queued and the run stops and says so.
- `--notes <DIR>`: where per-package notes are kept (default: `../auto-package-notes`, relative to the current directory; created on first write, outside this repository)
- OBS requests (`osc api` and the other `osc` calls) share one rate limiter: 5 requests per second, burst 10; `AUTO_PACKAGE_OBS_RPS=<n>` overrides the rate.
- `--no-agent`: Build only, never call the AI model
- `--unsafe-resolve`: Run pip without its sandbox (see the threat model)
- `--source-url [PKG=]URL`: Source repository for a wheel-only package; `PKG=wheel` repackages the upstream wheel instead of building from source (e.g. `--source-url torch=wheel`), replacing an existing source recipe
- `--force`: Rebuild even if already built
- `--regen`: Regenerate spec from scratch
- `--cleanup`: Delete rebuildable caches (`rpms/`, `state/<pkg>/sdist`) before building and print the bytes freed. Never deletes `runs/`, `state/`, `ledger.json` or osc checkouts, and refuses (listing them) when a checkout has uncommitted changes.
- `--jobs`: Maximum concurrent builds (default: 4; `--jobs 0` = unlimited)
- A package built from a prebuilt wheel has its unpacked tree (`state/<pkg>/sdist`) deleted once it is Built; a later run unpacks the wheel again.
- `--min-free-gb`: GiB that must be free on the work directory's disk before a package starts or downloads its RPMs; it waits up to 10 minutes, then fails with `No space left on device` (default: 3, 0 = off)
- `--local`: Run the build step with `osc build` on this machine, against the checkout, instead of committing to OBS and waiting for its build; the fix loop works from the local build log as it does from the OBS one, and nothing is committed from the build step. `osc build` needs root: run as root, or allow `osc build` through sudo for your user without a password (`sudo -n -l osc build` must succeed; a sudo that wants a password, a missing osc or a missing build root stops the build with a message saying so). The build root needs many GB, so a build refuses to start with less than `--local-min-free-gib` (default: 10) free where the root is; `--local-root <DIR>` sets the root (osc's default: `/var/tmp/build-root`). Builds run one at a time, with no terminal: a credential prompt fails at once. A build that runs past 4 hours is stopped (SIGTERM, then SIGKILL to sudo's session); a root process of it may survive that, and the log keeps what it printed.
  - Files `osc build` writes as root (in the checkout and under `rpms/`) stay root-owned; auto-package does not change their owner.
  - A local build is recorded as `local-success` in the package state, never as a success: a later run without `--local` builds the package on OBS as usual, and the run summary, the TUI and the submit refusal say `built locally`. Its RPMs are kept in `rpms/local/`, apart from the RPMs OBS published, and the dependencies of later local builds are taken from there. `--cleanup` deletes `rpms/`, `rpms/local/` with it.
  - On `update` it cannot be combined with `--submit` or `--maintenance`.
- `--nvidia <wheel|redist>`: How the NVIDIA CUDA libraries are packaged: `wheel` repackages the `nvidia-*` PyPI wheels as `python-nvidia-*` (default), `redist` packages NVIDIA's redistributable archives as `cuda-<name>`. The choice is remembered in the work directory (`<project>-<arch>/nvidia-mode`), so a restart keeps it; passing the other value switches it with a warning. `--nvidia-redist` is shorthand for `--nvidia redist`.

### Updating a whole project

`update` browses a *source* OBS project, finds the packages that have a newer
upstream release, branches them into a *branch* project, builds the new
versions there with the ordinary pipeline, and optionally submits them back.
The source project is only read, unless `--in-place` is given.

```bash
# See what would happen: plan, branch project, build paths, requests.
auto-package update --project Base:System --dry-run --submit

# Update the packages that have a release and build them in
# home:<you>:branches:Base:System.
auto-package update --project Base:System

# Only some packages, then submit requests for those that built.
auto-package update --project Base:System --only libfoo,libbar --submit
```

Options (besides the build-related `--work-dir`, `--notes`, `--distro`,
`--arch`, `--log`, `--tui`, `--max-attempts`, `--budget-usd`, `--no-agent`,
`--jobs`, `--max-total-usd`, `--max-packages`, `--regen`, `--unsafe-resolve` that `build` takes):

- `--branch-project`: where to branch and build (default
  `home:<osc user>:branches:<SOURCE>`, what `osc branch` uses). Created by the
  first branch if it does not exist. It must not be the source.
- `--only`, `--exclude`: comma-separated package names.
- `--dry-run` (alias `--check`): print the plan, the branch project, the build
  paths that would be added and the requests that would be created, and change
  nothing.
- `--in-place`: build the new versions straight in the project instead of in a
  branch project, for a project you own. Nothing is branched or submitted, so it
  cannot be combined with `--submit`, `--branch-project`, `--update-deps` or
  `--follow-devel`.
- `--update-deps`: also branch and update dependencies that live in another
  project and have a newer release themselves (see below).
- `--min-free-gb`: as for `build` (default: 3, 0 = off).
- `--local`, `--local-root`, `--local-min-free-gib`: as for `build`; `--local` cannot be combined with `--submit` or `--maintenance`.
- `--nvidia <wheel|redist>` / `--nvidia-redist`: as for `build` (default: `wheel`, or what the work directory last used).
- `--submit`: create submit requests (see below). `--yes` skips the question.
- `--deprecated`: include the packages of SOURCE whose openSUSE:Factory package
  names another devel project (see below). Also use it for a SOURCE that is not
  a devel project, where it turns the check off. Not for `--maintenance`.

Packages are branched with the OBS branch API, without following a devel
project, so the request goes back to the project named. A package that is
already in the branch project is reused. Maintenance projects and locked
projects are refused with an explanation. A PyPI-sourced package whose OBS name
is not `python-<name>` is skipped, since the pipeline would build a second
package next to it.

Before a package is branched, the `Version:` of its copy in the branch project
is compared with the origin's and with upstream's. A copy at upstream's version
whose build there succeeded is listed as already updated and not rebuilt (its
submit request can still be made). A copy at upstream's version whose build did
not succeed, or an older copy, is continued from and rebuilt. A copy that is
newer than upstream's is listed under "needs the maintainer" and left as it is,
unless `--to-version` names the target. A copy or a build result that cannot be
read leaves the package alone, skipped with the cause. The comparison is made
for an upgrade in a plain `update` only, not for `lint`, `--in-place`,
`--revision` or a maintenance backport.

A package whose source is not Repology is also compared with Repology's newest
version for it. When Repology is newer and differs in the major or minor
number, the package is skipped as needing a maintainer: the two sources
disagree about which release to package, and the tool never picks one
silently. The reason names both versions and the source, and a maintainer
chooses the version and runs `update` with `--to-version`, which is not
compared. A difference only in the patch level is noted in the plan and the
update goes ahead. A Repology error, or a project Repology does not know, has
no effect. A package held to a series or a major (`python-foo3`, a versioned
name, a series copy) takes that series' own newest and is not compared:
Repology's newest may be in another series. Each compared package costs one
Repology request. A calendar version on either side (a year first, as `2024.05`
or `20240101`) is not compared, since its major and minor numbers mean nothing.

A package of SOURCE whose openSUSE:Factory package names another devel project
than SOURCE is deprecated for SOURCE: `update` does not branch it, and lists it
under "needs the maintainer" with the reason. Pass `--deprecated` to include it
as any other package. This is the only case it skips. The check is meant for a
SOURCE that is a devel project of Factory: for another source (Leap, SLFO, a
branch project, a sibling devel project) Factory names a different project for
nearly every package, so most of them are listed under "needs the maintainer"
until `--deprecated` is passed. A package that Factory
does not have, a Factory package whose meta names no devel project, and a failed
read are not skipped, so a package Factory has dropped is not detected. Only
`update` of a source other than openSUSE:Factory checks this: not a dependency
from another project, `--in-place`, `lint`, or `--maintenance`.

An origin with a broken `_link` or no spec file is reported with its cause,
listed under "needs the maintainer", and not branched; an origin whose spec
read failed is skipped with that cause, since the read may pass on a retry.

The run ends with a summary (updated, failed, skipped, requests, build paths
added, branch project) and exits non-zero when a package failed.

### Improving the quality of a whole project

`lint` checks every package of an OBS project and fixes what it
finds, with the same branch-build-submit flow as `update`.

```bash
# Print the audit and what would be done, change nothing.
auto-package lint --project home:you:python --dry-run

# Fix in a branch project, then send the fixes back as requests.
auto-package lint --project Base:System --only libfoo --submit

# Commit straight into a project you own.
auto-package lint --project home:you:python --in-place
```

What it checks, per package:

- **build**: the latest build is failed, unresolvable or broken;
- **rpmlint**: errors and warnings in the build log, grouped by tag;
- **style**: `%check` after `%files`, a `%{python_expand #}` wrapper, a pure
  Python package without `BuildArch: noarch`, a literal version where
  `%{version}` belongs, unused Python `BuildRequires` of a wheel install;
- **no-check**: the spec has no `%check`;
- **dropped-tests**: tests removed from `%check` to get the package to build.

Style slips are fixed deterministically and committed without a build. Every
other finding sends the package through the ordinary pipeline at its current
version, with the findings handed to the agent as the problem to fix in the
existing recipe (so `--no-agent` skips those). A package whose source
auto-package does not recognise is only reported. Takes the options of
`update`, except `--update-deps`; `--in-place` cannot be combined with
`--submit` or `--branch-project`.

#### Dependencies in other projects

A package in `Base:System` may build-require packages published by another
project such as `devel:languages:c_c++`. The branch project builds against its
repository paths, so such a package resolves only if some path, directly or
through the paths of that path, reaches a project that publishes it.

After branching, each package's `BuildRequires` names are looked up on OBS (one
published-binary search per name, cached for the run) and compared with the
projects the branch project's paths reach, repository by repository. A name that
is published only outside that chain is made reachable by adding the publishing
project to the branch project's meta as `<path project=... repository=.../>`:

- the path points at the repository of the *same name* in the added project;
  a branch repository for which the project has none gets nothing, with a
  warning;
- it goes after the existing paths, so it only supplies what they lack, and is
  never added twice (re-running changes nothing);
- the fewest projects are added: the project covering the most missing names is
  taken first; personal (`home:`) projects are never added;
- every added path is reported as it happens and listed in the summary.

When a build later reports a missing dependency, OBS is searched for it before
the pipeline creates a package for it; if a project publishes it, that project
becomes a build path instead of a duplicate package being built.

Capability-style requirements (`pkgconfig(foo)`, `%{python_module foo}`) cannot
be looked up by name and are reported as not checked; the build finds those.

With `--update-deps`, a dependency that the build will use from a project other
than the source, the branch project, a distribution project (`openSUSE:`,
`SUSE:`) or a personal one, and that has a newer upstream release, is branched
and updated too, into the same branch project and ahead of the package that
needs it. Its submit request goes to its own origin project. Without the flag,
such a dependency is only referenced through a build path and never touched.

#### Submit requests

`--submit` creates one request per package that built, to the project the
package was branched from (which is not always the source project). It is
deliberately conservative:

- a package gets no request unless OBS reports its build in the branch project
  as succeeded and the branch spec is at the new version; failed and skipped
  packages never do;
- a package with an open request (new or in review) from the branch project to
  that origin gets none; if the open requests cannot be read, none is created;
- the list is shown and confirmed before anything is created; without a
  terminal, or with `--tui`, pass `--yes` or nothing is submitted;
- the description gives the old and new version and the newest `.changes`
  entry. Created request ids are printed and listed in the summary.

### Reviewing a submit request

`auto-package review <request-id>` reviews one OBS submit request, read only:

```sh
# The report on stdout, saved under ap-work/reviews/1394662/<run-id>/.
auto-package review 1394662

# The same as JSON, for jq; no source archives downloaded.
auto-package review 1394662 --format json --no-download | jq '.actions[].findings[].id'

# Only a blocker fails the review (exit status 1).
auto-package review 1394662 --fail-on blocker
```

It reads the request document (a revision number or a srcmd5 in the action are
both accepted), the source package at that revision and the target package as it
is now, the source diff, the build results of the source and of the target, the
other open requests to the same package, and the upstream version. Then it
runs checks on what the request *changes*, not on what was already there:

- the spec: the lint of `lint` for what the request adds, an unfinished
  skeleton, a lost `%check`, dropped tests;
- the newest `.changes` entry: header, date, `Update to <version>` against the
  `Version:`, notes, patches named, dependency bullets;
- patches and `rpmlintrc` files, with the rules the pipeline applies to its own
  edits;
- the risk flags of the submit summary (new scriptlets, hosts, services, large
  patches, a patch outside the archive);
- that every `Source` and `Patch` file is in the package, and what is left
  over;
- signatures and the keyring of the new archives (with `gpg`, in a fresh home);
- the build results against the target's, and the packaged version against
  upstream and Repology.

Each finding has a stable id (`spec.no-check`, `build.failed`, ...), a severity
(`blocker`, `warning`, `note`) and its origin. The ids are part of the JSON
(`schema` 1). The exit status is `0` when no finding is at or above `--fail-on`
(default `warning`), `1` when one is, and `2` when the review could not be
completed (see Exit codes). Two kinds of read can fail. A read the checks need
(the request, the listings, the spec, `.changes`, patch, `rpmlintrc` and
`_service` texts, the build results of the source and of the target, the
download of a signed archive) ends the review with status 2 and no report,
whatever `--fail-on` says: a review that could not see the build result must
not read as one with nothing found. An optional read (the diff, the other
open requests, the upstream and Repology lookups, the main archive) that
fails is a `review.incomplete` or `version.not-checked` note, and a check that
was skipped on purpose (`--no-download`, a file over the size cap) is listed
under "Not checked": it is never reported as passed. A request that holds an
action other than `submit` is not reviewed beyond a finding for that action,
and a role, bugowner, devel-project, delete or maintenance action is a
blocker. The upstream lookups only contact hosts the allowed-hosts list allows
(and the standard registries); a spec that names another host gets a note, not
a request to it.

**It never writes to OBS.** It accepts, declines, comments on and reviews
nothing, commits nothing and creates no request; the one call that is not a GET
is the source diff, which `osc rdiff` makes too. A test reads the source of the
read modules and fails on a write verb. Posting the report as a comment on the
request (a `--post-comment` flag) is wanted later, and would be the command's
only write, behind that explicit flag. It is not part of this command, and the
report formats leave room for it.

With `--agent`, one more turn of the model reads the request's files (the
request, the diff and both sides' spec, `.changes` and patches) for what the
checks cannot say: whether the bullets describe the diff, a refreshed patch
whose hunks changed meaning, a change the bump does not need. It runs on Sonnet
or better, with a budget of `--budget-usd` (default 2.00), and needs the
`claude` CLI logged in. The turn has no Edit, Write, Bash or search tool. It may
read the upstream forge host of the source, and only when the allowed-hosts list
(`hosts allow`, the file, `AUTO_PACKAGE_SOURCE_HOSTS` or the built-ins) already
allows that host: the agent can never allow a host. Its findings are the model's
opinion: they are printed apart, each line starts with `AGENT:`, the JSON keeps
them under `agent` and never in an action's `findings`, and a model's text is its
own summary, not ground truth. They do not change the exit status, unless
`--fail-on-agent` is given; the deterministic findings are never removed or
lowered by them. A turn that fails leaves the report and its exit status as
they were, except with `--fail-on-agent`, where a turn that did not run is
exit status 2. The turn may fetch only a host the list names exactly, not a
subdomain a spec picks.

The saved report is plain files under `<work-dir>/reviews/<request-id>/<run-id>/`
(`review.txt`, `review.json`, `request.xml`, `source.diff` and, with `--agent`,
`agent/` with the prompt, the answer and the files the turn read). `--cleanup`
keeps it. Two things are left out on purpose: the command does not read or
write a list of tracked requests, and `explain` does not show a review; use
`jq` on `review.json`.

### Maintenance requests (SLE and Leap code streams)

A package in a released code stream (`SUSE:SLE-15-SP7:Update`,
`openSUSE:Backports:SLE-15-SP7:Update`, ...) is changed through a maintenance
incident, not a submit request. `--maintenance` does what `osc mbranch` and
`osc maintenancerequest` do by hand, on top of the ordinary `update`/`lint` run:

```bash
# Where is the package maintained? (read only)
auto-package update --project openSUSE:Factory --only tor --maintenance-list

# Backport an upstream fix to every code stream, as a dry run first.
auto-package update --project openSUSE:Factory --only tor \
  --maintenance all \
  --backport https://gitlab.torproject.org/tpo/core/tor/-/commit/0123abc \
  --bug bsc#1234567 --bug CVE-2025-1234 --dry-run
```

- `--maintenance [CODESTREAM,...|all]` picks the code streams (the update
  projects `osc maintained` lists, also without `:Update`); without a value it
  asks. Each code stream is built from its own package in
  `home:<you>:branches:<update project>`, then the ones that built go into one
  `maintenance_incident` request to the maintenance project, after the plan is
  shown and confirmed (`--yes` to skip the question).
- `--maintenance-mode backport|update`: `backport` is the default, because it
  is what maintenance normally accepts: each code stream keeps its version and
  gets the fix as a patch. `update` updates each code stream to the newest
  release (or `--to-version`). `lint --maintenance` sends packaging fixes. The
  plan and the request description say which.
- `--backport SRC` (repeatable, needed in backport mode): a patch file, an https
  URL of a `.patch`/`.diff`, a GitHub/GitLab/Gitea commit URL or a GitHub pull
  request (the forge's `.patch` form is fetched), or a commit id together with
  `--backport-repo URL`. It is fetched once (https only, private addresses
  refused, 4 MiB cap, it must be a unified diff), stored in each branch as
  `CVE-2025-1234.patch` (or `fix-<commit>.patch` without a CVE) and the agent
  adds it with a `# PATCH-FIX-UPSTREAM <file> <refs> -- <reason>` comment and
  makes it apply to that code stream's source (rebased with quilt, never
  trimmed of failing hunks). The `.changes` bullet is written by auto-package:
  `- Add CVE-2025-1234.patch: <reason> (bsc#1234567, CVE-2025-1234)`.
- `--bug REF` (repeatable or comma separated): `bsc#`/`boo#`/`bnc#`, `jsc#` or
  `CVE-` references, added to the newest changelog entry and the description.
  Required; a backport refuses `--no-bug-ref`.
- `--affected-by COMMAND`: run as `COMMAND PACKAGE CODESTREAM REFS [BACKPORT...]`
  per code stream (no shell, 60 s timeout); a first stdout line
  `not-affected <reason>` leaves the code stream out with that reason,
  `affected`/`unknown` (or anything else, a failure, a timeout) keeps it.
  In code, the same seam is the `StreamCheck` trait of `ap_build::maintenance`
  (`run_with`).

Every code stream ends with a line in the summary: ready (with "applied
cleanly", "applied after a refresh for this code stream" or "updated to X"),
left out (an open request from the branch, or an accepted one of the same
source revision), not affected, or FAILED with the reason; a failed code stream
is never put into the request, and the run then exits non-zero.

Limits:

- code streams maintained in git (`[git]` lines of `osc maintained`, SLFO) are
  listed and skipped: not supported yet. A package maintained only in git is
  not seen at all;
- a maintenance run adds no build paths to the branch project: a package that
  needs one fails its code stream, because the incident could not build it;
  a dependency the pipeline builds itself in the branch does not travel with
  the request either;
- a backport is one fix of one package (`--only` names one); the package must
  have a source auto-package can rebuild (as for `update`);
- the bug references are checked for their form only, not looked up.

### Fixing a bug

`fix` takes one reported bug in one package of an ordinary project and builds
the fix in a branch project, at the version the package has. It is not for
maintenance projects (`:Update`, `SUSE:SLE-*`, `openSUSE:Leap:*`): those are
refused, with the `update --maintenance --backport` command that does it.

```bash
# What would be read, found and added? Reads the bug and the patch, changes nothing.
auto-package fix --project openSUSE:Factory --only tor --bug bsc#1234567 \
  --bug CVE-2025-1234 --dry-run

# Give the upstream fix, build it in home:<you>:branches:openSUSE:Factory,
# then ask whether to send a request.
auto-package fix --project openSUSE:Factory --only tor --bug bsc#1234567 \
  --commit-url https://gitlab.torproject.org/tpo/core/tor/-/commit/0123abc --submit
```

- The bug: `--bug REF` (`bsc#`/`boo#`/`bnc#`, `jsc#`, `CVE-`; repeatable or comma
  separated), `--cve CVE-YYYY-N`, `--description TEXT` (at most 4 KiB) or
  `--bug-file FILE` (a pasted report, at most 64 KiB, read instead of fetched).
  At least one is needed. Only the references you give are cited in the patch
  header, the spec comment and the `.changes` bullet.
- What is read, and which hosts you must allow first. Nothing is read from a
  host until you allow it, with `auto-package hosts allow HOST` (kept), `--allow-host
  HOST` (this run, logged as a `host_allowed` event) or the TUI; nothing the
  agent or a bug says can allow one. A bsc#, boo# or bnc# bug is read from
  `bugzilla.suse.com` (public bugs only, no API key: a private bug is reported
  as not read, and `--description` or `--bug-file` stands in). A CVE is read
  from `api.osv.dev` (it has the fixing commits) and `cveawg.mitre.org`, each
  only once allowed; `www.suse.com` is never read. A host that is not allowed
  is named in the run summary and the dry run, and the run goes on without it.
- The upstream fix. `--commit-url URL` (repeatable; a commit, pull request or
  `.patch`/`.diff` URL, a patch file, or a commit id with `--commit-repo`) is
  used as given. Without it the tool takes the one fix of the package's own
  repository (the `Url:` or `Source:` of the spec) that the bug text names, else
  the one GitHub's commit search finds for the references. Several candidates
  stop the run before anything is branched, listed, with exit 4: pass the ones
  to use with `--commit-url`. If an upstream release newer than the package
  has the fix, the dry run and the summary print the `update --to-version`
  command that would take it instead; the fix is made anyway.
- The patch. `<package>-CVE-2025-1234.patch` (the package's own prefix when its
  patches share another), with `References:` and `Upstream:` lines added after
  the subject of the upstream mail (or a header block for a plain diff), a
  `# PATCH-FIX-UPSTREAM <file> <refs> -- <reason>` comment above its `PatchN:`
  line and a `- Add <file>: <reason> (bsc#1234567, CVE-2025-1234)` bullet,
  written before the first commit so the fix is one commit and one build. A
  patch that applies cleanly or with an offset is added without a model; one
  with fuzz is refreshed with quilt first. Unless `--no-agent` is given, every
  fix patch goes through the patch review turn, and every edit through the same
  gates as any agent edit (rpmspec, patches apply, patch rules, removal proof).
- The agent. When the upstream patch does not apply, the spec has `%patch`
  lines, or no fix was found, up to three fix turns (Sonnet or better) rebase
  it with quilt, look for the fix in the upstream repository, or write the
  smallest fix themselves. Every site a turn may fetch from passes the allowed-hosts check, and the
  turn cannot add a host. With an empty allowed-hosts file it reaches `pypi.org`,
  `github.com` (with `raw.githubusercontent.com` and `api.github.com`),
  `salsa.debian.org` and `lore.kernel.org` (built-in trusted hosts), and a fixed
  forge (GitLab, Codeberg, Bitbucket, sourcehut) that is the package's upstream
  or a `--commit-url` host. It needs an entry (`auto-package hosts allow`,
  `--allow-host`) for `src.fedoraproject.org`, `gitweb.gentoo.org`, a
  self-hosted forge and any other `--commit-url` host. It has no web search.
  A fix with no upstream commit gets `Upstream: not yet reported` and
  `# PATCH-FIX-OPENSUSE`. Every patch the agent wrote is flagged, whatever its
  header says (the tool does not verify a URL the agent cites); so is an upstream
  patch changed to apply, and one that is a pull or merge request, or a commit
  that the user did not give and no release contains (a bug text can name a
  request of anyone). Any risk flag makes `--yes` refuse the request: a person
  reads the patch.
- `--submit` (with a confirmation, or `--yes`) sends a submit request to the
  origin (the devel project with `--follow-devel`) when the build succeeded and
  the branch holds and applies the patch. Without it nothing is submitted. There
  is no in-place fix. `--no-agent` only adds a given patch that applies.

`fix` exits `0` when the fix built (and the request, if asked, was made or
refused by the usual gate), `3` when the bug needs no fix (quilt proves the
patch is applied already, or the agent quotes the code that shows it is fixed or
not affected) or none was found, and `4` when a maintainer must decide: an ABI
or behaviour change, a fix that does not apply, several upstream candidates, or
`--no-agent` with the agent needed. Edits made stay in the branch; nothing is
submitted. `1` is any other failure, `2` the agent is unavailable, `130`
interrupted.

Limits:

- a maintenance backport (`update --maintenance --backport`) does not get the
  `References:`/`Upstream:` header or the package-prefixed name `fix` gives, and
  a staged backport that the agent leaves unchanged is not judged by the patch
  rules;
- the commit search and the fixed-release check use GitHub only; GitLab and
  Gitea repositories need `--commit-url` or the agent;
- a private or embargoed bug is not read (no API key); `jsc#` is not fetched;
- the lookup finds one commit: a fix in several commits is given by repeating
  `--commit-url`, each of which becomes its own patch;

### Exit codes

- `0`: success.
- `1`: the run failed (a package failed to build, bad arguments, ...).
- `2`: the agent is unavailable: the `claude` CLI is not logged in. The whole
  run stops at once (running children are killed, queued packages are not
  started, in-flight packages are recorded as interrupted) instead of retrying
  every package. Run `claude` and `/login`, then re-run. `build` and
  `update` check `claude auth status` (free) before starting.
- `130`: interrupted (Ctrl-C or SIGTERM).

`fix` also exits `3` (the bug needs no fix here, or none was found) and `4`
(a maintainer must decide); see "Fixing a bug". No other command uses them.

`review` has its own meaning for the first three: `0` no finding at or above
`--fail-on`, `1` at least one, `2` the review could not be completed: the request does
not exist or cannot be read, it has no submit action, a read the checks need
failed (the request, the listings, the spec, `.changes` or patch texts, the
build results, a signed archive's download), or `--agent` is given and
`claude` is logged out or the day's spend cap is reached, or `--fail-on-agent`
is given and the agent turn did not run. No report is written
for `2`.

### Licence guards

A package that cannot build is the worst outcome, so a guard is answered, not
obeyed blindly. A guard that asks the builder to *confirm a fact*
(`I_CONFIRM_THIS_IS_NOT_A_LICENSE_VIOLATION=1`, as torchcodec's wheel-state
check does about the FFmpeg it links) is satisfied after the agent checks the
fact, and the spec records the check in a `# licence-guard-verified: <what was
checked>` comment. Without that comment the package fails with the line named.
A variable that *accepts terms* (`ACCEPT_EULA=Y`) is a person's decision and is
never set by the tool; after reviewing the terms, run again with
`AUTO_PACKAGE_ALLOW_LICENSE_OVERRIDES=1`.

## Architecture

The project is organized as a Rust workspace:

- **core**: Event system and common data structures
- **ecosystem**: PyPI and package metadata handling  
- **build**: OBS integration, spec generation, and AI agent
- **cli**: Command-line interface and TUI
- **packaging-skills**: a Claude Code plugin (loaded via `--plugin-dir`) holding
  the AI agent's conditional packaging conventions — maturin, standalone
  Rust/Cargo, plain C/C++, monorepos, prebuilt wheels, systemd services,
  i18n, `-doc` subpackages, Django test settings. These only apply to a
  minority of packages, so they're loaded on demand via the `Skill` tool
  instead of being sent in every turn's prompt. The conventions are
  `build/conventions/N.md` (the table `build/src/conventions_table.rs`, the
  `INDEX.md` with one line per topic). Seven are inline in every turn's
  system prompt, with a summary of 62; the others are written to
  `conventions/` in the workspace and read on demand. A convention
  points at the relevant skill by name where one exists.

### Event System

All build activity flows through an event-driven architecture:

```rust
trait Reporter {
    fn report(&self, event: &Event);
}
```

Events are distributed to multiple sinks:
- `HumanSink`: Formatted text output (when TUI is disabled)
- `JsonlSink`: Structured JSONL logging for analysis
- `MemorySink`: In-memory storage for runtime queries

When `--tui` is enabled, `TuiController` additionally taps the reporter used
by each package's build thread — per-step events (`StepStarted`, `Command`,
`AgentToolUse`, ...) carry a `StepId` that only disambiguates steps *within*
one package, so the tap is what attributes them to the right row in the UI.

## TUI Layout

The TUI displays:

1. **Summary header**: Packages done/total, elapsed time, total cost
2. **Package table**: One row per package — status, version, current step, time, cost
3. **Detail pane**: Event log and agent activity for the selected package
4. **Footer**: Latest run-level message and keybindings

## Development

Build the project:

```bash
cargo build
```

Run tests:

```bash
cargo test
```

Run with debug logging:

```bash
cargo run -- build <package> --project <project> --log debug
```

## Threat model

Upstream content is untrusted: PyPI sdists and wheels, source repositories,
build logs, and anything the agent or a spec macro produces from them. The
tool assumes these may be hostile and limits what they can reach:

- **Bug reports and fixes (`fix`)**: the text of a bug, a CVE record, a commit
  message and a fetched patch are written by anyone who can file a bug or
  open a pull request. The report reaches the agent only as fenced untrusted
  data, cut to a size, and a URL in it is fetched by the tool only when it is a
  patch of the package's own upstream repository on an allowed host. The
  bugzilla and CVE hosts are off until the user allows them, and the agent
  cannot edit the allowed-hosts file. The fix turn reads the upstream forge and
  the distributions' patch hosts the user allowed, read only, with no web
  search; its edits pass the gates of every turn's edit, its patch is reviewed,
  and a fix that no upstream commit stands behind, or an upstream fix that was
  changed, is flagged so `--yes` never submits it.

- **Archives**: links are refused if they can reach outside the unpacked tree
  (including through a chain of earlier links), zip symlinks are never
  created, and archives are bounded in total size, per-file size and entry
  count. An archive that will not unpack is reported, not skipped silently.
- **Spec macros**: `rpmspec` expansion runs in a bubblewrap sandbox with no
  network and no home directory. Without a sandbox, specs with code-running
  macros (`%(...)`, `%{lua:...}`) are refused.
- **Dependency resolution**: resolving a package can build an sdist, which
  runs its build backend and `setup.py` code from the index (and installs its
  `build-system.requires`). pip runs in a bubblewrap sandbox for that reason.
  - *What the sandbox does*: the system (`/usr`, `/lib*`, `/bin`, `/sbin`,
    and a short list of `/etc` files for DNS, TLS, users and compilers) is
    read-only. Your home directory, `~/.ssh`, `~/.config`, XDG directories,
    `/run/user` (D-Bus, ssh-agent and gpg-agent sockets), `/var` and `/opt` do
    not exist inside it. The only writable places are a private `/tmp`,
    `/run` and an empty home (`/home/sandbox`) in memory, gone when pip exits;
    pip's cache is there too, so nothing survives to poison a later run. It
    has its own PID, IPC, UTS and user namespaces and a new session, and it is
    killed with auto-package. The environment is cleared down to what pip
    needs (PATH, locale, proxy, CA bundle and index settings), so OBS, GitHub
    and Anthropic tokens are not visible to it.
  - *What it does not do*: the network stays open, because resolution needs
    the index. Anything a build backend can read inside the sandbox, it can
    send out: the allowlisted variables (an index URL with a password in
    `PIP_INDEX_URL` included), the pip config below, the interpreter prefix.
    Memory-backed scratch space is not size-limited, so a backend can use up
    RAM.
  - *What is shared on purpose*, each read-only and as a single path: the
    Python install when it is outside `/usr` (a venv or `/opt` prefix; a pip
    under `~/.local` or another credential directory is refused rather than
    bound whole); pip's own config for private indexes (`PIP_CONFIG_FILE`,
    `~/.config/pip/pip.conf`, `~/.pip/pip.conf`), whose index URLs may carry
    credentials that a backend could read; and custom CA bundles named by
    `SSL_CERT_FILE`, `SSL_CERT_DIR`, `REQUESTS_CA_BUNDLE` or `PIP_CERT`.
    `~/.netrc` is **not** shared unless `AUTO_PACKAGE_PIP_NETRC=1`, since it
    usually holds credentials for more than the index. Keyring-based pip
    authentication is not available inside the sandbox.
  - *Without a working sandbox* (no `bwrap`, or unprivileged user namespaces
    disabled; probed once per run), pip is not run on the host. Resolution
    falls back to wheels only (`--only-binary=:all:`) with a warning, and
    releases that ship only an sdist fail to resolve. `--unsafe-resolve` (or
    `AUTO_PACKAGE_UNSAFE_RESOLVE=1`) opts into running pip without the sandbox
    and building sdists; it also turns the sandbox off where it would work,
    for example when a private index setup does not fit it. In that mode the
    cleared environment is the only protection, and files in your home
    directory are readable by build backends.
- **Agent**: the build turn can read only the workspace and checkout, run
  `osc status`/`osc vc`, `ls`, `quilt` and the read-only
  `zypper search`/`what-provides` queries, and fetch PyPI metadata. It has no
  shell `rm` or general web access (the dependency-name lookup turn may search
  the web, but can touch no local file). It cannot write the osc and git
  metadata of its checkout (`.osc`, `.git`) with Edit, Write or NotebookEdit,
  nor name it in a quilt command spelled out in full. The metadata is copied
  before each turn and compared after it: a change (a glob the shell expands,
  a patch that creates a file) is undone and the turn rejected. A symbolic
  link the turn makes that leads into that metadata, into the checkout root,
  or out of the workspace's `source/` and the checkout, is removed the same way. Credential variables are removed from
  its environment, except the Anthropic credentials the CLI itself needs. Those
  remain visible to its commands. The environment is an allowlist
  (PATH, locale, proxy and CA settings, `ANTHROPIC_*`, `CLAUDE_*`), so
  `AWS_*`, `GOOGLE_*` and other Bedrock/Vertex variables are dropped. To use
  those backends, name the variables to hand over in
  `AUTO_PACKAGE_AGENT_ENV_PASS`, e.g.
  `AUTO_PACKAGE_AGENT_ENV_PASS=AWS_PROFILE,AWS_REGION,CLAUDE_CODE_USE_BEDROCK`.
  Named variables skip every credential check (the agent's commands can then
  read them), except `OSC*` ones, which are never passed. Build logs, notes, and names taken
  from a spec are fenced as data in prompts.
- **Outbound HTTP**: downloads use https only, redirects included, and refuse
  loopback, private, link-local, carrier-grade NAT, reserved and multicast
  addresses (including IPv4 ones embedded in IPv4-mapped, NAT64 and 6to4
  IPv6 addresses), checked on the address actually connected to. Behind a
  proxy (`HTTPS_PROXY`) the proxy resolves the target, so only IP literals in
  the URL can be refused; a hostname that resolves to a private address is not
  caught.
- **Allowed hosts**: the hosts services and source fetches may contact are the
  built-in list, `AUTO_PACKAGE_SOURCE_HOSTS`, the user's `allowed-hosts` file
  and `--allow-host` / the TUI for a run. Only the user adds to them, through a
  command they type, the file, or a confirmed TUI key. The agent's tools may
  not read or write the file, and a host is checked by the same rules whatever
  its source.
- **Upstream sources**: a repository found by search is not built without
  confirmation. Only a curated mapping, the repository PyPI records for the
  project, the upstream another distribution's spec records, or an NVIDIA-org
  repository is used automatically. A source the agent finds on the web must
  also be on an allowed host and on a site the project itself records. Confirm
  another with `--source-url`; the archive's sha256 is recorded in the spec.
- **OBS commits**: only the spec, archives, service files, changelog, rpmlint
  config and referenced patches are committed. Other files in the checkout
  are reported and left out.
- **Local files**: lock files are opened without following symlinks. Scratch
  files and the working directory of `gh` and `osc` live in a private,
  randomly named, mode-0700 directory rather than a predictable shared path.
  A child's process group is signalled before the child is reaped, so a reused
  process id is never hit.
- **Terminal output**: control sequences in printed diagnostics are removed.
- **Not covered**: the machine running auto-package, the OBS server, and the
  packages it builds. Credentials are not isolated from the agent's own
  commands.

## License

MIT
