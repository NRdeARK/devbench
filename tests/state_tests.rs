use devbench::adapters::codex::scan_codex_rollouts;
use devbench::lock::atomic_write;
use devbench::paths::{check_path_length, sanitize_slug};
use devbench::state::{
    write_frozen_snapshot, CaptureState, SessionStatus, SessionTier, WorkSession, WorkState,
    WorkStatus,
};
use std::fs;
use std::path::Path;
use std::time::{Duration, SystemTime};
use tempfile::TempDir;

#[test]
fn sanitize_slug_blocks_windows_reserved_and_cleans_chars() {
    assert_eq!(sanitize_slug("Fix Auth Token!").unwrap(), "fix-auth-token");
    assert_eq!(sanitize_slug("  api.v2... ").unwrap(), "api-v2");
    assert!(sanitize_slug("CON").is_err());
    assert!(sanitize_slug("nul").is_err());
    assert!(sanitize_slug("COM1").is_err());
    assert!(sanitize_slug("   ").is_err());
}

#[test]
fn path_length_guard_rejects_excessive_paths() {
    let long_path = format!("C:/work/{}", "a".repeat(210));
    assert!(check_path_length(Path::new(&long_path)).is_err());
    assert!(check_path_length(Path::new("C:/work/fix-auth")).is_ok());
}

#[test]
fn atomic_write_overwrites_safely() {
    let dir = TempDir::new().unwrap();
    let target = dir.path().join("demo.json");
    atomic_write(&target, b"{\"v\":1}").unwrap();
    assert_eq!(fs::read_to_string(&target).unwrap(), "{\"v\":1}");
    atomic_write(&target, b"{\"v\":2}").unwrap();
    assert_eq!(fs::read_to_string(&target).unwrap(), "{\"v\":2}");
}

#[test]
fn frozen_snapshot_and_session_upsert_work() {
    let dir = TempDir::new().unwrap();
    let mut state = WorkState::new(
        "fix-auth".to_string(),
        "Fix auth".to_string(),
        dir.path().display().to_string(),
    );
    state.status = WorkStatus::Active;
    state.upsert_session(WorkSession {
        key: "owner".to_string(),
        tier: SessionTier::Owner,
        domain: None,
        harness: "claude".to_string(),
        cwd: dir.path().display().to_string(),
        hc_terminal_id: Some("t7".to_string()),
        hc_name: Some("db-fix-auth-owner".to_string()),
        herdr_tab_id: None,
        herdr_pane_id: None,
        native_session_id: Some("uuid-123".to_string()),
        capture: CaptureState::Auto,
        status: SessionStatus::Running,
        first_message_sent: true,
        started_at: WorkState::now_iso(),
        last_seen_at: WorkState::now_iso(),
    });
    assert_eq!(state.sessions.len(), 1);
    let snap = write_frozen_snapshot(dir.path(), &state).unwrap();
    assert!(snap.is_file());
    let parsed: WorkState = serde_json::from_str(&fs::read_to_string(snap).unwrap()).unwrap();
    assert_eq!(parsed.slug, "fix-auth");
    assert_eq!(parsed.sessions[0].native_session_id.as_deref(), Some("uuid-123"));
}

#[test]
fn codex_rollout_scan_matches_cwd() {
    let dir = TempDir::new().unwrap();
    let sub = dir.path().join("2026").join("11");
    fs::create_dir_all(&sub).unwrap();
    let cwd = dir.path().join("work").join("fix-auth").join("api");
    let rollout = sub.join("rollout-2026-11-22-abc.jsonl");
    let line = serde_json::json!({
        "type": "session_meta",
        "payload": {
            "id": "codex-sess-999",
            "cwd": cwd.display().to_string()
        }
    });
    let launched_at = SystemTime::now() - Duration::from_secs(5);
    fs::write(&rollout, format!("{line}\n")).unwrap();

    let found = scan_codex_rollouts(dir.path(), &cwd, launched_at);
    assert_eq!(found.as_deref(), Some("codex-sess-999"));
}
