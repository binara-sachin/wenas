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
    /// Indexes into `rows`, file rows only.
    pub matches: Vec<usize>,
    pub current: Option<usize>,
}

/// A kind of file the list can leave out (spec §6.3). Each has a key and a
/// chip in the filter bar.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Filter {
    Identical,
    /// Files on one side only, either side.
    Missing,
    Binary,
    /// Anything with a path part starting with `.`.
    Hidden,
}

impl Filter {
    pub const ALL: [Filter; 4] = [
        Filter::Identical,
        Filter::Missing,
        Filter::Binary,
        Filter::Hidden,
    ];

    pub fn key(self) -> char {
        match self {
            Filter::Identical => 'i',
            Filter::Missing => 'm',
            Filter::Binary => 'b',
            Filter::Hidden => '.',
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Filter::Identical => "identical",
            Filter::Missing => "missing",
            Filter::Binary => "binary",
            Filter::Hidden => "hidden",
        }
    }

    fn matches(self, e: &Entry) -> bool {
        match self {
            Filter::Identical => !e.status.differs(),
            Filter::Missing => matches!(e.status, Status::LeftOnly | Status::RightOnly),
            Filter::Binary => e.binary || matches!(e.status, Status::Binary { .. }),
            Filter::Hidden => e
                .rel
                .components()
                .any(|c| c.as_os_str().as_encoded_bytes().first() == Some(&b'.')),
        }
    }
}

/// Which kinds of file the list shows. A file is listed only when every
/// filter it matches is on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Filters {
    pub identical: bool,
    pub missing: bool,
    pub binary: bool,
    pub hidden: bool,
}

impl Default for Filters {
    /// Everything but identical files, which are noise in most comparisons.
    fn default() -> Self {
        Filters {
            identical: false,
            missing: true,
            binary: true,
            hidden: true,
        }
    }
}

impl Filters {
    pub fn shows_kind(&self, f: Filter) -> bool {
        match f {
            Filter::Identical => self.identical,
            Filter::Missing => self.missing,
            Filter::Binary => self.binary,
            Filter::Hidden => self.hidden,
        }
    }

    pub fn toggle(&mut self, f: Filter) {
        let on = match f {
            Filter::Identical => &mut self.identical,
            Filter::Missing => &mut self.missing,
            Filter::Binary => &mut self.binary,
            Filter::Hidden => &mut self.hidden,
        };
        *on = !*on;
    }

    pub fn shows(&self, e: &Entry) -> bool {
        Filter::ALL
            .into_iter()
            .all(|f| self.shows_kind(f) || !f.matches(e))
    }
}

/// The screen row of the filter bar, between the header and the list.
const FILTER_Y: u16 = 1;

/// Where each filter's chip sits in the bar, as `(filter, x, width)`. Shared
/// by drawing and clicking. Chips that don't fit are left out.
fn chip_spans(width: u16) -> Vec<(Filter, u16, u16)> {
    let mut out = Vec::new();
    let mut x = 1 + FILTER_LABEL.len() as u16;
    for f in Filter::ALL {
        let w = chip_text(f, true).chars().count() as u16;
        if x + w + 1 > width {
            break;
        }
        out.push((f, x, w));
        x += w + 3;
    }
    out
}

const FILTER_LABEL: &str = "show  ";

/// "i [x] identical": the key, whether the kind is shown, its name.
fn chip_text(f: Filter, on: bool) -> String {
    format!("{} [{}] {}", f.key(), if on { 'x' } else { ' ' }, f.label())
}

/// What a line of the tree stands for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Node {
    /// A folder holding at least one listed file. `files` counts them all,
    /// however deep.
    Dir { rel: PathBuf, files: usize },
    /// A listed entry (index into `entries`).
    File(usize),
}

/// One line of the tree. Rows are in the walk's order (preorder), so a
/// folder is always followed by everything under it.
#[derive(Debug, Clone)]
pub struct Row {
    pub node: Node,
    pub depth: usize,
    parent: Option<usize>,
    /// No sibling follows (yet: one streaming in later clears it).
    last: bool,
    /// For folders, the newest child so far.
    last_child: Option<usize>,
}

