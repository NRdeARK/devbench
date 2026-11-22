use crate::adapters::{HarnessAdapter, LaunchPlan};
use crate::config::{AgentConfig, CaptureMode};
use crate::state::CaptureState;
use uuid::Uuid;

pub struct ClaudeAdapter;

impl HarnessAdapter for ClaudeAdapter {
    fn prepare_launch(&self, cfg: &AgentConfig) -> LaunchPlan {
        let mut argv = cfg.argv.clone();
        if cfg.capture == CaptureMode::Preassign {
            let sid = Uuid::new_v4().to_string();
            argv.push("--session-id".to_string());
            argv.push(sid.clone());
            LaunchPlan {
                argv,
                preassigned_session_id: Some(sid),
                capture: CaptureState::Auto,
            }
        } else {
            LaunchPlan {
                argv,
                preassigned_session_id: None,
                capture: CaptureState::Manual,
            }
        }
    }
}
