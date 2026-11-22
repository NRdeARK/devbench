use crate::adapters::{HarnessAdapter, LaunchPlan};
use crate::config::AgentConfig;
use crate::state::CaptureState;

pub struct AgyAdapter;

impl HarnessAdapter for AgyAdapter {
    fn prepare_launch(&self, cfg: &AgentConfig) -> LaunchPlan {
        LaunchPlan {
            argv: cfg.argv.clone(),
            preassigned_session_id: None,
            capture: CaptureState::Manual,
        }
    }
}
