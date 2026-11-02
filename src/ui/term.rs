//! Terminal setup and the event loop.

use super::app::{App, Outcome};
use super::render;
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::crossterm::event::{self, Event};
use ratatui::crossterm::execute;
use ratatui::crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode, size,
};
use std::io::{self, Stdout};

/// Restores the terminal when dropped, including on panic unwinding.
pub struct TerminalGuard;

impl TerminalGuard {
    /// Raw mode only; call `enter_screen` after any terminal queries.
    pub fn raw() -> io::Result<TerminalGuard> {
        enable_raw_mode()?;
        let default_hook = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            restore();
            default_hook(info);
        }));
        Ok(TerminalGuard)
    }

    pub fn enter_screen(&self) -> io::Result<Terminal<CrosstermBackend<Stdout>>> {
        execute!(io::stdout(), EnterAlternateScreen)?;
        Terminal::new(CrosstermBackend::new(io::stdout()))
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        restore();
    }
}

fn restore() {
    let _ = execute!(io::stdout(), LeaveAlternateScreen);
    let _ = disable_raw_mode();
}

pub fn terminal_size() -> (u16, u16) {
    size().unwrap_or((80, 24))
}

pub fn run_loop(
    terminal: &mut Terminal<CrosstermBackend<Stdout>>,
    app: &mut App,
) -> io::Result<Outcome> {
    loop {
        terminal.draw(|f| render::draw(f, app))?;
        match event::read()? {
            Event::Key(k) => app.on_key(k),
            Event::Resize(..) => {}
            _ => {}
        }
        if let Some(o) = app.outcome {
            return Ok(o);
        }
    }
}
