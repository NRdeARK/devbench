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
pub struct SpaceCreated {
    pub space_id: String,
    pub initial_terminal_id: Option<String>,
    pub initial_pane_id: Option<String>,
}

#[derive(Debug, Clone)]
pub struct TerminalPlacement {
    pub tab_id: String,
    pub pane_id: Option<String>,
}

#[derive(Debug, Clone)]
pub struct HcTerminalInfo {
    pub id: String,
    pub live: bool,
    pub status: Option<String>,
    pub space_label: Option<String>,
    pub first_message_sent: Option<bool>,
    pub first_message_reason: Option<String>,
}

#[derive(Debug, Clone)]
pub enum HcGetResult {
    Live(HcTerminalInfo),
    Finished(HcTerminalInfo),
    NotFound,
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
    /// Labels the initial shell tab `"shell"` and captures space ID + initial terminal/pane IDs.
    pub fn space_new(&self, name: &str, folder: &Path, focus: bool) -> Result<SpaceCreated, String> {
        let payload = serde_json::json!({
            "name": name,
            "folder": folder.to_string_lossy(),
            "tab": "shell",
            "focus": focus,
        });
        let out = run_cmd(&self.app_bin, &["do", "space.new", &payload.to_string()])
            .map_err(|e| format!("herdr-app space.new failed (is Herdr open?): {e}"))?;
        let val: Value = serde_json::from_str(&out)
            .map_err(|e| format!("Unexpected JSON from herdr-app space.new: {e} ({out})"))?;
        if let Some(err) = val.get("error").and_then(|v| v.as_str()) {
            return Err(format!("herdr-app space.new error: {err}"));
        }
        let space_id = val
            .get("space")
            .or_else(|| val.get("id"))
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .ok_or_else(|| format!("herdr-app space.new did not return a space id: {out}"))?;

        let initial_terminal_id = val
            .get("terminal")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        let initial_pane_id = val
            .get("pane")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());