impl Row {
    fn dir_rel(&self) -> Option<&PathBuf> {
        match &self.node {
            Node::Dir { rel, .. } => Some(rel),
            Node::File(_) => None,
        }
    }

    fn same(&self, node: &Node) -> bool {
        match (&self.node, node) {
            (Node::Dir { rel: a, .. }, Node::Dir { rel: b, .. }) => a == b,
            (a, b) => a == b,
        }
    }
}

pub struct DirView {
    pub left: PathBuf,
    pub right: PathBuf,
    pub entries: Vec<Entry>,
    /// The tree of listed entries and the folders that hold them.
    pub rows: Vec<Row>,
    /// Indexes into `rows` that are not inside a folded folder, in order.
    /// `cursor` and `top` are positions in this list.
    pub visible: Vec<usize>,
    /// Folded folders, by path, so folding survives rebuilding the tree.
    pub folded: HashSet<PathBuf>,
    pub filters: Filters,
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
    /// The row last pressed, and when, to recognise a double-click.
    last_click: Option<(usize, Instant)>,
    /// Folder rows holding the newest file, outermost first, so the next
    /// entry can reuse the ones it shares.
    open_dirs: Vec<usize>,
    /// The newest top-level row.
    last_top: Option<usize>,
    scan: Option<Scan>,
    started: Instant,
}

