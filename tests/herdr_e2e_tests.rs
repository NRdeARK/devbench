#![cfg(feature = "test-shims")]

use serde_json::Value;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use tempfile::TempDir;

struct TestEnv {
    tmp: TempDir,
    cfg_path: PathBuf,
    state_dir: PathBuf,
    log_path: PathBuf,
    fake_state: PathBuf,
    script_path: PathBuf,
}

impl TestEnv {
    fn new() -> Self {
        let tmp = TempDir::new().unwrap();
        let repo_api = tmp.path().join("repos").join("api");
        let repo_web = tmp.path().join("repos").join("web");
        init_git_repo(&repo_api);
        init_git_repo(&repo_web);

        let shim = env!("CARGO_BIN_EXE_fake_herdr");
        let cfg_path = tmp.path().join("config.json");
        let state_dir = tmp.path().join("state");
        let log_path = tmp.path().join("herdr.log.jsonl");
        let fake_state = tmp.path().join("herdr.state.json");
        let script_path = tmp.path().join("herdr.script.json");

        let cfg = serde_json::json!({
            "schema_version": 1,
            "work_root": tmp.path().join("work").display().to_string(),
            "archive_root": tmp.path().join("archive").display().to_string(),
            "branch_prefix": "devbench/",
            "default_agent": "claude",
            "repos": {
                "api": { "path": repo_api.display().to_string(), "base_ref": "main" },
                "web": { "path": repo_web.display().to_string(), "base_ref": "main" }
            },
            "agents": {
                "claude": {
                    "argv": ["claude"],
                    "resume_argv": ["claude", "--resume", "{session_id}"],
                    "capture": "preassign"
                },
                "codex": {
                    "argv": ["codex", "--no-daemon"],
                    "resume_argv": ["codex", "resume", "{session_id}"],
                    "capture": "scan"
                },
                "agy": {
                    "argv": ["agy"],
                    "resume_argv": null,
                    "capture": "manual"
                }
            },
            "herdr": {
                "name_prefix": "db",
                "focus_on_create": false,
                "worker_layout": "tab",
                "hc_bin": shim,
                "app_bin": shim
            },
            "archive": {
                "on_dirty": "block",
                "warn_unpushed": true,
                "snapshot_wip": false
            }
        });
        fs::write(&cfg_path, serde_json::to_string_pretty(&cfg).unwrap()).unwrap();
        fs::write(&script_path, "{}").unwrap();

        Self {
            tmp,
            cfg_path,
            state_dir,
            log_path,
            fake_state,
            script_path,
        }
    }

    fn set_script(&self, val: Value) {
        fs::write(&self.script_path, serde_json::to_string_pretty(&val).unwrap()).unwrap();
    }

    fn clear_log(&self) {
        let _ = fs::remove_file(&self.log_path);
    }

