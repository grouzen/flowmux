use async_trait::async_trait;
use crate::models::{AgentStatus, ContextInfo};

#[async_trait]
pub trait AgentAdapter: Send + Sync {
    async fn get_status(&self) -> AgentStatus;
    async fn get_context(&self) -> Option<ContextInfo>;
    async fn get_first_prompt(&self) -> Option<String>;
    async fn get_last_model_response(&self) -> Option<String>;
    /// Returns the model identifier for the most recent assistant message (e.g. "claude-sonnet-4-5").
    async fn get_model_name(&self) -> Option<String>;
    /// Returns the total milliseconds spent on model responses across the session.
    async fn get_total_work_ms(&self) -> u64;
    /// Returns the currently cached session ID for this adapter, if known.
    fn get_cached_session_id(&self) -> Option<String>;
}

pub mod opencode;
