//! The directory list (spec §6.3, §7.2).

use super::render::{CONTENT_Y, bold, hints, hints_len, key_style, put, thousands, too_small};
use crate::dircmp::{Entry, Msg, Scan, Status};
use crate::theme::Theme;
use ratatui::buffer::Buffer;
use ratatui::crossterm::event::{
    KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use std::collections::HashSet;
use std::path::PathBuf;
use std::time::{Duration, Instant};

/// Two presses on an entry this close together open it. Terminals don't report
/// double-clicks, and the desktop's own setting isn't visible from here.
pub const DOUBLE_CLICK: Duration = Duration::from_millis(400);

/// What the shell must do after a key press in the list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DirAction {
    None,
    /// Open this entry (index into `entries`).
    Open(usize),
    Quit,
    Abort,
}

#[derive(Debug, Clone)]
pub struct DirSearch {
    pub query: String,
    /// Indexes into `shown`.
    pub matches: Vec<usize>,
    pub current: Option<usize>,
}

pub struct DirView {
    pub left: PathBuf,
    pub right: PathBuf,
    pub entries: Vec<Entry>,
    /// Indexes into `entries` that are visible, in order.
    pub shown: Vec<usize>,
    pub show_identical: bool,
    pub cursor: usize,
    pub top: usize,
    pub width: u16,
    pub height: u16,
    pub scanning: bool,
    pub files_seen: usize,
    pub differ: usize,
    pub identical: usize,
    /// Entries whose opened file has unsaved changes.
    pub unsaved: HashSet<usize>,
    pub search: Option<DirSearch>,
    pub prompt: Option<String>,
    pub message: Option<String>,
    pub message_is_error: bool,
    pub help: bool,
    pending_g: bool,
    /// The entry last pressed, and when, to recognise a double-click.
    last_click: Option<(usize, Instant)>,
    scan: Option<Scan>,
    started: Instant,
}

impl DirView {
    pub fn new(
        left: PathBuf,
        right: PathBuf,
        show_identical: bool,
        scan: Scan,
        first: Vec<Msg>,
    ) -> DirView {
        let mut v = DirView {
            left,
            right,
            entries: Vec::new(),
            shown: Vec::new(),
            show_identical,
            cursor: 0,
            top: 0,
            width: 80,
            height: 24,
            scanning: true,
            files_seen: 0,
            differ: 0,
            identical: 0,
            unsaved: HashSet::new(),
            search: None,
            prompt: None,
            message: None,
            message_is_error: false,
            help: false,
            pending_g: false,
            last_click: None,
            scan: Some(scan),
            started: Instant::now(),
        };
        for m in first {
            v.apply(m);
        }
        v
    }

    fn content_height(&self) -> usize {
        (self.height as usize).saturating_sub(3).max(1)
    }

    pub fn apply(&mut self, m: Msg) {
        match m {
            Msg::Entry(e) => {
                if e.status.differs() {
                    self.differ += 1;
                } else {
                    self.identical += 1;
                }
                let show = e.status.differs() || self.show_identical;
                self.entries.push(e);
                if show {
                    self.shown.push(self.entries.len() - 1);
                }
            }
            Msg::Scanned(n) => self.files_seen = n.max(self.files_seen),
            Msg::Done => {
                self.scanning = false;
                self.scan = None;
            }
        }
    }

    /// Take whatever the worker has produced so far.
    pub fn poll(&mut self) {
        let mut batch = Vec::new();
        if let Some(scan) = &self.scan {
            while let Ok(m) = scan.rx.try_recv() {
                batch.push(m);
                if batch.len() >= 2000 {
                    break;
                }
            }
            // The worker is gone without saying Done (cancelled).
            if batch.is_empty()
                && matches!(
                    scan.rx.try_recv(),
                    Err(std::sync::mpsc::TryRecvError::Disconnected)
                )
            {
                batch.push(Msg::Done);
            }
        }
        for m in batch {
            self.apply(m);
        }
    }

    pub fn selected(&self) -> Option<usize> {
        self.shown.get(self.cursor).copied()
    }

