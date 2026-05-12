mod agent_discovery;
mod agents;
mod app;
mod config;
mod global_config;
mod models;
mod runner;
mod tmux;
mod tui;
mod ui;

use anyhow::Result;
use clap::Parser;

use agent_discovery::DiscoveredAgents;
use app::App;
use config::Config;
use global_config::GlobalConfig;
use models::{AgentEntry, AgentMeta};
use runner::AgentRunner;

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
    let global_config = GlobalConfig::load()?;

    // Initialise the tmux session name before any tmux operations.
    tmux::init(&cli.tmux_session);

    // Ensure the tmux session exists (starts the server if needed)
    tmux::ensure_session()?;

    // Load persisted config for this session
    let mut config = Config::load(&cli.tmux_session)?;

    // Build the AgentRunner which owns all agent lifecycle logic.
    let mut runner = AgentRunner::new(discovered, global_config, cli.tmux_session.clone());

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
        };
        agents.push(AgentEntry {
            config: agent_config.clone(),
            meta,
        });
        agent_adapters.push(adapter);
    }

    // Build App and spawn background tasks
    let mut app = App::new(config, agents, agent_adapters, runner);
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
