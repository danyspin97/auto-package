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

Launch the build with a real-time terminal UI that shows:
- Current package and build step
- Live event stream
- Agent activity and reasoning
- Token usage and costs
- Build progress

```bash
auto-package build <package-name> --project <obs-project> --tui
```

### TUI Controls

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

### Event System

All build activity flows through an event-driven architecture:

```rust
trait Reporter {
    fn report(&self, event: &Event);
}
```

Events are distributed to multiple sinks:
- `HumanSink`: Formatted text output (when TUI is disabled)
- `TuiSink`: Interactive terminal UI (when `--tui` is enabled)
- `JsonlSink`: Structured JSONL logging for analysis
- `MemorySink`: In-memory storage for runtime queries

## TUI Layout

The TUI displays four main sections:

1. **Header**: Current package and version being built
2. **Current Step**: Active build step with progress
3. **Event Log**: Scrolling log of build events with color coding
4. **Agent Activity**: AI model tool calls and reasoning
5. **Stats Footer**: Token usage, cost, and elapsed time

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

## License

MIT