    /// Rebuild `shown`, keeping the cursor on the same entry when it is still visible.
    fn reshow(&mut self) {
        let keep = self.selected();
        self.shown = (0..self.entries.len())
            .filter(|&i| self.show_identical || self.entries[i].status.differs())
            .collect();
        self.cursor = keep
            .and_then(|k| self.shown.iter().position(|&i| i == k))
            .unwrap_or(self.cursor);
        self.cursor = self.cursor.min(self.shown.len().saturating_sub(1));
        self.ensure_visible();
        self.search = None;
    }

    /// An opened file turned out to have no remaining differences.
    pub fn mark_identical(&mut self, idx: usize) {
        if self.entries[idx].status.differs() {
            self.entries[idx].status = Status::Identical;
            self.differ -= 1;
            self.identical += 1;
            self.reshow();
        }
    }

    fn ensure_visible(&mut self) {
        let h = self.content_height();
        if self.cursor < self.top {
            self.top = self.cursor;
        } else if self.cursor >= self.top + h {
            self.top = self.cursor + 1 - h;
        }
    }

    fn move_by(&mut self, d: isize) {
        let last = self.shown.len().saturating_sub(1) as isize;
        self.cursor = (self.cursor as isize + d).clamp(0, last.max(0)) as usize;
        self.ensure_visible();
    }

    fn run_search(&mut self, query: String) {
        let ci = !query.chars().any(|c| c.is_uppercase());
        let q = if ci {
            query.to_lowercase()
        } else {
            query.clone()
        };
        let matches: Vec<usize> = self
            .shown
            .iter()
            .enumerate()
            .filter(|(_, e)| {
                let p = self.entries[**e].rel.display().to_string();
                if ci {
                    p.to_lowercase().contains(&q)
                } else {
                    p.contains(&q)
                }
            })
            .map(|(i, _)| i)
            .collect();
        let current = matches
            .iter()
            .position(|&m| m >= self.cursor)
            .or(if matches.is_empty() { None } else { Some(0) });
        if matches.is_empty() {
            self.message = Some("no matches".into());
        }
        self.search = Some(DirSearch {
            query,
            matches,
            current,
        });
        self.goto_match();
    }

    fn goto_match(&mut self) {
        if let Some(s) = &self.search
            && let Some(c) = s.current
        {
            self.cursor = s.matches[c];
            self.ensure_visible();
        }
    }

    fn step_match(&mut self, back: bool) {
        let Some(s) = &mut self.search else {
            self.message = Some("no search".into());
            return;
        };
        if s.matches.is_empty() {
            self.message = Some("no matches".into());
            return;
        }
        let n = s.matches.len();
        let cur = s.current.unwrap_or(0);
        s.current = Some(if back {
            (cur + n - 1) % n
        } else {
            (cur + 1) % n
        });
        self.goto_match();
    }

