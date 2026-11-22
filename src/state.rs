use crate::lock::{atomic_write, WorkLock};
use crate::paths::{devbench_dir, expand_tilde};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::env;
use std::fs;
use std::path::{Path, PathBuf};

pub const STATE_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WorkStatus {
    Creating,
    Active,
    Archiving,
    Archived,
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ArchivePhase {
    HerdrClosed,
    WorktreesDetached,
    SnapshotWritten,
    Moved,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RepoStatus {
    Ready,
    Removed,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkRepo {
    pub alias: String,
    pub source_path: String,
    pub worktree_path: String,
    pub branch: String,
    pub base_ref: String,
    pub base_sha: String,
    pub status: RepoStatus,
    #[serde(default)]
    pub final_head_sha: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SessionTier {
    Owner,
    Worker,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CaptureState {
    Auto,
    Manual,
    Pending,
    Unavailable,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SessionStatus {
    Launching,
    Running,
    Idle,
    Blocked,
    Exited,
    Lost,
    Archived,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkSession {
    pub key: String,
    pub tier: SessionTier,
    #[serde(default)]
    pub domain: Option<String>,
    pub harness: String,
    pub cwd: String,
    #[serde(default)]
    pub hc_terminal_id: Option<String>,
    #[serde(default)]
    pub hc_name: Option<String>,
    #[serde(default)]
    pub herdr_tab_id: Option<String>,
    #[serde(default)]
    pub herdr_pane_id: Option<String>,
    #[serde(default)]
    pub native_session_id: Option<String>,
    pub capture: CaptureState,
    pub status: SessionStatus,
    #[serde(default)]
    pub first_message_sent: bool,
    pub started_at: String,
    pub last_seen_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkState {
    pub schema_version: u32,
    pub slug: String,
    #[serde(default)]
    pub title: String,
    pub status: WorkStatus,
    #[serde(default)]
    pub archive_phase: Option<ArchivePhase>,
    #[serde(default)]
    pub host_id: String,
    #[serde(default)]
    pub herdr_space_id: Option<String>,
    #[serde(default)]
    pub herdr_space_name: Option<String>,
    pub work_dir: String,
    #[serde(default)]
    pub archive_path: Option<String>,
    #[serde(default)]
    pub repos: Vec<WorkRepo>,
    #[serde(default)]
    pub sessions: Vec<WorkSession>,
    pub created_at: String,
    pub updated_at: String,
}

impl WorkState {
    pub fn now_iso() -> String {
        Utc::now().to_rfc3339()
    }

    pub fn current_host_id() -> String {
        env::var("COMPUTERNAME")
            .or_else(|_| env::var("HOSTNAME"))
            .unwrap_or_else(|_| "local".to_string())
    }

    pub fn new(slug: String, title: String, work_dir: String) -> Self {
        let now = Self::now_iso();
        Self {
            schema_version: STATE_SCHEMA_VERSION,
            slug,
            title,
            status: WorkStatus::Creating,
            archive_phase: None,
            host_id: Self::current_host_id(),
            herdr_space_id: None,
            herdr_space_name: None,
            work_dir,
            archive_path: None,
            repos: Vec::new(),
            sessions: Vec::new(),
            created_at: now.clone(),
            updated_at: now,
        }
    }

    pub fn upsert_session(&mut self, session: WorkSession) {
        if let Some(existing) = self.sessions.iter_mut().find(|s| s.key == session.key) {
            *existing = session;
        } else {
            self.sessions.push(session);
        }
        self.updated_at = Self::now_iso();
    }
}

/// Central works state directory: `$DEVBENCH_STATE_DIR/works` or `~/.devbench/state/works`.
pub fn works_state_dir() -> Result<PathBuf, String> {
    if let Ok(custom) = env::var("DEVBENCH_STATE_DIR") {
        let trimmed = custom.trim();
        if !trimmed.is_empty() {
            return Ok(expand_tilde(trimmed)?.join("works"));
        }
    }
    Ok(devbench_dir()?.join("state").join("works"))
}

pub fn work_state_path(slug: &str) -> Result<PathBuf, String> {
    Ok(works_state_dir()?.join(format!("{slug}.json")))
}

/// Load one work state by slug.
pub fn load_work(slug: &str) -> Result<WorkState, String> {
    let path = work_state_path(slug)?;
    if !path.is_file() {
        return Err(format!(
            "Work '{slug}' not found in central state ({})",
            path.display()
        ));
    }
    let raw = fs::read_to_string(&path)
        .map_err(|e| format!("Failed to read {}: {e}", path.display()))?;
    serde_json::from_str(&raw).map_err(|e| format!("Corrupt state {}: {e}", path.display()))
}

/// Save one work state atomically under its per-work file lock.
pub fn save_work(state: &mut WorkState) -> Result<PathBuf, String> {
    let dir = works_state_dir()?;
    let _lock = WorkLock::acquire(&dir, &state.slug)?;
    state.updated_at = WorkState::now_iso();
    let path = dir.join(format!("{}.json", state.slug));
    let bytes = serde_json::to_vec_pretty(state)
        .map_err(|e| format!("Failed to serialize work state: {e}"))?;
    atomic_write(&path, &bytes)?;
    Ok(path)
}

/// List all works tracked in the central state directory, sorted by slug.
pub fn list_works() -> Result<Vec<WorkState>, String> {
    let dir = works_state_dir()?;
    if !dir.is_dir() {
        return Ok(Vec::new());
    }
    let mut works = Vec::new();
    for entry in fs::read_dir(&dir).map_err(|e| format!("Failed to read {}: {e}", dir.display()))? {
        let entry = entry.map_err(|e| format!("Failed to read state entry: {e}"))?;
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
            if name.starts_with('.') {
                continue;
            }
        }
        let raw = fs::read_to_string(&path)
            .map_err(|e| format!("Failed to read {}: {e}", path.display()))?;
        let state: WorkState = serde_json::from_str(&raw)
            .map_err(|e| format!("Invalid state {}: {e}", path.display()))?;
        works.push(state);
    }
    works.sort_by(|a, b| a.slug.cmp(&b.slug));
    Ok(works)
}

/// Write a frozen snapshot of work state into the work/archive folder on completion.
pub fn write_frozen_snapshot(work_dir: &Path, state: &WorkState) -> Result<PathBuf, String> {
    let snapshot_path = work_dir.join("state.snapshot.json");
    let bytes = serde_json::to_vec_pretty(state)
        .map_err(|e| format!("Failed to serialize frozen snapshot: {e}"))?;
    atomic_write(&snapshot_path, &bytes)?;
    Ok(snapshot_path)
}
