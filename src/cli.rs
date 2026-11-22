use crate::adapters::adapter_for;
use crate::adapters::codex::{codex_sessions_dir, scan_codex_rollouts};
use crate::archive::archive_work;
use crate::config::{check_config, init_config, load_config, resolve_config_path};
use crate::herdr::{HcGetResult, HerdrClient, TerminalPlacement};
use crate::paths::{build_hc_name, sanitize_identifier, sanitize_slug};
use crate::state::{
    list_works_with_warnings, load_work, mutate_work, save_work, CaptureState, SessionStatus,
    SessionTier, WorkSession, WorkState, WorkStatus,
};
use crate::worktree::{add_track_worktree, setup_workspace};
use chrono::DateTime;
use clap::{Parser, Subcommand, ValueEnum};
use serde_json::json;
use std::collections::HashSet;
use std::path::PathBuf;
use std::time::SystemTime;

#[derive(Parser, Debug)]
#[command(
    name = "devbench",
    version,
    about = "Multi-agent workspace and session orchestrator built on herdr-core and herdr-app"
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Commands,
}

#[derive(Subcommand, Debug)]
pub enum Commands {
    /// Initialize external user config (~/.devbench/config.json) from committed template
    Init,
    /// Manage or validate user config
    Config {
        #[command(subcommand)]
        action: ConfigAction,
    },
    /// Create a central work folder, git worktrees per repo, and central work state
    New {
        /// Work slug (sanitized for Windows/Unix safety)
        slug: String,
        /// Comma-separated repo aliases from config.repos (e.g. api,web)
        #[arg(long, value_delimiter = ',')]
        repos: Vec<String>,
        /// Optional human-readable task title
        #[arg(long)]
        title: Option<String>,
    },
    /// Add an extra repo or parallel branch track worktree inside an existing active work
    AddTrack {
        /// Existing active work slug
        slug: String,
        /// Repo alias from config.repos
        #[arg(long)]
        repo: String,
        /// Optional parallel track name (creates <repo>--<track> on branch <prefix><slug>--<track>)
        #[arg(long)]
        track: Option<String>,
        /// Optional git base ref override (defaults to repo's config.base_ref)
        #[arg(long)]
        base_ref: Option<String>,
    },
    /// Launch a persistent Owner or domain-worker session inside the work's 1:1 Herdr workspace
    Spawn {
        slug: String,
        #[arg(long, value_enum, default_value_t = RoleArg::Owner)]
        role: RoleArg,
        /// Required when --role worker unless --track is provided (e.g. backend, web, review; cannot be 'owner')
        #[arg(long)]
        domain: Option<String>,
        /// Agent harness key in config.agents (defaults to config.default_agent)
        #[arg(long)]
        agent: Option<String>,
        /// Repo alias inside the work folder to use as cwd (defaults to work root folder)
        #[arg(long)]
        cwd_repo: Option<String>,
        /// Optional parallel branch track inside the same work (auto-creates <repo>--<track> worktree if missing)
        #[arg(long)]
        track: Option<String>,
        /// First task prompt passed directly into `hc run`
        #[arg(long)]
        task: Option<String>,
    },
    /// Inspect all works (or one work) from central state, reconciled against live Herdr
    Status {
        slug: Option<String>,
        /// Skip live Herdr reconciliation (offline inspection)
        #[arg(long)]
        offline: bool,
    },
    /// Manually bind or update a native AI session ID on a tracked session
    Bind {
        slug: String,
        /// Session key (e.g. owner, worker-web)
        #[arg(long, default_value = "owner")]
        key: String,
        #[arg(long)]
        session_id: String,
    },
    /// Focus a live session or resume a stopped session using its saved (or provided) native AI ID
    Resume {
        slug: String,
        #[arg(long, default_value = "owner")]
        key: String,
        /// Optional native AI session ID override (rechecks central state if omitted)
        #[arg(long)]
        session_id: Option<String>,
    },
    /// Close 1:1 Herdr workspace, detach clean git worktrees, write frozen snapshot, and move to archive_root
    Archive {
        slug: String,
        /// Commit uncommitted edits as a WIP snapshot on task branches instead of blocking
        #[arg(long)]
        snapshot_wip: bool,
        /// Allow running archive from inside the work's own Herdr terminal (closes own space last)
        #[arg(long)]
        allow_self_archive: bool,
    },
}