    pub fn on_key(&mut self, key: KeyEvent) -> DirAction {
        if key.kind == KeyEventKind::Release {
            return DirAction::None;
        }
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        if ctrl && key.code == KeyCode::Char('c') {
            return DirAction::Abort;
        }
        self.message = None;
        self.message_is_error = false;
        if self.help {
            if matches!(key.code, KeyCode::F(1) | KeyCode::Esc | KeyCode::Char('q')) {
                self.help = false;
            }
            return DirAction::None;
        }
        if let Some(p) = &mut self.prompt {
            match key.code {
                KeyCode::Esc => self.prompt = None,
                KeyCode::Enter => {
                    let q = std::mem::take(p);
                    self.prompt = None;
                    if !q.is_empty() {
                        self.run_search(q);
                    }
                }
                KeyCode::Backspace => {
                    if p.pop().is_none() {
                        self.prompt = None;
                    }
                }
                KeyCode::Char(c) if !ctrl => p.push(c),
                _ => {}
            }
            return DirAction::None;
        }
        let g = std::mem::take(&mut self.pending_g);
        if g && key.code == KeyCode::Char('g') {
            self.cursor = 0;
            self.ensure_visible();
            return DirAction::None;
        }
        match key.code {
            KeyCode::Char('j') | KeyCode::Down => self.move_by(1),
            KeyCode::Char('k') | KeyCode::Up => self.move_by(-1),
            KeyCode::Char('d') if ctrl => self.move_by((self.content_height() / 2).max(1) as isize),
            KeyCode::Char('u') if ctrl => {
                self.move_by(-((self.content_height() / 2).max(1) as isize))
            }
            KeyCode::Char('g') => self.pending_g = true,
            KeyCode::Char('G') => {
                self.cursor = self.shown.len().saturating_sub(1);
                self.ensure_visible();
            }
            KeyCode::Enter => {
                if let Some(i) = self.selected() {
                    return DirAction::Open(i);
                }
            }
            KeyCode::Char('i') => {
                self.show_identical = !self.show_identical;
                self.reshow();
            }
            KeyCode::Char('/') => self.prompt = Some(String::new()),
            KeyCode::Char('n') => self.step_match(false),
            KeyCode::Char('N') => self.step_match(true),
            KeyCode::Char('q') => return DirAction::Quit,
            KeyCode::Esc => {
                if let Some(scan) = &self.scan {
                    scan.cancel();
                    self.message = Some("stopping the scan".into());
                } else {
                    self.search = None;
                }
            }
            KeyCode::F(1) => self.help = true,
            _ => {}
        }
        DirAction::None
    }

    fn takes_mouse(&self) -> bool {
        !self.help && self.prompt.is_none()
    }

    /// A mouse action ends the same things a key press does.
    fn pointer_acted(&mut self) {
        self.message = None;
        self.message_is_error = false;
        self.pending_g = false;
    }

    /// Pasted text goes into the name search prompt and nowhere else.
    pub fn on_paste(&mut self, text: &str) -> bool {
        if self.help {
            return false;
        }
        self.pending_g = false;
        match &mut self.prompt {
            Some(p) => p.push_str(&super::paste_line(text)),
            None => {
                self.message = Some(super::PASTE_IGNORED.into());
                self.message_is_error = true;
            }
        }
        true
    }

    /// Wheel movement, keeping the selection on screen. Returns whether anything changed.
    pub fn on_scroll(&mut self, lines: isize) -> bool {
        let Some(last) = self.shown.len().checked_sub(1) else {
            return false;
        };
        if !self.takes_mouse() || lines == 0 {
            return false;
        }
        let before = (self.top, self.cursor);
        let h = self.content_height();
        let max_top = self.shown.len().saturating_sub(h).max(self.top);
        self.top = (self.top as isize + lines).clamp(0, max_top as isize) as usize;
        let bottom = (self.top + h - 1).min(last);
        self.cursor = self.cursor.clamp(self.top.min(bottom), bottom);
        let changed = before != (self.top, self.cursor);
        if changed {
            self.pointer_acted();
        }
        changed
    }

    /// The position in `shown` drawn at this screen row.
    fn row_at(&self, y: u16) -> Option<usize> {
        if too_small(self.width, self.height) || y < CONTENT_Y {
            return None;
        }
        let offset = (y - CONTENT_Y) as usize;
        let row = self.top + offset;
        (offset < self.content_height() && row < self.shown.len()).then_some(row)
    }

    /// A press selects an entry and a second one on it opens it. `now` is passed
    /// in so tests can control the timing. `None` means the event did nothing.
    pub fn on_mouse(&mut self, event: MouseEvent, now: Instant) -> Option<DirAction> {
        if event.kind != MouseEventKind::Down(MouseButton::Left)
            || !event.modifiers.is_empty()
            || !self.takes_mouse()
        {
            return None;
        }
        let row = self.row_at(event.row)?;
        let entry = self.shown[row];
        self.cursor = row;
        self.pointer_acted();
        let double = self
            .last_click
            .is_some_and(|(e, at)| e == entry && now.duration_since(at) <= DOUBLE_CLICK);
        if double {
            self.last_click = None;
            Some(DirAction::Open(entry))
        } else {
            self.last_click = Some((entry, now));
            Some(DirAction::None)
        }
    }

    /// True while the screen changes on its own (a running scan).
    pub fn animating(&self) -> bool {
        self.scanning
    }
}

