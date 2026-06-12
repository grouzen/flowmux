use std::path::PathBuf;

/// Paths to agent binaries discovered on `$PATH` at startup.
#[derive(Debug, Clone)]
pub struct DiscoveredAgents {
    pub claude: Option<PathBuf>,
    pub codex: Option<PathBuf>,
    pub opencode: Option<PathBuf>,
}

impl DiscoveredAgents {
    /// Probe `$PATH` for known agent binaries.
    pub fn probe() -> Self {
        Self {
            claude: which::which("claude").ok(),
            codex: which::which("codex").ok(),
            opencode: which::which("opencode").ok(),
        }
    }
}