#[derive(Subcommand, Debug)]
pub enum ConfigAction {
    /// Validate config syntax, schema_version, template key diff, and repo paths
    Check,
}

#[derive(ValueEnum, Clone, Debug, PartialEq, Eq)]
pub enum RoleArg {
    Owner,
    Worker,
}

pub fn run(cli: Cli) -> Result<String, String> {
    match cli.command {
        Commands::Init => {
            let (path, created, report) = init_config()?;
            Ok(serde_json::to_string_pretty(&json!({
                "action": if created { "created" } else { "existing_untouched" },
                "config_path": path.display().to_string(),
                "check": report,
            }))
            .unwrap())
        }
        Commands::Config {
            action: ConfigAction::Check,
        } => {
            let (path, source) = resolve_config_path()?;
            let report = check_config(&path, source);
            let out = serde_json::to_string_pretty(&report).unwrap();
            if report.ok {
                Ok(out)
            } else {
                Err(out)
            }
        }
        Commands::New { slug, repos, title } => {
            let (_, cfg) = load_config()?;
            let mut state = setup_workspace(&cfg, &slug, title, &repos)?;
            let state_path = save_work(&mut state)?;
            Ok(serde_json::to_string_pretty(&json!({
                "state_path": state_path.display().to_string(),
                "work": state,
            }))
            .unwrap())
        }
        Commands::AddTrack {
            slug,
            repo,
            track,
            base_ref,
        } => {
            let (_, cfg) = load_config()?;
            let clean_slug = sanitize_slug(&slug)?;
            let added = add_track_worktree(
                &cfg,
                &clean_slug,
                &repo,
                track.as_deref(),
                base_ref.as_deref(),
            )?;
            Ok(serde_json::to_string_pretty(&json!({
                "slug": clean_slug,
                "repo": added,
            }))
            .unwrap())
        }
        Commands::Spawn {
            slug,
            role,
            domain,
            agent,
            cwd_repo,
            track,
            task,
        } => cmd_spawn(&slug, role, domain, agent, cwd_repo, track, task),
        Commands::Status { slug, offline } => cmd_status(slug.as_deref(), offline),
        Commands::Bind {
            slug,
            key,
            session_id,
        } => {
            let slug = sanitize_slug(&slug)?;
            let trimmed_id = session_id.trim();
            if trimmed_id.is_empty() {
                return Err("--session-id cannot be empty".to_string());
            }
            let sid_owned = trimmed_id.to_string();
            let (_, ()) = mutate_work(&slug, |state| {
                let session = state
                    .sessions
                    .iter_mut()
                    .find(|s| s.key == key)
                    .ok_or_else(|| format!("Session '{key}' not found in work '{slug}'"))?;
                session.native_session_id = Some(sid_owned.clone());
                session.capture = CaptureState::Manual;
                session.last_seen_at = WorkState::now_iso();
                Ok((true, ()))
            })?;
            Ok(serde_json::to_string_pretty(&json!({
                "slug": slug,
                "key": key,
                "native_session_id": sid_owned,
                "capture": "manual",
            }))
            .unwrap())
        }
        Commands::Resume {
            slug,
            key,
            session_id,
        } => cmd_resume(&slug, &key, session_id.as_deref()),
        Commands::Archive {
            slug,
            snapshot_wip,
            allow_self_archive,
        } => {
            let slug = sanitize_slug(&slug)?;
            let (_, cfg) = load_config()?;
            let report = archive_work(&cfg, &slug, snapshot_wip, allow_self_archive)?;
            Ok(serde_json::to_string_pretty(&report).unwrap())
        }
    }
}