impl DirView {
    pub fn new(
        left: PathBuf,
        right: PathBuf,
        filters: Filters,
        scan: Scan,
        first: Vec<Msg>,
    ) -> DirView {
        let mut v = DirView {
            left,
            right,
            entries: Vec::new(),
            rows: Vec::new(),
            visible: Vec::new(),
            folded: HashSet::new(),
            filters,
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
            open_dirs: Vec::new(),
            last_top: None,
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
                let show = self.filters.shows(&e);
                self.entries.push(e);
                if show {
                    self.push_entry(self.entries.len() - 1);
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

    /// Add a listed entry to the tree, with any folders it needs. Entries
    /// arrive in the walk's order, so the folders it shares with the previous
    /// entry are the open ones it reuses.
    fn push_entry(&mut self, e: usize) {
        let rel = self.entries[e].rel.clone();
        let parts: Vec<_> = rel.components().collect();
        let dirs = parts.len().saturating_sub(1);
        let mut at = PathBuf::new();
        for (depth, part) in parts[..dirs].iter().enumerate() {
            at.push(part);
            let reuse = self
                .open_dirs
                .get(depth)
                .is_some_and(|&r| self.rows[r].dir_rel() == Some(&at));
            if !reuse {
                self.open_dirs.truncate(depth);
                let r = self.add_row(Node::Dir {
                    rel: at.clone(),
                    files: 0,
                });
                self.open_dirs.push(r);
            }
        }
        self.open_dirs.truncate(dirs);
        for &d in &self.open_dirs {
            if let Node::Dir { files, .. } = &mut self.rows[d].node {
                *files += 1;
            }
        }
        self.add_row(Node::File(e));
    }

    /// Append a row under the innermost open folder.
    fn add_row(&mut self, node: Node) -> usize {
        let parent = self.open_dirs.last().copied();
        let r = self.rows.len();
        let prev = match parent {
            Some(p) => self.rows[p].last_child.replace(r),
            None => self.last_top.replace(r),
        };
        if let Some(prev) = prev {
            self.rows[prev].last = false;
        }
        self.rows.push(Row {
            node,
            depth: self.open_dirs.len(),
            parent,
            last: true,
            last_child: None,
        });
        if !self.inside_folded(r) {
            self.visible.push(r);
        }
        r
    }

    fn is_folded(&self, row: usize) -> bool {
        self.rows[row]
            .dir_rel()
            .is_some_and(|rel| self.folded.contains(rel))
    }

    fn inside_folded(&self, row: usize) -> bool {
        let mut p = self.rows[row].parent;
        while let Some(a) = p {
            if self.is_folded(a) {
                return true;
            }
            p = self.rows[a].parent;
        }
        false
    }

    /// The row under the cursor.
    fn current_row(&self) -> Option<usize> {
        self.visible.get(self.cursor).copied()
    }

    /// The entry at a position in `visible`, if that row is a file.
    pub fn entry_at(&self, pos: usize) -> Option<usize> {
        match self.rows[*self.visible.get(pos)?].node {
            Node::File(e) => Some(e),
            Node::Dir { .. } => None,
        }
    }

    /// Where the entry with this path is in `visible`.
    pub fn position_of(&self, rel: &std::path::Path) -> Option<usize> {
        (0..self.visible.len())
            .find(|&p| self.entry_at(p).is_some_and(|e| self.entries[e].rel == rel))
    }

    pub fn selected(&self) -> Option<usize> {
        self.entry_at(self.cursor)
    }

    /// Put the cursor on a row, or the nearest visible one before it.
    fn cursor_to(&mut self, row: usize) {
        self.cursor = match self.visible.binary_search(&row) {
            Ok(p) => p,
            Err(p) => p.saturating_sub(1),
        };
        self.cursor = self.cursor.min(self.visible.len().saturating_sub(1));
        self.ensure_visible();
    }

    /// Show or hide a kind of file, keeping the selection where it can.
    pub fn toggle(&mut self, f: Filter) {
        self.filters.toggle(f);
        self.reshow();
    }

    /// Entries the filters leave out.
    pub fn filtered_out(&self) -> usize {
        let listed = self
            .rows
            .iter()
            .filter(|r| matches!(r.node, Node::File(_)))
            .count();
        self.entries.len() - listed
    }

    /// Recompute `visible` after folding or unfolding, keeping the cursor's row.
    fn refold(&mut self) {
        let keep = self.current_row();
        let mut visible = Vec::with_capacity(self.rows.len());
        // While set, rows deeper than this are inside a folded folder.
        let mut skip: Option<usize> = None;
        for (i, r) in self.rows.iter().enumerate() {
            if let Some(d) = skip {
                if r.depth > d {
                    continue;
                }
                skip = None;
            }
            visible.push(i);
            if self.is_folded(i) {
                skip = Some(r.depth);
            }
        }
        self.visible = visible;
        self.last_click = None;
        if let Some(k) = keep {
            self.cursor_to(k);
        }
        // Folding near the end would otherwise leave the screen half empty.
        self.top = self
            .top
            .min(self.visible.len().saturating_sub(self.content_height()));
    }

    fn set_folded(&mut self, row: usize, fold: bool) {
        let Some(rel) = self.rows[row].dir_rel().cloned() else {
            return;
        };
        let changed = if fold {
            self.folded.insert(rel)
        } else {
            self.folded.remove(&rel)
        };
        if changed {
            self.refold();
        }
    }

    /// Rebuild the tree, keeping the cursor on the same row when it is still listed.
    fn reshow(&mut self) {
        let keep = self.current_row().map(|r| self.rows[r].node.clone());
        let at = self.cursor;
        self.rows.clear();
        self.visible.clear();
        self.open_dirs.clear();
        self.last_top = None;
        self.last_click = None;
        for i in 0..self.entries.len() {
            if self.filters.shows(&self.entries[i]) {
                self.push_entry(i);
            }
        }
        self.cursor = keep
            .and_then(|k| self.visible.iter().position(|&r| self.rows[r].same(&k)))
            .unwrap_or(at);
        self.cursor = self.cursor.min(self.visible.len().saturating_sub(1));
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
        let last = self.visible.len().saturating_sub(1) as isize;
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
        // Full paths, so `auth/tok` works, and files inside folded folders count too.
        let matches: Vec<usize> = (0..self.rows.len())
            .filter(|&r| {
                let Node::File(e) = self.rows[r].node else {
                    return false;
                };
                let p = self.entries[e].rel.display().to_string();
                if ci {
                    p.to_lowercase().contains(&q)
                } else {
                    p.contains(&q)
                }
            })
            .collect();
        let here = self.current_row().unwrap_or(0);
        let current = matches
            .iter()
            .position(|&m| m >= here)
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
            let row = s.matches[c];
            // Unfold whatever hides the match.
            let mut unfolded = false;
            let mut p = self.rows[row].parent;
            while let Some(a) = p {
                if let Some(rel) = self.rows[a].dir_rel() {
                    unfolded |= self.folded.remove(rel);
                }
                p = self.rows[a].parent;
            }
            if unfolded {
                self.refold();
            }
            self.cursor_to(row);
        }
    }

    /// `h`: fold the folder under the cursor, or go up to the one holding it.
    fn fold_or_up(&mut self) {
        let Some(row) = self.current_row() else {
            return;
        };
        if self.rows[row].dir_rel().is_some() && !self.is_folded(row) {
            self.set_folded(row, true);
        } else if let Some(p) = self.rows[row].parent {
            self.cursor_to(p);
        }
    }

    /// `l`: unfold the folder under the cursor, or step into it.
    fn unfold_or_down(&mut self) {
        let Some(row) = self.current_row() else {
            return;
        };
        if self.is_folded(row) {
            self.set_folded(row, false);
        } else if self.rows[row].dir_rel().is_some() {
            self.move_by(1);
        }
    }

    /// Enter, or a double-click: open a file, or fold or unfold a folder.
    fn activate(&mut self) -> DirAction {
        let Some(row) = self.current_row() else {
            return DirAction::None;
        };
        match self.rows[row].node {
            Node::File(e) => DirAction::Open(e),
            Node::Dir { .. } => {
                let fold = !self.is_folded(row);
                self.set_folded(row, fold);
                DirAction::None
            }
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
                self.cursor = self.visible.len().saturating_sub(1);
                self.ensure_visible();
            }
            KeyCode::Char('h') | KeyCode::Left => self.fold_or_up(),
            KeyCode::Char('l') | KeyCode::Right => self.unfold_or_down(),
            KeyCode::Enter => return self.activate(),
            KeyCode::Char(c) if Filter::ALL.iter().any(|f| f.key() == c) => {
                let f = Filter::ALL.into_iter().find(|f| f.key() == c).unwrap();
                self.toggle(f);
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
        let Some(last) = self.visible.len().checked_sub(1) else {
            return false;
        };
        if !self.takes_mouse() || lines == 0 {
            return false;
        }
        let before = (self.top, self.cursor);
        let h = self.content_height();
        let max_top = self.visible.len().saturating_sub(h).max(self.top);
        self.top = (self.top as isize + lines).clamp(0, max_top as isize) as usize;
        let bottom = (self.top + h - 1).min(last);
        self.cursor = self.cursor.clamp(self.top.min(bottom), bottom);
        let changed = before != (self.top, self.cursor);
        if changed {
            self.pointer_acted();
        }
        changed
    }

    /// The position in `visible` drawn at this screen row.
    fn row_at(&self, y: u16) -> Option<usize> {
        if too_small(self.width, self.height) || y < CONTENT_Y {
            return None;
        }
        let offset = (y - CONTENT_Y) as usize;
        let row = self.top + offset;
        (offset < self.content_height() && row < self.visible.len()).then_some(row)
    }

    /// A press on a filter chip flips it. A press selects a row and a second
    /// one on it opens the file or folds the folder. `now` is passed in so tests can control the timing. `None`
    /// means the event did nothing.
    pub fn on_mouse(&mut self, event: MouseEvent, now: Instant) -> Option<DirAction> {
        if event.kind != MouseEventKind::Down(MouseButton::Left)
            || !event.modifiers.is_empty()
            || !self.takes_mouse()
        {
            return None;
        }
        if event.row == FILTER_Y && !too_small(self.width, self.height) {
            let (f, ..) = chip_spans(self.width)
                .into_iter()
                .find(|&(_, x, w)| (x..x + w).contains(&event.column))?;
            self.pointer_acted();
            self.toggle(f);
            return Some(DirAction::None);
        }
        let pos = self.row_at(event.row)?;
        let row = self.visible[pos];
        self.cursor = pos;
        self.pointer_acted();
        let double = self
            .last_click
            .is_some_and(|(r, at)| r == row && now.duration_since(at) <= DOUBLE_CLICK);
        if double {
            self.last_click = None;
            Some(self.activate())
        } else {
            self.last_click = Some((row, now));
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

/// The last part of a path, or all of it when it has none (an error on a root).
fn leaf_name(rel: &std::path::Path) -> String {
    rel.file_name().map_or_else(
        || rel.display().to_string(),
        |n| n.to_string_lossy().into_owned(),
    )
}

/// The `tree`-style lines in front of a row's name: a `│` for every enclosing
/// folder that has more to come, then `├──` or `└──` for the row itself.
fn branches(dir: &DirView, row: usize) -> String {
    let mut parts = Vec::with_capacity(dir.rows[row].depth + 1);
    let r = &dir.rows[row];
    parts.push(if r.last { "└── " } else { "├── " });
    let mut p = r.parent;
    while let Some(a) = p {
        parts.push(if dir.rows[a].last { "    " } else { "│   " });
        p = dir.rows[a].parent;
    }
    parts.iter().rev().copied().collect()
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
    // The paths give way (keeping their ends) so both stay in view, however
    // long the temp directories are (macOS paths easily exceed 60 columns).
    let budget = (area.width as usize).saturating_sub(2 + HEADER_LABELS);
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
    draw_filters(buf, dir, t, area);
    if dir.help {
        draw_dir_help(buf, t, area);
    } else {
        draw_rows(buf, dir, t, area);
    }
    draw_dir_status(buf, dir, t, area);
}

/// The filter bar: a chip per kind of file, bright when shown.
fn draw_filters(buf: &mut Buffer, dir: &DirView, t: &Theme, area: Rect) {
    let mut x = 1;
    put(
        buf,
        &mut x,
        FILTER_Y,
        area.width,
        FILTER_LABEL,
        Style::default().fg(t.dim),
    );
    for (f, cx, _) in chip_spans(area.width) {
        let on = dir.filters.shows_kind(f);
        let text = chip_text(f, on);
        let (key, rest) = text.split_at(1);
        let mut x = cx;
        put(buf, &mut x, FILTER_Y, area.width, key, key_style(t));
        let style = if on {
            Style::default().fg(t.bright)
        } else {
            Style::default().fg(t.dim)
        };
        put(buf, &mut x, FILTER_Y, area.width, rest, style);
    }
}

fn draw_rows(buf: &mut Buffer, dir: &DirView, t: &Theme, area: Rect) {
    let h = dir.height as usize - 3;
    let detail_x = (area.width as usize * 21 / 40).max(24);
    let path_w = detail_x.saturating_sub(6);
    if dir.visible.is_empty() && !dir.scanning {
        let msg = if dir.entries.is_empty() {
            "No files."
        } else if dir.differ == 0 && !dir.filters.identical {
            "No differences. Press i to show identical files."
        } else {
            "Every file is filtered out. Turn a filter on above to see more."
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
        let Some(&row) = dir.visible.get(i) else {
            break;
        };
        let y = r as u16 + CONTENT_Y;
        let selected = i == dir.cursor;
        if selected {
            buf.set_style(
                Rect::new(0, y, area.width, 1),
                t.emphasis(t.bg(t.changed_bg)),
            );
        }
        let base = if selected {
            t.bg(t.changed_bg)
        } else {
            Style::default()
        };
        let node = &dir.rows[row].node;
        let status = match node {
            Node::File(e) => Some(&dir.entries[*e].status),
            Node::Dir { .. } => None,
        };
        let letter = status.map_or(' ', Status::letter).to_string();
        let mut ls = status.map_or(Style::default(), |s| letter_style(t, s));
        if selected {
            ls = if t.no_color {
                ls.add_modifier(Modifier::REVERSED)
            } else {
                bold(Style::default().fg(t.on_amber).bg(t.amber))
            };
        }
        buf.set_stringn(1, y, format!(" {letter} "), 3, ls);
        let (name, name_style) = match node {
            Node::Dir { rel, .. } => (format!("{}/", leaf_name(rel)), bold(base.fg(t.muted))),
            Node::File(e) => {
                let entry = &dir.entries[*e];
                let fg = if entry.status.differs() {
                    t.bright
                } else {
                    t.dim
                };
                (leaf_name(&entry.rel), base.fg(fg))
            }
        };
        // The branches give way first on a deep tree, then the name's start.
        let line = clip_left(&format!("{}{name}", branches(dir, row)), path_w);
        let total = line.chars().count();
        let split = total - name.chars().count().min(total);
        let split = line
            .char_indices()
            .nth(split)
            .map_or(line.len(), |(b, _)| b);
        buf.set_stringn(5, y, &line[..split], path_w, base.fg(t.dim));
        let nx = 5 + line[..split].chars().count() as u16;
        buf.set_stringn(nx, y, &line[split..], path_w, name_style);
        let (text, style) = match node {
            Node::Dir { files, .. } if dir.is_folded(row) => (
                format!(
                    "folded, {} file{}",
                    thousands(*files),
                    if *files == 1 { "" } else { "s" }
                ),
                base.fg(t.dim),
            ),
            Node::Dir { .. } => (String::new(), base),
            Node::File(e) if dir.unsaved.contains(e) => {
                ("unsaved changes [+]".to_string(), bold(base.fg(t.amber)))
            }
            Node::File(e) => match &dir.entries[*e].status {
                Status::Error(m) => (m.clone(), base.fg(t.removed_fg)),
                s => (s.detail(), base.fg(t.dim)),
            },
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
        let out = dir.filtered_out();
        if out > 0 {
            put(
                buf,
                &mut x,
                y,
                end,
                &format!("  {} filtered out", thousands(out)),
                bg(t.dim),
            );
        }
        if let Some(s) = &dir.search {
            let pos = s.current.map_or("no matches".to_string(), |c| {
                format!("{} of {}", c + 1, s.matches.len())
            });
            put(buf, &mut x, y, end, &format!("   {pos}"), bg(t.amber));
        }
    }
    let toggle = if dir.filters.identical {
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
                ("h l", "fold, unfold a folder"),
            ],
        ),
        (
            "Files",
            &[
                ("Enter", "open the file, fold a folder"),
                (
                    "i m b .",
                    "show or hide identical, missing, binary, dot files",
                ),
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
                ("Click", "select, twice to open or fold"),
                ("Click chip", "show or hide that kind"),
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
    fn long_header_paths_are_clipped_to_fit() {
        let (l, r) = fit_paths("/short", "/a/very/long/path/to/the/working/tree", 30);
        assert_eq!(l, "/short");
        assert!(r.chars().count() <= 24 && r.starts_with('…') && r.ends_with("tree"));
        let (l, r) = fit_paths(&"x".repeat(80), &"y".repeat(80), 40);
        assert_eq!((l.chars().count(), r.chars().count()), (20, 20));
        assert_eq!(fit_paths("a", "b", 40), ("a".into(), "b".into()));
        assert_eq!(fit_paths("abc", "def", 0), ("…".into(), "…".into()));
    }

    #[test]
    fn chips_that_do_not_fit_are_left_out() {
        assert_eq!(chip_spans(200).len(), 4);
        let narrow = chip_spans(40);
        assert!(!narrow.is_empty() && narrow.len() < 4);
        assert!(narrow.iter().all(|&(_, x, w)| x + w < 40));
        assert!(chip_spans(10).is_empty());
    }

    #[test]
    fn clip_keeps_the_end_of_long_paths() {
        assert_eq!(clip_left("src/a.rs", 20), "src/a.rs");
        assert_eq!(clip_left("some/long/path/file.rs", 10), "…h/file.rs");
        assert_eq!(clip_left("abc", 1), "…");
    }
}
