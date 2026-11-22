use devbench::adapters::codex::scan_codex_rollouts;
use devbench::adapters::adapter_for;
use devbench::config::{AgentConfig, CaptureMode};
use devbench::lock::atomic_write;
use devbench::paths::{
    build_hc_name, check_path_length, normalize_for_compare, sanitize_identifier, sanitize_slug,
    validate_repo_alias,
};
use devbench::state::{
    list_works_with_warnings, mutate_work, save_work, write_frozen_snapshot, CaptureState,
    SessionStatus, SessionTier, WorkSession, WorkState, WorkStatus,
};
use std::collections::HashSet;
use std::fs;
use std::path::Path;
use std::sync::{Arc, Barrier};
use std::thread;
use std::time::{Duration, SystemTime};
use tempfile::TempDir;

#[test]
fn sanitize_slug_handles_separators_unicode_length_and_windows_reserved() {
    assert_eq!(sanitize_slug("Fix Auth Token!").unwrap(), "fix-auth-token");
    assert_eq!(sanitize_slug("feat/login").unwrap(), "feat-login");
    assert_eq!(sanitize_slug("  api.v2... ").unwrap(), "api-v2");
    assert_eq!(sanitize_slug("-x-").unwrap(), "x");
    assert_eq!(sanitize_slug("a--b").unwrap(), "a-b");
    assert_eq!(sanitize_slug("con-1").unwrap(), "con-1");

    // Windows reserved device names with and without extensions
    assert!(sanitize_slug("CON").is_err());
    assert!(sanitize_slug("CON.txt").is_err());
    assert!(sanitize_slug("COM1.log").is_err());
    assert!(sanitize_slug("nul").is_err());
    assert!(sanitize_slug("LPT9").is_err());

    // Non-ASCII / punctuation-only / length bounds
    assert!(sanitize_slug("ทดสอบ").is_err());
    assert!(sanitize_slug("!!!").is_err());
    assert!(sanitize_slug("   ").is_err());
    assert!(sanitize_slug(&"a".repeat(64)).is_ok());
    assert!(sanitize_slug(&"a".repeat(65)).is_err());

    // Domain error message uses "Worker domain", not "Work slug"
    let dom_err = sanitize_identifier("Worker domain", "!!!").unwrap_err();
    assert!(dom_err.contains("Worker domain"));

    // Repo alias safety
    assert!(validate_repo_alias("api").is_ok());
    assert!(validate_repo_alias("../evil").is_err());
    assert!(validate_repo_alias("a/b").is_err());
    assert!(validate_repo_alias("C:\\repo").is_err());
    assert!(validate_repo_alias("CON").is_err());
}

