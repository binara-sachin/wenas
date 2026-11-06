//! View state and key handling. Rendering lives in `render`.

use crate::align::{self, Row, RowKind};
use crate::document::DiskId;
use crate::editor::EditorRequest;
use crate::inline::{Inline, inline_diff};
use crate::input::EolStyle;
use crate::ops::{Cancelled, Dir, EditLoc, OpError, SaveError, Session, Side};
use crate::theme::Theme;
use crate::ui::render::{Hit, hit_test};
use ratatui::crossterm::event::{
    KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
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
    /// Directory mode: return to the file list (the session is kept).
    Back,
    /// Directory mode: the user asked to quit; the shell decides whether to ask first.
    QuitAsk,
    /// The first diff finished with no differences, so there is nothing to show
    /// (file mode prints `identical`).
    Identical,
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

/// A question that takes over the bottom of the screen until answered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Modal {
    /// The file changed on disk since it was opened (spec §7.4).
    ChangedOnDisk { side: Side, quit_after: bool },
    /// `q` with unsaved changes.
    QuitUnsaved,
    /// `e` on a side with unsaved changes (spec §7.3).
    SaveFirst { side: Side },
}

/// What to do when the running background diff completes (or is cancelled).
struct AfterDiff {
    /// The cursor position to keep, captured before the operation.
    anchor: Option<(Option<usize>, Option<usize>)>,
    /// Move the cursor to this edit (undo and redo).
    jump: Option<EditLoc>,
    message: Option<String>,
    /// The first diff of this session, not a re-diff after an edit.
    initial: bool,
}

/// Text for the bottom panel of a modal.
pub struct ModalText {
    pub title: String,
    pub body: Vec<String>,
    pub hints: Vec<(&'static str, &'static str)>,
}

