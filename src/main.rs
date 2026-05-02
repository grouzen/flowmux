mod agent_discovery;
mod agents;
mod app;
mod config;
mod global_config;
mod models;
mod tmux;
mod tui;
mod ui;

use anyhow::Result;
use clap::Parser;

use agent_discovery::DiscoveredAgents;
use agents::opencode::OpenCodeAdapter;
use agents::AgentAdapter;
use app::App;
use config::Config;
use global_config::GlobalConfig;
use models::{AgentEntry, AgentMeta, AgentStatus};

/// stable — multi-agent TUI dashboard
#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Cli {
    /// Name of the tmux session to use
    #[arg(long, default_value = "stable")]
    tmux_session: String,
}

#[tokio::main]
async fn main() -> Result<()> {
    // Parse CLI
    let cli = Cli::parse();

    // Probe $PATH for agent binaries
    let discovered = DiscoveredAgents::probe();

    // Load global (cross-session) config
    let _global_config = GlobalConfig::load()?;

    // Initialise the tmux session name before any tmux operations.
    tmux::init(&cli.tmux_session);

    // Ensure the tmux session exists (starts the server if needed)
    tmux::ensure_session()?;

    // Load persisted config for this session
    let mut config = Config::load(&cli.tmux_session)?;

    // Auto-resume any agents whose tmux pane died (e.g. after a tmux server
    // restart).  For each dead pane we open a new tmux window and relaunch
    // opencode with `--session <id>` so the existing session is preserved.
    let mut config_dirty = false;
    for agent_config in config.agents.iter_mut() {
        if !tmux::is_alive(&agent_config.pane) {
            match OpenCodeAdapter::restart(
                &agent_config.directory,
                &agent_config.name,
                agent_config.session_id.as_deref(),
            )
            .await
            {
                Ok((_adapter, window_index, new_port)) => {
                    agent_config.pane = format!("{}:{}.0", tmux::session_name(), window_index);
                    agent_config.port = new_port;
                    config_dirty = true;
                }
                Err(_) => {
                    // Could not restart this agent — leave config unchanged so
                    // the user can manually restart or remove it from the UI.
                }
            }
        }
    }
    if config_dirty {
        let _ = config.save();
    }

    // Reconstruct agents and adapters from stored config
    let mut agents: Vec<AgentEntry> = Vec::new();
    let mut adapters: Vec<Box<dyn AgentAdapter>> = Vec::new();

    for agent_config in &config.agents {
        let adapter = OpenCodeAdapter::new(agent_config.port, agent_config.session_id.clone());
        agents.push(AgentEntry {
            config: agent_config.clone(),
            meta: AgentMeta {
                status: AgentStatus::Unknown,
                context: None,
                first_prompt: None,
                last_model_response: None,
                model_name: None,
                total_work_ms: 0,
            },
        });
        adapters.push(Box::new(adapter));
    }

    // Build App and spawn background tasks
    let mut app = App::new(config, agents, adapters, discovered);
    app.spawn_tasks();

    tui::run(|mut terminal| async move {
        loop {
            // Draw only when state has changed since the last frame.
            if app.dirty {
                app.dirty = false;
                let state = app.state.clone();
                terminal.draw(|f| {
                    let area = f.area();
                    match &state {
                        app::AppState::Dashboard => {
                            ui::dashboard::render_dashboard(f, area, &app.agents, app.selected, &app.card_scroll, &mut app.card_response_heights, &mut app.card_response_widths, false);
                        }
                        app::AppState::AgentView(idx) => {
                            if let Some(entry) = app.agents.get(*idx) {
                                ui::agent_view::render_agent_view(
                                    f,
                                    area,
                                    &app.agent_view_state,
                                    entry,
                                    &app.agents,
                                );
                            }
                        }
                        app::AppState::CreateAgentDialog => {
                            ui::dashboard::render_dashboard(f, area, &app.agents, app.selected, &app.card_scroll, &mut app.card_response_heights, &mut app.card_response_widths, true);
                            ui::create_agent::render_create_agent(f, area, &app.create_state);
                        }
                        app::AppState::RemoveAgentDialog(idx) => {
                            ui::dashboard::render_dashboard(f, area, &app.agents, app.selected, &app.card_scroll, &mut app.card_response_heights, &mut app.card_response_widths, true);
                            let name = app
                                .agents
                                .get(*idx)
                                .map(|e| e.config.name.as_str())
                                .unwrap_or("");
                            ui::remove_agent::render_remove_agent(f, area, name);
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
