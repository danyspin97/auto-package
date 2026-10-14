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