fn cmd_spawn(
    raw_slug: &str,
    role: RoleArg,
    domain: Option<String>,
    agent_opt: Option<String>,
    cwd_repo: Option<String>,
    track_opt: Option<String>,
    task: Option<String>,
) -> Result<String, String> {
    let slug = sanitize_slug(raw_slug)?;
    let (_, cfg) = load_config()?;

    let clean_track = track_opt
        .as_deref()
        .map(|t| sanitize_identifier("Track", t))
        .transpose()?;

    let (tier, key, clean_domain) = match role {
        RoleArg::Owner => (SessionTier::Owner, "owner".to_string(), None),
        RoleArg::Worker => {
            let dom_source = domain.as_deref().or(clean_track.as_deref());
            let dom = dom_source
                .map(|d| sanitize_identifier("Worker domain", d))
                .transpose()?
                .ok_or_else(|| {
                    "--domain <name> (or --track <name>) is required when --role worker".to_string()
                })?;
            if dom == "owner" {
                return Err(
                    "Worker --domain cannot be 'owner' (reserved for the Owner tier)".to_string(),
                );
            }
            (SessionTier::Worker, format!("worker-{dom}"), Some(dom))
        }
    };

    let harness = agent_opt.unwrap_or_else(|| cfg.default_agent.clone());
    let agent_cfg = cfg
        .agents
        .get(&harness)
        .ok_or_else(|| format!("Unknown agent harness '{harness}' in config.agents"))?;

    let state = load_work(&slug)?;
    if matches!(state.status, WorkStatus::Archived | WorkStatus::Archiving) {
        return Err(format!(
            "Cannot spawn into work '{slug}': status is {:?}",
            state.status
        ));
    }

    let cwd = if let Some(track) = clean_track.as_deref() {
        let target_alias = if let Some(alias) = cwd_repo.as_deref() {
            alias.trim().to_string()
        } else {
            let distinct: Vec<&str> = {
                let mut seen = HashSet::new();
                state
                    .repos
                    .iter()
                    .filter(|r| seen.insert(r.alias.as_str()))
                    .map(|r| r.alias.as_str())
                    .collect()
            };
            if distinct.len() == 1 {
                distinct[0].to_string()
            } else {
                return Err(format!(
                    "Specify --cwd-repo <alias> when using --track '{track}' in multi-repo work '{slug}'"
                ));
            }
        };

        if let Some(existing) = state
            .repos
            .iter()
            .find(|r| r.alias == target_alias && r.track.as_deref() == Some(track))
        {
            PathBuf::from(&existing.worktree_path)
        } else {
            let added = add_track_worktree(&cfg, &slug, &target_alias, Some(track), None)?;
            PathBuf::from(&added.worktree_path)
        }
    } else if let Some(alias) = cwd_repo {
        let repo = state
            .repos
            .iter()
            .find(|r| r.alias == alias && r.track.is_none())
            .or_else(|| state.repos.iter().find(|r| r.alias == alias))
            .ok_or_else(|| format!("Repo alias '{alias}' is not part of work '{slug}'"))?;
        PathBuf::from(&repo.worktree_path)
    } else {
        PathBuf::from(&state.work_dir)
    };
    if !cwd.is_dir() {
        return Err(format!("Target cwd does not exist: {}", cwd.display()));
    }

    let herdr = HerdrClient::from_config(&cfg.herdr);
    let hc_name = build_hc_name(&cfg.herdr.name_prefix, &slug, clean_domain.as_deref());

    // Idempotent relaunch guard (UC-03 edge case): abort on daemon Err, verify space label match!
    match herdr.hc_get(&hc_name) {
        Ok(HcGetResult::Live(live_info)) => {
            let space_matches = match (&state.herdr_space_id, &live_info.space_label) {
                (Some(expected), Some(actual)) => expected == actual,
                _ => true,
            };
            if space_matches {
                let _ = herdr.agent_focus(&live_info.id);
                // Ensure state records the reused terminal if missing
                let _ = mutate_work(&slug, |s| {
                    if let Some(existing) = s.sessions.iter_mut().find(|sess| sess.key == key) {
                        existing.hc_terminal_id = Some(live_info.id.clone());
                        existing.last_seen_at = WorkState::now_iso();
                        Ok((true, ()))
                    } else {
                        Ok((false, ()))
                    }
                });
                return Ok(serde_json::to_string_pretty(&json!({
                    "reused": true,
                    "slug": slug,
                    "key": key,
                    "hc_terminal_id": live_info.id,
                    "hc_name": hc_name,
                }))
                .unwrap());
            }
        }
        Ok(HcGetResult::Finished(_)) | Ok(HcGetResult::NotFound) => {}
        Err(e) => {
            return Err(format!(
                "Cannot check Herdr state before spawn (open Herdr first): {e}"
            ));
        }
    }

    // Ensure 1:1 Herdr workspace exists (recreate if missing)
    let mut created_space = false;
    let mut new_space_initial: Option<String> = None;
    let space_id = match state.herdr_space_id.clone() {
        Some(existing) if herdr.space_exists(&existing).unwrap_or(true) => existing,
        _ => {
            let created = herdr.space_new(
                &slug,
                &PathBuf::from(&state.work_dir),
                cfg.herdr.focus_on_create,
            )?;
            created_space = true;
            let sid = created.space_id.clone();
            let init_tid = created.initial_terminal_id.clone();
            new_space_initial = init_tid.clone();
            let _ = mutate_work(&slug, |s| {
                s.herdr_space_id = Some(sid.clone());
                s.herdr_space_name = Some(slug.clone());
                s.herdr_space_initial_terminal = init_tid.clone();
                Ok((true, ()))
            })?;
            sid
        }
    };

    let existing_native_ids: HashSet<String> = state
        .sessions
        .iter()
        .filter_map(|s| s.native_session_id.clone())
        .collect();

    let adapter = adapter_for(&harness);
    let plan = adapter.prepare_launch(agent_cfg);
    let launched_at = SystemTime::now();

    let hc_info = match herdr.hc_run_persistent(&hc_name, &cwd, &space_id, &plan.argv, task.as_deref()) {
        Ok(info) => info,
        Err(err) => {
            if created_space {
                let _ = herdr.space_close(&space_id);
                let _ = mutate_work(&slug, |s| {
                    s.herdr_space_id = None;
                    s.herdr_space_initial_terminal = None;
                    Ok((true, ()))
                });
            }
            return Err(format!("Failed to launch {hc_name} via herdr-core: {err}"));
        }
    };

    let (native_id, capture) =
        adapter.capture_after_launch(&cwd, launched_at, &plan, &existing_native_ids);

    let (status, first_sent, screen_preview) = evaluate_readiness(&herdr, &hc_info, task.is_some());

    let tab_label = match tier {
        SessionTier::Owner => "owner".to_string(),
        SessionTier::Worker => format!("worker:{}", clean_domain.as_deref().unwrap_or("worker")),
    };
    let placement = herdr.wait_and_label_tab(&space_id, &hc_info.id, &hc_name, &tab_label);
    let initial_retired =
        retire_initial_tab(&herdr, &space_id, new_space_initial.as_deref(), placement.as_ref());

    let now = WorkState::now_iso();
    let session = WorkSession {
        key: key.clone(),
        tier,
        domain: clean_domain,
        harness: harness.clone(),
        cwd: cwd.to_string_lossy().to_string(),
        hc_terminal_id: Some(hc_info.id.clone()),
        hc_name: Some(hc_name),
        herdr_tab_id: placement.as_ref().map(|p| p.tab_id.clone()),
        herdr_pane_id: placement.as_ref().and_then(|p| p.pane_id.clone()),
        native_session_id: native_id,
        capture,
        status,
        first_message_sent: first_sent,
        started_at: now.clone(),
        last_seen_at: now,
    };

    let _ = mutate_work(&slug, |s| {
        s.herdr_space_id = Some(space_id.clone());
        s.herdr_space_name = Some(slug.clone());
        if initial_retired {
            s.herdr_space_initial_terminal = None;
        }
        s.upsert_session(session.clone());
        Ok((true, ()))
    })?;

    Ok(serde_json::to_string_pretty(&json!({
        "reused": false,
        "slug": slug,
        "herdr_space_id": space_id,
        "session": session,
        "screen_preview": screen_preview,
    }))
    .unwrap())
}

