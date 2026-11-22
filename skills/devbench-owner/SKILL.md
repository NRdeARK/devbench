---
name: devbench-owner
description: >-
  Conventions for persistent Owner and worker sessions running inside a devbench 1:1 Herdr
  workspace: scope discipline, spawning parallel track or domain workers inside the same
  initiative workspace, reporting back to the parent via hc send, and leaving archive to the Dispatcher.
---

# devbench — Owner & Worker Conventions

When running as a persistent **Owner** or **worker** inside a `devbench` initiative workspace:

1. **Stay inside your assigned worktree:** Only edit files inside your work's git worktrees (`<work_root>/<slug>/<repo>` or `<work_root>/<slug>/<repo>--<track>`). Never modify main checkouts in source repositories.
2. **Keep related work in the same 1:1 Herdr workspace:** 1 work = 1 initiative/milestone. Never run `devbench new` for sub-tasks or phases of your own initiative.
   - **Sequential phase / review on the same branch:**
     ```sh
     devbench spawn <slug> --role worker --domain <review|phase2> --cwd-repo <alias> --task "<exact scope>"
     ```
   - **Parallel editing track touching the same repo (isolated branch & worktree `<repo>--<track>`):**
     ```sh
     devbench spawn <slug> --role worker --track <track> --cwd-repo <alias> --task "<exact scope>"
     ```
   - Keep ephemeral one-shot sub-agents in your own delegation layer — do not register them in `devbench`.
3. **Report back explicitly:** When done or blocked, send a concise summary (and file paths for long outputs) back to your caller with `hc send <caller_terminal_id> "<summary>"`.
4. **Do not self-archive blindly:** Archiving closes this initiative's 1:1 Herdr workspace first to release Windows file locks. Flush notes, ensure all worktrees are clean, and ask the Dispatcher (outside this workspace) to run `devbench archive <slug>`.
5. **Own the how:** The brief says *what*, *why*, the rules and what counts as done. You decide the approach, the code and the tests. If the brief leaves a real choice open, make it and record it in your report; ask the caller only when a wrong guess would be costly or can't be undone.

## Working a task

1. **Plan:** read the brief and every input file it names. List the changes and the evidence you will show for each one.
2. **Reproduce first:** for a bug or review finding, write a test (or repro) that fails *before* you fix it.
3. **Implement:** one small commit per logical change. Never amend or rewrite commits. Don't push, publish or delete anything unless the brief says so.
4. **Verify:** run every acceptance check in the brief and capture the real output. Don't claim a check passed if you didn't run it.
5. **Report:** write the report file the brief names (changes, evidence per acceptance item, decisions, open questions, commit list), then `hc send` the caller a one-line summary with the path.

## Handling review findings

When the brief points you at a review file:
- **Triage every finding:** *fix*, or *push back* with a reason (wrong, out of scope, or not worth the cost). Never silently skip one.
- Fix blockers first. Each fix needs a test that proves the behaviour, not just that the code runs.
- In the report, write one line per finding (ID, what you did, evidence such as the test name or command output) so the re-reviewer can check each one.
