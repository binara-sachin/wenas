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
use ratatui::crossterm::event::{self, Event, MouseEvent, MouseEventKind};
use ratatui::crossterm::execute;
use ratatui::crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode, size,
};
use std::io::{self, Stdout};
use std::io::{IsTerminal, Write};
use std::os::fd::{AsRawFd, IntoRawFd};
use std::path::Path;
use std::sync::OnceLock;
use std::time::Duration;

/// Press, release and drag reports, SGR encoded. crossterm's `EnableMouseCapture`
/// also asks for every pointer movement (`?1003`), which nothing here needs.
const MOUSE_ON: &[u8] = b"\x1b[?1002h\x1b[?1006h";
const MOUSE_OFF: &[u8] = b"\x1b[?1006l\x1b[?1002l";

/// Rows per wheel event. Terminals already scale these: Ghostty sends one per row
/// of trackpad travel and three per mouse wheel notch.
const WHEEL_LINES: isize = 1;
/// Columns per sideways wheel event; trackpads send many of these in a swipe.
const WHEEL_COLUMNS: isize = 2;
/// Most events handled between two frames, so a flood can't hold back the screen.
const MAX_BATCH: usize = 256;

fn set_mouse(on: bool) {
    let mut out = io::stdout();
    let _ = out.write_all(if on { MOUSE_ON } else { MOUSE_OFF });
    let _ = out.flush();
}

/// What a fatal signal restores: the terminal device and its original settings.
static TTY_STATE: OnceLock<(i32, libc::termios)> = OnceLock::new();

/// Put the terminal back and die of the same signal. Only async-signal-safe
/// calls here: `write`, `tcsetattr` and `raise`, plus an atomic read in `get`.
extern "C" fn restore_and_die(signal: libc::c_int) {
    const RESET: &[u8] = b"\x1b[?1006l\x1b[?1002l\x1b[?1049l\x1b[?25h";
    // SAFETY: the buffer and termios are valid for the duration of each call, and
    // `SA_RESETHAND | SA_NODEFER` make the `raise` take the default action.
    unsafe {
        libc::write(libc::STDOUT_FILENO, RESET.as_ptr().cast(), RESET.len());
        if let Some((fd, termios)) = TTY_STATE.get() {
            libc::tcsetattr(*fd, libc::TCSANOW, termios);
        }
        libc::raise(signal);
    }
}

/// Without this, a killed wenas leaves mouse reporting on and every pointer
/// click prints escape codes into the shell. Call before entering raw mode.
fn restore_on_fatal_signals() {
    if let Ok(tty) = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open("/dev/tty")
    {
        // SAFETY: `termios` is plain data that `tcgetattr` fills in before it is read.
        unsafe {
            let mut termios = std::mem::zeroed();
            if libc::tcgetattr(tty.as_raw_fd(), &mut termios) == 0 {
                // Kept open for the life of the process, for the handler.
                let _ = TTY_STATE.set((tty.into_raw_fd(), termios));
            }
        }
    }
    // SAFETY: `action` is fully initialised and the handler is async-signal-safe.
    unsafe {
        let mut action: libc::sigaction = std::mem::zeroed();
        action.sa_sigaction = restore_and_die as *const () as usize;
        action.sa_flags = libc::SA_RESETHAND | libc::SA_NODEFER;
        libc::sigemptyset(&mut action.sa_mask);
        for signal in [libc::SIGTERM, libc::SIGHUP] {
            libc::sigaction(signal, &action, std::ptr::null_mut());
        }
    }
}

/// Restores the terminal when dropped, including on panic unwinding.
pub struct TerminalGuard;

impl TerminalGuard {
    /// Raw mode only; call `enter_screen` after any terminal queries.
    pub fn raw() -> io::Result<TerminalGuard> {
        restore_on_fatal_signals();
        enable_raw_mode()?;
        let default_hook = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            restore();
            default_hook(info);
        }));
        Ok(TerminalGuard)
    }

    /// Mouse reports start here, after the theme query, so they can't land in its reply.
    pub fn enter_screen(&self, mouse: bool) -> io::Result<Terminal<CrosstermBackend<Stdout>>> {
        execute!(io::stdout(), EnterAlternateScreen)?;
            if mouse {
            set_mouse(true);
        }
        Terminal::new(CrosstermBackend::new(io::stdout()))
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        restore();
    }
}

