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
    #[serde(default)]
    pub track: Option<String>,
    pub source_path: String,
    pub worktree_path: String,
    pub branch: String,
    pub base_ref: String,
    pub base_sha: String,
    pub status: RepoStatus,
    #[serde(default)]
    pub final_head_sha: Option<String>,
}

impl WorkRepo {
    pub fn display_label(&self) -> String {
        match self.track.as_deref() {
            Some(t) if !t.is_empty() => format!("{}:{t}", self.alias),
            _ => self.alias.clone(),
        }
    }
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
    #[serde(default)]
    pub herdr_space_initial_terminal: Option<String>,
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
            herdr_space_initial_terminal: None,
            work_dir,
            archive_path: None,
            repos: Vec::new(),
            sessions: Vec::new(),
            created_at: now.clone(),
            updated_at: now,
        }
    }

    /// Upsert a session while preserving an existing `native_session_id` if the update has `None`.
    pub fn upsert_session(&mut self, mut session: WorkSession) {
        if let Some(existing) = self.sessions.iter_mut().find(|s| s.key == session.key) {
            if session.native_session_id.is_none() && existing.native_session_id.is_some() {
                session.native_session_id = existing.native_session_id.clone();
                session.capture = existing.capture.clone();
            }
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

fn load_work_unlocked(path: &Path, slug: &str) -> Result<WorkState, String> {
    if !path.is_file() {
        return Err(format!(
            "Work '{slug}' not found in central state ({})",
            path.display()
        ));
    }
    let raw = fs::read_to_string(path)
        .map_err(|e| format!("Failed to read {}: {e}", path.display()))?;
    serde_json::from_str(&raw).map_err(|e| format!("Corrupt state {}: {e}", path.display()))
}

fn save_work_unlocked(path: &Path, state: &mut WorkState) -> Result<(), String> {
    state.updated_at = WorkState::now_iso();
    let bytes = serde_json::to_vec_pretty(state)
        .map_err(|e| format!("Failed to serialize work state: {e}"))?;
    atomic_write(path, &bytes)
}

/// Load one work state by slug (acquires per-work lock for consistent snapshot).
pub fn load_work(slug: &str) -> Result<WorkState, String> {
    let dir = works_state_dir()?;
    let _lock = WorkLock::acquire(&dir, slug)?;
    let path = dir.join(format!("{slug}.json"));
    load_work_unlocked(&path, slug)
}

/// Save one work state atomically under its per-work file lock.
pub fn save_work(state: &mut WorkState) -> Result<PathBuf, String> {
    let dir = works_state_dir()?;
    let _lock = WorkLock::acquire(&dir, &state.slug)?;
    let path = dir.join(format!("{}.json", state.slug));
    save_work_unlocked(&path, state)?;
    Ok(path)
}

/// Execute a locked read-modify-write transaction on a single work's central state file.
/// Prevents lost updates when concurrent `spawn`, `bind`, `status`, or `archive` commands run.
pub fn mutate_work<F, T>(slug: &str, f: F) -> Result<(WorkState, T), String>
where
    F: FnOnce(&mut WorkState) -> Result<(bool, T), String>,
{
    let dir = works_state_dir()?;
    let _lock = WorkLock::acquire(&dir, slug)?;
    let path = dir.join(format!("{slug}.json"));
    let mut state = load_work_unlocked(&path, slug)?;
    let (dirty, val) = f(&mut state)?;
    if dirty {
        save_work_unlocked(&path, &mut state)?;
    }
    Ok((state, val))
}

/// List all valid works tracked in the central state directory, sorted by slug.
/// Corrupt JSON files are collected as warnings rather than aborting inspection.
pub fn list_works_with_warnings() -> Result<(Vec<WorkState>, Vec<String>), String> {
    let dir = works_state_dir()?;
    if !dir.is_dir() {
        return Ok((Vec::new(), Vec::new()));
    }
    let mut works = Vec::new();
    let mut warnings = Vec::new();
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
        match fs::read_to_string(&path)
            .map_err(|e| format!("read error: {e}"))
            .and_then(|raw| {
                serde_json::from_str::<WorkState>(&raw).map_err(|e| format!("parse error: {e}"))
            }) {
            Ok(state) => works.push(state),
            Err(reason) => warnings.push(format!(
                "Skipping corrupt state file {}: {reason}",
                path.display()
            )),
        }
    }
    works.sort_by(|a, b| a.slug.cmp(&b.slug));
    Ok((works, warnings))
}

pub fn list_works() -> Result<Vec<WorkState>, String> {
    let (works, _) = list_works_with_warnings()?;
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
