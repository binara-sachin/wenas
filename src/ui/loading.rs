//! The "Comparing…" screen for large inputs, shown when a diff takes more than
//! a moment. `Esc` cancels (spec §10, §7.1). When the diff arrives it becomes
//! the normal file view.

use super::app::{App, EditorRequest, Outcome, ViewConfig};
use super::render::{self, bold, hints, put, thousands};
use super::term::Ui;
use crate::diff::{Hunk, Whitespace};
use crate::document::Document;
use crate::ops::{Session, Side};
use crate::theme::Theme;
use ratatui::Frame;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::layout::Rect;
use ratatui::style::Style;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, TryRecvError};
use std::time::Instant;

const SPINNER: [&str; 8] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧"];

pub struct Loading {
    rx: Receiver<Vec<Hunk>>,
    docs: Option<(Document, Document)>,
    ws: Whitespace,
    theme: Theme,
    view: ViewConfig,
    size: (u16, u16),
    lines: usize,
    started: Instant,
    app: Option<App>,
    outcome: Option<Outcome>,
    /// Set when the diff finished and found nothing, so `main` can say so.
    pub identical: Arc<AtomicBool>,
}

impl Loading {
    pub fn new(
        rx: Receiver<Vec<Hunk>>,
        docs: (Document, Document),
        ws: Whitespace,
        theme: Theme,
        view: ViewConfig,
        size: (u16, u16),
        identical: Arc<AtomicBool>,
    ) -> Loading {
        let lines = docs.0.len() + docs.1.len();
        Loading {
            rx,
            docs: Some(docs),
            ws,
            theme,
            view,
            size,
            lines,
            started: Instant::now(),
            app: None,
            outcome: None,
            identical,
        }
    }

    fn finish(&mut self, hunks: Vec<Hunk>) {
        let Some((l, r)) = self.docs.take() else {
            return;
        };
        if hunks.is_empty() {
            // Nothing to show: behave like identical inputs.
            self.identical.store(true, Ordering::Relaxed);
            self.outcome = Some(Outcome::Quit);
            return;
        }
        let session = Session::from_parts(l, r, self.ws, hunks);
        self.app = Some(App::new(
            session,
            self.theme.clone(),
            self.view.clone(),
            self.size,
        ));
    }
}

impl Ui for Loading {
    fn draw(&mut self, f: &mut Frame) {
        let area = f.area();
        self.size = (area.width, area.height);
        if let Some(app) = &mut self.app {
            render::draw(f, app);
            return;
        }
        let t = &self.theme;
        let buf = f.buffer_mut();
        if area.width < 20 || area.height < 4 {
            buf.set_string(0, 0, "comparing…", Style::default());
            return;
        }
        let frame = SPINNER[(self.started.elapsed().as_millis() / 100) as usize % SPINNER.len()];
        let mut x = 2;
        put(
            buf,
            &mut x,
            2,
            area.width,
            frame,
            Style::default().fg(t.amber),
        );
        put(
            buf,
            &mut x,
            2,
            area.width,
            " Comparing",
            bold(Style::default().fg(t.bright)),
        );
        let detail = format!(
            "{} lines, {:.1}s",
            thousands(self.lines),
            self.started.elapsed().as_secs_f32()
        );
        buf.set_stringn(
            2,
            3,
            detail,
            area.width as usize - 3,
            Style::default().fg(t.muted),
        );
        let y = area.height - 1;
        buf.set_style(
            Rect::new(0, y, area.width, 1),
            Style::default().bg(t.panel_bg),
        );
        let mut x = 1;
        hints(buf, &mut x, y, area.width, t, &[("Esc", "cancel")], "  ");
    }

    fn on_key(&mut self, key: KeyEvent) {
        if let Some(app) = &mut self.app {
            app.on_key(key);
            return;
        }
        if key.kind == KeyEventKind::Release {
            return;
        }
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            self.outcome = Some(Outcome::Abort);
        } else if matches!(key.code, KeyCode::Esc | KeyCode::Char('q')) {
            // Leaving ends the process, which also ends the worker thread.
            self.outcome = Some(Outcome::Quit);
        }
    }

    fn tick(&mut self) {
        if self.app.is_some() || self.outcome.is_some() {
            return;
        }
        match self.rx.try_recv() {
            Ok(h) => self.finish(h),
            Err(TryRecvError::Empty) => {}
            Err(TryRecvError::Disconnected) => self.outcome = Some(Outcome::Abort),
        }
    }

    fn polling(&self) -> bool {
        self.app.is_none() && self.outcome.is_none()
    }

    fn outcome(&self) -> Option<Outcome> {
        self.app.as_ref().map_or(self.outcome, |a| a.outcome)
    }

    fn editor_request(&mut self) -> Option<EditorRequest> {
        self.app.as_mut().and_then(|a| a.editor_request.take())
    }

    fn finish_editor(&mut self, side: Side, result: Result<bool, String>) {
        if let Some(app) = &mut self.app {
            app.finish_editor(side, result);
        }
    }
}
