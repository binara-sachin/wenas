//! View state and key handling. Rendering lives in `render`.

use crate::align::{self, Row, RowKind};
use crate::inline::{Inline, inline_diff};
use crate::input::EolStyle;
use crate::ops::{Session, Side};
use crate::theme::Theme;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use std::collections::HashMap;
use std::ops::Range;
use std::rc::Rc;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum View {
    SideBySide,
    Unified,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Quit,
    /// `Ctrl-c`: exit code 1 so git stops the rest of a difftool run.
    Abort,
}

#[derive(Debug, Clone)]
pub struct ViewConfig {
    pub tab_width: usize,
    pub unified_below_columns: u16,
    /// `--unified` forces the unified view regardless of width.
    pub force_unified: bool,
}

impl Default for ViewConfig {
    fn default() -> Self {
        ViewConfig {
            tab_width: 4,
            unified_below_columns: 100,
            force_unified: false,
        }
    }
}

/// Shown instead of a diff when either side is binary (spec §6.4).
#[derive(Debug, Clone)]
pub struct BinaryInfo {
    pub name: String,
    pub left_size: usize,
    pub right_size: usize,
    pub first_difference: Option<usize>,
    pub utf16: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Match {
    pub row: usize,
    pub side: Side,
    pub range: Range<usize>,
}

#[derive(Debug, Clone)]
pub struct Search {
    pub query: String,
    pub backward: bool,
    pub matches: Vec<Match>,
    pub current: Option<usize>,
}

#[derive(Debug, Clone)]
pub struct Prompt {
    pub backward: bool,
    pub text: String,
}

pub struct App {
    pub session: Session,
    pub theme: Theme,
    pub cfg: ViewConfig,
    pub binary: Option<BinaryInfo>,
    pub width: u16,
    pub height: u16,
    pub rows: Vec<Row>,
    pub first_rows: Vec<usize>,
    pub view: View,
    view_override: Option<View>,
    pub cursor: usize,
    pub top: usize,
    pub hscroll: usize,
    pub focus: Side,
    pub search: Option<Search>,
    pub prompt: Option<Prompt>,
    pub help: bool,
    pub message: Option<String>,
    pub outcome: Option<Outcome>,
    pub eols: (EolStyle, EolStyle),
    pending: Option<char>,
    inline_cache: HashMap<usize, Rc<Inline>>,
}

const SIDEWAYS_STEP: usize = 8;

impl App {
    pub fn new(session: Session, theme: Theme, cfg: ViewConfig, size: (u16, u16)) -> App {
        let mut app = App {
            session,
            theme,
            view_override: cfg.force_unified.then_some(View::Unified),
            cfg,
            binary: None,
            width: size.0,
            height: size.1,
            rows: Vec::new(),
            first_rows: Vec::new(),
            view: View::SideBySide,
            cursor: 0,
            top: 0,
            hscroll: 0,
            focus: Side::Right,
            search: None,
            prompt: None,
            help: false,
            message: None,
            outcome: None,
            eols: (EolStyle::None, EolStyle::None),
            pending: None,
            inline_cache: HashMap::new(),
        };
        app.view = app.effective_view();
        app.rebuild(None);
        if !app.first_rows.is_empty() {
            app.goto_hunk(0);
        }
        app
    }

    pub fn binary_screen(info: BinaryInfo, theme: Theme, size: (u16, u16)) -> App {
        use crate::diff::Whitespace;
        use crate::document::Document;
        let session = Session::new(
            Document::from_bytes(vec![]),
            Document::from_bytes(vec![]),
            Whitespace::Off,
        );
        let mut app = App::new(session, theme, ViewConfig::default(), size);
        app.binary = Some(info);
        app
    }

    /// Rows available for file content: header, spacer and status line are fixed.
    pub fn content_height(&self) -> usize {
        (self.height as usize).saturating_sub(3).max(1)
    }

    fn effective_view(&self) -> View {
        self.view_override
            .unwrap_or(if self.width < self.cfg.unified_below_columns {
                View::Unified
            } else {
                View::SideBySide
            })
    }

    /// Called by the renderer each frame so resizes switch views at the threshold.
    pub fn sync_size(&mut self, width: u16, height: u16) {
        self.width = width;
        self.height = height;
        let view = self.effective_view();
        if view != self.view {
            let anchor = self.anchor();
            self.view = view;
            self.rebuild(anchor);
        }
    }

    /// A line of the file that stays meaningful across row rebuilds.
    fn anchor(&self) -> Option<(Option<usize>, Option<usize>)> {
        self.rows.get(self.cursor).map(|r| (r.left, r.right))
    }

