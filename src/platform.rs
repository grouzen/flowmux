use std::process::{Command, Output};

pub fn logged_output(mut command: Command, context: &str) -> std::io::Result<Output> {
    let display = command_display(&command);
    let output = command.output();
    match &output {
        Ok(output) => log_command_output(context, &display, output),
        Err(error) => log::warn!("{context}: failed to run `{display}`: {error}"),
    }
    output
}

pub fn process_command_line(pid: i32) -> Option<String> {
    #[cfg(target_os = "linux")]
    {
        let bytes = std::fs::read(format!("/proc/{pid}/cmdline")).ok()?;
        if bytes.is_empty() {
            return None;
        }
        Some(
            bytes
                .split(|byte| *byte == 0)
                .filter(|part| !part.is_empty())
                .map(|part| String::from_utf8_lossy(part))
                .collect::<Vec<_>>()
                .join("\0"),
        )
    }

    #[cfg(all(unix, not(target_os = "linux")))]
    {
        let mut command = Command::new("ps");
        command.args(["-p", &pid.to_string(), "-o", "command="]);
        let output = logged_output(command, "inspect process command line").ok()?;
        if !output.status.success() {
            return None;
        }
        let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if stdout.is_empty() {
            None
        } else {
            Some(stdout)
        }
    }

    #[cfg(not(unix))]
    {
        let _ = pid;
        None
    }
}

pub fn pids_listening_on_tcp_port(port: u16) -> Vec<i32> {
    #[cfg(unix)]
    {
        let mut command = Command::new("lsof");
        command.args(["-nP", &format!("-iTCP:{port}"), "-sTCP:LISTEN", "-t"]);
        let Ok(output) = logged_output(command, "lookup TCP listener owner") else {
            return Vec::new();
        };
        if !output.status.success() {
            return Vec::new();
        }
        parse_pid_lines(&String::from_utf8_lossy(&output.stdout))
    }

    #[cfg(not(unix))]
    {
        let _ = port;
        Vec::new()
    }
}

pub fn signal_pid(pid: i32, signal: i32) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        let result = unsafe { libc::kill(pid, signal) };
        if result == 0 {
            Ok(())
        } else {
            Err(std::io::Error::last_os_error())
        }
    }

    #[cfg(not(unix))]
    {
        let _ = (pid, signal);
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "signals are not supported on this platform",
        ))
    }
}

pub fn parse_pid_lines(output: &str) -> Vec<i32> {
    output
        .lines()
        .filter_map(|line| line.trim().parse::<i32>().ok())
        .collect()
}

fn command_display(command: &Command) -> String {
    let mut parts = Vec::new();
    parts.push(command.get_program().to_string_lossy().into_owned());
    parts.extend(
        command
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned()),
    );
    parts.join(" ")
}

fn log_command_output(context: &str, display: &str, output: &Output) {
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    if output.status.success() {
        log::debug!("{context}: `{display}` exited with {}", output.status);
    } else {
        log::warn!("{context}: `{display}` exited with {}", output.status);
    }
    if !stdout.trim().is_empty() {
        log::debug!("{context}: stdout: {}", stdout.trim());
    }
    if !stderr.trim().is_empty() {
        if output.status.success() {
            log::debug!("{context}: stderr: {}", stderr.trim());
        } else {
            log::warn!("{context}: stderr: {}", stderr.trim());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_pid_lines_keeps_valid_pids_only() {
        assert_eq!(parse_pid_lines("123\n 456 \nnope\n\n"), vec![123, 456]);
    }

    #[test]
    fn parse_pid_lines_allows_empty_output() {
        assert!(parse_pid_lines("").is_empty());
    }
}
