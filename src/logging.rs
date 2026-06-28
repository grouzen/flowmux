use std::fs::OpenOptions;
use std::io;
use std::path::PathBuf;

use anyhow::Context;
use env_logger::{Builder, Env, Target};

const DEFAULT_FILTER: &str = "flowmux=info,warn";

pub fn log_path(session_name: &str) -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("flowmux")
        .join("logs")
        .join(format!("{}.log", log_file_stem(session_name)))
}

pub fn init(session_name: &str) -> anyhow::Result<PathBuf> {
    let path = log_path(session_name);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("create log directory {:?}", parent))?;
    }

    let file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .with_context(|| format!("open log file {:?}", path))?;

    Builder::from_env(Env::default().default_filter_or(DEFAULT_FILTER))
        .target(Target::Pipe(Box::new(file)))
        .format_timestamp_secs()
        .try_init()
        .map_err(|error| io::Error::other(error.to_string()))
        .context("initialize logger")?;

    log::info!("logging initialized at {}", path.display());
    Ok(path)
}

fn log_file_stem(session_name: &str) -> String {
    let stem: String = session_name
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' || ch == '.' {
                ch
            } else {
                '-'
            }
        })
        .collect();
    let stem = stem.trim_matches('-');
    if stem.is_empty() {
        "flowmux".to_string()
    } else {
        stem.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn log_file_stem_sanitizes_session_name() {
        assert_eq!(log_file_stem("dev/session one"), "dev-session-one");
        assert_eq!(log_file_stem("..flowmux.."), "..flowmux..");
        assert_eq!(log_file_stem("///"), "flowmux");
    }
}