/// Close the blank `shell` tab of a space this command just created, so the space holds only
/// agent tabs. Only runs when the agent's own tab was located (otherwise closing could empty the
/// space). Best-effort: a failure is warned about, never fails the launch. Returns true if closed.
fn retire_initial_tab(
    herdr: &HerdrClient,
    space_id: &str,
    initial_terminal_id: Option<&str>,
    agent_placement: Option<&TerminalPlacement>,
) -> bool {
    let (Some(initial), Some(agent)) = (initial_terminal_id, agent_placement) else {
        return false;
    };
    match herdr.close_initial_tab(space_id, initial, &agent.tab_id) {
        Ok(closed) => closed,
        Err(e) => {
            eprintln!("warning: could not close blank shell tab of space {space_id}: {e}");
            false
        }
    }
}

fn evaluate_readiness(
    herdr: &HerdrClient,
    hc_info: &crate::herdr::HcTerminalInfo,
    had_task: bool,
) -> (SessionStatus, bool, Option<String>) {
    if hc_info.status.as_deref() == Some("blocked") {
        let preview = herdr.hc_read(&hc_info.id, 30).ok();
        return (SessionStatus::Blocked, false, preview);
    }
    if had_task {
        match (hc_info.first_message_sent, hc_info.first_message_reason.as_deref()) {
            (Some(true), _) => (SessionStatus::Running, true, None),
            (Some(false), Some("pending")) => (SessionStatus::Launching, false, None),
            (Some(false), _) => {
                let preview = herdr.hc_read(&hc_info.id, 30).ok();
                (SessionStatus::Blocked, false, preview)
            }
            (None, _) => (SessionStatus::Launching, false, None),
        }
    } else {
        (SessionStatus::Running, true, None)
    }
}

