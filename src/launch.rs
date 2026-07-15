use std::ffi::OsString;
use std::fs;
use std::path::PathBuf;
use std::process::{Command, ExitStatus, Stdio};
use std::time::Duration;

use anyhow::{Context, Result, anyhow};
use clap::{Args, Subcommand};
use tokio::time::sleep;

use crate::logging;
use crate::tmux;

#[derive(Subcommand, Debug, Clone)]
pub enum LaunchCommand {
    /// Internal helper used to launch agent processes behind a compact shell command.
    Opencode(LaunchOpencodeArgs),
    /// Internal helper used to launch agent processes behind a compact shell command.
    Claude(LaunchClaudeArgs),
    /// Internal helper used to launch agent processes behind a compact shell command.
    Codex(LaunchCodexArgs),
    /// Internal helper used to launch agent processes behind a compact shell command.
    Pi(LaunchPiArgs),
}

#[derive(Args, Debug, Clone)]
pub struct LaunchOpencodeArgs {
    #[arg(long)]
    pub port: u16,
    #[arg(long)]
    pub session_id: Option<String>,
}

#[derive(Args, Debug, Clone)]
pub struct LaunchClaudeArgs {
    #[arg(long)]
    pub flowmux_agent_id: String,
    #[arg(long)]
    pub session_id: Option<String>,
}

#[derive(Args, Debug, Clone)]
pub struct LaunchCodexArgs {
    #[arg(long)]
    pub port: u16,
    #[arg(long)]
    pub session_id: Option<String>,
}

#[derive(Args, Debug, Clone)]
pub struct LaunchPiArgs {
    #[arg(long)]
    pub flowmux_agent_id: String,
    #[arg(long)]
    pub hook_port: u16,
    #[arg(long)]
    pub session_id: Option<String>,
}

pub async fn run(command: LaunchCommand) -> Result<()> {
    match command {
        LaunchCommand::Opencode(args) => {
            let mut command = Command::new("opencode");
            command.arg("--port").arg(args.port.to_string());
            if let Some(session_id) = args.session_id {
                command.arg("--session").arg(session_id);
            }
            let status = spawn_foreground(&mut command)
                .with_context(|| format!("failed to launch opencode on port {}", args.port))?;
            exit_with_status(status);
        }
        LaunchCommand::Claude(args) => {
            let mut command = Command::new("claude");
            command.env("FLOWMUX_AGENT_ID", &args.flowmux_agent_id);
            if let Some(session_id) = args.session_id {
                command.arg("--resume").arg(session_id);
            }
            let status = spawn_foreground(&mut command).with_context(|| {
                format!(
                    "failed to launch claude for FLOWMUX_AGENT_ID={}",
                    args.flowmux_agent_id
                )
            })?;
            exit_with_status(status);
        }
        LaunchCommand::Codex(args) => run_codex(args).await?,
        LaunchCommand::Pi(args) => run_pi(args)?,
    }

    Ok(())
}

fn run_pi(args: LaunchPiArgs) -> Result<()> {
    let extension_path = write_pi_extension(&args.flowmux_agent_id, args.hook_port)?;
    let mut pi = Command::new("pi");
    pi.arg("--extension").arg(extension_path);
    if let Some(session_id) = args.session_id {
        pi.arg("--session").arg(session_id);
    }
    let status = spawn_foreground(&mut pi).context("failed to start pi")?;
    exit_with_status(status);
}

fn pi_extension_path(flowmux_agent_id: &str) -> PathBuf {
    std::env::temp_dir().join(format!("flowmux-pi-{flowmux_agent_id}.ts"))
}

const PI_EXTENSION_TEMPLATE: &str = include_str!("agents/pi/flowmux-extension.ts");

fn write_pi_extension(flowmux_agent_id: &str, hook_port: u16) -> Result<PathBuf> {
    let endpoint = serde_json::to_string(&format!("http://127.0.0.1:{hook_port}/hook"))?;
    let agent_id = serde_json::to_string(flowmux_agent_id)?;
    let source = PI_EXTENSION_TEMPLATE
        .replace("__FLOWMUX_ENDPOINT__", &endpoint)
        .replace("__FLOWMUX_AGENT_ID__", &agent_id);
    let path = pi_extension_path(flowmux_agent_id);
    std::fs::write(&path, source)
        .with_context(|| format!("write Pi extension {}", path.display()))?;
    Ok(path)
}

pub fn flowmux_launch_command(agent: &str, args: &[OsString]) -> String {
    let mut parts = vec![
        flowmux_invocation(),
        "--tmux-session".to_string(),
        shell_quote(tmux::session_name()),
        "launch".to_string(),
        agent.to_string(),
    ];
    parts.extend(args.iter().map(|arg| shell_quote(&arg.to_string_lossy())));
    format!("{}\n", parts.join(" "))
}