    pub fn rebuild(&mut self, anchor: Option<(Option<usize>, Option<usize>)>) {
        let (hunks, ll, rl) = (
            &self.session.hunks,
            self.session.left.len(),
            self.session.right.len(),
        );
        self.rows = match self.view {
            View::SideBySide => align::side_by_side(hunks, ll, rl),
            View::Unified => align::unified(hunks, ll, rl),
        };
        self.first_rows = align::hunk_first_rows(&self.rows, hunks.len());
        self.inline_cache.clear();
        self.eols = (self.session.left.style(), self.session.right.style());
        if let Some((l, r)) = anchor {
            let found = self.rows.iter().position(|row| {
                (l.is_some() && row.left == l) || (l.is_none() && r.is_some() && row.right == r)
            });
            if let Some(i) = found {
                self.cursor = i;
            }
        }
        self.cursor = self.cursor.min(self.rows.len().saturating_sub(1));
        self.top = self.top.min(self.cursor);
        self.ensure_visible();
        if let Some(s) = &self.search {
            let (q, back, cur_row) = (s.query.clone(), s.backward, self.cursor);
            self.run_search(q, back, cur_row, false);
        }
    }

    pub fn current_hunk(&self) -> Option<usize> {
        align::current_hunk(&self.rows, self.cursor)
    }

    pub fn inline_for(&mut self, hunk: usize) -> Rc<Inline> {
        if let Some(i) = self.inline_cache.get(&hunk) {
            return i.clone();
        }
        let h = &self.session.hunks[hunk];
        let l: Vec<&[u8]> = h.left.clone().map(|i| self.session.left.line(i)).collect();
        let r: Vec<&[u8]> = h
            .right
            .clone()
            .map(|i| self.session.right.line(i))
            .collect();
        let inl = Rc::new(inline_diff(&l, &r));
        self.inline_cache.insert(hunk, inl.clone());
        inl
    }

    /// The single line a row shows in unified view.
    pub fn unified_line(row: &Row) -> Option<(Side, usize)> {
        match (row.left, row.right) {
            (_, Some(r)) => Some((Side::Right, r)),
            (Some(l), None) => Some((Side::Left, l)),
            _ => None,
        }
    }

    /// Lines shown on a row, per side, for the current view.
    pub fn row_lines(&self, row: &Row) -> [Option<(Side, usize)>; 2] {
        match self.view {
            View::SideBySide => [
                row.left.map(|l| (Side::Left, l)),
                row.right.map(|r| (Side::Right, r)),
            ],
            View::Unified => [Self::unified_line(row), None],
        }
    }

    // ---- movement ----

    pub fn ensure_visible(&mut self) {
        let h = self.content_height();
        if self.cursor < self.top {
            self.top = self.cursor;
        } else if self.cursor >= self.top + h {
            self.top = self.cursor + 1 - h;
        }
    }

    fn move_cursor(&mut self, delta: isize) {
        let last = self.rows.len().saturating_sub(1) as isize;
        self.cursor = (self.cursor as isize + delta).clamp(0, last.max(0)) as usize;
        self.ensure_visible();
    }

    fn half_page(&mut self, down: bool) {
        let step = (self.content_height() / 2).max(1) as isize;
        let d = if down { step } else { -step };
        self.move_cursor(d);
        let last = self.rows.len().saturating_sub(1);
        self.top = (self.top as isize + d).clamp(0, last as isize) as usize;
        self.ensure_visible();
    }

    fn center_on(&mut self, row: usize) {
        self.cursor = row;
        self.top = row.saturating_sub(self.content_height() / 2);
    }

    pub fn goto_hunk(&mut self, h: usize) {
        if let Some(&row) = self.first_rows.get(h) {
            self.center_on(row);
        }
    }

    fn next_hunk(&mut self) {
        match (0..self.first_rows.len()).find(|&h| self.first_rows[h] > self.cursor) {
            Some(h) => self.goto_hunk(h),
            None => self.message = Some("last hunk".into()),
        }
    }

    fn prev_hunk(&mut self) {
        match (0..self.first_rows.len())
            .rev()
            .find(|&h| self.first_rows[h] < self.cursor)
        {
            Some(h) => self.goto_hunk(h),
            None => self.message = Some("first hunk".into()),
        }
    }

    fn max_line_width(&self) -> usize {
        let end = (self.top + self.content_height()).min(self.rows.len());
        self.rows[self.top.min(end)..end]
            .iter()
            .flat_map(|r| self.row_lines(r))
            .flatten()
            .map(|(side, i)| {
                crate::ui::render::display_width(self.session.doc(side).line(i), self.cfg.tab_width)
            })
            .max()
            .unwrap_or(0)
    }

    fn scroll_sideways(&mut self, right: bool) {
        if right {
            // Stop when the longest visible line's end reaches the pane edge.
            let limit = self
                .max_line_width()
                .saturating_sub(crate::ui::render::text_width(self));
            self.hscroll = (self.hscroll + SIDEWAYS_STEP).min(limit.max(self.hscroll));
        } else {
            self.hscroll = self.hscroll.saturating_sub(SIDEWAYS_STEP);
        }
    }

