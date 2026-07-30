mod agent_discovery;
mod agents;
mod app;
mod config;
mod ghostty;
mod git;
mod global_config;
mod host_terminal;
mod launch;
mod logging;
mod model_registry;
mod models;
mod platform;
mod runner;
mod tmux;
mod tui;
mod ui;

use std::path::PathBuf;

use anyhow::Result;
use clap::{Parser, Subcommand};

use agent_discovery::DiscoveredAgents;
use app::App;
use config::Config;
use global_config::GlobalConfig;
use models::AgentType;
use runner::AgentRunner;

/// flowmux — multi-agent TUI dashboard
#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,

    /// Name of the tmux session to use
    #[arg(long, default_value = "flowmux")]
    tmux_session: String,

    /// Base directory for git worktrees created by flowmux.
    /// Defaults to ~/.local/share/flowmux/worktrees
    #[arg(long)]
    git_worktrees_location: Option<PathBuf>,

    /// Comma-separated list of agent types to enable (e.g. "opencode,claude,codex,pi").
    /// Overrides the global config's `enabled_agents` setting.
    #[arg(long, value_delimiter = ',')]
    enabled_agents: Option<Vec<String>>,
}

#[derive(Subcommand, Debug, Clone)]
enum Commands {
    Launch {
        #[command(subcommand)]
        command: launch::LaunchCommand,
    },
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
        .truncate(false)
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
    let cli = Cli::parse();

    if let Some(Commands::Launch { command }) = cli.command.clone() {
        return launch::run(command).await;
    }

    let log_path = logging::init(&cli.tmux_session)?;
    log::debug!("flowmux logs: {}", log_path.display());

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
    let config = Config::load(&cli.tmux_session)?;

    // Resolve enabled agents: CLI overrides global config.
    let enabled_agents = cli
        .enabled_agents
        .or_else(|| global_config.enabled_agents.clone());

    // Validate and warn about unknown agent names.
    if let Some(ref names) = enabled_agents {
        for name in names {
            if AgentType::from_name(name).is_none() {
                log::warn!("unknown agent type '{}' in enabled_agents", name);
            }
        }
    }

    // Build AgentRunner which owns all agent lifecycle logic.
    let runner = AgentRunner::new(
        discovered,
        global_config,
        cli.tmux_session.clone(),
        worktrees_base,
        enabled_agents,
    );

    if runner.available_agent_types().is_empty() {
        eprintln!(
            "error: no agents available (none discovered or all filtered out by enabled_agents)"
        );
        std::process::exit(1);
    }

    // Build the first dashboard frame now. Persisted agents are restored after
    // the terminal enters its alternate screen so startup remains responsive.
    let host_colors = match host_terminal::probe_host_colors() {
        Ok(colors) => colors,
        Err(e) => {
            log::warn!("failed to probe host terminal colors: {e}");
            host_terminal::HostColors::default()
        }
    };
    let mut app = App::new_with_startup_placeholders(config, runner, host_colors);
    app.spawn_tasks();

