use crate::adapters::{HarnessAdapter, LaunchPlan};
use crate::config::{AgentConfig, CaptureMode};
use crate::paths::{expand_tilde, home_dir, normalize_for_compare};
use crate::state::CaptureState;
use chrono::{DateTime, Utc};
use serde_json::Value;
use std::collections::HashSet;
use std::env;
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
        exclude_ids: &HashSet<String>,
    ) -> (Option<String>, CaptureState) {
        if plan.capture != CaptureState::Pending {
            return (plan.preassigned_session_id.clone(), plan.capture.clone());
        }
        let Some(sessions_root) = codex_sessions_dir() else {
            return (None, CaptureState::Pending);
        };
        if let Some(id) = scan_codex_rollouts(&sessions_root, cwd, launched_at, exclude_ids) {
            (Some(id), CaptureState::Auto)
        } else {
            (None, CaptureState::Pending)
        }
    }

    fn build_resume_argv(
        &self,
        cfg: &AgentConfig,
        session_id: &str,
    ) -> Result<Vec<String>, String> {
        let Some(template) = &cfg.resume_argv else {
            return Err("Codex harness does not configure resume_argv".to_string());
        };
        let mut argv: Vec<String> = template
            .iter()
            .map(|arg| arg.replace("{session_id}", session_id))
            .collect();
        // Always ensure --no-daemon is present on resumed Codex sessions too (hc requirement).
        if !argv.iter().any(|a| a == "--no-daemon") {
            if argv.len() >= 2 {
                argv.insert(1, "--no-daemon".to_string());
            } else {
                argv.push("--no-daemon".to_string());
            }
        }
        Ok(argv)
    }
}

pub fn codex_sessions_dir() -> Option<PathBuf> {
    if let Ok(custom) = env::var("CODEX_HOME") {
        let trimmed = custom.trim();
        if !trimmed.is_empty() {
            if let Ok(base) = expand_tilde(trimmed) {
                return Some(base.join("sessions"));
            }
        }
    }
    home_dir().ok().map(|h| h.join(".codex").join("sessions"))
}

/// Scan `<codex_home>/sessions/**/rollout-*.jsonl` for a session whose `session_meta`
/// matches `expected_cwd`, was created at or after `launched_at` (checked via `session_meta.timestamp`
/// when present, falling back to mtime), and is not in `exclude_ids`.
pub fn scan_codex_rollouts(
    sessions_root: &Path,
    expected_cwd: &Path,
    launched_at: SystemTime,
    exclude_ids: &HashSet<String>,
) -> Option<String> {
    if !sessions_root.is_dir() {
        return None;
    }
    let target_cwd = normalize_for_compare(expected_cwd);
    let launched_utc: DateTime<Utc> = launched_at.into();

    let mut files = Vec::new();
    collect_rollouts(sessions_root, launched_at, &mut files);

    let mut matched: Vec<(DateTime<Utc>, String)> = Vec::new();
    for (mtime, path) in files {
        if let Some((created_at, id)) = parse_rollout_meta(&path, &target_cwd, mtime) {
            if created_at >= launched_utc && !exclude_ids.contains(&id) {
                matched.push((created_at, id));
            }
        }
    }
    matched.sort_by(|a, b| b.0.cmp(&a.0));
    matched.into_iter().next().map(|(_, id)| id)
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

fn parse_rollout_meta(
    path: &Path,
    expected_cwd_norm: &str,
    fallback_mtime: SystemTime,
) -> Option<(DateTime<Utc>, String)> {
    let file = fs::File::open(path).ok()?;
    let reader = BufReader::new(file);
    for line in reader.lines().take(10).flatten() {
        let Ok(val) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        // Only inspect session_meta entries
        if val.get("type").and_then(|t| t.as_str()) != Some("session_meta") {
            continue;
        }
        let payload = val.get("payload").unwrap_or(&val);
        let cwd = payload.get("cwd").and_then(|c| c.as_str())?;
        let id = payload.get("id").and_then(|i| i.as_str())?;
        if normalize_for_compare(Path::new(cwd)) != expected_cwd_norm {
            return None;
        }
        let ts_str = payload
            .get("timestamp")
            .or_else(|| val.get("timestamp"))
            .and_then(|t| t.as_str());
        let created = ts_str
            .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
            .map(|dt| dt.with_timezone(&Utc))
            .unwrap_or_else(|| DateTime::<Utc>::from(fallback_mtime));
        return Some((created, id.to_string()));
    }
    None
}