    // ---- search ----

    fn lines_for_search(&self, row: &Row) -> Vec<(Side, usize)> {
        self.row_lines(row).into_iter().flatten().collect()
    }

    /// Find all matches, then jump to the nearest one from `from_row`.
    pub fn run_search(&mut self, query: String, backward: bool, from_row: usize, jump: bool) {
        let insensitive = !query.bytes().any(|b| b.is_ascii_uppercase());
        let needle: Vec<u8> = if insensitive {
            query.to_ascii_lowercase().into_bytes()
        } else {
            query.clone().into_bytes()
        };
        let mut matches = Vec::new();
        if !needle.is_empty() {
            for (ri, row) in self.rows.iter().enumerate() {
                for (side, li) in self.lines_for_search(row) {
                    let line = self.session.doc(side).line(li);
                    let hay: std::borrow::Cow<[u8]> = if insensitive {
                        line.to_ascii_lowercase().into()
                    } else {
                        line.into()
                    };
                    let mut from = 0;
                    while let Some(p) = find(&hay[from..], &needle) {
                        let start = from + p;
                        matches.push(Match {
                            row: ri,
                            side,
                            range: start..start + needle.len(),
                        });
                        from = start + needle.len();
                    }
                }
            }
        }
        let current = if matches.is_empty() {
            None
        } else if backward {
            matches
                .iter()
                .rposition(|m| m.row <= from_row)
                .or(Some(matches.len() - 1))
        } else {
            matches.iter().position(|m| m.row >= from_row).or(Some(0))
        };
        self.search = Some(Search {
            query,
            backward,
            matches,
            current,
        });
        if jump {
            if self.search.as_ref().is_some_and(|s| s.matches.is_empty()) {
                self.message = Some("no matches".into());
            } else {
                self.show_current_match();
            }
        }
    }

    fn show_current_match(&mut self) {
        let Some(m) = self
            .search
            .as_ref()
            .and_then(|s| s.current.map(|c| s.matches[c].clone()))
        else {
            return;
        };
        let h = self.content_height();
        self.cursor = m.row;
        if m.row < self.top || m.row >= self.top + h {
            self.top = m.row.saturating_sub(h / 2);
        }
        // Bring the match into view horizontally.
        let line = self.session.doc(m.side).line(self.line_of(m.row, m.side));
        let col = crate::ui::render::display_width(
            &line[..m.range.start.min(line.len())],
            self.cfg.tab_width,
        );
        let text_w = crate::ui::render::text_width(self).max(1);
        if col < self.hscroll || col >= self.hscroll + text_w {
            self.hscroll = col.saturating_sub(text_w / 4);
        }
    }

    fn line_of(&self, row: usize, side: Side) -> usize {
        let r = &self.rows[row];
        match self.view {
            View::SideBySide => if side == Side::Left { r.left } else { r.right }.unwrap_or(0),
            View::Unified => Self::unified_line(r).map_or(0, |(_, i)| i),
        }
    }

    fn step_match(&mut self, reverse: bool) {
        let Some(s) = &mut self.search else {
            self.message = Some("no search".into());
            return;
        };
        if s.matches.is_empty() {
            self.message = Some("no matches".into());
            return;
        }
        let n = s.matches.len();
        let forward = s.backward == reverse;
        let cur = s.current.unwrap_or(0);
        // If the cursor moved away from the current match, restart from the cursor.
        let base = if s.matches[cur].row == self.cursor {
            cur
        } else if forward {
            s.matches
                .iter()
                .rposition(|m| m.row < self.cursor)
                .unwrap_or(n - 1)
        } else {
            s.matches
                .iter()
                .position(|m| m.row > self.cursor)
                .unwrap_or(0)
        };
        let next = if forward {
            (base + 1) % n
        } else {
            (base + n - 1) % n
        };
        if (forward && next <= base) || (!forward && next >= base) {
            self.message = Some("search wrapped".into());
        }
        s.current = Some(next);
        self.show_current_match();
    }

    fn clear_search(&mut self) {
        self.search = None;
    }

    // ---- keys ----

