use crate::config::AgentConfig;

/// Which agent type to create / is running.
#[derive(Debug, Clone, PartialEq)]
pub enum AgentType {
    Opencode,
    Claude,
    Codex,
}

impl AgentType {
    pub fn name(&self) -> &'static str {
        match self {
            AgentType::Opencode => "opencode",
            AgentType::Claude => "claude",
            AgentType::Codex => "codex",
        }
    }

    pub fn from_name(s: &str) -> Option<AgentType> {
        match s {
            "opencode" => Some(AgentType::Opencode),
            "claude" => Some(AgentType::Claude),
            "codex" => Some(AgentType::Codex),
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
    pub model_name: Option<String>,
    pub total_work_ms: u64,
    pub status_changed_at: Option<std::time::Instant>,
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
            status_changed_at: None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct AgentEntry {
    pub config: AgentConfig,
    pub meta: AgentMeta,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct AgentStatusCounts {
    pub running: usize,
    pub waiting: usize,
    pub idle: usize,
}

impl AgentStatusCounts {
    pub fn for_project(agents: &[AgentEntry], project: &str) -> Self {
        let mut counts = Self::default();

        for agent in agents
            .iter()
            .filter(|agent| agent.config.project == project)
        {
            match agent.meta.status {
                AgentStatus::Running => counts.running += 1,
                AgentStatus::WaitingForInput => counts.waiting += 1,
                AgentStatus::Idle => counts.idle += 1,
                AgentStatus::Stopped | AgentStatus::Unknown => {}
            }
        }

        counts
    }
}
