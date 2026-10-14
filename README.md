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
- `--distro`: Target distribution (default: `openSUSE_Tumbleweed`)
- `--arch`: Target architecture (default: `x86_64`)
- `--tui`: Enable interactive TUI mode
- `--log`: Log level when not using TUI: error, warn, info, debug, trace (default: `info`)
- `--max-attempts`: Maximum AI fix attempts per package (default: 10)
- `--budget-usd`: USD budget cap for AI calls (default: 2.0)
- `--no-agent`: Build only, never call the AI model
- `--unsafe-resolve`: Run pip without its sandbox (see the threat model)
- `--force`: Rebuild even if already built
- `--regen`: Regenerate spec from scratch
- `--cleanup`: Remove work directory before building
- `--jobs`: Maximum concurrent builds (0 = unlimited)

### Updating a whole project

`update-project` browses a *source* OBS project, finds the packages that have
a newer upstream release (the same sweep as `update --check`), branches them
into a *branch* project, builds the new versions there with the ordinary
pipeline, and optionally submits them back. The source project is only read.

```bash
# See what would happen: plan, branch project, build paths, requests.
auto-package update-project Base:System --dry-run --submit

# Update the packages that have a release and build them in
# home:<you>:branches:Base:System.
auto-package update-project Base:System

# Only some packages, then submit requests for those that built.
auto-package update-project Base:System --only libfoo,libbar --submit
```

Options (besides the build-related `--work-dir`, `--notes`, `--distro`,
`--arch`, `--log`, `--tui`, `--max-attempts`, `--budget-usd`, `--no-agent`,
`--jobs`, `--regen` that `update` takes):

- `--branch-project`: where to branch and build (default
  `home:<osc user>:branches:<SOURCE>`, what `osc branch` uses). Created by the
  first branch if it does not exist. It must not be the source.
- `--only`, `--exclude`: comma-separated package names.
- `--dry-run`: print the plan, the branch project, the build paths that would
  be added and the requests that would be created, and change nothing.
- `--update-deps`: also branch and update dependencies that live in another
  project and have a newer release themselves (see below).
- `--submit`: create submit requests (see below). `--yes` skips the question.

Packages are branched with the OBS branch API, without following a devel
project, so the request goes back to the project named. A package that is
already in the branch project is reused. Maintenance projects and locked
projects are refused with an explanation. A PyPI-sourced package whose OBS name
is not `python-<name>` is skipped, since the pipeline would build a second
package next to it.

The run ends with a summary (updated, failed, skipped, requests, build paths
added, branch project) and exits non-zero when a package failed.

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
  instead of being sent in every turn's prompt; `build/src/agent.rs`'s
  `CONVENTIONS` constant holds the always-applicable rules and briefly
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
  `rpmspec`, `osc status`/`osc vc`, `ls`, `quilt` and the read-only
  `zypper search`/`what-provides` queries, and fetch PyPI metadata. It has no
  shell `rm` or general web access (the dependency-name lookup turn may search
  the web, but can touch no local file). Credential variables are removed from
  its environment, except the Anthropic credentials the CLI itself needs.
  Those remain visible to its commands. Build logs, notes, and names taken
  from a spec are fenced as data in prompts.
- **Outbound HTTP**: downloads use https only, redirects included, and refuse
  loopback, private, link-local, carrier-grade NAT, reserved and multicast
  addresses (including IPv4 ones embedded in IPv4-mapped, NAT64 and 6to4
  IPv6 addresses), checked on the address actually connected to. Behind a
  proxy (`HTTPS_PROXY`) the proxy resolves the target, so only IP literals in
  the URL can be refused; a hostname that resolves to a private address is not
  caught.
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
