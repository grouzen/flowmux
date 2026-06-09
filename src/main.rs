mod agent_discovery;
mod agents;
mod app;
mod config;
mod ghostty;
mod git;
mod global_config;
mod host_terminal;
mod model_registry;
mod models;
mod runner;
mod tmux;
mod tui;
mod ui;

use std::path::PathBuf;

use anyhow::Result;
use clap::Parser;

use agent_discovery::DiscoveredAgents;
use app::App;
use config::Config;
use global_config::GlobalConfig;
use models::{AgentEntry, AgentMeta, AgentType};
use runner::AgentRunner;

/// flowmux — multi-agent TUI dashboard
#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Cli {
    /// Name of the tmux session to use
    #[arg(long, default_value = "flowmux")]
    tmux_session: String,

    /// Base directory for git worktrees created by flowmux.
    /// Defaults to ~/.local/share/flowmux/worktrees
    #[arg(long)]
    git_worktrees_location: Option<PathBuf>,

    /// Comma-separated list of agent types to enable (e.g. "opencode,claude").
    /// Overrides the global config's `enabled_agents` setting.
    #[arg(long, value_delimiter = ',')]
    enabled_agents: Option<Vec<String>>,
}

/// Resolve the effective worktrees base directory.
///
/// Uses the CLI override when provided; otherwise falls back to
/// `~/.local/share/flowmux/worktrees`.
fn resolve_worktrees_base(override_path: Option<PathBuf>) -> PathBuf {
    if let Some(p) = override_path {
        return p;
    }
    dirs::data_local_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("flowmux")
        .join("worktrees")
}

/// Acquires an exclusive flock on `/tmp/flowmux-<session>.lock`.
///
/// The returned `File` must be kept alive for the duration of the process —
/// dropping it releases the lock.  The OS also releases it automatically on
/// process exit or crash, so no cleanup code is required.
fn acquire_session_lock(session: &str) -> Result<std::fs::File> {
    use fs2::FileExt as _;

    let lock_path = PathBuf::from(format!("/tmp/flowmux-{session}.lock"));

    let file = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .open(&lock_path)?;

    file.try_lock_exclusive().map_err(|_| {
        anyhow::anyhow!(
            "Another instance of flowmux is already running for tmux session '{session}'."
        )
    })?;

    Ok(file)
}

