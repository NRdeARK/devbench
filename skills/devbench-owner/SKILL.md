---
name: devbench-owner
description: >-
  Conventions for persistent Owner and domain-worker sessions running inside a devbench 1:1 Herdr
  workspace: scope discipline, spawning domain workers inside the same workspace, reporting back to
  the parent via hc send, and leaving archive to the Dispatcher.
---

# devbench — Owner & Domain-Worker Conventions

When running as a persistent **Owner** or **domain worker** inside a `devbench` work:

1. **Stay inside your central work folder:** Only edit files inside your work's git worktrees (`<work_root>/<slug>/<repo>`). Never modify main checkouts in source repositories.
2. **Domain workers share your 1:1 Herdr workspace:** If an Owner needs a long-lived specialist (e.g., `backend`, `frontend`, `docs`), spawn it into the same work via:
   ```sh
   devbench spawn <slug> --role worker --domain <domain> --cwd-repo <alias> --task "<exact scope>"
   ```
   Keep ephemeral one-shot sub-agents in your own delegation layer — do not register them in `devbench`.
3. **Report back explicitly:** When done or blocked, send a concise summary (and file paths for long outputs) back to your caller with `hc send <caller_terminal_id> "<summary>"`.
4. **Do not self-archive blindly:** Archiving closes this work's 1:1 Herdr workspace first to release Windows file locks. Flush notes, ensure worktrees are clean, and ask the Dispatcher (outside this workspace) to run `devbench archive <slug>`.