    tui::run(|mut terminal| async move {
        app.begin_startup_restore();
        loop {
            // Draw only when state has changed since the last frame.
            if app.dirty {
                app.dirty = false;

                // Detect status count changes on every render frame (catches
                // changes from both dashboard tick and agent view tick).
                let status_counts = app.global_status_counts();
                app.notification.observe(status_counts);

                let state = app.state.clone();
                let blink_running = app.notification.should_render_blink_running();
                let blink_waiting = app.notification.should_render_blink_waiting();
                let copy_notice = app.copy_feedback_badge();
                let selection = app.current_copy_selection_range();
                terminal.draw(|f| {
                    let area = f.area();
                    let theme = app.theme();
                    match &state {
                        app::AppState::Dashboard => {
                            let visible_indices = app.visible_agent_indices();
                            ui::dashboard::render_dashboard(
                                f,
                                area,
                                theme,
                                &app.agents,
                                &visible_indices,
                                Some(app.selected),
                                &app.config.projects,
                                app.active_project_idx,
                                &app.card_scroll,
                                &app.card_horizontal_scroll,
                                &mut app.card_response_heights,
                                &mut app.card_response_widths,
                                &mut app.card_response_content_heights,
                                &mut app.card_response_content_widths,
                                false,
                                status_counts,
                                blink_running,
                                blink_waiting,
                                app.startup_progress.as_ref(),
                            );
                        }
                        app::AppState::StartupGuide(guide) => {
                            let visible_indices = app.visible_agent_indices();
                            ui::dashboard::render_dashboard(
                                f,
                                area,
                                theme,
                                &app.agents,
                                &visible_indices,
                                Some(app.selected),
                                &app.config.projects,
                                app.active_project_idx,
                                &app.card_scroll,
                                &app.card_horizontal_scroll,
                                &mut app.card_response_heights,
                                &mut app.card_response_widths,
                                &mut app.card_response_content_heights,
                                &mut app.card_response_content_widths,
                                true,
                                status_counts,
                                blink_running,
                                blink_waiting,
                                app.startup_progress.as_ref(),
                            );
                            ui::startup_guide::render_startup_guide(f, area, theme, guide);
                        }
                        app::AppState::SettingsDialog(settings_state) => {
                            let visible_indices = app.visible_agent_indices();
                            ui::dashboard::render_dashboard(
                                f,
                                area,
                                theme,
                                &app.agents,
                                &visible_indices,
                                Some(app.selected),
                                &app.config.projects,
                                app.active_project_idx,
                                &app.card_scroll,
                                &app.card_horizontal_scroll,
                                &mut app.card_response_heights,
                                &mut app.card_response_widths,
                                &mut app.card_response_content_heights,
                                &mut app.card_response_content_widths,
                                true,
                                status_counts,
                                blink_running,
                                blink_waiting,
                                app.startup_progress.as_ref(),
                            );
                            ui::settings::render_settings(f, area, theme, settings_state);
                        }
                        app::AppState::AgentView(idx) => {
                            if let Some(entry) = app.agents.get(*idx) {
                                ui::agent_view::render_agent_view(
                                    f,
                                    area,
                                    theme,
                                    &app.agent_view_state,
                                    entry,
                                    status_counts,
                                    app.host_colors,
                                    blink_running,
                                    blink_waiting,
                                    copy_notice
                                        .as_ref()
                                        .map(|(text, color)| (text.as_str(), *color)),
                                    selection,
                                    app.startup_state(*idx),
                                );
                            }
                        }
                        app::AppState::CreateAgentDialog => {
                            let visible_indices = app.visible_agent_indices();
                            ui::dashboard::render_dashboard(
                                f,
                                area,
                                theme,
                                &app.agents,
                                &visible_indices,
                                Some(app.selected),
                                &app.config.projects,
                                app.active_project_idx,
                                &app.card_scroll,
                                &app.card_horizontal_scroll,
                                &mut app.card_response_heights,
                                &mut app.card_response_widths,
                                &mut app.card_response_content_heights,
                                &mut app.card_response_content_widths,
                                true,
                                status_counts,
                                blink_running,
                                blink_waiting,
                                app.startup_progress.as_ref(),
                            );
                            ui::create_agent::render_create_agent(
                                f,
                                area,
                                theme,
                                &app.create_state,
                            );
                        }
                        app::AppState::CreateProjectDialog => {
                            let visible_indices = app.visible_agent_indices();
                            ui::dashboard::render_dashboard(
                                f,
                                area,
                                theme,
                                &app.agents,
                                &visible_indices,
                                Some(app.selected),
                                &app.config.projects,
                                app.active_project_idx,
                                &app.card_scroll,
                                &app.card_horizontal_scroll,
                                &mut app.card_response_heights,
                                &mut app.card_response_widths,
                                &mut app.card_response_content_heights,
                                &mut app.card_response_content_widths,
                                true,
                                status_counts,
                                blink_running,
                                blink_waiting,
                                app.startup_progress.as_ref(),
                            );
                            ui::create_project::render_create_project(
                                f,
                                area,
                                theme,
                                &app.create_project_state,
                            );
                        }
                        app::AppState::RemoveAgentDialog(remove_state) => {
                            let visible_indices = app.visible_agent_indices();
                            ui::dashboard::render_dashboard(
                                f,
                                area,
                                theme,
                                &app.agents,
                                &visible_indices,
                                Some(app.selected),
                                &app.config.projects,
                                app.active_project_idx,
                                &app.card_scroll,
                                &app.card_horizontal_scroll,
                                &mut app.card_response_heights,
                                &mut app.card_response_widths,
                                &mut app.card_response_content_heights,
                                &mut app.card_response_content_widths,
                                true,
                                status_counts,
                                blink_running,
                                blink_waiting,
                                app.startup_progress.as_ref(),
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
                                theme,
                                name,
                                has_worktree,
                                remove_state.remove_worktree,
                                remove_state.stop_agent,
                                remove_state.focus,
                            );
                        }
                        app::AppState::RemoveProjectDialog(remove_state) => {
                            let visible_indices = app.visible_agent_indices();
                            ui::dashboard::render_dashboard(
                                f,
                                area,
                                theme,
                                &app.agents,
                                &visible_indices,
                                Some(app.selected),
                                &app.config.projects,
                                app.active_project_idx,
                                &app.card_scroll,
                                &app.card_horizontal_scroll,
                                &mut app.card_response_heights,
                                &mut app.card_response_widths,
                                &mut app.card_response_content_heights,
                                &mut app.card_response_content_widths,
                                true,
                                status_counts,
                                blink_running,
                                blink_waiting,
                                app.startup_progress.as_ref(),
                            );
                            ui::remove_project::render_remove_project(
                                f,
                                area,
                                theme,
                                &remove_state.name,
                                remove_state.agent_count,
                                remove_state.confirm_remove_agents,
                            );
                        }
                        app::AppState::GitViewer(gv) => {
                            if let Some(entry) = app.agents.get(gv.agent_idx) {
                                ui::git_viewer::render_git_viewer(
                                    f,
                                    area,
                                    theme,
                                    gv,
                                    entry,
                                    status_counts,
                                    app.host_colors,
                                    blink_running,
                                    blink_waiting,
                                    copy_notice
                                        .as_ref()
                                        .map(|(text, color)| (text.as_str(), *color)),
                                    selection,
                                );
                            }
                        }
                        app::AppState::TerminalView(tv) => {
                            if let Some(entry) = app.agents.get(tv.agent_idx) {
                                ui::terminal_view::render_terminal_view(
                                    f,
                                    area,
                                    theme,
                                    tv,
                                    entry,
                                    status_counts,
                                    app.host_colors,
                                    blink_running,
                                    blink_waiting,
                                    copy_notice
                                        .as_ref()
                                        .map(|(text, color)| (text.as_str(), *color)),
                                    selection,
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