fn letter_style(t: &Theme, s: &Status) -> Style {
    let fg = match s {
        Status::Modified | Status::SymlinkDiffers { .. } => t.changed_fg,
        Status::LeftOnly | Status::Error(_) => t.removed_fg,
        Status::RightOnly => t.added_fg,
        Status::Binary { .. } => t.muted,
        Status::Mismatch(_) => t.bright,
        Status::Identical => t.dim,
    };
    bold(Style::default().fg(fg))
}

/// Cut a path to `width` columns, keeping the end and marking the cut with `…`.
fn clip_left(s: &str, width: usize) -> String {
    let n = s.chars().count();
    if n <= width {
        return s.to_string();
    }
    let keep = width.saturating_sub(1);
    format!("…{}", s.chars().skip(n - keep).collect::<String>())
}

/// Width of the fixed words in the header: "left  " and "   right  ".
const HEADER_LABELS: usize = 16;

/// Share `budget` columns between the two header paths, clipping the longer
/// ones from the left. A short path leaves its unused room to the other.
fn fit_paths(left: &str, right: &str, budget: usize) -> (String, String) {
    let (ll, rl) = (left.chars().count(), right.chars().count());
    if ll + rl <= budget {
        return (left.to_string(), right.to_string());
    }
    let mut lmax = budget / 2;
    let mut rmax = budget - lmax;
    if ll < lmax {
        rmax += lmax - ll;
        lmax = ll;
    } else if rl < rmax {
        lmax += rmax - rl;
        rmax = rl;
    }
    (clip_left(left, lmax), clip_left(right, rmax))
}

const SPINNER: [&str; 8] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧"];

pub fn draw_dir(buf: &mut Buffer, dir: &mut DirView, t: &Theme, area: Rect) {
    dir.width = area.width;
    dir.height = area.height;
    dir.ensure_visible();
    if too_small(area.width, area.height) {
        buf.set_string(0, 0, "terminal too small", Style::default());
        return;
    }
    let panel = Style::default().bg(t.panel_bg);
    // Header.
    buf.set_style(
        Rect::new(0, 0, area.width, 1),
        if t.no_color {
            bold(Style::default())
        } else {
            panel
        },
    );
    let tag = if dir.show_identical {
        "identical shown"
    } else {
        "identical hidden"
    };
    // The paths give way (keeping their ends) so the tag always fits, however
    // long the temp directories are (macOS paths easily exceed 60 columns).
    let budget = (area.width as usize).saturating_sub(1 + tag.len() + 3 + HEADER_LABELS);
    let (left_path, right_path) = fit_paths(
        &dir.left.display().to_string(),
        &dir.right.display().to_string(),
        budget,
    );
    let mut x = 1;
    let dim = Style::default().fg(t.dim).bg(t.panel_bg);
    put(buf, &mut x, 0, area.width, "left  ", dim);
    put(
        buf,
        &mut x,
        0,
        area.width,
        &left_path,
        Style::default().fg(t.muted).bg(t.panel_bg),
    );
    put(buf, &mut x, 0, area.width, "   right  ", dim);
    put(
        buf,
        &mut x,
        0,
        area.width,
        &right_path,
        Style::default().fg(t.bright).bg(t.panel_bg),
    );
    let tx = area.width.saturating_sub(tag.len() as u16 + 1);
    if tx >= x + 2 {
        let style = if dir.show_identical {
            Style::default().fg(t.bright).bg(t.panel_bg)
        } else {
            dim
        };
        let mut x = tx;
        put(buf, &mut x, 0, area.width, tag, style);
    }
    if dir.help {
        draw_dir_help(buf, t, area);
    } else {
        draw_rows(buf, dir, t, area);
    }
    draw_dir_status(buf, dir, t, area);
}

