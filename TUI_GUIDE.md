# Interactive TUI Guide

## Overview

The Terminal User Interface (TUI) provides a real-time, visual monitoring experience for package builds. It replaces the standard text output with an organized, color-coded interface that makes it easy to track build progress, agent activity, and resource usage.

## Enabling the TUI

Add the `--tui` flag to any build command:

```bash
auto-package build <package> --project <project> --tui
```

## TUI Layout

The TUI is divided into 5 sections:

```
┌─────────────────────────────────────────────────────────────┐
│ Building: markupsafe 2.1.5                                  │ ← Header
└─────────────────────────────────────────────────────────────┘
┌─────────────────────────────────────────────────────────────┐
│ Current Step                                                │
│ Step 3: Build                                               │ ← Current Step
└─────────────────────────────────────────────────────────────┘
┌─────────────────────────────────────────────────────────────┐
│ Event Log                                                   │
│ Building markupsafe 2.1.5                                   │
│ Step 1: Checkout                                            │
│ Step 1 ok: Checkout (0.234s)                                │
│ Step 2: GenerateSpec                                        │
│ Step 2 ok: GenerateSpec (1.456s)                            │ ← Scrolling Event Log
│ Step 3: Build                                               │
│ $ osc build openSUSE_Tumbleweed x86_64                      │
│ Build failed: CheckFailed                                   │
│ Agent: Fixing spec (attempt 1/10)                           │
└─────────────────────────────────────────────────────────────┘
┌─────────────────────────────────────────────────────────────┐
│ Agent Activity                                              │
│ Fixing spec (attempt 1/10)                                  │
│ → Read source/pyproject.toml                                │
│ → Grep 'console_scripts' in source/setup.cfg                │
│ → Edit /path/to/spec                                        │ ← Agent Tool Calls
│ Edited (confidence: high, $0.0234)                          │
│ → Bash rpmspec --parse /path/to/spec                        │
└─────────────────────────────────────────────────────────────┘
┌─────────────────────────────────────────────────────────────┐
│ Stats                                                       │
│ Tokens: 12,456 in / 3,289 out │ Cost: $0.1234 │ Elapsed: 45s│ ← Stats Footer
└─────────────────────────────────────────────────────────────┘
```

## Color Coding

The TUI uses colors to make information easier to parse at a glance:

### Event Log
- **White**: Informational messages
- **Yellow**: Warnings and skipped steps
- **Red**: Errors and failed steps
- **Green**: Successful completions

### Headers and Labels
- **Cyan**: Section headers and package names
- **Yellow**: Current step indicator
- **Magenta**: Agent activity

### Status Indicators
- **Green**: Successful outcomes, high confidence
- **Yellow**: Medium confidence, warnings
- **Red**: Failures, low confidence, errors

## Interactive Controls

- **`q` or `Esc`**: Exit the TUI gracefully
- **`Ctrl+C`**: Force quit immediately

The TUI automatically exits 2 seconds after the build completes, giving you time to review the final state.

## What You'll See

### During Build

1. **Package Info**: The header shows which package and version is currently building
2. **Progress**: The current step section shows what's happening right now
3. **Event Stream**: A scrolling log of all build events in chronological order
4. **Agent Work**: When the AI agent is active, you'll see:
   - Which tools it's using (Read, Edit, Grep, Bash)
   - Abbreviated inputs to each tool
   - The action it took (Edited, NoChange, Blocked)
   - Confidence level and cost per turn
5. **Resource Usage**: Real-time token consumption and cost tracking

### After Build

The TUI will show:
- Final outcome (Success/Failed)
- Total time elapsed
- Total cost
- Complete event history

## Example Session

```bash
$ auto-package build markupsafe --project home:user:python --tui

# TUI launches and shows:
# 1. Package checkout
# 2. Spec generation
# 3. First build attempt → fails in %check
# 4. Agent review:
#    - Reads pyproject.toml
#    - Finds missing test dependencies
#    - Edits spec to add BuildRequires
# 5. Second build attempt → succeeds
# 6. TUI exits showing success, $0.05 spent
```

## When to Use TUI vs Text Output

### Use TUI (`--tui`) when:
- Building interactively and watching progress
- Debugging build failures in real-time
- You want immediate visual feedback
- Working on a single package build

### Use Text Output (default) when:
- Running in CI/CD pipelines
- Building multiple packages in background
- Logging output to files
- Using `--log debug` or `--log trace` for detailed logs
- Running via cron or automated scripts

## Technical Details

### Event Forwarding

The TUI doesn't replace logging—it runs alongside it:
- All events go to the TUI display
- All events are still written to `run.jsonl` for analysis
- Events are still available in-memory for interruption handling

### Thread Safety

The TUI runs in a separate rendering thread and receives events via a channel. This ensures:
- Zero impact on build performance
- Clean shutdown on Ctrl+C
- No blocking on slow terminal output

### Terminal Requirements

The TUI requires:
- A terminal that supports ANSI escape codes
- At least 80x24 character display (larger is better)
- Crossterm-compatible terminal (most modern terminals)

### Fallback Behavior

If the TUI fails to initialize (e.g., not a TTY, unsupported terminal), the build automatically falls back to standard text output with an error message.
