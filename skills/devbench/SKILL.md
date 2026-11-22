---
name: devbench
description: >-
  Activate Dispatcher mode for multi-repo initiative orchestration on Herdr (herdr-core + herdr-app)
  using the devbench CLI. Use whenever the user says "be the dispatcher", "act as dispatcher",
  "start dispatcher mode", "/devbench", or asks to inspect all works/states, start a new work,
  spawn an Owner or worker/track, resume a session, or archive a finished work.
---

# devbench — Dispatcher Mode Playbook

When this skill is activated, **you are the Dispatcher (Tier 1)** overseeing all active **works (initiatives / milestones)** across repositories through the `devbench` CLI.

```text
Dispatcher (you, Tier 1)
  └─> Owner (Tier 2, 1 per work, in dedicated 1:1 Herdr workspace)
        └─> worker(s) (Tier 3, optional: domain specialists, parallel branch tracks, or sequential phases/reviewers in the same space)
              └─> ephemeral sub-agents (Tier 4, decoupled — NOT managed by devbench)
```

---

## Step 0 — Startup & State Gathering (Always run first when entering Dispatcher mode)

Before acting or answering about active work, gather live truth in two commands:

1. **Verify config:**
   ```sh
   devbench config check
   ```
   - Config is external-first: `$DEVBENCH_CONFIG` -> `~/.devbench/config.json` (never edit files inside a plugin cache).
   - If config does not exist yet, run `devbench init` and help the user fill in `work_root`, `archive_root`, and `repos` aliases before proceeding.

2. **Gather & reconcile all work states:**
   ```sh
   devbench status
   ```
   - Reads the central index at `~/.devbench/state/works/<slug>.json` and reconciles every non-archived session against live `herdr-core` (`hc`).
   - Summarize the current bench in a concise table for the user:
     - Active works (slug, title, repos/tracks & branches, Owner & worker statuses: `running`, `idle`, `blocked`, `exited`, `lost`)
     - Any `blocked` sessions needing human attention (run `hc read --lines 30 <terminal>` to show the prompt — never `--force`!)
     - Recently archived works if relevant.
   - Then ask what the user wants to start, inspect, resume, or archive.

3. **Label your own Dispatcher space (once per Herdr session):**
   Run `herdr-app ls`, locate the space & tab holding `$HC_TERMINAL_ID`, and rename both using JSON payloads:
   ```sh
   herdr-app do space.rename '{"id":"<space_id>","name":"dispatcher"}'
   herdr-app do tab.rename '{"id":"<tab_id>","name":"dispatcher"}'
   ```

---

## Sizing Rule: What is 1 `work`? (Initiative / Milestone Model)

**1 Work = 1 Initiative / Milestone / Focus Area = 1 Herdr Workspace** (e.g. `ga-v02`, `auth-overhaul`, `checkout-q4`). It holds one or more tracks/branches inside `<work_root>/<slug>/` and archives together when the initiative finishes.

Always check `devbench status` first and use this exact decision matrix:

| Situation | Action | Command |
| :--- | :--- | :--- |
| **Unrelated initiative / milestone / standalone hotfix** that archives on its own timeline | Create a **new work** (new Herdr space) | `devbench new <slug> --repos <r1,r2> --title "<initiative>"` |
| **Follow-up, fix-up, or review on an existing branch/track** inside an active initiative | **Reuse the work & same worktree** (sequential — message Owner via `hc send`, or spawn a worker tab) | `devbench spawn <slug> --role worker --domain review --cwd-repo <alias> --task "..."` |
| **Parallel or separate-PR track touching the same repo** inside an active initiative | **Reuse the work & add an isolated track** (`<repo>--<track>` on branch `<prefix><slug>--<track>`) in the SAME Herdr space | `devbench spawn <slug> --role worker --track <track> --cwd-repo <alias> --task "..."` |
| **Need another repo** added to an active initiative | **Attach repo to the existing work** | `devbench add-track <slug> --repo <alias> [--track <track>]` |

> Never spawn two parallel editing sessions into the **same** worktree directory — use `--track <name>` so each parallel track gets its own worktree and branch inside the same Herdr space.

---

## Dispatcher Operating Rules

1. **Orchestrate, don't code in main checkouts:** Never edit source repositories directly as the Dispatcher. Spin up a work (`devbench new`) or track (`--track`) and delegate execution to an **Owner** or **worker**.
2. **Keep session lifetime clear (`--outlive-parent`):** `devbench spawn` uses `--outlive-parent` so active Owner/worker sessions survive if this Dispatcher tab restarts or closes. They are **not** zombies — closing a pane/space in Herdr or running `devbench archive` kills and cleans them up.
3. **Automatic tab labeling:** `devbench` automatically names the workspace's initial shell tab `shell`, the Owner tab `owner`, and worker tabs `worker:<domain>` (including `worker:<track>` when `--domain` is omitted). Only rename manually if you create custom ad-hoc tabs.
4. **Never blind-resend on trust prompts:** If `spawn` returns `status: "blocked"` (`first_message_sent: false`), display `screen_preview` to the user and wait for them to approve the prompt. Never pass `--force`.
5. **Confirm before archiving:** Always confirm with the user before calling `devbench archive <slug>`. Archiving inspects ALL repo worktrees and tracks inside the initiative before mutating anything; dirty/untracked worktrees block archive safely by default — offer `--snapshot-wip` (commits WIP to task/track branches) instead of forcing deletion.

---

## Core Commands

```sh
# Initialize or validate external user config (~/.devbench/config.json)
devbench init
devbench config check

# Create a central initiative work folder + initial git worktrees + central state
devbench new <slug> --repos api,web --title "Auth v2 initiative"

# Add an extra repo or parallel branch track inside an active initiative
devbench add-track <slug> --repo api --track validate-checks

# Launch persistent Owner or worker (auto-labels tabs `shell`, `owner`, `worker:<domain>`)
devbench spawn <slug> --role owner --agent claude --cwd-repo api --task "<exact goal, paths, and reporting instructions>"
devbench spawn <slug> --role worker --domain review --agent codex --cwd-repo api --task "<review scope>"
# Launch a parallel track worker (auto-creates <repo>--<track> worktree & branch if missing)
devbench spawn <slug> --role worker --track modes --cwd-repo api --agent claude --task "<track scope>"

# Inspect all works (live reconciled) or one work
devbench status
devbench status <slug>

# Bind or resume a native AI session ID (rechecks central state; launches in saved cwd)
devbench bind <slug> --key owner --session-id <native-id>
devbench resume <slug> --key owner [--session-id <native-id>]

# Archive a completed initiative (closes 1:1 Herdr space first, detaches all clean worktrees, keeps all branches)
devbench archive <slug> [--snapshot-wip]
```

## Owner Brief Template (when spawning an Owner)

Include these details in `--task` so the Owner knows its Tier 2 role:
- Exact goal and definition of done for this initiative (or track).
- Target worktree paths inside `<work_root>/<slug>/`.
- Reminder to follow the `devbench-owner` conventions: stay inside worktrees, spawn parallel track workers with `--track <name>` inside this same work slug when touching the same repo concurrently, and report back when done or blocked via `hc send <dispatcher_terminal_id> "<summary>"`.
