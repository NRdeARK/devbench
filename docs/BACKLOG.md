# devbench backlog

## DB-001 — Initiative sizing model & multi-track worktrees inside one work (`Option B`)
- **Found:** 2026-11-22, generate-artifact v0.2 work. The Dispatcher created two works (`ga-validate-checks`, `ga-modes`) for parallel tracks of the same milestone → two Herdr spaces, two folders, user had to hop between them. Follow-up observation: the word "work" was too broad without an explicit granularity boundary.
- **Decision (Option B):** **1 Work = 1 Initiative / Milestone / Focus Area = 1 Herdr Workspace**, with isolated parallel tracks inside `<work_root>/<slug>/<repo>--<track>` on branch `<prefix><slug>--<track>` when multiple tracks touch the same repo concurrently.
- **Implemented (2026-11-22):**
  - Rust CLI: added `devbench add-track <slug> --repo <alias> [--track <name>] [--base-ref <ref>]` and `devbench spawn --track <name>` (auto-provisions `<repo>--<track>` worktree & `<prefix><slug>--<track>` branch if missing, defaults worker `--domain` to `<track>`, and archives/preserves all track branches cleanly).
  - Skills: updated `skills/devbench/SKILL.md` and `skills/devbench-owner/SKILL.md` with the explicit Initiative sizing decision table.
  - Tests: covered in `tests/worktree_tests.rs` and `tests/herdr_e2e_tests.rs`.
- **Status:** Done.

## DB-002 — Name Herdr spaces/tabs by role & fix live `herdr-core get` contract
- **Found:** 2026-11-22. The Dispatcher's space showed as `~` and work tabs showed as `1`/`2`; live `herdr-core get` also returned `"address"` (not `"id"`) and object `"status": {"app", "state"}` (not bare string).
- **Implemented (2026-11-22):**
  - Rust CLI: `space.new` now sends `"tab": "shell"` for the initial shell tab; `devbench spawn` and `devbench resume` look up the spawned terminal in `herdr-app ls`, record `herdr_tab_id` + `herdr_pane_id`, and automatically call `herdr-app do tab.rename` (`owner` / `worker:<domain>`).
  - Rust CLI: `parse_hc_terminal_json` now reads `"address"` (fallback `"id"`) and decodes both object `status.state` and string `status`.
  - Skills: fixed Step 0.3 in `skills/devbench/SKILL.md` to use real `herdr-app do` JSON payload syntax (`'{"id":"...","name":"dispatcher"}'`).
  - Tests: updated `fake_herdr` and `tests/herdr_e2e_tests.rs` to enforce `tab: "shell"`, `tab.rename`, `address`, and object `status`.
- **Status:** Done.
