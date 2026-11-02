//! Terminal setup and the event loop.

use super::app::{App, EditorRequest, Outcome};
use super::render;
use crate::ops::Side;
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::crossterm::event::{self, Event};
use ratatui::crossterm::execute;
use ratatui::crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode, size,
};
use std::io::{self, Stdout};
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

/// `$EDITOR`, then `$VISUAL`, then `vi` (spec §7.3).
pub fn resolve_editor() -> String {
    let get = |k: &str| std::env::var(k).ok().filter(|v| !v.trim().is_empty());
    get("EDITOR")
        .or_else(|| get("VISUAL"))
        .unwrap_or_else(|| "vi".into())
}

/// `<editor> +<line> <file>`. Run through `sh` so editors configured with
/// arguments (`code -w`) work; the file and line are passed as real arguments,
/// never interpolated into the command string.
pub fn editor_command(editor: &str, req: &EditorRequest) -> std::process::Command {
    let mut cmd = std::process::Command::new("sh");
    cmd.arg("-c")
        .arg(format!("{editor} \"$@\""))
        .arg("wenas")
        .arg(format!("+{}", req.line))
        .arg(&req.path);
    cmd
}

/// Run the editor to completion. `Ok(true)` when it exited successfully.
pub fn spawn_editor(editor: &str, req: &EditorRequest) -> Result<bool, String> {
    let mut cmd = editor_command(editor, req);
    // wenas's own stdin may be a pipe (`-` as an input); the editor needs the terminal.
    if let Ok(tty) = std::fs::File::open("/dev/tty") {
        cmd.stdin(tty);
    }
    cmd.status().map(|s| s.success()).map_err(|e| e.to_string())
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ops::Side;

    fn req(line: usize, path: &str) -> EditorRequest {
        EditorRequest {
            side: Side::Right,
            line,
            path: path.into(),
        }
    }

    #[test]
    fn editor_command_passes_line_and_path_as_arguments() {
        let cmd = editor_command("nvim", &req(41, "it's a file; rm -rf x.rs"));
        let args: Vec<_> = cmd
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            args,
            [
                "-c",
                "nvim \"$@\"",
                "wenas",
                "+41",
                "it's a file; rm -rf x.rs"
            ]
        );
    }

    #[test]
    fn editor_runs_with_arguments_and_reports_status() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("f.txt");
        std::fs::write(&file, "a\n").unwrap();
        let r = req(3, file.to_str().unwrap());
        // An "editor" with its own arguments that records what it was given.
        let ed = "sh -c 'echo \"$1 $2\" >> \"$2\"' ed";
        assert_eq!(spawn_editor(ed, &r), Ok(true));
        assert_eq!(
            std::fs::read_to_string(&file).unwrap(),
            format!("a\n+3 {}\n", file.display())
        );
        assert_eq!(spawn_editor("false", &r), Ok(false));
    }
}
