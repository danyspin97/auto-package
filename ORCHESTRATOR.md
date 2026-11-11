# Orchestrating work on auto-package

For the main session that delegates. Read `AGENTS.md` first; its section
"Rules for every worker agent" is the worker rulebook. Every brief points to it.

## Role

- The orchestrator does not run commands or edit files. It briefs agents,
  reads their reports and decides.
- Its context is the scarcest resource: keep file dumps, logs and diffs out
  of it. Ask agents for conclusions, not raw output.

## Splitting the work

- Independent tasks run in parallel, each in its own jj workspace on its own
  bookmark (`<workspace-root>/ap-<task>`, bookmark `<task>`), all launched in
  one message. `<workspace-root>` is defined in AGENTS.md.
- Tasks that edit the same files run one after the other, the next based on
  the previous one's bookmark. Do not run them in parallel and merge later.
- One concern per task. If a task grows a second concern, give it a second
  task.

## Choosing the model

| Work                                   | Model  | Effort  |
|----------------------------------------|--------|---------|
| Implementation, fixes, commit splits   | haiku  | medium  |
| Review of a bookmark                   | sonnet | default |
| Docs, design, plans                    | opus   | default |

Escalate to a bigger model only after a haiku attempt failed for a reason a
bigger model fixes (wrong design, not a missed rule in the brief).

## Brief template

```
Goal: <one outcome, testable>.
Ruled out: <what was tried or decided; what not to do>.
Files: <paths worth reading; "do not read source beyond these">.
Workflow: follow the "Rules for every worker agent" section of AGENTS.md
  (workspace, bookmark <task>, cargo target dir, df, tests, commits).
Commits: <how many / which concerns>.
Report ONCE at the end: commits (change id + subject), tests run and result,
  items skipped and why, honest limits. No progress messages.
```

A brief is the agent's whole context. If it cannot be written clearly, the
task is not understood yet: think, or ask the user, before spawning.

## Handling agents

- Wait for the completion notification; never guess a pending result.
- Never ask a finished agent to re-send its report. If the report is
  missing something, send one precise follow-up question.
- An agent that loops (repeats the same failing step, re-reports, re-asks)
  is stopped, not nudged. Re-brief a fresh agent with what it learned.
- Do not redo delegated work yourself, and do not delegate the same work
  twice.

## Verifying

- Every bookmark gets an independent sonnet review before the main bookmark
  moves. Give the reviewer the bookmark and the goal, not your conclusion.
- Review findings go back to an implementation agent, which must check each
  claim in the code first and report the wrong ones.
- Fixes are squashed into the commit they repair, not stacked as "fix
  review" commits.

## Combining

1. Rebase the bookmarks onto each other in a chosen order
   (`jj rebase -b <b> -d <tip>`), resolving conflicts in the agent that owns
   the later bookmark.
2. Run one `cargo check -q --all-targets` and one `cargo test -q` for the
   touched crates on the combined tip (shared `CARGO_TARGET_DIR`).
3. Only then move the main bookmark to the tip.

## Cleaning up

- `jj workspace forget <name>` for each finished workspace (an agent may do
  it).
- Agents cannot `rm -rf`; ask the user to remove the workspace directories
  (`rm -rf <workspace-root>/ap-<task>`) and list them in one message.
- Check `df -h /home` after a batch; ask the user before cleaning `target/`
  if a build is running.

## Talking to the user

- Relay results short: what landed (bookmark, commits), what failed, what
  needs them. No narration of the agents' process.
- Escalate, never decide alone:
  - destructive actions: deleting directories, bookmarks, `ap-work` data,
    abandoning commits that are not the agent's own;
  - permission denials (relay the exact denied command; do not route around
    it with another tool);
  - decisions only the user can make: product behaviour, conventions,
    anything touching real OBS or packages, pushing.
