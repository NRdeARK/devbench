pub mod agy;
pub mod claude;
pub mod codex;
pub mod generic;

use crate::config::AgentConfig;
use crate::state::CaptureState;
use std::path::Path;
use std::time::SystemTime;

#[derive(Debug, Clone)]
pub struct LaunchPlan {
    pub argv: Vec<String>,
    pub preassigned_session_id: Option<String>,
    pub capture: CaptureState,
}

pub trait HarnessAdapter {
    fn prepare_launch(&self, cfg: &AgentConfig) -> LaunchPlan;
    fn capture_after_launch(
        &self,
        _cwd: &Path,
        _launched_at: SystemTime,
        plan: &LaunchPlan,
    ) -> (Option<String>, CaptureState) {
        (plan.preassigned_session_id.clone(), plan.capture.clone())
    }
    fn build_resume_argv(
        &self,
        cfg: &AgentConfig,
        session_id: &str,
    ) -> Result<Vec<String>, String> {
        let Some(template) = &cfg.resume_argv else {
            return Err("This harness does not configure resume_argv".to_string());
        };
        Ok(template
            .iter()
            .map(|arg| arg.replace("{session_id}", session_id))
            .collect())
    }
}

pub fn adapter_for(harness: &str) -> Box<dyn HarnessAdapter> {
    match harness {
        "claude" => Box::new(claude::ClaudeAdapter),
        "codex" => Box::new(codex::CodexAdapter),
        "agy" => Box::new(agy::AgyAdapter),
        _ => Box::new(generic::GenericAdapter),
    }
}