impl BinaryInfo {
    /// For two buffers that are already in memory.
    pub fn from_bytes(name: String, left: &[u8], right: &[u8]) -> BinaryInfo {
        BinaryInfo {
            name,
            left_size: left.len(),
            right_size: right.len(),
            first_difference: Some(crate::input::first_difference(left, right).unwrap_or(0)),
            utf16: crate::input::has_utf16_bom(left) || crate::input::has_utf16_bom(right),
        }
    }
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
    /// Opened from the directory view: `Backspace` goes back, `q` defers to the shell.
    pub in_directory: bool,
    /// The first diff was cancelled, so this session has nothing usable in it.
    pub diff_cancelled: bool,
    after_diff: Option<AfterDiff>,
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
    pub message_is_error: bool,
    pub modal: Option<Modal>,
    pub editor_request: Option<EditorRequest>,
    pub outcome: Option<Outcome>,
    pub eols: (EolStyle, EolStyle),
    pending: Option<char>,
    /// A copy arrow held down: which one, and the screen cell it was pressed on.
    pressed_arrow: Option<(Dir, u16, u16)>,
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
            in_directory: false,
            diff_cancelled: false,
            after_diff: None,
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
            message_is_error: false,
            modal: None,
            editor_request: None,
            outcome: None,
            eols: (EolStyle::None, EolStyle::None),
            pending: None,
            pressed_arrow: None,
            inline_cache: HashMap::new(),
        };
        app.view = app.effective_view();
        if app.session.is_busy() {
            // The first diff is still running: rows come when it completes.
            app.after_diff = Some(AfterDiff {
                anchor: None,
                jump: None,
                message: None,
                initial: true,
            });
        } else {
            app.finish_initial();
        }
        app
    }

    /// The first diff is available: build the rows and show the first hunk.
    fn finish_initial(&mut self) {
        self.rebuild(None);
        if !self.first_rows.is_empty() {
            self.goto_hunk(0);
        } else if !self.in_directory && self.binary.is_none() && self.session.hunks.is_empty() {
            self.outcome = Some(Outcome::Identical);
        }
    }

    /// Called every event-loop turn: take a finished background diff.
    pub fn tick(&mut self) {
        if !self.session.poll() {
            return;
        }
        match self.after_diff.take() {
            Some(a) if a.initial => self.finish_initial(),
            Some(a) => self.finish_diff(a),
            None => self.rebuild(None),
        }
    }

    fn finish_diff(&mut self, a: AfterDiff) {
        self.rebuild(a.anchor);
        if let Some(loc) = a.jump {
            self.jump_to_line(loc);
        }
        if a.message.is_some() {
            self.message = a.message;
        }
    }

    /// An edit changed the documents: show the new diff now, or when it arrives.
    fn settle(
        &mut self,
        anchor: Option<(Option<usize>, Option<usize>)>,
        jump: Option<EditLoc>,
        message: Option<String>,
    ) {
        let after = AfterDiff {
            anchor,
            jump,
            message,
            initial: false,
        };
        if self.session.is_busy() {
            self.after_diff = Some(after);
        } else {
            self.finish_diff(after);
        }
    }

    /// `Esc` while comparing: abandon the diff and undo the change that started it.
    fn cancel_diff(&mut self) {
        match self.session.cancel() {
            Some(Cancelled::Initial) => {
                self.diff_cancelled = true;
                self.outcome = Some(if self.in_directory {
                    Outcome::Back
                } else {
                    Outcome::Quit
                });
            }
            Some(Cancelled::Reverted) => {
                let anchor = self.after_diff.take().and_then(|a| a.anchor);
                self.rebuild(anchor);
                self.message = Some("comparison cancelled; the change was undone".into());
            }
            None => {}
        }
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
        // Two empty documents are "identical", which is not what this screen means.
        app.outcome = None;
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
        if self.session.is_busy() {
            return; // rows are built when the diff completes
        }
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
        let step = SIDEWAYS_STEP as isize;
        self.scroll_columns(if right { step } else { -step });
    }

    fn scroll_columns(&mut self, delta: isize) {
        if delta > 0 {
            // Stop when the longest visible line's end reaches the pane edge.
            let limit = self
                .max_line_width()
                .saturating_sub(crate::ui::render::text_width(self));
            self.hscroll = (self.hscroll + delta as usize).min(limit.max(self.hscroll));
        } else {
            self.hscroll = self.hscroll.saturating_sub(delta.unsigned_abs());
        }
    }

    /// Move the view, not the cursor, except to keep the cursor on screen: `>` and
    /// `<` act on the hunk at the cursor, which must never be out of sight.
    fn scroll_lines(&mut self, delta: isize) {
        let Some(last) = self.rows.len().checked_sub(1) else {
            return;
        };
        let h = self.content_height();
        // `center_on` can leave `top` past this, which scrolling must not undo.
        let max_top = self.rows.len().saturating_sub(h).max(self.top);
        self.top = (self.top as isize + delta).clamp(0, max_top as isize) as usize;
        let bottom = (self.top + h - 1).min(last);
        self.cursor = self.cursor.clamp(self.top.min(bottom), bottom);
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
        self.message_is_error = false;
        if self.session.is_busy() {
            // Comparing: only `Esc` (cancel) and `Ctrl-c` (above) do anything.
            if key.code == KeyCode::Esc {
                self.cancel_diff();
            }
            return;
        }
        if self.binary.is_some() {
            let leave = matches!(
                key.code,
                KeyCode::Char('q' | 'Q') | KeyCode::Esc | KeyCode::Enter | KeyCode::Backspace
            );
            if leave && self.in_directory {
                self.outcome = Some(Outcome::Back);
            } else if leave && key.code != KeyCode::Backspace {
                self.outcome = Some(Outcome::Quit);
            }
            return;
        }
        if self.modal.is_some() {
            self.on_modal_key(key);
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
            KeyCode::Char('r') if ctrl => self.redo(),
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
                self.settle(anchor, None, Some(format!("whitespace: {}", ws.label())));
            }
            KeyCode::Char('>') => self.copy(Dir::LeftToRight),
            KeyCode::Char('<') => self.copy(Dir::RightToLeft),
            KeyCode::Char('u') => self.undo(),
            KeyCode::Char('e') => self.request_editor(),
            KeyCode::Char('w') => self.save_all(false),
            KeyCode::Char('q') => self.quit(),
            KeyCode::Char('Q') => self.outcome = Some(Outcome::Quit),
            KeyCode::Esc => self.clear_search(),
            KeyCode::Backspace if self.in_directory => self.outcome = Some(Outcome::Back),
            KeyCode::F(1) => self.help = true,
            _ => {}
        }
    }

    // ---- mouse ----

    /// Mouse input only acts on the plain file view. Anything that's waiting for
    /// an answer stays keyboard-only, so a stray click can't answer it.
    fn takes_mouse(&self) -> bool {
        !self.session.is_busy()
            && self.binary.is_none()
            && self.modal.is_none()
            && !self.help
            && self.prompt.is_none()
    }

    /// Wheel movement. Returns whether anything changed.
    pub fn on_scroll(&mut self, lines: isize, columns: isize) -> bool {
        if !self.takes_mouse() {
            return false;
        }
        let before = (self.top, self.cursor, self.hscroll);
        if lines != 0 {
            self.scroll_lines(lines);
        }
        if columns != 0 {
            self.scroll_columns(columns);
        }
        let changed = before != (self.top, self.cursor, self.hscroll);
        if changed {
            self.pointer_acted();
        }
        changed
    }

    /// A click moves the cursor and focus when pressed. Copying waits for the
    /// release on the same arrow, so it can be abandoned by dragging away, and so
    /// the press that makes a hunk current can't also copy it.
    pub fn on_mouse(&mut self, event: MouseEvent) -> bool {
        if matches!(event.kind, MouseEventKind::Drag(_)) {
            return false;
        }
        let pressed_arrow = self.pressed_arrow.take();
        if !self.takes_mouse() || !event.modifiers.is_empty() {
            return false;
        }
        let (x, y) = (event.column, event.row);
        match event.kind {
            MouseEventKind::Down(MouseButton::Left) => match hit_test(self, x, y) {
                Some(Hit::Arrow(dir)) => {
                    self.pressed_arrow = Some((dir, x, y));
                    false
                }
                Some(Hit::Row { row, side }) => {
                    self.cursor = row;
                    if let Some(side) = side {
                        self.focus = side;
                    }
                    self.pointer_acted();
                    true
                }
                None => false,
            },
            MouseEventKind::Up(MouseButton::Left) => match pressed_arrow {
                Some((dir, px, py))
                    if (px, py) == (x, y) && hit_test(self, x, y) == Some(Hit::Arrow(dir)) =>
                {
                    self.pointer_acted();
                    self.copy(dir);
                    true
                }
                _ => false,
            },
            _ => false,
        }
    }

    /// A mouse action ends the same things a key press does.
    fn pointer_acted(&mut self) {
        self.message = None;
        self.message_is_error = false;
        self.pending = None;
    }

    /// Pasted text goes into the search prompt and nowhere else: it never runs
    /// as keys, and never answers a question.
    pub fn on_paste(&mut self, text: &str) -> bool {
        if self.modal.is_some() || self.help || self.session.is_busy() || self.binary.is_some() {
            return false;
        }
        self.pending = None;
        match &mut self.prompt {
            Some(p) => p.text.push_str(&super::paste_line(text)),
            None => self.error(super::PASTE_IGNORED),
        }
        true
    }

    fn center_top(&mut self, row: usize) {
        self.cursor = row;
        self.top = 0;
    }

    fn error(&mut self, msg: impl Into<String>) {
        self.message = Some(msg.into());
        self.message_is_error = true;
    }

    pub fn side_name(&self, side: Side) -> String {
        crate::ui::render::side_name(self, side)
    }

    // ---- editing ----

    /// Copy the current hunk across (`>` or `<`).
    fn copy(&mut self, dir: Dir) {
        let Some(h) = self.current_hunk() else {
            self.message = Some("no hunk to copy".into());
            return;
        };
        let src = if dir == Dir::LeftToRight {
            Side::Left
        } else {
            Side::Right
        };
        // Anchor on the side we are not changing, so the cursor stays put.
        let anchor = self
            .rows
            .get(self.cursor)
            .map(|r| match (src, r.left, r.right) {
                (Side::Left, Some(l), _) => (Some(l), None),
                (Side::Right, _, Some(rr)) => (None, Some(rr)),
                _ => (r.left, r.right),
            });
        match self.session.copy_hunk(h, dir) {
            Ok(()) => {
                let to = if dir == Dir::LeftToRight {
                    "right"
                } else {
                    "left"
                };
                self.settle(anchor, None, Some(format!("hunk {} copied to {to}", h + 1)));
            }
            Err(OpError::Busy) => {}
            Err(OpError::ReadOnly(side)) => {
                let (name, other_key, other_name) = match side {
                    Side::Left => ("left", '>', "right"),
                    Side::Right => ("right", '<', "left"),
                };
                let other = if side == Side::Left {
                    Side::Right
                } else {
                    Side::Left
                };
                let mut msg = format!("The {name} side {} read-only.", self.readonly_reason(side));
                if !self.session.doc(other).is_readonly() {
                    msg.push_str(&format!(" Press {other_key} to copy {other_name} instead."));
                }
                self.error(msg);
            }
            Err(OpError::NoSuchHunk) => self.message = Some("no hunk to copy".into()),
        }
    }

    /// "is git's old version, so it's" or just "is", to precede "read-only".
    fn readonly_reason(&self, side: Side) -> String {
        match self.session.doc(side).readonly_reason() {
            Some(reason) => format!("is {}, so it's", reason.describe()),
            None => "is".into(),
        }
    }

    fn jump_to_line(&mut self, loc: EditLoc) {
        let pos = self.rows.iter().position(|r| {
            if loc.side == Side::Left {
                r.left
            } else {
                r.right
            }
            .is_some_and(|x| x >= loc.start)
        });
        if let Some(i) = pos {
            let h = self.content_height();
            self.cursor = i;
            if i < self.top || i >= self.top + h {
                self.top = i.saturating_sub(h / 2);
            }
        }
    }

    fn undo(&mut self) {
        let anchor = self.anchor();
        match self.session.undo() {
            Some(loc) => self.settle(anchor, Some(loc), Some("undone".into())),
            None => self.message = Some("nothing to undo".into()),
        }
    }

    fn redo(&mut self) {
        let anchor = self.anchor();
        match self.session.redo() {
            Some(loc) => self.settle(anchor, Some(loc), Some("redone".into())),
            None => self.message = Some("nothing to redo".into()),
        }
    }

    /// Save every changed side. On a conflict with the disk, asks what to do
    /// and resumes from there. With `quit_after`, quits once nothing is left.
    pub fn save_all(&mut self, quit_after: bool) {
        let sides = self.session.dirty_sides();
        let mut saved = Vec::new();
        for side in sides {
            match self.session.save(side, false) {
                Ok(()) => saved.push(self.side_name(side)),
                Err(SaveError::ChangedOnDisk) => {
                    self.modal = Some(Modal::ChangedOnDisk { side, quit_after });
                    return;
                }
                Err(e) => {
                    let name = self.side_name(side);
                    self.error(format!("could not save {name}: {e}"));
                    return;
                }
            }
        }
        if quit_after {
            self.outcome = Some(if self.in_directory {
                Outcome::QuitAsk
            } else {
                Outcome::Quit
            });
        } else if saved.is_empty() {
            self.message = Some("nothing to save".into());
        } else {
            self.message = Some(format!("saved {}", saved.join(", ")));
        }
    }

    fn quit(&mut self) {
        if self.in_directory {
            self.outcome = Some(Outcome::QuitAsk);
        } else if self.session.is_dirty() {
            self.modal = Some(Modal::QuitUnsaved);
        } else {
            self.outcome = Some(Outcome::Quit);
        }
    }

    /// Re-read a side from disk as one undoable step. Returns whether it changed.
    pub fn reload_from_disk(&mut self, side: Side) -> Result<bool, String> {
        let path = self
            .session
            .doc(side)
            .path
            .clone()
            .ok_or("no file to reload")?;
        let real = std::fs::canonicalize(&path).unwrap_or(path);
        let bytes =
            std::fs::read(&real).map_err(|e| format!("cannot read {}: {e}", real.display()))?;
        let mtime = std::fs::metadata(&real).and_then(|m| m.modified()).ok();
        let anchor = self.anchor();
        let changed = self
            .session
            .replace_content(side, &bytes, DiskId::of_bytes(&bytes, mtime));
        if changed {
            self.settle(anchor, None, None);
        }
        Ok(changed)
    }

    fn on_modal_key(&mut self, key: KeyEvent) {
        let Some(modal) = self.modal else { return };
        let code = key.code;
        match modal {
            Modal::QuitUnsaved => match code {
                KeyCode::Char('w') => {
                    self.modal = None;
                    self.save_all(true);
                }
                KeyCode::Char('Q') => self.outcome = Some(Outcome::Quit),
                KeyCode::Esc => self.modal = None,
                _ => {}
            },
            Modal::ChangedOnDisk { side, quit_after } => match code {
                KeyCode::Char('o') => {
                    self.modal = None;
                    match self.session.save(side, true) {
                        Ok(()) => self.save_all(quit_after),
                        Err(e) => self.error(format!("could not save: {e}")),
                    }
                }
                KeyCode::Char('r') => {
                    self.modal = None;
                    match self.reload_from_disk(side) {
                        Ok(_) => {
                            let name = self.side_name(side);
                            self.save_all(quit_after);
                            if self.outcome.is_none() && self.message.is_none() {
                                self.message = Some(format!("reloaded {name} from disk"));
                            }
                        }
                        Err(e) => self.error(e),
                    }
                }
                KeyCode::Esc => self.modal = None,
                _ => {}
            },
            Modal::SaveFirst { side } => match code {
                KeyCode::Char('w') => {
                    self.modal = None;
                    match self.session.save(side, false) {
                        Ok(()) => self.request_editor(),
                        Err(SaveError::ChangedOnDisk) => {
                            self.modal = Some(Modal::ChangedOnDisk {
                                side,
                                quit_after: false,
                            });
                        }
                        Err(e) => self.error(format!("could not save: {e}")),
                    }
                }
                KeyCode::Esc => self.modal = None,
                _ => {}
            },
        }
    }

    pub fn modal_text(&self) -> Option<ModalText> {
        let modal = self.modal?;
        Some(match modal {
            Modal::QuitUnsaved => {
                let sides = self.session.dirty_sides();
                let title = match sides.len() {
                    1 => "1 file has unsaved changes".to_string(),
                    n => format!("{n} files have unsaved changes"),
                };
                let body = sides
                    .iter()
                    .map(|&s| {
                        let n = self.session.edit_count(s);
                        format!(
                            "{}   {n} {}",
                            self.side_name(s),
                            if n == 1 { "edit" } else { "edits" }
                        )
                    })
                    .collect();
                let hints = vec![
                    ("w", "Save and quit"),
                    ("Q", "Quit without saving"),
                    ("Esc", "Keep working"),
                ];
                ModalText { title, body, hints }
            }
            Modal::ChangedOnDisk { side, .. } => {
                let n = self.session.edit_count(side);
                ModalText {
                    title: format!(
                        "{} changed on disk after you opened it.",
                        self.side_name(side)
                    ),
                    body: vec![format!(
                        "Another program changed it. Your {n} {} in that version.",
                        if n == 1 { "edit isn't" } else { "edits aren't" }
                    )],
                    hints: vec![
                        ("o", "Overwrite with your version"),
                        ("r", "Reload from disk, dropping your changes"),
                        ("Esc", "Cancel"),
                    ],
                }
            }
            Modal::SaveFirst { side } => ModalText {
                title: format!("{} has unsaved changes.", self.side_name(side)),
                body: vec!["Save before opening it in your editor.".into()],
                hints: vec![("w", "Save and open editor"), ("Esc", "Cancel")],
            },
        })
    }

    // ---- $EDITOR ----

    /// Ask the event loop to run `$EDITOR` on the focused side at the cursor line.
    fn request_editor(&mut self) {
        let side = self.focus;
        let doc = self.session.doc(side);
        let name = self.side_name(side);
        if doc.is_readonly() {
            let which = if side == Side::Left { "left" } else { "right" };
            let why = self.readonly_reason(side);
            self.error(format!(
                "The {which} side {why} read-only, so it can't be edited."
            ));
            return;
        }
        let Some(path) = doc.path.clone().filter(|p| p.as_os_str() != "-") else {
            self.error(format!("{name} is not a file"));
            return;
        };
        if doc.is_dirty() {
            self.modal = Some(Modal::SaveFirst { side });
            return;
        }
        let line = self.line_near_cursor(side) + 1;
        self.editor_request = Some(EditorRequest { side, line, path });
    }

    /// The side's line at the cursor row, or the nearest one above it.
    fn line_near_cursor(&self, side: Side) -> usize {
        let pick = |r: &Row| if side == Side::Left { r.left } else { r.right };
        (0..=self.cursor.min(self.rows.len().saturating_sub(1)))
            .rev()
            .find_map(|i| self.rows.get(i).and_then(pick))
            .or_else(|| self.rows.iter().find_map(pick))
            .unwrap_or(0)
    }

    /// Called by the event loop after the editor returns (`Ok(true)` = exited cleanly).
    pub fn finish_editor(&mut self, side: Side, result: Result<bool, String>) {
        match result {
            Err(e) => self.error(format!("could not run editor: {e}")),
            Ok(false) => self.error("editor exited with an error; file not reloaded"),
            Ok(true) => match self.reload_from_disk(side) {
                Ok(true) => {
                    self.message = Some(format!("reloaded {} after editing", self.side_name(side)))
                }
                Ok(false) => self.message = Some("no changes".into()),
                Err(e) => self.error(e),
            },
        }
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
