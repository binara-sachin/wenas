//! Terminal setup and the event loop.

use super::app::{App, Outcome};
use super::render;
use crate::config::Config;
use crate::editor::{EditorRequest, resolve_editor, spawn_editor};
use crate::input;
use crate::ops::Side;
use crate::theme::{Depth, Theme, resolve_dark};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::crossterm::event::{self, Event};
use ratatui::crossterm::execute;
use ratatui::crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode, size,
};
use std::io::IsTerminal;
use std::io::{self, Stdout};
use std::path::Path;
use std::time::Duration;

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

/// Something the event loop can drive: a single file view or a directory session.
pub trait Ui {
    fn draw(&mut self, f: &mut ratatui::Frame);
    fn on_key(&mut self, key: ratatui::crossterm::event::KeyEvent);
    /// Called between events while `polling()` is true (e.g. to collect scan results).
    fn tick(&mut self) {}
    /// True while the screen changes on its own and must be redrawn periodically.
    fn polling(&self) -> bool {
        false
    }
    fn outcome(&self) -> Option<Outcome>;
    fn editor_request(&mut self) -> Option<EditorRequest>;
    fn finish_editor(&mut self, side: Side, result: Result<bool, String>);
}

impl Ui for App {
    fn draw(&mut self, f: &mut ratatui::Frame) {
        render::draw(f, self);
    }
    fn on_key(&mut self, key: ratatui::crossterm::event::KeyEvent) {
        App::on_key(self, key);
    }
    fn outcome(&self) -> Option<Outcome> {
        self.outcome
    }
    fn editor_request(&mut self) -> Option<EditorRequest> {
        self.editor_request.take()
    }
    fn finish_editor(&mut self, side: Side, result: Result<bool, String>) {
        App::finish_editor(self, side, result);
    }
}

pub fn run_loop<U: Ui>(
    terminal: &mut Terminal<CrosstermBackend<Stdout>>,
    ui: &mut U,
) -> io::Result<Outcome> {
    loop {
        ui.tick();
        terminal.draw(|f| ui.draw(f))?;
        // While something is animating, wake up regularly instead of blocking.
        if ui.polling() && !event::poll(Duration::from_millis(60))? {
            continue;
        }
        match event::read()? {
            Event::Key(k) => ui.on_key(k),
            Event::Resize(..) => {}
            _ => {}
        }
        if let Some(req) = ui.editor_request() {
            let result =
                with_terminal_suspended(terminal, || spawn_editor(&resolve_editor(), &req));
            ui.finish_editor(req.side, result);
        }
        if let Some(o) = ui.outcome() {
            return Ok(o);
        }
    }
}

fn with_terminal_suspended<T>(
    terminal: &mut Terminal<CrosstermBackend<Stdout>>,
    f: impl FnOnce() -> T,
) -> T {
    let _ = execute!(io::stdout(), LeaveAlternateScreen);
    let _ = disable_raw_mode();
    let out = f();
    let _ = enable_raw_mode();
    let _ = execute!(io::stdout(), EnterAlternateScreen);
    let _ = terminal.clear();
    out
}

/// Raw mode, theme detection, alternate screen, event loop, restore.
pub fn run_interactive<U: Ui>(
    cfg: &Config,
    build: impl FnOnce(Theme, (u16, u16)) -> U,
) -> anyhow::Result<Outcome> {
    let guard = TerminalGuard::raw()?;
    // The background query must run in raw mode but before the alternate screen.
    let dark = resolve_dark(cfg.theme);
    let no_color = std::env::var_os("NO_COLOR").is_some_and(|v| !v.is_empty());
    let depth = Depth::detect();
    let mut theme = Theme::new(dark, depth, no_color);
    theme.apply_overrides(&cfg.colors, depth);
    let mut terminal = guard.enter_screen()?;
    let mut ui = build(theme, terminal_size());
    let outcome = run_loop(&mut terminal, &mut ui);
    drop(terminal);
    drop(guard);
    Ok(outcome?)
}

/// Ask before loading anything over 100 MB (spec §9). Files over 1 GB are
/// refused outright. Returns false when the user declines.
pub fn confirm_large_files(paths: &[&Path]) -> anyhow::Result<bool> {
    let mut large = Vec::new();
    for p in paths {
        if let input::SizeCheck::Large(n) =
            input::check_size(p, input::LARGE_FILE_BYTES, input::MAX_FILE_BYTES)?
        {
            large.push((p, n));
        }
    }
    if large.is_empty() {
        return Ok(true);
    }
    if !io::stdout().is_terminal() {
        // Nothing to ask and nothing to show; scripts get the plain behaviour.
        return Ok(true);
    }
    let list: Vec<String> = large
        .iter()
        .map(|(p, n)| format!("{} ({})", p.display(), input::human_size(*n)))
        .collect();
    let prompt = format!(
        "wenas: {} is large; loading may be slow. Load it anyway? [y/N] ",
        list.join(", ")
    );
    let mut tty = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open("/dev/tty")
        .map_err(|_| {
            anyhow::anyhow!(
                "{} is large and there is no terminal to ask on",
                list.join(", ")
            )
        })?;
    use std::io::{BufRead, Write};
    tty.write_all(prompt.as_bytes())?;
    let mut answer = String::new();
    io::BufReader::new(&tty).read_line(&mut answer)?;
    Ok(matches!(
        answer.trim().to_ascii_lowercase().as_str(),
        "y" | "yes"
    ))
}
