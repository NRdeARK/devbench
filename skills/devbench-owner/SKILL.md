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
