use crate::config::AgentConfig;

/// Which agent type to create / is running.
#[derive(Debug, Clone, PartialEq)]
pub enum AgentType {
    Opencode,
    Claude,
}

impl AgentType {
    pub fn name(&self) -> &'static str {
        match self {
            AgentType::Opencode => "opencode",
            AgentType::Claude => "claude",
        }
    }

    pub fn from_name(s: &str) -> Option<AgentType> {
        match s {
            "opencode" => Some(AgentType::Opencode),
            "claude" => Some(AgentType::Claude),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum AgentStatus {
    Running,
    WaitingForInput,
    Idle, // turn finished, ready for next user prompt
    Stopped,
    Unknown,
}

#[derive(Debug, Clone)]
pub struct ContextInfo {
    pub used: u64,
    pub total: Option<u64>,
}

#[derive(Debug, Clone)]
pub struct AgentMeta {
    pub status: AgentStatus,
    pub context: Option<ContextInfo>,
    pub first_prompt: Option<String>,
    pub last_model_response: Option<String>,
    /// Model identifier reported by the agent (e.g. "claude-sonnet-4-5").
    pub model_name: Option<String>,
    /// Total milliseconds spent on model responses across the session.
    pub total_work_ms: u64,
}

impl Default for AgentMeta {
    fn default() -> Self {
        Self {
            status: AgentStatus::Unknown,
            context: None,
            first_prompt: None,
            last_model_response: None,
            model_name: None,
            total_work_ms: 0,
        }
    }
}

#[derive(Debug, Clone)]
pub struct AgentEntry {
    pub config: AgentConfig,
    pub meta: AgentMeta,
}
