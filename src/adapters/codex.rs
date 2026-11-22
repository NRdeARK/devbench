use crate::adapters::{HarnessAdapter, LaunchPlan};
use crate::config::{AgentConfig, CaptureMode};
use crate::paths::{home_dir, normalize_for_compare};
use crate::state::CaptureState;
use serde_json::Value;
use std::fs;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

pub struct CodexAdapter;

impl HarnessAdapter for CodexAdapter {
    fn prepare_launch(&self, cfg: &AgentConfig) -> LaunchPlan {
        let mut argv = cfg.argv.clone();
        // Required by herdr-core: Codex must run with --no-daemon or commands escape hc.
        if !argv.iter().any(|a| a == "--no-daemon") {
            argv.push("--no-daemon".to_string());
        }
        let capture = if cfg.capture == CaptureMode::Scan {
            CaptureState::Pending
        } else {
            CaptureState::Manual
        };
        LaunchPlan {
            argv,
            preassigned_session_id: None,
            capture,
        }
    }

    fn capture_after_launch(
        &self,
        cwd: &Path,
        launched_at: SystemTime,
        plan: &LaunchPlan,
    ) -> (Option<String>, CaptureState) {
        if plan.capture != CaptureState::Pending {
            return (plan.preassigned_session_id.clone(), plan.capture.clone());
        }
        let Ok(home) = home_dir() else {
            return (None, CaptureState::Pending);
        };
        let sessions_root = home.join(".codex").join("sessions");
        if let Some(id) = scan_codex_rollouts(&sessions_root, cwd, launched_at) {
            (Some(id), CaptureState::Auto)
        } else {
            (None, CaptureState::Pending)
        }
    }
}

/// Scan `~/.codex/sessions/**/rollout-*.jsonl` for a file modified at/after `launched_at`
/// whose first `session_meta` entry has matching `cwd`.
pub fn scan_codex_rollouts(
    sessions_root: &Path,
    expected_cwd: &Path,
    launched_at: SystemTime,
) -> Option<String> {
    if !sessions_root.is_dir() {
        return None;
    }
    let target_cwd = normalize_for_compare(expected_cwd);
    let mut candidates: Vec<(SystemTime, PathBuf)> = Vec::new();
    collect_rollouts(sessions_root, launched_at, &mut candidates);
    candidates.sort_by(|a, b| b.0.cmp(&a.0));

    for (_, path) in candidates {
        if let Some(id) = parse_rollout_meta(&path, &target_cwd) {
            return Some(id);
        }
    }
    None
}

fn collect_rollouts(dir: &Path, launched_at: SystemTime, out: &mut Vec<(SystemTime, PathBuf)>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(meta) = entry.metadata() else {
            continue;
        };
        if meta.is_dir() {
            collect_rollouts(&path, launched_at, out);
        } else if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
            if name.starts_with("rollout-") && name.ends_with(".jsonl") {
                if let Ok(modified) = meta.modified() {
                    if modified >= launched_at {
                        out.push((modified, path));
                    }
                }
            }
        }
    }
}

fn parse_rollout_meta(path: &Path, expected_cwd_norm: &str) -> Option<String> {
    let file = fs::File::open(path).ok()?;
    let reader = BufReader::new(file);
    for line in reader.lines().take(5).flatten() {
        let Ok(val) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        let payload = val.get("payload").unwrap_or(&val);
        let cwd = payload.get("cwd").and_then(|c| c.as_str());
        let id = payload.get("id").and_then(|i| i.as_str());
        if let (Some(cwd_str), Some(id_str)) = (cwd, id) {
            if normalize_for_compare(Path::new(cwd_str)) == expected_cwd_norm {
                return Some(id_str.to_string());
            }
        }
    }
    None
}
