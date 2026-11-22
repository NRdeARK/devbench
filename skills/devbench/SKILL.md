---
name: devbench
description: >-
  Orchestrate multi-repo works and persistent agent sessions on Herdr (herdr-core + herdr-app)
  using the devbench CLI: Dispatcher -> Owner -> (domain) worker hierarchy, 1 work : 1 Herdr
  workspace, central git worktree folders, native AI session ID tracking, live status
  reconciliation, resume, and safe one-step archiving. Use when the user asks to start a new work,
  spawn an Owner or domain worker, check work status, resume a session, or archive a finished work.
---

# devbench — Dispatcher Guide

`devbench` is a native Rust CLI that manages **persistent** work sessions in a 4-tier hierarchy:

```text
Dispatcher (you) -> Owner (1 per work, primary pane) -> domain worker(s) (tabs/splits in same space)
                                                      \-> ephemeral sub-agents (decoupled; not managed by devbench)
```

Each work maps **1-to-1** to:
- One central work folder: `<work_root>/<slug>/` (holding git worktrees for each target repo + notes)
- One dedicated Herdr workspace (`herdr.space=<space_id>`)
- One central runtime state file: `~/.devbench/state/works/<slug>.json`

## Config & State Locations

- **User config (external-first):** `$DEVBENCH_CONFIG` -> `~/.devbench/config.json` (optional gitignored repo-local fallback in dev mode). Never edit files inside the plugin cache directory.
- **Central work state:** `~/.devbench/state/works/<slug>.json` (or `$DEVBENCH_STATE_DIR/works/<slug>.json`). Always reconciled live against `herdr-core` (`hc`) and `herdr-app`.

## Core Commands

```sh
# 1. Initialize or validate user config from templates/config.example.json
devbench init
devbench config check

# 2. Setup a new multi-repo workspace (preflights all repos before creating anything)
devbench new <slug> --repos api,web --title "Implement auth token refresh"

# 3. Launch Owner (primary pane) or domain worker (tab/split in same 1:1 space)
devbench spawn <slug> --role owner --agent claude --cwd-repo api --task "Own auth refresh across api and web..."
devbench spawn <slug> --role worker --domain web --agent codex --cwd-repo web --task "Update login client..."

# 4. Inspect all works (reconciled against live Herdr) or bind/resume a session
devbench status
devbench status <slug>
devbench bind <slug> --key owner --session-id <native-id>
devbench resume <slug> --key owner [--session-id <native-id>]

# 5. Archive a completed work (only after user confirms)
devbench archive <slug> [--snapshot-wip]
```

## Safety Rules

1. **Herdr lifetime (`--outlive-parent`):** `devbench spawn` passes `--outlive-parent` so active Owner/worker sessions survive when you close or restart the Dispatcher tab. They are **not** zombies: closing their pane/space in Herdr or running `devbench archive` kills and cleans them up.
2. **Never blind-resend on trust prompts:** If `devbench spawn` reports `status: blocked` (`first_message.sent = false`), run `hc read <terminal>` and ask the user to resolve the prompt. Never pass `--force`.
3. **Archive confirmation:** Always get user confirmation before running `devbench archive`. Dirty/untracked worktrees block archive by default; offer `--snapshot-wip` instead of force-deleting.
4. **Native AI session IDs are `cwd`-scoped and machine-local:** `devbench resume` automatically launches from the session's recorded `cwd` and rechecks the saved ID (or uses `--session-id` if passed).
