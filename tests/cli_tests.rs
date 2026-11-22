use std::fs;
use std::process::Command;
use tempfile::TempDir;

#[test]
fn cli_init_and_config_check_never_overwrite_existing() {
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
    let stdout1 = String::from_utf8_lossy(&out1.stdout);
    assert!(stdout1.contains("\"created\""));

    // 2. Customize config with valid temp git repo so config check passes
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
            "claude": { "argv": ["claude"], "resume_argv": ["claude", "--resume", "{session_id}"], "capture": "preassign" }
        }
    });
    fs::write(&cfg_path, serde_json::to_string_pretty(&custom_cfg).unwrap()).unwrap();

    // 3. Re-running init NEVER overwrites existing config
    let out2 = Command::new(bin)
        .arg("init")
        .env("DEVBENCH_CONFIG", &cfg_path)
        .output()
        .unwrap();
    assert!(out2.status.success());
    let stdout2 = String::from_utf8_lossy(&out2.stdout);
    assert!(stdout2.contains("\"existing_untouched\""));
    assert!(fs::read_to_string(&cfg_path).unwrap().contains("repo-a"));

    // 4. `devbench config check` succeeds with ok: true
    let out3 = Command::new(bin)
        .args(["config", "check"])
        .env("DEVBENCH_CONFIG", &cfg_path)
        .output()
        .unwrap();
    assert!(out3.status.success(), "stderr: {}", String::from_utf8_lossy(&out3.stderr));
    assert!(String::from_utf8_lossy(&out3.stdout).contains("\"ok\": true"));

    // 5. `devbench status --offline` returns clean empty works list against isolated state dir
    let out4 = Command::new(bin)
        .args(["status", "--offline"])
        .env("DEVBENCH_CONFIG", &cfg_path)
        .env("DEVBENCH_STATE_DIR", tmp.path().join("state"))
        .output()
        .unwrap();
    assert!(out4.status.success());
    assert!(String::from_utf8_lossy(&out4.stdout).contains("\"count\": 0"));
}
