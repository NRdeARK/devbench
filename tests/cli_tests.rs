use std::fs;
use std::process::Command;
use tempfile::TempDir;

#[test]
fn cli_init_precedence_and_config_check_validation() {
    let tmp = TempDir::new().unwrap();
    let cfg_path = tmp.path().join("custom-config.json");
    let bin = env!("CARGO_BIN_EXE_devbench");

    // 1. First init creates config from embedded template
    let out1 = Command::new(bin)
        .arg("init")
        .env("DEVBENCH_CONFIG", &cfg_path)
        .output()
        .unwrap();
    assert!(out1.status.success(), "stderr: {}", String::from_utf8_lossy(&out1.stderr));
    assert!(cfg_path.is_file());
    assert!(String::from_utf8_lossy(&out1.stdout).contains("\"created\""));

    // 2. Customize config with valid temp git repo + missing optional archive key to verify hints
    let sample_repo = tmp.path().join("repo-a");
    fs::create_dir_all(sample_repo.join(".git")).unwrap();
    let custom_cfg = serde_json::json!({
        "schema_version": 1,
        "work_root": tmp.path().join("work").display().to_string(),
        "archive_root": tmp.path().join("archive").display().to_string(),
        "branch_prefix": "devbench/",
        "default_agent": "claude",
        "repos": {
            "a": { "path": sample_repo.display().to_string(), "base_ref": "main" }
        },
        "agents": {
            "claude": { "argv": ["claude"], "resume_argv": ["claude", "--resume", "{session_id}"], "capture": "preassign" },
            "codex": { "argv": ["codex"], "resume_argv": ["codex", "resume", "{session_id}"], "capture": "scan" }
        }
    });
    let saved_bytes = serde_json::to_string_pretty(&custom_cfg).unwrap();
    fs::write(&cfg_path, &saved_bytes).unwrap();

    // 3. Re-running init NEVER overwrites existing config (bytes identical)
    let out2 = Command::new(bin)
        .arg("init")
        .env("DEVBENCH_CONFIG", &cfg_path)
        .output()
        .unwrap();
    assert!(out2.status.success());
    assert!(String::from_utf8_lossy(&out2.stdout).contains("\"existing_untouched\""));
    assert_eq!(fs::read_to_string(&cfg_path).unwrap(), saved_bytes);

    // 4. `config check` succeeds, warns on codex missing --no-daemon, and includes missing key hints
    let out3 = Command::new(bin)
        .args(["config", "check"])
        .env("DEVBENCH_CONFIG", &cfg_path)
        .output()
        .unwrap();
    assert!(out3.status.success(), "stderr: {}", String::from_utf8_lossy(&out3.stderr));
    let report3 = String::from_utf8_lossy(&out3.stdout);
    assert!(report3.contains("\"ok\": true"));
    assert!(report3.contains("--no-daemon"));
    assert!(report3.contains("missing_key_hints"));
    assert!(report3.contains("\"herdr\""));

    // 5. Invalid name_prefix and future schema_version fail config check
    let bad_cfg = serde_json::json!({
        "schema_version": 99,
        "work_root": tmp.path().join("work").display().to_string(),
        "archive_root": tmp.path().join("archive").display().to_string(),
        "default_agent": "missing",
        "herdr": { "name_prefix": "UPPER!" },
        "repos": {},
        "agents": {}
    });
    let bad_path = tmp.path().join("bad.json");
    fs::write(&bad_path, serde_json::to_string_pretty(&bad_cfg).unwrap()).unwrap();
    let out_bad = Command::new(bin)
        .args(["config", "check"])
        .env("DEVBENCH_CONFIG", &bad_path)
        .output()
        .unwrap();
    assert!(!out_bad.status.success());
    let stderr_bad = String::from_utf8_lossy(&out_bad.stderr);
    assert!(stderr_bad.contains("schema_version 99"));
    assert!(stderr_bad.contains("UPPER!"));
    assert!(stderr_bad.contains("default_agent"));

    // 6. Malformed JSON reports line and column
    let malformed = tmp.path().join("broken.json");
    fs::write(&malformed, "{\n  \"schema_version\": [bad\n}").unwrap();
    let out_mal = Command::new(bin)
        .args(["config", "check"])
        .env("DEVBENCH_CONFIG", &malformed)
        .output()
        .unwrap();
    assert!(!out_mal.status.success());
    assert!(String::from_utf8_lossy(&out_mal.stderr).contains("Malformed JSON at line"));

    // 7. Missing explicit DEVBENCH_CONFIG does NOT fall back to external
    let out_missing = Command::new(bin)
        .args(["config", "check"])
        .env("DEVBENCH_CONFIG", tmp.path().join("does-not-exist.json"))
        .output()
        .unwrap();
    assert!(!out_missing.status.success());
    assert!(String::from_utf8_lossy(&out_missing.stderr).contains("env:DEVBENCH_CONFIG"));

    // 8. `status --offline` returns clean empty works list against isolated state dir
    let out4 = Command::new(bin)
        .args(["status", "--offline"])
        .env("DEVBENCH_CONFIG", &cfg_path)
        .env("DEVBENCH_STATE_DIR", tmp.path().join("state"))
        .output()
        .unwrap();
    assert!(out4.status.success());
    assert!(String::from_utf8_lossy(&out4.stdout).contains("\"count\": 0"));
}
