use std::future::Future;
use std::io::{Stdout, stdout};
use std::panic;

use anyhow::Result;
use crossterm::{
    event::{DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::{Terminal, backend::CrosstermBackend};

pub type Tui = Terminal<CrosstermBackend<Stdout>>;

pub fn enter_terminal() -> Result<Tui> {
    enable_raw_mode()?;
    execute!(
        stdout(),
        EnterAlternateScreen,
        EnableMouseCapture,
        EnableBracketedPaste
    )?;
    let terminal = Terminal::new(CrosstermBackend::new(stdout()))?;
    Ok(terminal)
}

pub fn leave_terminal() -> Result<()> {
    disable_raw_mode()?;
    execute!(
        stdout(),
        LeaveAlternateScreen,
        DisableMouseCapture,
        DisableBracketedPaste
    )?;
    Ok(())
}

pub fn install_panic_hook() {
    let original = panic::take_hook();
    panic::set_hook(Box::new(move |info| {
        let _ = leave_terminal();
        original(info);
    }));
}

pub async fn run<F, Fut>(f: F) -> Result<()>
where
    F: FnOnce(Tui) -> Fut,
    Fut: Future<Output = Result<()>>,
{
    install_panic_hook();
    let terminal = enter_terminal()?;
    let result = f(terminal).await;
    leave_terminal()?;
    result
}