    pub fn on_key(&mut self, key: KeyEvent) {
        if key.kind == KeyEventKind::Release {
            return;
        }
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        if ctrl && key.code == KeyCode::Char('c') {
            self.outcome = Some(Outcome::Abort);
            return;
        }
        self.message = None;
        if self.binary.is_some() {
            if matches!(
                key.code,
                KeyCode::Char('q' | 'Q') | KeyCode::Esc | KeyCode::Enter
            ) {
                self.outcome = Some(Outcome::Quit);
            }
            return;
        }
        if self.help {
            if matches!(key.code, KeyCode::F(1) | KeyCode::Esc | KeyCode::Char('q')) {
                self.help = false;
            }
            return;
        }
        if self.prompt.is_some() {
            self.on_prompt_key(key);
            return;
        }
        let pending = self.pending.take();
        match (pending, key.code) {
            (Some('g'), KeyCode::Char('g')) => return self.center_top(0),
            (Some('z'), KeyCode::Char('h')) => return self.scroll_sideways(false),
            (Some('z'), KeyCode::Char('l')) => return self.scroll_sideways(true),
            (Some(']'), KeyCode::Char('c')) => return self.next_hunk(),
            (Some('['), KeyCode::Char('c')) => return self.prev_hunk(),
            _ => {}
        }
        match key.code {
            KeyCode::Char('d') if ctrl => self.half_page(true),
            KeyCode::Char('u') if ctrl => self.half_page(false),
            KeyCode::Char('r') if ctrl => self.not_yet("redo"),
            KeyCode::Char('j') | KeyCode::Down => self.move_cursor(1),
            KeyCode::Char('k') | KeyCode::Up => self.move_cursor(-1),
            KeyCode::Char('G') => {
                let last = self.rows.len().saturating_sub(1);
                self.cursor = last;
                self.ensure_visible();
            }
            KeyCode::Char(c @ ('g' | 'z' | '[' | ']')) => self.pending = Some(c),
            KeyCode::Char('J') => self.next_hunk(),
            KeyCode::Char('K') => self.prev_hunk(),
            KeyCode::Left => self.scroll_sideways(false),
            KeyCode::Right => self.scroll_sideways(true),
            KeyCode::Tab => {
                self.focus = if self.focus == Side::Left {
                    Side::Right
                } else {
                    Side::Left
                };
            }
            KeyCode::Char('/') => {
                self.prompt = Some(Prompt {
                    backward: false,
                    text: String::new(),
                })
            }
            KeyCode::Char('?') => {
                self.prompt = Some(Prompt {
                    backward: true,
                    text: String::new(),
                })
            }
            KeyCode::Char('n') => self.step_match(false),
            KeyCode::Char('N') => self.step_match(true),
            KeyCode::Char('v') => {
                let anchor = self.anchor();
                let next = if self.view == View::Unified {
                    View::SideBySide
                } else {
                    View::Unified
                };
                self.view_override = Some(next);
                self.view = next;
                self.rebuild(anchor);
            }
            KeyCode::Char('W') => {
                let anchor = self.anchor();
                let ws = self.session.ws.next();
                self.session.set_whitespace(ws);
                self.rebuild(anchor);
                self.message = Some(format!("whitespace: {}", ws.label()));
            }
            KeyCode::Char('>') => self.not_yet("copy to right"),
            KeyCode::Char('<') => self.not_yet("copy to left"),
            KeyCode::Char('u') => self.not_yet("undo"),
            KeyCode::Char('e') => self.not_yet("opening $EDITOR"),
            KeyCode::Char('w') => self.not_yet("saving"),
            KeyCode::Char('q') => self.quit(false),
            KeyCode::Char('Q') => self.quit(true),
            KeyCode::Esc => self.clear_search(),
            KeyCode::F(1) => self.help = true,
            _ => {}
        }
    }

    fn center_top(&mut self, row: usize) {
        self.cursor = row;
        self.top = 0;
    }

    fn not_yet(&mut self, what: &str) {
        self.message = Some(format!(
            "{what} arrives with editing support (milestone M3)"
        ));
    }

    fn quit(&mut self, _force: bool) {
        // Nothing can be modified yet, so there is never anything to confirm.
        self.outcome = Some(Outcome::Quit);
    }

    fn on_prompt_key(&mut self, key: KeyEvent) {
        let Some(p) = &mut self.prompt else { return };
        match key.code {
            KeyCode::Esc => self.prompt = None,
            KeyCode::Enter => {
                let p = self.prompt.take().unwrap();
                if p.text.is_empty() {
                    return;
                }
                let from = self.cursor;
                self.run_search(p.text, p.backward, from, true);
            }
            KeyCode::Backspace => {
                if p.text.pop().is_none() {
                    self.prompt = None;
                }
            }
            KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => p.text.push(c),
            _ => {}
        }
    }

    pub fn hunk_label(&self) -> String {
        let total = self.session.hunks.len();
        match self.current_hunk() {
            Some(h) => format!("{}/{}", h + 1, total),
            None => format!("-/{total}"),
        }
    }

    pub fn row_kind_at(&self, row: usize) -> Option<RowKind> {
        self.rows.get(row).map(|r| r.kind)
    }
}

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    memchr::memmem::find(hay, needle)
}
