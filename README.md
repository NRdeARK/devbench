# devbench

Multi-agent workspace and persistent session orchestrator built on [`herdr-core`](https://github.com/herdr/herdr-core) (`hc`) and `herdr-app` for **Antigravity**, **Claude Code**, and **Codex**.

## What it does

- **4-tier agent hierarchy:** `Dispatcher -> Owner -> worker` (domain specialists, parallel branch tracks, or sequential reviewers; leaving ephemeral `sub-agent` delegation decoupled).
- **1 Initiative (Work) : 1 Herdr Workspace:** Every initiative/milestone gets one central folder (`<work_root>/<slug>/`) holding git worktrees per repo (plus optional isolated parallel tracks `<repo>--<track>` on branch `<prefix><slug>--<track>`), paired 1-to-1 with a dedicated Herdr workspace with auto-labeled tabs (`shell`, `owner`, `worker:<domain>`).
- **Three separated stores:**
  1. **Plugin code** (read-only in marketplace caches + committed `templates/config.example.json`)
  2. **External-first user config** (`$DEVBENCH_CONFIG` -> `~/.devbench/config.json`, survives versioned marketplace updates, shared across `agy`, `claude`, `codex`)
  3. **Central runtime work state** (`~/.devbench/state/works/<slug>.json` with per-work advisory locks, atomic writes, live Herdr reconciliation, phase checkpoints, and a frozen snapshot copied into `<archive_root>/<slug>/` on archive)
- **Single native Rust binary (`devbench` / `devbench.exe`):** Zero runtime dependencies, fast startup, safe Windows argv handling and file locking.

## Quick start

```sh
# 1. Initialize external user config (~/.devbench/config.json) from template
devbench init
# Edit ~/.devbench/config.json with your work_root, archive_root, and repo aliases
devbench config check

# 2. Setup an initiative work folder + git worktrees
devbench new auth-v2 --repos api,web --title "Auth v2 initiative"

# 3. Launch persistent Owner & optional parallel track worker in the SAME Herdr space
devbench spawn auth-v2 --role owner --agent claude --cwd-repo api --task "Own auth v2..."
devbench spawn auth-v2 --role worker --track oauth --cwd-repo api --agent codex --task "Parallel OAuth track..."

# 4. Inspect all works from central state (reconciled live with Herdr) or resume
devbench status
devbench resume auth-v2 --key owner

# 5. Archive completed initiative (closes 1:1 Herdr space first, detaches all clean worktrees, keeps all branches)
devbench archive auth-v2
```

## Build from source

End users do not need Rust installed when using prebuilt release binaries. Contributors building from source:

```sh
cargo test
cargo build --release
```

## Solution Design Docs

Full intent and 5-tab solution design artifacts live in `docs/artifact/` (EN/TH).