#[test]
fn hc_name_respects_32_char_limit_and_avoids_cross_work_collisions() {
    let long_slug = "a".repeat(64);
    let owner_name = build_hc_name("db", &long_slug, None);
    let worker_name = build_hc_name("db", &long_slug, Some("backend-api"));
    assert!(owner_name.len() <= 32, "owner_name too long: {owner_name}");
    assert!(worker_name.len() <= 32, "worker_name too long: {worker_name}");
    assert!(owner_name
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_'));

    // Cross-work hyphen collision check: (a-b, worker c) != (a, worker b-c)
    let n1 = build_hc_name("db", "a-b", Some("c"));
    let n2 = build_hc_name("db", "a", Some("b-c"));
    assert_ne!(n1, n2);

    // Owner vs worker name distinct even if same slug
    assert_ne!(
        build_hc_name("db", "demo", None),
        build_hc_name("db", "demo", Some("web"))
    );
}

#[test]
fn path_length_and_unc_normalization() {
    let long_path = format!("C:/work/{}", "a".repeat(210));
    assert!(check_path_length(Path::new(&long_path)).is_err());
    assert!(check_path_length(Path::new("C:/work/fix-auth")).is_ok());

    let p1 = normalize_for_compare(Path::new(r"\\?\C:\Users\demo\work\api\"));
    let p2 = normalize_for_compare(Path::new("c:/users/demo/work/api"));
    if cfg!(windows) {
        assert_eq!(p1, p2);
    }
}

#[test]
fn atomic_write_overwrites_safely_with_no_temp_leftovers() {
    let dir = TempDir::new().unwrap();
    let target = dir.path().join("demo.json");
    for i in 0..25 {
        let payload = format!("{{\"v\":{i}}}");
        atomic_write(&target, payload.as_bytes()).unwrap();
        assert_eq!(fs::read_to_string(&target).unwrap(), payload);
    }
    let leftover: Vec<_> = fs::read_dir(dir.path())
        .unwrap()
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().contains(".tmp."))
        .collect();
    assert!(leftover.is_empty());
}

#[test]
fn adapters_claude_preassign_and_codex_no_daemon_on_launch_and_resume() {
    let claude_cfg = AgentConfig {
        argv: vec!["claude".to_string()],
        resume_argv: Some(vec![
            "claude".to_string(),
            "--resume".to_string(),
            "{session_id}".to_string(),
        ]),
        capture: CaptureMode::Preassign,
    };
    let claude = adapter_for("claude");
    let plan = claude.prepare_launch(&claude_cfg);
    assert_eq!(plan.capture, CaptureState::Auto);
    let sid = plan.preassigned_session_id.expect("UUID expected");
    assert!(plan.argv.windows(2).any(|w| w[0] == "--session-id" && w[1] == sid));
    assert_eq!(
        claude.build_resume_argv(&claude_cfg, "id with space").unwrap(),
        vec!["claude", "--resume", "id with space"]
    );

    // Codex must include --no-daemon exactly once on launch AND on resume!
    let codex_cfg = AgentConfig {
        argv: vec!["codex".to_string(), "--no-daemon".to_string()],
        resume_argv: Some(vec![
            "codex".to_string(),
            "resume".to_string(),
            "{session_id}".to_string(),
        ]),
        capture: CaptureMode::Scan,
    };
    let codex = adapter_for("codex");
    let cplan = codex.prepare_launch(&codex_cfg);
    assert_eq!(cplan.argv.iter().filter(|a| *a == "--no-daemon").count(), 1);
    let cresume = codex.build_resume_argv(&codex_cfg, "sess-42").unwrap();
    assert!(cresume.contains(&"--no-daemon".to_string()));
    assert!(cresume.contains(&"sess-42".to_string()));

    // Missing resume_argv returns Err
    let agy_cfg = AgentConfig {
        argv: vec!["agy".to_string()],
        resume_argv: None,
        capture: CaptureMode::Manual,
    };
    assert!(adapter_for("agy").build_resume_argv(&agy_cfg, "x").is_err());

    // Generic Unavailable
    let gen_cfg = AgentConfig {
        argv: vec!["tool".to_string()],
        resume_argv: None,
        capture: CaptureMode::Unavailable,
    };
    assert_eq!(
        adapter_for("custom").prepare_launch(&gen_cfg).capture,
        CaptureState::Unavailable
    );
}

#[test]
fn codex_rollout_scan_filters_by_meta_timestamp_and_excludes_bound_ids() {
    let dir = TempDir::new().unwrap();
    let sub = dir.path().join("2026").join("11");
    fs::create_dir_all(&sub).unwrap();
    let cwd = dir.path().join("work").join("fix-auth").join("api");

    let now = chrono::Utc::now();
    let old_ts = (now - chrono::Duration::hours(2)).to_rfc3339();
    let new_ts = (now + chrono::Duration::seconds(2)).to_rfc3339();
    let launched_at = SystemTime::now() - Duration::from_secs(1);

    // 1. Old active rollout with fresh mtime -> MUST BE REJECTED because meta.timestamp < launched_at
    let old_file = sub.join("rollout-old.jsonl");
    fs::write(
        &old_file,
        format!(
            "{}\n",
            serde_json::json!({
                "type": "session_meta",
                "payload": { "id": "old-owner", "cwd": cwd.display().to_string(), "timestamp": old_ts }
            })
        ),
    )
    .unwrap();

    // 2. Non-session_meta line -> ignored
    let bad_file = sub.join("rollout-noise.jsonl");
    fs::write(
        &bad_file,
        format!(
            "{}\n",
            serde_json::json!({
                "type": "message",
                "payload": { "id": "wrong-type", "cwd": cwd.display().to_string(), "timestamp": new_ts }
            })
        ),
    )
    .unwrap();

    // 3. Valid fresh rollout -> matched
    let fresh_file = sub.join("rollout-fresh.jsonl");
    fs::write(
        &fresh_file,
        format!(
            "{}\n",
            serde_json::json!({
                "type": "session_meta",
                "payload": { "id": "codex-new-1", "cwd": cwd.display().to_string(), "timestamp": new_ts }
            })
        ),
    )
    .unwrap();

    let empty = HashSet::new();
    assert_eq!(
        scan_codex_rollouts(dir.path(), &cwd, launched_at, &empty).as_deref(),
        Some("codex-new-1")
    );

    // 4. Excluding already-bound ID returns None
    let mut bound = HashSet::new();
    bound.insert("codex-new-1".to_string());
    assert_eq!(scan_codex_rollouts(dir.path(), &cwd, launched_at, &bound), None);
}

#[test]
fn concurrent_mutate_work_preserves_all_sessions_and_tolerates_corrupt_sibling() {
    let tmp = TempDir::new().unwrap();
    std::env::set_var("DEVBENCH_STATE_DIR", tmp.path().join("state"));

    let mut initial = WorkState::new(
        "race-test".to_string(),
        "Race test".to_string(),
        tmp.path().display().to_string(),
    );
    initial.status = WorkStatus::Active;
    save_work(&mut initial).unwrap();

    // Write a corrupt sibling json file to verify list_works_with_warnings survives it
    fs::write(
        tmp.path().join("state").join("works").join("corrupt.json"),
        "{not valid json",
    )
    .unwrap();

    // Spawn 8 concurrent threads each mutating a distinct worker session under WorkLock
    let n = 8;
    let barrier = Arc::new(Barrier::new(n));
    let mut handles = Vec::new();
    for i in 0..n {
        let b = Arc::clone(&barrier);
        let cwd = tmp.path().display().to_string();
        handles.push(thread::spawn(move || {
            b.wait();
            mutate_work("race-test", |s| {
                s.upsert_session(WorkSession {
                    key: format!("worker-d{i}"),
                    tier: SessionTier::Worker,
                    domain: Some(format!("d{i}")),
                    harness: "claude".to_string(),
                    cwd,
                    hc_terminal_id: Some(format!("t{i}")),
                    hc_name: Some(format!("db-race-w-d{i}")),
                    herdr_tab_id: None,
                    herdr_pane_id: None,
                    native_session_id: Some(format!("uuid-{i}")),
                    capture: CaptureState::Auto,
                    status: SessionStatus::Running,
                    first_message_sent: true,
                    started_at: WorkState::now_iso(),
                    last_seen_at: WorkState::now_iso(),
                });
                Ok((true, ()))
            })
            .unwrap();
        }));
    }
    for h in handles {
        h.join().unwrap();
    }

    let (works, warnings) = list_works_with_warnings().unwrap();
    assert_eq!(works.len(), 1);
    assert_eq!(warnings.len(), 1);
    assert_eq!(works[0].sessions.len(), n);

    let snap = write_frozen_snapshot(tmp.path(), &works[0]).unwrap();
    assert!(snap.is_file());
}
