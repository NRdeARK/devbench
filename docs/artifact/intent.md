# devbench · intent

## What
A standalone multi-agent plugin — distributed from its own GitHub marketplace repository (`NRdeARK/devbench`) for Antigravity, Claude Code, and Codex — built on `herdr-core` (`hc`) and `herdr-app` that structures work across a clear agent hierarchy: **Dispatcher → Owner → (domain) worker** (with ephemeral **sub-agents** handled by a separate delegation layer). For each task, the Dispatcher creates a central work folder under a user-configured work root containing auto-provisioned git worktrees for each target repository, launches persistent Owner and optional domain-worker sessions inside Herdr workspaces, tabs, or side panes, persists work state alongside each session's native AI session ID and status, and archives the entire work folder once the task is done.

## Why
Running multiple coding agents manually means opening panes by hand, setting up separate git worktrees per repo, and losing track of which AI session owns which task or domain. Packaging this as a neutral cross-agent plugin — with strict separation between plugin code and user configuration — turns one request into a resumable, isolated multi-repo work folder with clear session hierarchy, reliable state tracking, and one-step archiving.

## Before state
Today the user manually opens Herdr panes or terminals, navigates into repositories, and runs raw commands to start each agent session.
- Placing an Owner or domain-worker session into a new workspace, tab, or split pane and creating isolated git worktrees for each repo takes repeated manual setup
- Multi-repo tasks leave scattered branches with no single work folder or saved record of work status, Herdr pane IDs, and native AI session IDs to inspect or resume later
- Checking which agent session finished or got stuck means inspecting terminals one by one
- Orchestration logic, helper scripts, and personal machine config are mixed together across local skills instead of separated into a versioned plugin marketplace repo (with a config template) and a gitignored user config

## Constraints
- Must be packaged as a standalone multi-agent plugin in a dedicated GitHub marketplace repository (`NRdeARK/devbench` with `.antigravity-plugin/plugin.json`, `.claude-plugin/plugin.json`, `.claude-plugin/marketplace.json`, plus Codex skill compatibility)
- Must strictly separate **plugin code** and a versioned **config template** (committed in the repo so others can copy and follow) from **user configuration** (external-first `~/.devbench/config.json`, never committed: default work root, archive root, repo aliases, and default agent CLI commands) and **central runtime work state** (`~/.devbench/state/works/<slug>.json`, instantly inspectable by the Dispatcher)
- Must model persistent sessions using the **Dispatcher → Owner → (domain) worker** hierarchy with a **1 work : 1 Herdr workspace** mapping (Owner + domain workers live as tabs or split panes inside that work's dedicated workspace), persisting per-work state (work status, Herdr workspace ID, target repos, worktree paths/branches) and per-session state (tier/role, domain, harness, Herdr session/pane ID, native AI session ID, session status) so any work can be inspected and resumed across restarts and devices
- Must manage persistent terminals and messaging via `herdr-core` (`hc`) and window layout via `herdr-app`, working cleanly on Windows and any platform where `hc` and `git` run
- Must keep session creation simple and decoupled from both ephemeral **sub-agent** quota routing and automatic work-type → harness/model routing tables (deferred from v1; launch the requested or default configured agent CLI directly)
- Must close the work's Herdr panes and workspace, cleanly detach git worktrees, and archive the entire work folder (including saved state) upon explicit user confirmation without losing uncommitted changes or disrupting any other work's Herdr workspace

## Outcome success
The user installs `devbench` from its plugin marketplace repo, fills in their gitignored user config from the provided template, and gives one request to the Dispatcher; `devbench` creates a central work folder under the configured work root with auto-provisioned git worktrees for each target repo, opens the Owner and any needed domain-worker sessions in Herdr, persists work and AI session IDs/states for tracking or resumption, and archives the entire work folder when the work is done.
