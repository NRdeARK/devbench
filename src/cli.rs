use crate::adapters::adapter_for;
use crate::archive::archive_work;
use crate::config::{check_config, init_config, load_config, resolve_config_path};
use crate::herdr::HerdrClient;
use crate::paths::sanitize_slug;
use crate::state::{
    list_works, load_work, save_work, CaptureState, SessionStatus, SessionTier, WorkSession,
    WorkState,
};
use crate::worktree::setup_workspace;
use clap::{Parser, Subcommand, ValueEnum};
use serde_json::json;
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
    /// Launch a persistent Owner or domain-worker session inside the work's 1:1 Herdr workspace
    Spawn {
        slug: String,
        #[arg(long, value_enum, default_value_t = RoleArg::Owner)]
        role: RoleArg,
        /// Required when --role worker (e.g. backend, web, docs)
        #[arg(long)]
        domain: Option<String>,
        /// Agent harness key in config.agents (defaults to config.default_agent)
        #[arg(long)]
        agent: Option<String>,
        /// Repo alias inside the work folder to use as cwd (defaults to work root folder)
        #[arg(long)]
        cwd_repo: Option<String>,
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
        /// Allow running archive from inside the work's own Herdr terminal
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
        Commands::Spawn {
            slug,
            role,
            domain,
            agent,
            cwd_repo,
            task,
        } => cmd_spawn(&slug, role, domain, agent, cwd_repo, task),
        Commands::Status { slug, offline } => cmd_status(slug.as_deref(), offline),
        Commands::Bind {
            slug,
            key,
            session_id,
        } => {
            let slug = sanitize_slug(&slug)?;
            let mut state = load_work(&slug)?;
            let session = state
                .sessions
                .iter_mut()
                .find(|s| s.key == key)
                .ok_or_else(|| format!("Session '{key}' not found in work '{slug}'"))?;
            session.native_session_id = Some(session_id.clone());
            session.capture = CaptureState::Manual;
            session.last_seen_at = WorkState::now_iso();
            save_work(&mut state)?;
            Ok(serde_json::to_string_pretty(&json!({
                "slug": slug,
                "key": key,
                "native_session_id": session_id,
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
    task: Option<String>,
) -> Result<String, String> {
    let slug = sanitize_slug(raw_slug)?;
    let (_, cfg) = load_config()?;
    let mut state = load_work(&slug)?;

    let (tier, key, role_suffix) = match role {
        RoleArg::Owner => (SessionTier::Owner, "owner".to_string(), "owner".to_string()),
        RoleArg::Worker => {
            let dom = domain
                .as_deref()
                .map(|d| sanitize_slug(d))
                .transpose()?
                .ok_or_else(|| "--domain <name> is required when --role worker".to_string())?;
            (SessionTier::Worker, format!("worker-{dom}"), dom)
        }
    };

    let harness = agent_opt.unwrap_or_else(|| cfg.default_agent.clone());
    let agent_cfg = cfg
        .agents
        .get(&harness)
        .ok_or_else(|| format!("Unknown agent harness '{harness}' in config.agents"))?;

    let cwd = if let Some(alias) = cwd_repo {
        let repo = state
            .repos
            .iter()
            .find(|r| r.alias == alias)
            .ok_or_else(|| format!("Repo alias '{alias}' is not part of work '{slug}'"))?;
        PathBuf::from(&repo.worktree_path)
    } else {
        PathBuf::from(&state.work_dir)
    };

    let herdr = HerdrClient::from_config(&cfg.herdr);
    let hc_name = format!("{}-{}-{}", cfg.herdr.name_prefix, slug, role_suffix);

    // Idempotent relaunch guard (UC-03 edge case)
    if let Ok(Some(live_info)) = herdr.hc_get(&hc_name) {
        if live_info.live {
            let _ = herdr.agent_focus(&live_info.id);
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

    // Ensure 1:1 Herdr workspace exists
    let created_space = state.herdr_space_id.is_none();
    let space_id = match state.herdr_space_id.clone() {
        Some(existing) => existing,
        None => {
            let sid = herdr.space_new(
                &slug,
                &PathBuf::from(&state.work_dir),
                cfg.herdr.focus_on_create,
            )?;
            state.herdr_space_id = Some(sid.clone());
            state.herdr_space_name = Some(slug.clone());
            sid
        }
    };

    let adapter = adapter_for(&harness);
    let plan = adapter.prepare_launch(agent_cfg);
    let launched_at = SystemTime::now();

    let hc_info = match herdr.hc_run_persistent(&hc_name, &cwd, &space_id, &plan.argv, task.as_deref()) {
        Ok(info) => info,
        Err(err) => {
            if created_space {
                let _ = herdr.space_close(&space_id);
                state.herdr_space_id = None;
                let _ = save_work(&mut state);
            }
            return Err(format!("Failed to launch {hc_name} via herdr-core: {err}"));
        }
    };

    let (native_id, capture) = adapter.capture_after_launch(&cwd, launched_at, &plan);
    let first_sent = hc_info.first_message_sent.unwrap_or(true);
    let (status, screen_preview) = if !first_sent || hc_info.status.as_deref() == Some("blocked") {
        let preview = herdr.hc_read(&hc_info.id, 30).ok();
        (SessionStatus::Blocked, preview)
    } else {
        (SessionStatus::Running, None)
    };

    let now = WorkState::now_iso();
    let session = WorkSession {
        key: key.clone(),
        tier,
        domain: if key == "owner" { None } else { Some(role_suffix) },
        harness: harness.clone(),
        cwd: cwd.to_string_lossy().to_string(),
        hc_terminal_id: Some(hc_info.id.clone()),
        hc_name: Some(hc_name),
        herdr_tab_id: None,
        herdr_pane_id: None,
        native_session_id: native_id,
        capture,
        status,
        first_message_sent: first_sent,
        started_at: now.clone(),
        last_seen_at: now,
    };

    state.upsert_session(session.clone());
    save_work(&mut state)?;

    Ok(serde_json::to_string_pretty(&json!({
        "reused": false,
        "slug": slug,
        "herdr_space_id": space_id,
        "session": session,
        "screen_preview": screen_preview,
    }))
    .unwrap())
}

fn cmd_status(slug_opt: Option<&str>, offline: bool) -> Result<String, String> {
    let herdr_opt = if offline {
        None
    } else {
        load_config()
            .ok()
            .map(|(_, cfg)| HerdrClient::from_config(&cfg.herdr))
    };

    let mut works = if let Some(raw_slug) = slug_opt {
        vec![load_work(&sanitize_slug(raw_slug)?)?]
    } else {
        list_works()?
    };

    if let Some(herdr) = herdr_opt {
        for work in &mut works {
            let mut changed = false;
            for s in &mut work.sessions {
                if s.status == SessionStatus::Archived {
                    continue;
                }
                let Some(target) = s.hc_terminal_id.clone().or_else(|| s.hc_name.clone()) else {
                    continue;
                };
                match herdr.hc_get(&target) {
                    Ok(Some(info)) => {
                        let new_status = match info.status.as_deref() {
                            Some("blocked") => SessionStatus::Blocked,
                            Some("idle") | Some("done") => SessionStatus::Idle,
                            Some("working") => SessionStatus::Running,
                            _ if info.live => SessionStatus::Running,
                            _ => SessionStatus::Exited,
                        };
                        if s.status != new_status {
                            s.status = new_status;
                            s.last_seen_at = WorkState::now_iso();
                            changed = true;
                        }
                    }
                    Ok(None) => {
                        if s.status != SessionStatus::Lost && s.status != SessionStatus::Exited {
                            s.status = SessionStatus::Lost;
                            s.last_seen_at = WorkState::now_iso();
                            changed = true;
                        }
                    }
                    Err(_) => {
                        // Herdr daemon unreachable; leave cached state intact
                    }
                }
            }
            if changed {
                let _ = save_work(work);
            }
        }
    }

    Ok(serde_json::to_string_pretty(&json!({
        "count": works.len(),
        "works": works,
    }))
    .unwrap())
}

fn cmd_resume(raw_slug: &str, key: &str, explicit_id: Option<&str>) -> Result<String, String> {
    let slug = sanitize_slug(raw_slug)?;
    let (_, cfg) = load_config()?;
    let mut state = load_work(&slug)?;
    let herdr = HerdrClient::from_config(&cfg.herdr);

    let session_idx = state
        .sessions
        .iter()
        .position(|s| s.key == key)
        .ok_or_else(|| format!("Session '{key}' not found in work '{slug}'"))?;

    let session = state.sessions[session_idx].clone();
    let cwd = PathBuf::from(&session.cwd);
    if !cwd.is_dir() {
        return Err(format!(
            "Saved cwd does not exist on this machine: {} (if moved to another device, recreate worktrees from branches and start a fresh session with a handoff note)",
            cwd.display()
        ));
    }

    // Check if terminal is still live -> reattach/focus instead of duplicating
    if let Some(target) = session.hc_terminal_id.as_deref().or(session.hc_name.as_deref()) {
        if let Ok(Some(info)) = herdr.hc_get(target) {
            if info.live {
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
        }
    }

    // Recheck saved native AI session ID or use user-supplied override
    let native_id = explicit_id
        .map(|s| s.to_string())
        .or_else(|| session.native_session_id.clone())
        .ok_or_else(|| {
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

    // Ensure 1:1 space exists
    let space_id = match state.herdr_space_id.clone() {
        Some(sid) => sid,
        None => {
            let sid = herdr.space_new(
                &slug,
                &PathBuf::from(&state.work_dir),
                cfg.herdr.focus_on_create,
            )?;
            state.herdr_space_id = Some(sid.clone());
            sid
        }
    };

    let hc_name = session
        .hc_name
        .clone()
        .unwrap_or_else(|| format!("{}-{}-{}", cfg.herdr.name_prefix, slug, key));

    let hc_info = herdr.hc_run_persistent(&hc_name, &cwd, &space_id, &resume_argv, None)?;

    let s = &mut state.sessions[session_idx];
    s.hc_terminal_id = Some(hc_info.id.clone());
    s.native_session_id = Some(native_id.clone());
    if explicit_id.is_some() {
        s.capture = CaptureState::Manual;
    }
    s.status = SessionStatus::Running;
    s.last_seen_at = WorkState::now_iso();
    save_work(&mut state)?;

    Ok(serde_json::to_string_pretty(&json!({
        "action": "resumed",
        "slug": slug,
        "key": key,
        "cwd": cwd.display().to_string(),
        "hc_terminal_id": hc_info.id,
        "native_session_id": native_id,
    }))
    .unwrap())
}