fn restore() {
    set_mouse(false);
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
    /// Wheel movement, summed over a burst of events. Returns whether anything changed.
    fn on_scroll(&mut self, lines: isize, columns: isize) -> bool;
    /// Any other mouse event. Returns whether anything changed.
    fn on_mouse(&mut self, event: MouseEvent) -> bool;
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
    fn on_scroll(&mut self, lines: isize, columns: isize) -> bool {
        App::on_scroll(self, lines, columns)
    }
    fn on_mouse(&mut self, event: MouseEvent) -> bool {
        App::on_mouse(self, event)
    }
    fn tick(&mut self) {
        App::tick(self);
    }
    fn polling(&self) -> bool {
        self.session.is_busy()
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
    mouse: bool,
) -> io::Result<Outcome> {
    let mut redraw = true;
    loop {
        ui.tick();
        if redraw {
            terminal.draw(|f| ui.draw(f))?;
        }
        // While something is animating, wake up regularly instead of blocking.
        let animating = ui.polling();
        if animating && !event::poll(Duration::from_millis(60))? {
            redraw = true;
            continue;
        }
        redraw = handle_events(ui)? || animating;
        if let Some(req) = ui.editor_request() {
            let result =
                with_terminal_suspended(terminal, mouse, || spawn_editor(&resolve_editor(), &req));
            ui.finish_editor(req.side, result);
            redraw = true;
        }
        if let Some(o) = ui.outcome() {
            return Ok(o);
        }
    }
}

/// How far a wheel event moves, as (lines, columns).
fn wheel_delta(kind: MouseEventKind) -> Option<(isize, isize)> {
    match kind {
        MouseEventKind::ScrollUp => Some((-WHEEL_LINES, 0)),
        MouseEventKind::ScrollDown => Some((WHEEL_LINES, 0)),
        MouseEventKind::ScrollLeft => Some((0, -WHEEL_COLUMNS)),
        MouseEventKind::ScrollRight => Some((0, WHEEL_COLUMNS)),
        _ => None,
    }
}

/// Handle the next event and any mouse events queued behind it, so a trackpad
/// swipe costs one frame and scrolling stops when the swipe does. Wheel movement
/// is applied before whatever follows it, keeping events in order. A key always
/// ends the batch, because it may ask for the editor or to quit. Returns whether
/// the screen needs drawing.
fn handle_events<U: Ui>(ui: &mut U) -> io::Result<bool> {
    let mut redraw = false;
    let (mut lines, mut columns) = (0, 0);
    for _ in 0..MAX_BATCH {
        match event::read()? {
            Event::Mouse(m) => match wheel_delta(m.kind) {
                Some((l, c)) => {
                    lines += l;
                    columns += c;
                }
                None => {
                    redraw |= ui.on_scroll(lines, columns);
                    (lines, columns) = (0, 0);
                    redraw |= ui.on_mouse(m);
                }
            },
            Event::Key(k) => {
                ui.on_scroll(lines, columns);
                ui.on_key(k);
                return Ok(true);
            }
            Event::Resize(..) => redraw = true,
            _ => {}
        }
        if !event::poll(Duration::ZERO)? {
            break;
        }
    }
    redraw |= ui.on_scroll(lines, columns);
    Ok(redraw)
}

fn with_terminal_suspended<T>(
    terminal: &mut Terminal<CrosstermBackend<Stdout>>,
    mouse: bool,
    f: impl FnOnce() -> T,
) -> T {
    set_mouse(false);
    let _ = execute!(io::stdout(), LeaveAlternateScreen);
    let _ = disable_raw_mode();
    let out = f();
    let _ = enable_raw_mode();
    let _ = execute!(io::stdout(), EnterAlternateScreen);
    if mouse {
        set_mouse(true);
    }
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
    let mut terminal = guard.enter_screen(cfg.mouse)?;
    let mut ui = build(theme, terminal_size());
    let outcome = run_loop(&mut terminal, &mut ui, cfg.mouse);
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