fn flowmux_invocation() -> String {
    if which::which("flowmux").is_ok() {
        return "flowmux".to_string();
    }

    let path = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("flowmux"));
    shell_quote(&path.to_string_lossy())
}

fn shell_quote(value: &str) -> String {
    if !value.is_empty()
        && value
            .bytes()
            .all(|byte| matches!(byte, b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'/' | b'.' | b'_' | b'-' | b':'))
    {
        return value.to_string();
    }

    format!("'{}'", value.replace('\'', "'\"'\"'"))
}

fn spawn_foreground(command: &mut Command) -> Result<ExitStatus> {
    command
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()
        .context("failed to start child process")
}

fn exit_with_status(status: ExitStatus) -> ! {
    std::process::exit(status.code().unwrap_or(1));
}

async fn run_codex(args: LaunchCodexArgs) -> Result<()> {
    let pid_path = server_pid_path(args.port);
    let listen_addr = format!("ws://127.0.0.1:{}", args.port);
    let log_file = logging::open_session_log_file(tmux::session_name())
        .context("failed to open shared session log for codex app-server")?;
    let log_file_err = log_file
        .try_clone()
        .context("failed to clone shared session log handle for codex app-server")?;
    let mut server = Command::new("codex")
        .args(["app-server", "--listen", &listen_addr])
        .stdin(Stdio::null())
        .stdout(Stdio::from(log_file))
        .stderr(Stdio::from(log_file_err))
        .spawn()
        .with_context(|| format!("failed to start codex app-server on {}", listen_addr))?;

    fs::write(&pid_path, format!("{}\n", server.id()))
        .with_context(|| format!("failed to write codex pid file {}", pid_path))?;

    let client = reqwest::Client::builder()
        .timeout(Duration::from_millis(500))
        .build()?;

    let mut ready = false;
    for _ in 0..25 {
        if app_server_ready(&client, args.port).await {
            ready = true;
            break;
        }
        if let Some(status) = server.try_wait()? {
            let _ = fs::remove_file(&pid_path);
            return Err(anyhow!(
                "codex app-server exited early with status {}",
                status
            ));
        }
        sleep(Duration::from_millis(200)).await;
    }

    if !ready {
        let _ = server.kill();
        let _ = server.wait();
        let _ = fs::remove_file(&pid_path);
        return Err(anyhow!(
            "codex app-server did not become available on port {}",
            args.port
        ));
    }

    let remote = format!("ws://127.0.0.1:{}", args.port);
    let mut codex = Command::new("codex");
    if let Some(session_id) = args.session_id {
        codex.args(["resume", "--remote", &remote, &session_id]);
    } else {
        codex.args(["--remote", &remote]);
    }

    let status = spawn_foreground(&mut codex)
        .with_context(|| format!("failed to start codex client against {}", remote))?;

    let _ = server.kill();
    let _ = server.wait();
    let _ = fs::remove_file(&pid_path);

    exit_with_status(status);
}

async fn app_server_ready(client: &reqwest::Client, port: u16) -> bool {
    let url = format!("http://127.0.0.1:{port}/readyz");
    client
        .get(url)
        .send()
        .await
        .is_ok_and(|response| response.status().is_success())
}

fn server_pid_path(port: u16) -> String {
    format!("/tmp/flowmux-codex-{port}.pid")
}

#[cfg(test)]
mod tests {
    use super::{flowmux_launch_command, write_pi_extension};
    use std::ffi::OsString;

    #[test]
    fn launch_command_quotes_arguments_for_shell() {
        let command = flowmux_launch_command(
            "codex",
            &[
                OsString::from("--session-id"),
                OsString::from("thread with spaces"),
            ],
        );

        assert!(command.contains("launch codex"));
        assert!(command.contains("--tmux-session"));
        assert!(command.contains("--session-id 'thread with spaces'"));
        assert!(command.ends_with('\n'));
    }

    #[test]
    fn pi_extension_reports_lifecycle_over_loopback() {
        let agent_id = format!("test-{}", uuid::Uuid::new_v4());
        let path = write_pi_extension(&agent_id, 17123).unwrap();
        let source = std::fs::read_to_string(&path).unwrap();
        let _ = std::fs::remove_file(&path);

        assert!(source.contains("pi.on(\"session_start\""));
        assert!(source.contains("pi.on(\"agent_start\""));
        assert!(source.contains("pi.on(\"agent_end\""));
        assert!(source.contains("pi.on(\"session_shutdown\""));
        assert!(source.contains("http://127.0.0.1:17123/hook"));
        assert!(source.contains(&agent_id));
        assert!(!source.contains("__FLOWMUX_ENDPOINT__"));
        assert!(!source.contains("__FLOWMUX_AGENT_ID__"));
    }
}