fn cmd_status(slug_opt: Option<&str>, offline: bool) -> Result<String, String> {
    let herdr_opt = if offline {
        None
    } else {
        match load_config() {
            Ok((_, cfg)) => Some(HerdrClient::from_config(&cfg.herdr)),
            Err(e) => {
                // Only proceed offline if no config exists yet; otherwise surface load error
                if e.contains("not found") {
                    None
                } else {
                    return Err(e);
                }
            }
        }
    };

    let (mut works, warnings) = if let Some(raw_slug) = slug_opt {
        (vec![load_work(&sanitize_slug(raw_slug)?)?], Vec::new())
    } else {
        list_works_with_warnings()?
    };

    if let Some(herdr) = herdr_opt {
        for work in &mut works {
            if matches!(work.status, WorkStatus::Archived | WorkStatus::Archiving) {
                continue;
            }
            // Probe live Herdr outside the lock, then patch ONLY session fields under lock!
            let mut updates: Vec<(String, SessionStatus, Option<String>)> = Vec::new();
            let bound_ids: HashSet<String> = work
                .sessions
                .iter()
                .filter_map(|s| s.native_session_id.clone())
                .collect();

            for s in &work.sessions {
                if s.status == SessionStatus::Archived {
                    continue;
                }
                let Some(target) = s.hc_terminal_id.clone().or_else(|| s.hc_name.clone()) else {
                    continue;
                };
                let live_status = match herdr.hc_get(&target) {
                    Ok(HcGetResult::Live(info)) => Some(match info.status.as_deref() {
                        Some("blocked") => SessionStatus::Blocked,
                        Some("idle") | Some("done") => SessionStatus::Idle,
                        Some("working") => SessionStatus::Running,
                        _ => SessionStatus::Running,
                    }),
                    Ok(HcGetResult::Finished(_)) => Some(SessionStatus::Exited),
                    Ok(HcGetResult::NotFound) => Some(SessionStatus::Lost),
                    Err(_) => None, // Daemon unreachable: leave cached status unchanged
                };

                // Re-scan pending Codex sessions on status check too
                let mut newly_captured = None;
                if s.harness == "codex" && s.capture == CaptureState::Pending && s.native_session_id.is_none() {
                    if let (Some(sessions_root), Ok(started_dt)) =
                        (codex_sessions_dir(), DateTime::parse_from_rfc3339(&s.started_at))
                    {
                        let launched_at: SystemTime = started_dt.into();
                        newly_captured =
                            scan_codex_rollouts(&sessions_root, PathBuf::from(&s.cwd).as_path(), launched_at, &bound_ids);
                    }
                }

                if let Some(new_st) = live_status {
                    if new_st != s.status || newly_captured.is_some() {
                        updates.push((s.key.clone(), new_st, newly_captured));
                    }
                } else if newly_captured.is_some() {
                    updates.push((s.key.clone(), s.status.clone(), newly_captured));
                }
            }

            if !updates.is_empty() {
                if let Ok((fresh, _)) = mutate_work(&work.slug, |locked| {
                    // Never clobber an archive that started concurrently
                    if matches!(locked.status, WorkStatus::Archived | WorkStatus::Archiving) {
                        return Ok((false, ()));
                    }
                    let mut dirty = false;
                    for (key, new_st, cap_id) in &updates {
                        if let Some(sess) = locked.sessions.iter_mut().find(|x| &x.key == key) {
                            if sess.status != *new_st {
                                sess.status = new_st.clone();
                                sess.last_seen_at = WorkState::now_iso();
                                dirty = true;
                            }
                            if let Some(id) = cap_id {
                                if sess.native_session_id.is_none() {
                                    sess.native_session_id = Some(id.clone());
                                    sess.capture = CaptureState::Auto;
                                    dirty = true;
                                }
                            }
                        }
                    }
                    Ok((dirty, ()))
                }) {
                    *work = fresh;
                }
            }
        }
    }

    Ok(serde_json::to_string_pretty(&json!({
        "count": works.len(),
        "works": works,
        "warnings": warnings,
    }))
    .unwrap())
}