fn draw_rows(buf: &mut Buffer, dir: &DirView, t: &Theme, area: Rect) {
    let h = dir.height as usize - 3;
    let detail_x = (area.width as usize * 21 / 40).max(24);
    let path_w = detail_x.saturating_sub(6);
    if dir.shown.is_empty() && !dir.scanning {
        let msg = if dir.entries.is_empty() {
            "No files."
        } else {
            "No differences. Press i to show identical files."
        };
        buf.set_stringn(
            2,
            3,
            msg,
            area.width as usize - 3,
            Style::default().fg(t.muted),
        );
        return;
    }
    for r in 0..h {
        let i = dir.top + r;
        let Some(&e) = dir.shown.get(i) else { break };
        let entry = &dir.entries[e];
        let y = r as u16 + CONTENT_Y;
        let selected = i == dir.cursor;
        if selected {
            buf.set_style(
                Rect::new(0, y, area.width, 1),
                t.emphasis(t.bg(t.changed_bg)),
            );
        }
        let letter = entry.status.letter().to_string();
        let mut ls = letter_style(t, &entry.status);
        if selected {
            ls = if t.no_color {
                ls.add_modifier(Modifier::REVERSED)
            } else {
                bold(Style::default().fg(t.on_amber).bg(t.amber))
            };
        }
        buf.set_stringn(1, y, format!(" {letter} "), 3, ls);
        let rel = entry.rel.display().to_string();
        let shown = clip_left(&rel, path_w);
        let split = shown.rfind('/').map_or(0, |p| p + 1);
        let identical = !entry.status.differs();
        let base = if selected {
            t.bg(t.changed_bg)
        } else {
            Style::default()
        };
        let (dir_style, name_style) = if identical {
            (base.fg(t.dim), base.fg(t.dim))
        } else {
            (base.fg(t.dim), base.fg(t.bright))
        };
        buf.set_stringn(5, y, &shown[..split], path_w, dir_style);
        let nx = 5 + shown[..split].chars().count() as u16;
        buf.set_stringn(nx, y, &shown[split..], path_w, name_style);
        let (text, style) = if dir.unsaved.contains(&e) {
            ("unsaved changes [+]".to_string(), bold(base.fg(t.amber)))
        } else if let Status::Error(m) = &entry.status {
            (m.clone(), base.fg(t.removed_fg))
        } else {
            (entry.status.detail(), base.fg(t.dim))
        };
        buf.set_stringn(
            detail_x as u16,
            y,
            &text,
            (area.width as usize).saturating_sub(detail_x + 1),
            style,
        );
    }
}

fn draw_dir_status(buf: &mut Buffer, dir: &DirView, t: &Theme, area: Rect) {
    let y = area.height - 1;
    let end = area.width;
    if dir.message_is_error {
        let style = if t.no_color {
            bold(Style::default()).add_modifier(Modifier::REVERSED)
        } else {
            Style::default().fg(t.removed_fg).bg(t.removed_bg)
        };
        buf.set_style(Rect::new(0, y, area.width, 1), style);
        let mut x = 1;
        put(
            buf,
            &mut x,
            y,
            end,
            &format!("× {}", dir.message.as_deref().unwrap_or("")),
            style,
        );
        return;
    }
    buf.set_style(
        Rect::new(0, y, area.width, 1),
        Style::default().bg(t.panel_bg),
    );
    let bg = |c| Style::default().fg(c).bg(t.panel_bg);
    let mut x = 1;
    if let Some(p) = &dir.prompt {
        put(buf, &mut x, y, end, "/", key_style(t).bg(t.panel_bg));
        put(buf, &mut x, y, end, p, bg(t.bright));
        put(buf, &mut x, y, end, "█", bg(t.amber));
    } else if let Some(m) = &dir.message {
        put(buf, &mut x, y, end, m, bg(t.amber));
    } else if dir.scanning {
        let frame = SPINNER[(dir.started.elapsed().as_millis() / 100) as usize % SPINNER.len()];
        put(buf, &mut x, y, end, frame, bg(t.amber));
        put(buf, &mut x, y, end, " scanning", bg(t.bright));
        put(
            buf,
            &mut x,
            y,
            end,
            &format!("  {} files", thousands(dir.files_seen)),
            bg(t.muted),
        );
        put(
            buf,
            &mut x,
            y,
            end,
            &format!("   {} differ so far", thousands(dir.differ)),
            bg(t.muted),
        );
    } else {
        let total = dir.differ + dir.identical;
        put(
            buf,
            &mut x,
            y,
            end,
            &format!("{} files", thousands(total)),
            bg(t.bright),
        );
        put(
            buf,
            &mut x,
            y,
            end,
            &format!("   {} differ", thousands(dir.differ)),
            bg(t.muted),
        );
        put(
            buf,
            &mut x,
            y,
            end,
            &format!("  {} identical", thousands(dir.identical)),
            bg(t.muted),
        );
        if let Some(s) = &dir.search {
            let pos = s.current.map_or("no matches".to_string(), |c| {
                format!("{} of {}", c + 1, s.matches.len())
            });
            put(buf, &mut x, y, end, &format!("   {pos}"), bg(t.amber));
        }
    }
    let toggle = if dir.show_identical {
        "hide identical"
    } else {
        "show identical"
    };
    let mut items: Vec<(&str, &str)> = if dir.help {
        vec![]
    } else if dir.prompt.is_some() {
        vec![("Enter", "search"), ("Esc", "cancel")]
    } else if dir.scanning {
        vec![("Enter", "open"), ("i", toggle), ("Esc", "stop scan")]
    } else {
        vec![("Enter", "open"), ("i", toggle), ("q", "quit")]
    };
    while items.len() > 1 && hints_len(&items) + 2 + x > end {
        items.remove(1);
    }
    let need = hints_len(&items);
    if !items.is_empty() && need + 2 <= end.saturating_sub(x) {
        let mut rx = end - need - 1;
        hints(buf, &mut rx, y, end, t, &items, "  ");
    }
}

