use crate::config::HerdrConfig;
use serde_json::Value;
use std::env;
use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Debug, Clone)]
pub struct HerdrClient {
    pub hc_bin: PathBuf,
    pub app_bin: PathBuf,
}

#[derive(Debug, Clone)]
pub struct HcTerminalInfo {
    pub id: String,
    pub live: bool,
    pub status: Option<String>,
    pub first_message_sent: Option<bool>,
    pub first_message_reason: Option<String>,
}

impl HerdrClient {
    pub fn from_config(cfg: &HerdrConfig) -> Self {
        let hc_bin = resolve_bin(
            cfg.hc_bin.as_deref(),
            &["hc", "herdr-core"],
            &["herdr-core\\herdr-core.exe"],
        );
        let app_bin = resolve_bin(
            cfg.app_bin.as_deref(),
            &["herdr-app"],
            &["herdr-app\\herdr-app.exe"],
        );
        Self { hc_bin, app_bin }
    }

    /// Create a dedicated 1:1 Herdr workspace via `herdr-app do space.new`.
    pub fn space_new(&self, name: &str, folder: &Path, focus: bool) -> Result<String, String> {
        let payload = serde_json::json!({
            "name": name,
            "folder": folder.to_string_lossy(),
            "focus": focus,
        });
        let out = run_cmd(&self.app_bin, &["do", "space.new", &payload.to_string()])
            .map_err(|e| format!("herdr-app space.new failed (is Herdr open?): {e}"))?;
        let val: Value = serde_json::from_str(&out)
            .map_err(|e| format!("Unexpected JSON from herdr-app space.new: {e} ({out})"))?;
        if let Some(err) = val.get("error").and_then(|v| v.as_str()) {
            return Err(format!("herdr-app space.new error: {err}"));
        }
        val.get("space")
            .or_else(|| val.get("id"))
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .ok_or_else(|| format!("herdr-app space.new did not return a space id: {out}"))
    }

    /// Close a 1:1 Herdr workspace via `herdr-app do space.close`.
    pub fn space_close(&self, space_id: &str) -> Result<(), String> {
        let payload = serde_json::json!({ "space": space_id });
        let _ = run_cmd(&self.app_bin, &["do", "space.close", &payload.to_string()])?;
        Ok(())
    }

    /// Focus an already-live terminal in the Herdr window.
    pub fn agent_focus(&self, terminal_id: &str) -> Result<(), String> {
        let payload = serde_json::json!({ "terminal": terminal_id });
        let _ = run_cmd(&self.app_bin, &["do", "agent.focus", &payload.to_string()])?;
        Ok(())
    }

    /// Inspect one terminal via `hc get <target>` (piped stdout returns JSON).
    pub fn hc_get(&self, target: &str) -> Result<Option<HcTerminalInfo>, String> {
        let output = Command::new(&self.hc_bin)
            .args(["get", target])
            .output()
            .map_err(|e| format!("Failed to invoke {}: {e}", self.hc_bin.display()))?;

        if !output.status.success() {
            // Exit code 3 = not_found, 11 = finished
            if matches!(output.status.code(), Some(3) | Some(11)) {
                return Ok(None);
            }
            let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
            return Err(format!("hc get {target} failed: {stderr}"));
        }

        let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
        let val: Value = serde_json::from_str(&stdout)
            .map_err(|e| format!("Failed to parse hc get JSON: {e}"))?;

        let id = val
            .get("id")
            .and_then(|v| v.as_str())
            .unwrap_or(target)
            .to_string();
        let state_str = val.get("state").and_then(|v| v.as_str()).unwrap_or("live");
        let live = state_str == "live";
        let status = val
            .get("status")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        let fm = val.get("first_message");
        let first_message_sent = fm.and_then(|m| m.get("sent")).and_then(|v| v.as_bool());
        let first_message_reason = fm
            .and_then(|m| m.get("reason"))
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());

        Ok(Some(HcTerminalInfo {
            id,
            live,
            status,
            first_message_sent,
            first_message_reason,
        }))
    }

    /// Launch a persistent agent via `hc run --bg --outlive-parent` labeled with `herdr.space=<id>`.
    /// Note: `--outlive-parent` ensures active Owner/worker sessions survive when the Dispatcher tab
    /// closes or restarts; closing their pane/space or running `devbench archive` kills them cleanly.
    pub fn hc_run_persistent(
        &self,
        name: &str,
        cwd: &Path,
        space_id: &str,
        argv: &[String],
        task: Option<&str>,
    ) -> Result<HcTerminalInfo, String> {
        if argv.is_empty() {
            return Err("Agent argv cannot be empty".to_string());
        }
        let mut args: Vec<String> = vec![
            "run".to_string(),
            "--bg".to_string(),
            "--outlive-parent".to_string(),
            "--name".to_string(),
            name.to_string(),
            "--cwd".to_string(),
            cwd.to_string_lossy().to_string(),
            "--label".to_string(),
            format!("herdr.space={space_id}"),
        ];
        args.extend(argv.iter().cloned());
        if let Some(prompt) = task {
            if !prompt.trim().is_empty() {
                args.push(prompt.to_string());
            }
        }

        let str_refs: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
        let _stdout = run_cmd(&self.hc_bin, &str_refs)?;

        // Query structured state via `hc get <name>` to verify ID and `first_message.sent`
        match self.hc_get(name)? {
            Some(info) => Ok(info),
            None => Ok(HcTerminalInfo {
                id: name.to_string(),
                live: true,
                status: Some("launching".to_string()),
                first_message_sent: Some(task.is_none()),
                first_message_reason: None,
            }),
        }
    }

    /// Read recent screen lines from a terminal (used when blocked on a trust prompt).
    pub fn hc_read(&self, target: &str, lines: usize) -> Result<String, String> {
        let lines_str = lines.to_string();
        run_cmd(&self.hc_bin, &["read", "--lines", &lines_str, target])
    }

    /// Kill and remove a specific terminal recorded for a work during archive.
    pub fn hc_kill_and_rm(&self, target: &str) {
        let _ = Command::new(&self.hc_bin).args(["kill", target]).output();
        let _ = Command::new(&self.hc_bin).args(["rm", target]).output();
    }
}

/// Returns the caller's own `$HC_TERMINAL_ID` if running inside a Herdr terminal.
pub fn current_hc_terminal_id() -> Option<String> {
    env::var("HC_TERMINAL_ID")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

fn run_cmd(bin: &Path, args: &[&str]) -> Result<String, String> {
    let output = Command::new(bin)
        .args(args)
        .output()
        .map_err(|e| format!("Failed to invoke {}: {e}", bin.display()))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
        let detail = if !stderr.is_empty() { stderr } else { stdout };
        return Err(detail);
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn resolve_bin(configured: Option<&str>, path_names: &[&str], win_local_rel: &[&str]) -> PathBuf {
    if let Some(c) = configured {
        if !c.trim().is_empty() {
            return PathBuf::from(c);
        }
    }
    // Check Windows standard per-user install location first if not on PATH
    if cfg!(windows) {
        if let Ok(local_app) = env::var("LOCALAPPDATA") {
            for rel in win_local_rel {
                let candidate = PathBuf::from(&local_app).join("Programs").join(rel);
                if candidate.is_file() {
                    return candidate;
                }
            }
        }
    }
    PathBuf::from(path_names[0])
}