fn cmd_resume(raw_slug: &str, key: &str, explicit_id: Option<&str>) -> Result<String, String> {
    let slug = sanitize_slug(raw_slug)?;
    let (_, cfg) = load_config()?;
    let state = load_work(&slug)?;
    let herdr = HerdrClient::from_config(&cfg.herdr);

    if matches!(state.status, WorkStatus::Archived | WorkStatus::Archiving) {
        return Err(format!(
            "Work '{slug}' is {:?}; cannot resume an archived work",
            state.status
        ));
    }

    let session = state
        .sessions
        .iter()
        .find(|s| s.key == key)
        .cloned()
        .ok_or_else(|| format!("Session '{key}' not found in work '{slug}'"))?;

    let cwd = PathBuf::from(&session.cwd);
    if !cwd.is_dir() {
        let cross_host = !state.host_id.is_empty() && state.host_id != WorkState::current_host_id();
        let hint = if cross_host {
            format!(
                "State originated on host '{}' (current host '{}'). Native AI sessions are machine-local; recreate worktrees from git branches and start a fresh session with a handoff note.",
                state.host_id,
                WorkState::current_host_id()
            )
        } else {
            "Recreate worktrees from branches or verify work_root.".to_string()
        };
        return Err(format!(
            "Saved cwd does not exist on this machine: {}. {}",
            cwd.display(),
            hint
        ));
    }

    // Check live Herdr first — ABORT on daemon Err (never duplicate!)
    if let Some(target) = session.hc_terminal_id.as_deref().or(session.hc_name.as_deref()) {
        match herdr.hc_get(target) {
            Ok(HcGetResult::Live(info)) => {
                if info.status.as_deref() == Some("blocked") {
                    let preview = herdr.hc_read(&info.id, 30).unwrap_or_default();
                    return Err(format!(
                        "Session '{key}' ({}) is blocked on a prompt; resolve it manually (never --force):\n{}",
                        info.id, preview
                    ));
                }
                let _ = herdr.agent_focus(&info.id);
                return Ok(serde_json::to_string_pretty(&json!({
                    "action": "reattached",
                    "slug": slug,
                    "key": key,
                    "hc_terminal_id": info.id,
                }))
                .unwrap());
            }
            Ok(HcGetResult::Finished(_)) | Ok(HcGetResult::NotFound) => {}
            Err(e) => {
                return Err(format!(
                    "Cannot verify live Herdr terminal before resume (refusing to launch duplicate): {e}"
                ));
            }
        }
    }

    // Recheck saved native AI session ID (also re-scan Codex rollouts if pending) or accept explicit override
    let mut resolved_id = explicit_id
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .or_else(|| session.native_session_id.clone());

    if resolved_id.is_none() && session.harness == "codex" {
        if let (Some(sessions_root), Ok(started_dt)) = (
            codex_sessions_dir(),
            DateTime::parse_from_rfc3339(&session.started_at),
        ) {
            let bound: HashSet<String> = state
                .sessions
                .iter()
                .filter_map(|s| s.native_session_id.clone())
                .collect();
            let launched_at: SystemTime = started_dt.into();
            resolved_id = scan_codex_rollouts(&sessions_root, &cwd, launched_at, &bound);
        }
    }

    let native_id = resolved_id.ok_or_else(|| {
        format!(
            "No native AI session ID saved for '{slug}/{key}'. Pass `--session-id <id>` or run `devbench bind {slug} --key {key} --session-id <id>` first."
        )
    })?;

    let agent_cfg = cfg
        .agents
        .get(&session.harness)
        .ok_or_else(|| format!("Harness '{}' missing from config.agents", session.harness))?;
    let adapter = adapter_for(&session.harness);
    let resume_argv = adapter.build_resume_argv(agent_cfg, &native_id)?;

    // Ensure 1:1 Herdr space exists (recreate if manually closed)
    let mut created_space = false;
    let mut new_space_initial: Option<String> = None;
    let space_id = match state.herdr_space_id.clone() {
        Some(sid) if herdr.space_exists(&sid).unwrap_or(false) => sid,
        _ => {
            let created = herdr.space_new(
                &slug,
                &PathBuf::from(&state.work_dir),
                cfg.herdr.focus_on_create,
            )?;
            created_space = true;
            let sid = created.space_id.clone();
            let init_tid = created.initial_terminal_id.clone();
            new_space_initial = init_tid.clone();
            let _ = mutate_work(&slug, |s| {
                s.herdr_space_id = Some(sid.clone());
                s.herdr_space_name = Some(slug.clone());
                s.herdr_space_initial_terminal = init_tid.clone();
                Ok((true, ()))
            })?;
            sid
        }
    };

    let hc_name = session
        .hc_name
        .clone()
        .unwrap_or_else(|| build_hc_name(&cfg.herdr.name_prefix, &slug, session.domain.as_deref()));

    let hc_info = match herdr.hc_run_persistent(&hc_name, &cwd, &space_id, &resume_argv, None) {
        Ok(info) => info,
        Err(err) => {
            if created_space {
                let _ = herdr.space_close(&space_id);
                let _ = mutate_work(&slug, |s| {
                    s.herdr_space_id = None;
                    s.herdr_space_initial_terminal = None;
                    Ok((true, ()))
                });
            }
            return Err(format!("Failed to resume {hc_name} via herdr-core: {err}"));
        }
    };

    let (new_status, _, screen_preview) = evaluate_readiness(&herdr, &hc_info, false);
    let tab_label = match session.tier {
        SessionTier::Owner => "owner".to_string(),
        SessionTier::Worker => format!("worker:{}", session.domain.as_deref().unwrap_or("worker")),
    };
    let placement = herdr.wait_and_label_tab(&space_id, &hc_info.id, &hc_name, &tab_label);
    let initial_retired =
        retire_initial_tab(&herdr, &space_id, new_space_initial.as_deref(), placement.as_ref());

    let capture_override = if explicit_id.is_some() {
        Some(CaptureState::Manual)
    } else if session.native_session_id.is_none() {
        Some(CaptureState::Auto)
    } else {
        None
    };

    let _ = mutate_work(&slug, |s| {
        s.herdr_space_id = Some(space_id.clone());
        s.herdr_space_name = Some(slug.clone());
        if initial_retired {
            s.herdr_space_initial_terminal = None;
        }
        if let Some(sess) = s.sessions.iter_mut().find(|x| x.key == key) {
            sess.hc_terminal_id = Some(hc_info.id.clone());
            sess.herdr_tab_id = placement.as_ref().map(|p| p.tab_id.clone());
            sess.herdr_pane_id = placement.as_ref().and_then(|p| p.pane_id.clone());
            sess.native_session_id = Some(native_id.clone());
            if let Some(cap) = capture_override.clone() {
                sess.capture = cap;
            }
            sess.status = new_status.clone();
            sess.last_seen_at = WorkState::now_iso();
        }
        Ok((true, ()))
    })?;

    Ok(serde_json::to_string_pretty(&json!({
        "action": "resumed",
        "slug": slug,
        "key": key,
        "cwd": cwd.display().to_string(),
        "herdr_space_id": space_id,
        "hc_terminal_id": hc_info.id,
        "native_session_id": native_id,
        "status": new_status,
        "screen_preview": screen_preview,
    }))
    .unwrap())
}
