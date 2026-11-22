use crate::adapters::{HarnessAdapter, LaunchPlan};
use crate::config::{AgentConfig, CaptureMode};
use crate::state::CaptureState;

pub struct GenericAdapter;

impl HarnessAdapter for GenericAdapter {
    fn prepare_launch(&self, cfg: &AgentConfig) -> LaunchPlan {
        let capture = match cfg.capture {
            CaptureMode::Unavailable => CaptureState::Unavailable,
            _ => CaptureState::Manual,
        };
        LaunchPlan {
            argv: cfg.argv.clone(),
            preassigned_session_id: None,
            capture,
        }
    }
}