#[tokio::main]
async fn main() -> Result<()> {
    // Parse CLI
    let cli = Cli::parse();
    let worktrees_base = resolve_worktrees_base(cli.git_worktrees_location);

    // Ensure only one instance runs per tmux session.
    let _session_lock = acquire_session_lock(&cli.tmux_session)?;

    // Probe $PATH for agent binaries
    let discovered = DiscoveredAgents::probe();

    // Load global (cross-session) config
    let global_config = GlobalConfig::load()?;

    // Initialise the tmux session name before any tmux operations.
    tmux::init(&cli.tmux_session);

    // Ensure the tmux session exists (starts the server if needed)
    tmux::ensure_session()?;

    // Load persisted config for this session
    let mut config = Config::load(&cli.tmux_session)?;

    // Resolve enabled agents: CLI overrides global config.
    let enabled_agents = cli.enabled_agents.or_else(|| global_config.enabled_agents.clone());

    // Validate and warn about unknown agent names.
    if let Some(ref names) = enabled_agents {
        for name in names {
            if AgentType::from_name(name).is_none() {
                eprintln!("warning: unknown agent type '{}' in enabled_agents", name);
            }
        }
    }

    // Build AgentRunner which owns all agent lifecycle logic.
    let mut runner = AgentRunner::new(
        discovered,
        global_config,
        cli.tmux_session.clone(),
        worktrees_base,
        enabled_agents,
    );

    if runner.available_agent_types().is_empty() {
        eprintln!("error: no agents available (none discovered or all filtered out by enabled_agents)");
        std::process::exit(1);
    }

    // Auto-resume any agents whose tmux pane died (e.g. after a tmux server
    // restart).  Uses AgentRunner::restart so Claude agents are skipped
    // gracefully (restart returns Err for Claude).
    let mut config_dirty = false;
    for agent_config in config.agents.iter_mut() {
        if !tmux::is_alive(&agent_config.pane) {
            if let Ok((updated_config, _adapter)) = runner.restart(agent_config).await {
                *agent_config = updated_config;
                config_dirty = true;
            }
            // On failure (including Claude agents) the config is left unchanged.
        }
    }
    if config_dirty {
        let _ = config.save();
    }

    // Reconstruct agents and adapters from stored config.
    let mut agents: Vec<AgentEntry> = Vec::new();
    let mut agent_adapters: Vec<Box<dyn agents::AgentAdapter>> = Vec::new();

    for agent_config in &config.agents {
        let adapter = runner.restore(agent_config);
        // Eagerly populate meta from the adapter so the dashboard shows
        // meaningful data on the very first frame, before any tick fires.
        let meta = AgentMeta {
            status: adapter.get_status().await,
            context: adapter.get_context().await,
            first_prompt: adapter.get_first_prompt().await,
            last_model_response: adapter.get_last_model_response().await,
            model_name: adapter.get_model_name().await,
            total_work_ms: adapter.get_total_work_ms().await,
            status_changed_at: None,
        };
        agents.push(AgentEntry {
            config: agent_config.clone(),
            meta,
        });
        agent_adapters.push(adapter);
    }

    // Build App and spawn background tasks
    let host_colors = match host_terminal::probe_host_colors() {
        Ok(colors) => colors,
        Err(e) => {
            eprintln!("Warning: failed to probe host terminal colors: {}", e);
            host_terminal::HostColors::default()
        }
    };
    let mut app = App::new(config, agents, agent_adapters, runner, host_colors);
    crossterm::terminal::enable_raw_mode()?;
    app.spawn_tasks();

    tui::run(|mut terminal| async move {
        loop {
            // Draw only when state has changed since the last frame.
            if app.dirty {
                app.dirty = false;

                // Detect status count changes on every render frame (catches
                // changes from both dashboard tick and agent view tick).
                let current_running = app
                    .agents
                    .iter()
                    .filter(|a| matches!(a.meta.status, models::AgentStatus::Running))
                    .count();
                let current_waiting = app
                    .agents
                    .iter()
                    .filter(|a| matches!(a.meta.status, models::AgentStatus::WaitingForInput))
                    .count();

                if app.notification.initialized {
                    let running_decrease =
                        app.notification.prev_running.saturating_sub(current_running);
                    let waiting_increase =
                        current_waiting.saturating_sub(app.notification.prev_waiting);

                    if running_decrease > waiting_increase {
                        app.notification.running_blink = Some(std::time::Instant::now());
                    }
                    if current_waiting > app.notification.prev_waiting {
                        app.notification.waiting_blink = Some(std::time::Instant::now());
                    }
                }
                app.notification.prev_running = current_running;
                app.notification.prev_waiting = current_waiting;
                app.notification.initialized = true;

                let state = app.state.clone();
                let blink_running = app.notification.should_render_blink_running();
                let blink_waiting = app.notification.should_render_blink_waiting();
                terminal.draw(|f| {
                    let area = f.area();
                    match &state {
                        app::AppState::Dashboard => {
                            ui::dashboard::render_dashboard(
                                f,
                                area,
                                &app.agents,
                                app.selected,
                                &app.card_scroll,
                                &mut app.card_response_heights,
                                &mut app.card_response_widths,
                                false,
                                blink_running,
                                blink_waiting,
                            );
                        }
                        app::AppState::AgentView(idx) => {
                            if let Some(entry) = app.agents.get(*idx) {
                                ui::agent_view::render_agent_view(
                                    f,
                                    area,
                                    &app.agent_view_state,
                                    entry,
                                    &app.agents,
                                    app.host_colors,
                                    blink_running,
                                    blink_waiting,
                                );
                            }
                        }
                        app::AppState::CreateAgentDialog => {
                            ui::dashboard::render_dashboard(
                                f,
                                area,
                                &app.agents,
                                app.selected,
                                &app.card_scroll,
                                &mut app.card_response_heights,
                                &mut app.card_response_widths,
                                true,
                                blink_running,
                                blink_waiting,
                            );
                            ui::create_agent::render_create_agent(f, area, &app.create_state);
                        }
                        app::AppState::RemoveAgentDialog(remove_state) => {
                            ui::dashboard::render_dashboard(
                                f,
                                area,
                                &app.agents,
                                app.selected,
                                &app.card_scroll,
                                &mut app.card_response_heights,
                                &mut app.card_response_widths,
                                true,
                                blink_running,
                                blink_waiting,
                            );
                            let name = app
                                .agents
                                .get(remove_state.idx)
                                .map(|e| e.config.name.as_str())
                                .unwrap_or("");
                            let has_worktree = app
                                .agents
                                .get(remove_state.idx)
                                .and_then(|e| e.config.git_repo_root.as_ref())
                                .is_some();
                            ui::remove_agent::render_remove_agent(
                                f,
                                area,
                                name,
                                has_worktree,
                                remove_state.remove_worktree,
                                remove_state.stop_agent,
                                remove_state.focus,
                            );
                        }
                        app::AppState::GitViewer(gv) => {
                            if let Some(entry) = app.agents.get(gv.agent_idx) {
                                ui::git_viewer::render_git_viewer(
                                    f,
                                    area,
                                    gv,
                                    entry,
                                    &app.agents,
                                    app.host_colors,
                                    blink_running,
                                    blink_waiting,
                                );
                            }
                        }
                        app::AppState::TerminalView(tv) => {
                            if let Some(entry) = app.agents.get(tv.agent_idx) {
                                ui::terminal_view::render_terminal_view(
                                    f,
                                    area,
                                    tv,
                                    entry,
                                    &app.agents,
                                    app.host_colors,
                                    blink_running,
                                    blink_waiting,
                                );
                            }
                        }
                    }
                })?;
            }

            // Wait for next event and dispatch
            let should_continue = if let Some(event) = app.rx.recv().await {
                app.handle_event(event).await
            } else {
                false
            };

            if !should_continue {
                break;
            }
        }
        Ok(())
    })
    .await
}