        Ok(SpaceCreated {
            space_id,
            initial_terminal_id,
            initial_pane_id,
        })
    }

    /// Rename a Herdr tab via `herdr-app do tab.rename {id, name}`.
    pub fn tab_rename(&self, tab_id: &str, name: &str) -> Result<(), String> {
        let payload = serde_json::json!({ "id": tab_id, "name": name });
        let out = run_cmd(&self.app_bin, &["do", "tab.rename", &payload.to_string()])?;
        if let Ok(val) = serde_json::from_str::<Value>(&out) {
            if let Some(err) = val.get("error").and_then(|v| v.as_str()) {
                return Err(format!("herdr-app tab.rename error: {err}"));
            }
        }
        Ok(())
    }

    /// Locate a terminal's Herdr tab ID and pane ID via `herdr-app ls`.
    pub fn find_terminal_placement(
        &self,
        space_id: &str,
        terminal_id: &str,
        hc_name: &str,
    ) -> Result<Option<TerminalPlacement>, String> {
        let out = run_cmd(&self.app_bin, &["ls"])?;
        let val: Value = serde_json::from_str(&out)
            .map_err(|e| format!("Unexpected JSON from herdr-app ls: {e}"))?;
        let Some(spaces) = val
            .get("spaces")
            .and_then(|s| s.as_array())
            .or_else(|| val.as_array())
        else {
            return Ok(None);
        };

        let search_space = |space: &Value| -> Option<TerminalPlacement> {
            let tabs = space.get("tabs").and_then(|t| t.as_array())?;
            for tab in tabs {
                let Some(tab_id) = tab.get("id").and_then(|v| v.as_str()) else {
                    continue;
                };
                let Some(terminals) = tab.get("terminals").and_then(|t| t.as_array()) else {
                    continue;
                };
                for term in terminals {
                    let t_id = term.get("terminal").and_then(|v| v.as_str());
                    let t_name = term.get("name").and_then(|v| v.as_str());
                    if t_id == Some(terminal_id) || t_name == Some(hc_name) {
                        let pane_id = term
                            .get("pane")
                            .and_then(|v| v.as_str())
                            .map(|s| s.to_string());
                        return Some(TerminalPlacement {
                            tab_id: tab_id.to_string(),
                            pane_id,
                        });
                    }
                }
            }
            None
        };

        // Prefer the target space first, then fall back to any space
        if let Some(target) = spaces.iter().find(|s| {
            s.get("id")
                .or_else(|| s.get("space"))
                .and_then(|v| v.as_str())
                == Some(space_id)
        }) {
            if let Some(found) = search_space(target) {
                return Ok(Some(found));
            }
        }
        for space in spaces {
            if let Some(found) = search_space(space) {
                return Ok(Some(found));
            }
        }
        Ok(None)
    }

    /// Best-effort: wait briefly for Herdr to attach a spawned terminal to its space tab,
    /// rename the tab to its role label (`owner`, `worker:<domain>`), and return tab/pane IDs.
    pub fn wait_and_label_tab(
        &self,
        space_id: &str,
        terminal_id: &str,
        hc_name: &str,
        tab_label: &str,
    ) -> Option<TerminalPlacement> {
        for attempt in 0..5 {
            if let Ok(Some(placement)) = self.find_terminal_placement(space_id, terminal_id, hc_name)
            {
                let _ = self.tab_rename(&placement.tab_id, tab_label);
                return Some(placement);
            }
            if attempt < 4 {
                std::thread::sleep(std::time::Duration::from_millis(120));
            }
        }
        None
    }

    /// Close a 1:1 Herdr workspace via `herdr-app do space.close {id, confirmed: true}`.
    /// Matches the exact `herdr-app commands` schema (`id`, `confirmed`).
    pub fn space_close(&self, space_id: &str) -> Result<(), String> {
        let payload = serde_json::json!({ "id": space_id, "confirmed": true });
        let out = run_cmd(&self.app_bin, &["do", "space.close", &payload.to_string()])?;
        if let Ok(val) = serde_json::from_str::<Value>(&out) {
            if let Some(err) = val.get("error").and_then(|v| v.as_str()) {
                return Err(format!("herdr-app space.close error: {err}"));
            }
        }
        Ok(())
    }

    /// Check whether a Herdr space ID still exists via `herdr-app ls`.
    pub fn space_exists(&self, space_id: &str) -> Result<bool, String> {
        let out = run_cmd(&self.app_bin, &["ls"])?;
        if let Ok(val) = serde_json::from_str::<Value>(&out) {
            let spaces = val
                .get("spaces")
                .and_then(|s| s.as_array())
                .or_else(|| val.as_array());
            if let Some(arr) = spaces {
                return Ok(arr.iter().any(|item| {
                    item.get("id")
                        .or_else(|| item.get("space"))
                        .and_then(|v| v.as_str())
                        == Some(space_id)
                }));
            }
        }
        Ok(out.contains(space_id))
    }

    /// Focus an already-live terminal in the Herdr window.
    pub fn agent_focus(&self, terminal_id: &str) -> Result<(), String> {
        let payload = serde_json::json!({ "terminal": terminal_id });
        let _ = run_cmd(&self.app_bin, &["do", "agent.focus", &payload.to_string()])?;
        Ok(())
    }

    /// Inspect one terminal via `hc get <target>` (piped stdout returns JSON).
    /// Fails closed on unknown state and distinguishes NotFound (3), Finished (11), and daemon errors (5).
    pub fn hc_get(&self, target: &str) -> Result<HcGetResult, String> {
        let output = Command::new(&self.hc_bin)
            .args(["get", target])
            .output()
            .map_err(|e| format!("Failed to invoke {}: {e}", self.hc_bin.display()))?;

        if !output.status.success() {
            match output.status.code() {
                Some(3) => return Ok(HcGetResult::NotFound),
                Some(11) => {
                    return Ok(HcGetResult::Finished(HcTerminalInfo {
                        id: target.to_string(),
                        live: false,
                        status: Some("exited".to_string()),
                        space_label: None,
                        first_message_sent: None,
                        first_message_reason: None,
                    }));
                }
                code => {
                    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
                    return Err(format!(
                        "hc get {target} failed (exit {:?}): {stderr}",
                        code
                    ));
                }
            }
        }

        let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
        let val: Value = serde_json::from_str(&stdout)
            .map_err(|e| format!("Failed to parse hc get JSON: {e}"))?;

        let info = parse_hc_terminal_json(&val, target)?;
        if info.live {
            Ok(HcGetResult::Live(info))
        } else {
            Ok(HcGetResult::Finished(info))
        }
    }

    /// Launch a persistent agent via `hc run --bg --outlive-parent` labeled with `herdr.space=<id>`.
    /// `--outlive-parent` keeps active Owner/worker sessions alive if the Dispatcher tab restarts;
    /// closing the pane/space or running `devbench archive` kills and cleans them up.
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
        let stdout = run_cmd(&self.hc_bin, &str_refs)?;

        // Query authoritative terminal info via `hc get <name>`
        match self.hc_get(name) {
            Ok(HcGetResult::Live(info)) => Ok(info),
            Ok(HcGetResult::Finished(info)) => Err(format!(
                "Agent terminal '{}' ({}) exited immediately after launch (status: {:?})",
                name, info.id, info.status
            )),
            Ok(HcGetResult::NotFound) => Err(format!(
                "Agent terminal '{}' not found after hc run (stdout: {})",
                name, stdout
            )),
            Err(e) => {
                // Attempt best-effort parse from run output or return error without pretending live
                if let Ok(val) = serde_json::from_str::<Value>(&stdout) {
                    if let Ok(info) = parse_hc_terminal_json(&val, name) {
                        return Ok(info);
                    }
                }
                Err(format!(
                    "hc run succeeded but post-launch hc get '{name}' failed: {e}"
                ))
            }
        }
    }

    /// Read recent screen lines from a terminal (used when blocked on a trust prompt).
    pub fn hc_read(&self, target: &str, lines: usize) -> Result<String, String> {
        let lines_str = lines.to_string();
        run_cmd(&self.hc_bin, &["read", "--lines", &lines_str, target])
    }

    /// Kill, briefly wait for exit (so Windows releases cwd/file locks), and remove a recorded terminal.
    pub fn hc_kill_and_rm(&self, target: &str) -> Result<(), String> {
        let kill_out = Command::new(&self.hc_bin)
            .args(["kill", target])
            .output()
            .map_err(|e| format!("Failed to invoke hc kill {target}: {e}"))?;
        if !kill_out.status.success() {
            // Exit 3 (not_found) or 11 (already finished) is fine during archive cleanup
            if !matches!(kill_out.status.code(), Some(3) | Some(11)) {
                let stderr = String::from_utf8_lossy(&kill_out.stderr).trim().to_string();
                return Err(format!("hc kill {target} failed: {stderr}"));
            }
        }
        // Wait briefly for the process to exit so Windows unlocks worktree files
        let _ = Command::new(&self.hc_bin)
            .args(["wait", target, "--timeout", "3"])
            .output();
        let _ = Command::new(&self.hc_bin).args(["rm", target]).output();
        Ok(())
    }
}

fn parse_hc_terminal_json(val: &Value, fallback_id: &str) -> Result<HcTerminalInfo, String> {
    let state_str = val
        .get("state")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "hc get JSON missing required 'state' field".to_string())?;
    let live = match state_str {
        "live" => true,
        "finished" | "exited" => false,
        other => {
            return Err(format!("Unexpected hc terminal state '{other}'"));
        }
    };
    let id = val
        .get("address")
        .or_else(|| val.get("id"))
        .and_then(|v| v.as_str())
        .unwrap_or(fallback_id)
        .to_string();
    let status = val
        .get("status")
        .and_then(|v| {
            v.as_str()
                .or_else(|| v.get("state").and_then(|s| s.as_str()))
        })
        .map(|s| s.to_string());
    let space_label = val
        .get("labels")
        .and_then(|l| l.get("herdr.space"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let fm = val.get("first_message");
    let first_message_sent = fm.and_then(|m| m.get("sent")).and_then(|v| v.as_bool());
    let first_message_reason = fm
        .and_then(|m| m.get("reason"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    Ok(HcTerminalInfo {
        id,
        live,
        status,
        space_label,
        first_message_sent,
        first_message_reason,
    })
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
