pub mod claude_hook_server;

use async_trait::async_trait;

use crate::agents::AgentAdapter;
use crate::models::{AgentStatus, ContextInfo};
use claude_hook_server::HookStateMap;

// ---------------------------------------------------------------------------
// ClaudeAdapter
// ---------------------------------------------------------------------------

pub struct ClaudeAdapter {
    stable_agent_id: String,
    hook_state: HookStateMap,
}

impl ClaudeAdapter {
    pub fn new(stable_agent_id: String, hook_state: HookStateMap) -> Self {
        Self { stable_agent_id, hook_state }
    }
}

#[async_trait]
impl AgentAdapter for ClaudeAdapter {
    async fn get_status(&self) -> AgentStatus {
        let map = self.hook_state.lock().unwrap();
        map.get(&self.stable_agent_id)
            .map(|s| s.status.clone())
            .unwrap_or(AgentStatus::Unknown)
    }

    async fn get_context(&self) -> Option<ContextInfo> {
        let map = self.hook_state.lock().unwrap();
        let context_used = map.get(&self.stable_agent_id)?.context_used?;
        Some(ContextInfo { used: context_used, total: None })
    }

    async fn get_first_prompt(&self) -> Option<String> {
        let map = self.hook_state.lock().unwrap();
        map.get(&self.stable_agent_id)?.first_prompt.clone()
    }

    async fn get_last_model_response(&self) -> Option<String> {
        let map = self.hook_state.lock().unwrap();
        map.get(&self.stable_agent_id)?.last_model_response.clone()
    }

    async fn get_model_name(&self) -> Option<String> {
        let map = self.hook_state.lock().unwrap();
        map.get(&self.stable_agent_id)?.model_name.clone()
    }

    /// Claude Code tracks work time internally; we return 0 here.
    async fn get_total_work_ms(&self) -> u64 {
        0
    }

    fn get_cached_session_id(&self) -> Option<String> {
        let map = self.hook_state.lock().unwrap();
        map.get(&self.stable_agent_id)?.session_id.clone()
    }
}