    fn read_log(&self) -> Vec<Vec<String>> {
        let Ok(raw) = fs::read_to_string(&self.log_path) else {
            return Vec::new();
        };
        raw.lines()
            .filter_map(|l| serde_json::from_str::<Value>(l).ok())
            .filter_map(|v| {
                v.get("argv")
                    .and_then(|a| a.as_array())
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|s| s.as_str().map(|x| x.to_string()))
                            .collect()
                    })
            })
            .collect()
    }

    /// Tab names of one fake space in order, e.g. `["owner", "worker:web"]`.
    fn tab_names(&self, space: &str) -> Vec<String> {
        let v: Value = serde_json::from_str(&fs::read_to_string(&self.fake_state).unwrap()).unwrap();
        v["spaces"][space]["tabs"]
            .as_array()
            .map(|tabs| tabs.iter().map(|t| t["name"].as_str().unwrap().to_string()).collect())
            .unwrap_or_default()
    }

    /// All terminal IDs currently tracked in fake `hc` state.
    fn fake_terminal_ids(&self) -> Vec<String> {
        let v: Value = serde_json::from_str(&fs::read_to_string(&self.fake_state).unwrap()).unwrap();
        v["terminals"]
            .as_object()
            .map(|m| {
                m.values()
                    .filter_map(|t| t["id"].as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Read persisted central work state JSON.
    fn work_state(&self, slug: &str) -> Value {
        let path = self.state_dir.join("works").join(format!("{slug}.json"));
        serde_json::from_str(&fs::read_to_string(path).unwrap()).unwrap()
    }

    fn run(&self, args: &[&str], extra_env: &[(&str, &str)]) -> std::process::Output {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_devbench"));
        cmd.args(args)
            .env("DEVBENCH_HOME", self.tmp.path())
            .env("DEVBENCH_CONFIG", &self.cfg_path)
            .env("DEVBENCH_STATE_DIR", &self.state_dir)
            .env("FAKE_HERDR_LOG", &self.log_path)
            .env("FAKE_HERDR_STATE", &self.fake_state)
            .env("FAKE_HERDR_SCRIPT", &self.script_path)
            .env_remove("HC_TERMINAL_ID");
        for (k, v) in extra_env {
            cmd.env(k, v);
        }
        cmd.output().unwrap()
    }
}

fn init_git_repo(dir: &Path) {
    fs::create_dir_all(dir).unwrap();
    for args in [
        &["init", "-b", "main"][..],
        &["config", "user.name", "Test"][..],
        &["config", "user.email", "test@example.com"][..],
        &["config", "commit.gpgsign", "false"][..],
    ] {
        let ok = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .status()
            .unwrap()
            .success();
        assert!(ok);
    }
    fs::write(dir.join("README.md"), "# repo\n").unwrap();
    Command::new("git").arg("-C").arg(dir).args(["add", "."]).status().unwrap();
    Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["commit", "-m", "init"])
        .status()
        .unwrap();
}

#[test]
fn e2e_spawn_reuse_blocked_status_bind_resume_and_archive() {
    let env = TestEnv::new();

    // 1. Setup work with a long 50-char slug to verify hc --name stays <=32 chars!
    let slug = "very-long-work-slug-for-testing-hc-name-length-50c";
    let new_out = env.run(&["new", slug, "--repos", "api,web", "--title", "Long slug test"], &[]);
    assert!(new_out.status.success(), "{}", String::from_utf8_lossy(&new_out.stderr));

    // 2. Reject worker without --domain and worker with --domain owner
    assert!(!env.run(&["spawn", slug, "--role", "worker"], &[]).status.success());
    assert!(!env.run(&["spawn", slug, "--role", "worker", "--domain", "owner"], &[]).status.success());

    // 3. Spawn Owner (Claude preassign)
    env.clear_log();
    let sp1 = env.run(
        &["spawn", slug, "--role", "owner", "--agent", "claude", "--cwd-repo", "api", "--task", "Own task"],
        &[],
    );
    assert!(sp1.status.success(), "stderr: {}", String::from_utf8_lossy(&sp1.stderr));
    let sp1_json: Value = serde_json::from_slice(&sp1.stdout).unwrap();
    assert_eq!(sp1_json["reused"], false);
    assert_eq!(sp1_json["session"]["capture"], "auto");
    let owner_tid = sp1_json["session"]["hc_terminal_id"].as_str().unwrap().to_string();
    let owner_native_id = sp1_json["session"]["native_session_id"].as_str().unwrap().to_string();
    assert!(!owner_native_id.is_empty());

    // Verify shim log: space.new with tab="shell", hc run --bg --outlive-parent --label herdr.space=s1 ... claude --session-id <uuid>, then tab.rename to "owner"
    let log1 = env.read_log();
    assert!(log1
        .iter()
        .any(|a| a.len() >= 3 && a[0] == "do" && a[1] == "space.new" && a[2].contains("\"tab\":\"shell\"")));
    let run_call = log1.iter().find(|a| a.first().map(|s| s.as_str()) == Some("run")).unwrap();
    assert!(run_call.contains(&"--bg".to_string()));
    assert!(run_call.contains(&"--outlive-parent".to_string()));
    assert!(run_call.contains(&"herdr.space=s1".to_string()));
    assert!(run_call.contains(&"--session-id".to_string()));
    assert!(run_call.contains(&owner_native_id));
    assert!(log1
        .iter()
        .any(|a| a.len() >= 3 && a[0] == "do" && a[1] == "tab.rename" && a[2].contains("\"name\":\"owner\"")));
    assert!(sp1_json["session"]["herdr_tab_id"].as_str().is_some());
    assert!(sp1_json["session"]["herdr_pane_id"].as_str().is_some());
    assert!(owner_tid.starts_with('t'));

    // 4. Second spawn of same role reuses live terminal idempotently (no new `run`)
    env.clear_log();
    let sp_dup = env.run(&["spawn", slug, "--role", "owner"], &[]);
    assert!(sp_dup.status.success());
    let sp_dup_json: Value = serde_json::from_slice(&sp_dup.stdout).unwrap();
    assert_eq!(sp_dup_json["reused"], true);
    let log_dup = env.read_log();
    assert!(!log_dup.iter().any(|a| a.first().map(|s| s.as_str()) == Some("run")));
    assert!(log_dup.iter().any(|a| a.len() >= 2 && a[0] == "do" && a[1] == "agent.focus"));

    // 5. Spawn Codex worker with trust dialog blocked (`first_message_sent: false, reason: "blocked"`)
    env.set_script(serde_json::json!({
        "first_message_sent": false,
        "first_message_reason": "blocked",
        "read_output": "Allow workspace trust? [y/n]"
    }));
    let sp_w = env.run(
        &["spawn", slug, "--role", "worker", "--domain", "web", "--agent", "codex", "--cwd-repo", "web", "--task", "Fix UI"],
        &[],
    );
    assert!(sp_w.status.success(), "stderr: {}", String::from_utf8_lossy(&sp_w.stderr));
    let sp_w_json: Value = serde_json::from_slice(&sp_w.stdout).unwrap();
    assert_eq!(sp_w_json["session"]["status"], "blocked");
    assert!(sp_w_json["screen_preview"].as_str().unwrap().contains("Allow workspace trust"));
    env.set_script(serde_json::json!({}));

    // 6. Bind empty ID rejected; valid ID binds with capture=manual
    assert!(!env.run(&["bind", slug, "--key", "worker-web", "--session-id", "   "], &[]).status.success());
    let bind_ok = env.run(&["bind", slug, "--key", "worker-web", "--session-id", "codex-42"], &[]);
    assert!(bind_ok.status.success());

    // 7. Status --offline makes ZERO shim calls
    env.clear_log();
    let st_off = env.run(&["status", slug, "--offline"], &[]);
    assert!(st_off.status.success());
    assert!(env.read_log().is_empty());

    // 8. Resume while live reattaches without calling `run`
    env.clear_log();
    let res_live = env.run(&["resume", slug, "--key", "owner"], &[]);
    assert!(res_live.status.success());
    assert_eq!(serde_json::from_slice::<Value>(&res_live.stdout).unwrap()["action"], "reattached");
    assert!(!env.read_log().iter().any(|a| a.first().map(|s| s.as_str()) == Some("run")));

    // 9. Resume aborts on daemon error (exit 5) without duplicating
    env.set_script(serde_json::json!({ "hc_get_exit": 5 }));
    env.clear_log();
    let res_err = env.run(&["resume", slug, "--key", "owner"], &[]);
    assert!(!res_err.status.success());
    assert!(!env.read_log().iter().any(|a| a.first().map(|s| s.as_str()) == Some("run")));

    // 10. Resume Codex after terminal lost (exit 3) relaunches with `--no-daemon` in saved cwd!
    env.set_script(serde_json::json!({}));
    // Delete worker terminal from fake state so `hc get` returns exit 3
    let mut fs_val: Value = serde_json::from_str(&fs::read_to_string(&env.fake_state).unwrap()).unwrap();
    let worker_hc_name = sp_w_json["session"]["hc_name"].as_str().unwrap().to_string();
    fs_val["terminals"].as_object_mut().unwrap().remove(&worker_hc_name);
    fs::write(&env.fake_state, serde_json::to_string_pretty(&fs_val).unwrap()).unwrap();

    env.clear_log();
    let res_codex = env.run(&["resume", slug, "--key", "worker-web"], &[]);
    assert!(res_codex.status.success(), "stderr: {}", String::from_utf8_lossy(&res_codex.stderr));
    let log_res = env.read_log();
    let codex_run = log_res.iter().find(|a| a.first().map(|s| s.as_str()) == Some("run")).unwrap();
    assert!(codex_run.contains(&"--no-daemon".to_string()));
    assert!(codex_run.contains(&"codex-42".to_string()));

    // 11. Self-archive guard blocks when HC_TERMINAL_ID matches owner (zero shim calls)
    env.clear_log();
    let self_blk = env.run(&["archive", slug], &[("HC_TERMINAL_ID", &owner_tid)]);
    assert!(!self_blk.status.success());
    assert!(env.read_log().is_empty());

    // 12. Archive succeeds: calls `space.close {"id":"s1","confirmed":true}` -> `kill`/`wait`/`rm`
    env.clear_log();
    let arch = env.run(&["archive", slug], &[]);
    assert!(arch.status.success(), "stderr: {}", String::from_utf8_lossy(&arch.stderr));
    let log_arch = env.read_log();
    let close_idx = log_arch
        .iter()
        .position(|a| a.len() >= 3 && a[0] == "do" && a[1] == "space.close" && a[2].contains("\"confirmed\":true"))
        .expect("space.close with {id, confirmed: true} must be called");
    let kill_idx = log_arch
        .iter()
        .position(|a| a.first().map(|s| s.as_str()) == Some("kill"))
        .expect("hc kill must be called");
    assert!(close_idx < kill_idx);
}

#[test]
fn e2e_spawn_failure_rolls_back_newly_created_space() {
    let env = TestEnv::new();
    let new_out = env.run(&["new", "rollback-space", "--repos", "api"], &[]);
    assert!(new_out.status.success());

    env.set_script(serde_json::json!({ "fail_hc_run": true }));
    env.clear_log();
    let sp = env.run(&["spawn", "rollback-space", "--role", "owner"], &[]);
    assert!(!sp.status.success());

    // Verify space.close {id, confirmed: true} cleaned up the orphan space!
    let log = env.read_log();
    assert!(log.iter().any(|a| a.len() >= 2 && a[0] == "do" && a[1] == "space.new"));
    assert!(log
        .iter()
        .any(|a| a.len() >= 3 && a[0] == "do" && a[1] == "space.close" && a[2].contains("\"id\":\"s1\"")));
}

#[test]
fn e2e_initiative_multi_track_spawn_tab_labels_status_done_and_archive() {
    let env = TestEnv::new();

    // 1. Create single-repo initiative work
    let new_out = env.run(&["new", "ga-v02", "--repos", "api", "--title", "Initiative v0.2"], &[]);
    assert!(new_out.status.success(), "{}", String::from_utf8_lossy(&new_out.stderr));

    // 2. Spawn Owner on default track (with status "done" to verify object status decoding!)
    env.set_script(serde_json::json!({ "run_status": "done" }));
    let sp_owner = env.run(&["spawn", "ga-v02", "--role", "owner", "--cwd-repo", "api"], &[]);
    assert!(sp_owner.status.success(), "{}", String::from_utf8_lossy(&sp_owner.stderr));
    env.set_script(serde_json::json!({}));

    // 3. Reconcile status: live object status {"state": "done"} maps to SessionStatus::Idle
    let st_out = env.run(&["status", "ga-v02"], &[]);
    assert!(st_out.status.success());
    let st_json: Value = serde_json::from_slice(&st_out.stdout).unwrap();
    assert_eq!(st_json["works"][0]["sessions"][0]["status"], "idle");

    // 4. Spawn parallel track worker (--track modes, auto-infers single repo 'api' + domain 'modes')
    env.clear_log();
    let sp_track = env.run(
        &["spawn", "ga-v02", "--role", "worker", "--track", "modes", "--task", "Build modes"],
        &[],
    );
    assert!(sp_track.status.success(), "stderr: {}", String::from_utf8_lossy(&sp_track.stderr));
    let sp_track_json: Value = serde_json::from_slice(&sp_track.stdout).unwrap();
    assert_eq!(sp_track_json["session"]["key"], "worker-modes");
    assert!(sp_track_json["session"]["cwd"].as_str().unwrap().ends_with("api--modes"));
    assert!(sp_track_json["session"]["herdr_tab_id"].as_str().is_some());

    // Must reuse existing space (NO second space.new) and rename tab to "worker:modes"
    let log_t = env.read_log();
    assert!(!log_t.iter().any(|a| a.len() >= 2 && a[0] == "do" && a[1] == "space.new"));
    assert!(log_t
        .iter()
        .any(|a| a.len() >= 3 && a[0] == "do" && a[1] == "tab.rename" && a[2].contains("\"name\":\"worker:modes\"")));

    // 5. Explicitly attach a second repo (`web`) to the active initiative via `add-track`
    let add_web = env.run(&["add-track", "ga-v02", "--repo", "web"], &[]);
    assert!(add_web.status.success(), "stderr: {}", String::from_utf8_lossy(&add_web.stderr));

    // 6. Archive detaches ALL 3 worktrees (api, api--modes, web) and preserves ALL 3 branches
    let arch = env.run(&["archive", "ga-v02"], &[]);
    assert!(arch.status.success(), "stderr: {}", String::from_utf8_lossy(&arch.stderr));
    let arch_json: Value = serde_json::from_slice(&arch.stdout).unwrap();
    let kept: Vec<&str> = arch_json["kept_branches"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|v| v.as_str())
        .collect();
    assert_eq!(kept.len(), 3);
    assert!(kept.iter().any(|b| b.contains("devbench/ga-v02@")));
    assert!(kept.iter().any(|b| b.contains("api:modes:devbench/ga-v02--modes@")));
    assert!(kept.iter().any(|b| b.contains("web:devbench/ga-v02@")));
}

#[test]
fn e2e_spawn_and_resume_leave_only_agent_tabs() {
    let env = TestEnv::new();
    assert!(env.run(&["new", "one-tab", "--repos", "api"], &[]).status.success());

    // Spawn owner: the blank "shell" tab is closed, only the owner tab and terminal remain,
    // and persisted state forgets `herdr_space_initial_terminal`.
    env.clear_log();
    let sp = env.run(&["spawn", "one-tab", "--role", "owner"], &[]);
    assert!(sp.status.success(), "{}", String::from_utf8_lossy(&sp.stderr));
    let sp_json: Value = serde_json::from_slice(&sp.stdout).unwrap();
    let owner_tid = sp_json["session"]["hc_terminal_id"].as_str().unwrap().to_string();
    assert_eq!(env.tab_names("s1"), vec!["owner"]);
    assert_eq!(env.fake_terminal_ids(), vec![owner_tid.clone()]);
    assert!(env.work_state("one-tab")["herdr_space_initial_terminal"].is_null());
    let log = env.read_log();
    let close_idx = log
        .iter()
        .position(|a| a.len() >= 3 && a[1] == "tab.close" && a[2].contains("\"confirmed\":true"))
        .expect("tab.close must run");
    let run_idx = log.iter().position(|a| a[0] == "run").unwrap();
    assert!(run_idx < close_idx, "agent must be launched before the blank tab is closed");

    // A second terminal reuses the existing space and must not close anything.
    env.clear_log();
    let sp_w = env.run(&["spawn", "one-tab", "--role", "worker", "--domain", "web"], &[]);
    assert!(sp_w.status.success(), "{}", String::from_utf8_lossy(&sp_w.stderr));
    assert_eq!(env.tab_names("s1"), vec!["owner", "worker:web"]);
    assert!(!env.read_log().iter().any(|a| a.len() >= 2 && a[1] == "tab.close"));

    // Resume after the whole space and terminals vanished: recreated space ends with one tab
    // and only the resumed agent terminal record.
    let mut fs_val: Value = serde_json::from_str(&fs::read_to_string(&env.fake_state).unwrap()).unwrap();
    fs_val["spaces"] = serde_json::json!({});
    fs_val["terminals"] = serde_json::json!({});
    fs::write(&env.fake_state, serde_json::to_string_pretty(&fs_val).unwrap()).unwrap();
    let res = env.run(&["resume", "one-tab", "--key", "owner"], &[]);
    assert!(res.status.success(), "{}", String::from_utf8_lossy(&res.stderr));
    let res_json: Value = serde_json::from_slice(&res.stdout).unwrap();
    let resumed_tid = res_json["hc_terminal_id"].as_str().unwrap().to_string();
    assert_eq!(env.tab_names("s2"), vec!["owner"]);
    assert_eq!(env.fake_terminal_ids(), vec![resumed_tid]);
    assert!(env.work_state("one-tab")["herdr_space_initial_terminal"].is_null());

    // Archive after retirement still succeeds (initial terminal no longer tracked).
    assert!(env.run(&["archive", "one-tab"], &[]).status.success());
    assert!(env.fake_terminal_ids().is_empty());
}

#[test]
fn e2e_tab_close_failure_does_not_fail_spawn() {
    let env = TestEnv::new();
    assert!(env.run(&["new", "close-fails", "--repos", "api"], &[]).status.success());
    env.set_script(serde_json::json!({ "fail_tab_close": true }));
    let sp = env.run(&["spawn", "close-fails", "--role", "owner"], &[]);
    assert!(sp.status.success(), "{}", String::from_utf8_lossy(&sp.stderr));
    assert!(String::from_utf8_lossy(&sp.stderr).contains("could not close blank shell tab"));
    // Blank tab is left for now (warned), agent tab present.
    assert_eq!(env.tab_names("s1"), vec!["shell", "owner"]);

    // The next spawn retries the retirement and succeeds.
    env.set_script(serde_json::json!({}));
    let sp_w = env.run(&["spawn", "close-fails", "--role", "worker", "--domain", "web"], &[]);
    assert!(sp_w.status.success(), "{}", String::from_utf8_lossy(&sp_w.stderr));
    assert_eq!(env.tab_names("s1"), vec!["owner", "worker:web"]);

    // Once retired it is forgotten: a further spawn closes nothing.
    env.clear_log();
    let sp_x = env.run(&["spawn", "close-fails", "--role", "worker", "--domain", "api"], &[]);
    assert!(sp_x.status.success());
    assert!(!env.read_log().iter().any(|a| a.len() >= 2 && a[1] == "tab.close"));
}

#[test]
fn e2e_failed_resume_launch_closes_recreated_space() {
    let env = TestEnv::new();
    assert!(env.run(&["new", "resume-fails", "--repos", "api"], &[]).status.success());
    assert!(env.run(&["spawn", "resume-fails", "--role", "owner"], &[]).status.success());

    // Space + terminals vanish; the relaunch fails: no empty/broken space may be left behind.
    let mut fs_val: Value = serde_json::from_str(&fs::read_to_string(&env.fake_state).unwrap()).unwrap();
    fs_val["spaces"] = serde_json::json!({});
    fs_val["terminals"] = serde_json::json!({});
    fs::write(&env.fake_state, serde_json::to_string_pretty(&fs_val).unwrap()).unwrap();
    env.set_script(serde_json::json!({ "fail_hc_run": true }));
    let res = env.run(&["resume", "resume-fails", "--key", "owner"], &[]);
    assert!(!res.status.success());
    let after: Value = serde_json::from_str(&fs::read_to_string(&env.fake_state).unwrap()).unwrap();
    assert!(after["spaces"].as_object().unwrap().is_empty());
}

#[test]
fn e2e_cleanup_failure_is_retried_by_reuse_spawn() {
    let env = TestEnv::new();
    assert!(env.run(&["new", "rm-fails", "--repos", "api"], &[]).status.success());

    // hc rm of the initial terminal fails: tab is closed but the id must stay recorded.
    env.set_script(serde_json::json!({ "fail_hc_rm": true }));
    let sp = env.run(&["spawn", "rm-fails", "--role", "owner"], &[]);
    assert!(sp.status.success(), "{}", String::from_utf8_lossy(&sp.stderr));
    assert!(String::from_utf8_lossy(&sp.stderr).contains("could not close blank shell tab"));
    assert_eq!(env.tab_names("s1"), vec!["owner"]);

    // Re-running the same spawn (reuse path) retries cleanup and then forgets the id.
    env.set_script(serde_json::json!({}));
    env.clear_log();
    let again = env.run(&["spawn", "rm-fails", "--role", "owner"], &[]);
    assert!(again.status.success());
    assert!(env.read_log().iter().any(|a| a.first().map(|s| s.as_str()) == Some("rm")));
    env.clear_log();
    assert!(env.run(&["spawn", "rm-fails", "--role", "owner"], &[]).status.success());
    assert!(!env.read_log().iter().any(|a| a.first().map(|s| s.as_str()) == Some("rm")));
}

#[test]
fn e2e_archive_warns_when_unretired_initial_terminal_cleanup_fails() {
    let env = TestEnv::new();
    assert!(env.run(&["new", "arch-warn", "--repos", "api"], &[]).status.success());
    env.set_script(serde_json::json!({ "fail_hc_rm": true }));
    assert!(env.run(&["spawn", "arch-warn", "--role", "owner"], &[]).status.success());

    let arch = env.run(&["archive", "arch-warn"], &[]);
    assert!(arch.status.success(), "{}", String::from_utf8_lossy(&arch.stderr));
    let arch_json: Value = serde_json::from_slice(&arch.stdout).unwrap();
    let warnings = arch_json["warnings"].as_array().unwrap();
    assert!(warnings
        .iter()
        .filter_map(|v| v.as_str())
        .any(|w| w.contains("hc cleanup for initial terminal")));
}

#[test]
fn e2e_split_initial_tab_preserves_unrelated_terminal() {
    let env = TestEnv::new();
    assert!(env.run(&["new", "split-tab", "--repos", "api"], &[]).status.success());

    // First spawn leaves the initial tab intact so we can simulate a user splitting it.
    env.set_script(serde_json::json!({ "fail_tab_close": true }));
    let sp = env.run(&["spawn", "split-tab", "--role", "owner"], &[]);
    assert!(sp.status.success(), "{}", String::from_utf8_lossy(&sp.stderr));
    assert_eq!(env.tab_names("s1"), vec!["shell", "owner"]);
    let init_tid = env.work_state("split-tab")["herdr_space_initial_terminal"]
        .as_str()
        .unwrap()
        .to_string();

    // Inject an unrelated user terminal `t99` into the initial `shell` tab (simulating pane.split).
    let mut fs_val: Value = serde_json::from_str(&fs::read_to_string(&env.fake_state).unwrap()).unwrap();
    fs_val["spaces"]["s1"]["tabs"][0]["terminals"]
        .as_array_mut()
        .unwrap()
        .push(serde_json::json!({
            "terminal": "t99",
            "pane": "p99",
            "name": "user-shell"
        }));
    fs_val["terminals"]["user-shell"] = serde_json::json!({
        "id": "t99",
        "name": "user-shell",
        "state": "live",
        "status": "idle",
        "labels": { "herdr.space": "s1" },
        "first_message_sent": true,
        "first_message_reason": null
    });
    fs::write(&env.fake_state, serde_json::to_string_pretty(&fs_val).unwrap()).unwrap();

    // Retry via reuse spawn: must close ONLY the owned initial pane (`pane.close`), never `tab.close`,
    // and must keep the unrelated `t99` terminal live in both the tab and hc state!
    env.set_script(serde_json::json!({}));
    env.clear_log();
    let again = env.run(&["spawn", "split-tab", "--role", "owner"], &[]);
    assert!(again.status.success(), "{}", String::from_utf8_lossy(&again.stderr));
    let log = env.read_log();
    assert!(
        !log.iter().any(|a| a.len() >= 2 && a[1] == "tab.close"),
        "must never call tab.close on a split initial tab"
    );
    assert!(
        log.iter().any(|a| a.len() >= 2 && a[1] == "pane.close"),
        "must close only the owned initial pane"
    );

    let after: Value = serde_json::from_str(&fs::read_to_string(&env.fake_state).unwrap()).unwrap();
    assert_eq!(env.tab_names("s1"), vec!["shell", "owner"]);
    let shell_terms = after["spaces"]["s1"]["tabs"][0]["terminals"].as_array().unwrap();
    assert_eq!(shell_terms.len(), 1);
    assert_eq!(shell_terms[0]["terminal"], "t99");
    assert_eq!(after["terminals"]["user-shell"]["state"], "live");
    assert!(
        !env.fake_terminal_ids().contains(&init_tid),
        "initial terminal record should be removed"
    );
    assert!(env.work_state("split-tab")["herdr_space_initial_terminal"].is_null());
}

#[test]
fn e2e_failed_launch_and_rm_cleaned_on_retry_then_archive() {
    let env = TestEnv::new();
    assert!(env.run(&["new", "retry-clean", "--repos", "api"], &[]).status.success());

    // Launch fails and rollback's `hc rm` of initial terminal also fails -> space closed,
    // `herdr_space_id` cleared, but initial terminal ID retained for cleanup.
    env.set_script(serde_json::json!({ "fail_hc_run": true, "fail_hc_rm": true }));
    let sp1 = env.run(&["spawn", "retry-clean", "--role", "owner"], &[]);
    assert!(!sp1.status.success());
    let st1 = env.work_state("retry-clean");
    assert!(st1["herdr_space_id"].is_null());
    let stale_tid = st1["herdr_space_initial_terminal"]
        .as_str()
        .expect("retained initial terminal ID after failed rm")
        .to_string();
    assert!(env.fake_terminal_ids().contains(&stale_tid));

    // If stale cleanup fails again on retry, spawn aborts before creating a replacement space
    // and keeps the retained initial terminal ID tracked so it cannot be orphaned.
    env.set_script(serde_json::json!({ "fail_hc_rm": true }));
    env.clear_log();
    let sp_retry_fail = env.run(&["spawn", "retry-clean", "--role", "owner"], &[]);
    assert!(!sp_retry_fail.status.success());
    assert!(
        !env.read_log().iter().any(|a| a.len() >= 2 && a[1] == "space.new"),
        "must not create a replacement space when cleaning up previous initial terminal fails"
    );
    assert_eq!(
        env.work_state("retry-clean")["herdr_space_initial_terminal"].as_str(),
        Some(stale_tid.as_str())
    );

    // Retry succeeds: retained `stale_tid` is cleaned up before replacing `herdr_space_initial_terminal`,
    // and the new space's initial terminal is also retired.
    env.set_script(serde_json::json!({}));
    let sp2 = env.run(&["spawn", "retry-clean", "--role", "owner"], &[]);
    assert!(sp2.status.success(), "{}", String::from_utf8_lossy(&sp2.stderr));
    let sp2_json: Value = serde_json::from_slice(&sp2.stdout).unwrap();
    let owner_tid = sp2_json["session"]["hc_terminal_id"].as_str().unwrap().to_string();
    assert_eq!(env.tab_names("s2"), vec!["owner"]);
    assert_eq!(env.fake_terminal_ids(), vec![owner_tid]);
    assert!(env.work_state("retry-clean")["herdr_space_initial_terminal"].is_null());

    // Archive finishes cleanly with zero warnings and zero leftover terminals.
    let arch = env.run(&["archive", "retry-clean"], &[]);
    assert!(arch.status.success(), "{}", String::from_utf8_lossy(&arch.stderr));
    let arch_json: Value = serde_json::from_slice(&arch.stdout).unwrap();
    assert!(arch_json["warnings"].as_array().unwrap().is_empty());
    assert!(env.fake_terminal_ids().is_empty());
}