fn draw_dir_help(buf: &mut Buffer, t: &Theme, area: Rect) {
    const HELP: &[(&str, &[(&str, &str)])] = &[
        (
            "Move",
            &[
                ("j k", "down, up"),
                ("Ctrl-d u", "half page"),
                ("gg G", "first, last"),
            ],
        ),
        (
            "Files",
            &[
                ("Enter", "open the file"),
                ("i", "show or hide identical"),
                ("Backspace", "back here from a file"),
            ],
        ),
        (
            "Search",
            &[
                ("/", "find a file name"),
                ("n N", "next, previous"),
                ("Esc", "stop the scan, clear search"),
            ],
        ),
        ("Other", &[("q", "quit"), ("Ctrl-c", "abort, exit 1")]),
        (
            "Mouse",
            &[
                ("Wheel", "scroll the list"),
                ("Click", "select, twice to open"),
            ],
        ),
    ];
    let mut y = 3u16;
    for (title, items) in HELP {
        if y + 1 >= area.height - 1 {
            break;
        }
        buf.set_stringn(2, y, title, 20, bold(Style::default().fg(t.bright)));
        y += 1;
        for (k, d) in *items {
            if y >= area.height - 1 {
                break;
            }
            buf.set_stringn(2, y, k, 10, key_style(t));
            buf.set_stringn(
                13,
                y,
                d,
                (area.width as usize).saturating_sub(14),
                Style::default(),
            );
            y += 1;
        }
        y += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn long_header_paths_are_clipped_to_leave_room_for_the_tag() {
        let (l, r) = fit_paths("/short", "/a/very/long/path/to/the/working/tree", 30);
        assert_eq!(l, "/short");
        assert!(r.chars().count() <= 24 && r.starts_with('…') && r.ends_with("tree"));
        let (l, r) = fit_paths(&"x".repeat(80), &"y".repeat(80), 40);
        assert_eq!((l.chars().count(), r.chars().count()), (20, 20));
        assert_eq!(fit_paths("a", "b", 40), ("a".into(), "b".into()));
        assert_eq!(fit_paths("abc", "def", 0), ("…".into(), "…".into()));
    }

    #[test]
    fn clip_keeps_the_end_of_long_paths() {
        assert_eq!(clip_left("src/a.rs", 20), "src/a.rs");
        assert_eq!(clip_left("some/long/path/file.rs", 10), "…h/file.rs");
        assert_eq!(clip_left("abc", 1), "…");
    }
}
