//! The directory list (spec §6.3, §7.2).

use super::chrome::{
    BODY_Y, CHROME_ROWS, Roots, Spans, TITLE_Y, TOOLBAR_Y, Tier, clip_left, fill, middle_cut,
    split_name, tier, title_style,
};
use super::render::{bold, fit_hints, key_style, put, thousands, too_small};
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
use unicode_width::UnicodeWidthStr;

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

    /// The chip's name; shorter below 140 columns.
    pub fn label(self, tier: Tier) -> &'static str {
        match (self, tier) {
            (Filter::Identical, _) => "identical",
            (Filter::Missing, Tier::Full) => "one side only",
            (Filter::Missing, _) => "one side",
            (Filter::Binary, _) => "binary",
            (Filter::Hidden, Tier::Full) => "hidden files",
            (Filter::Hidden, _) => "hidden",
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

/// Where each filter's chip sits in the toolbar, as `(filter, x, width)`.
/// Shared by drawing and clicking. Chips that don't fit are left out.
fn chip_spans(width: u16) -> Vec<(Filter, u16, u16)> {
    let mut out = Vec::new();
    let t = tier(width);
    let gap = if t == Tier::Full { 4 } else { 3 };
    let mut x = 1 + FILTER_LABEL.len() as u16;
    for f in Filter::ALL {
        let w = chip_text(f, true, t).chars().count() as u16;
        if x + w + 1 > width {
            break;
        }
        out.push((f, x, w));
        x += w + gap;
    }
    out
}

const FILTER_LABEL: &str = "show  ";

/// "i ● identical": the key, whether the kind is shown, its name.
fn chip_text(f: Filter, on: bool, tier: Tier) -> String {
    format!(
        "{} {} {}",
        f.key(),
        if on { '●' } else { '○' },
        f.label(tier)
    )
}

/// What a line of the tree stands for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Node {
    /// A folder holding at least one listed file. `files` counts them all,
    /// however deep, and `changed` the ones that differ.
    Dir {
        rel: PathBuf,
        files: usize,
        changed: usize,
    },
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
    /// For folders, how many rows sit directly under it.
    children: usize,
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
    /// The first key of `gg`, `]f` or `[f`.
    pending: Option<char>,
    /// Rows changed since `visible` was last computed.
    stale: bool,
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
            pending: None,
            stale: false,
            last_click: None,
            open_dirs: Vec::new(),
            last_top: None,
            scan: Some(scan),
            started: Instant::now(),
        };
        for m in first {
            v.apply_one(m);
        }
        v.refresh();
        v
    }

    fn content_height(&self) -> usize {
        (self.height as usize)
            .saturating_sub(CHROME_ROWS as usize)
            .max(1)
    }

    /// Take one message from the scan.
    pub fn apply(&mut self, m: Msg) {
        self.apply_one(m);
        self.refresh();
    }

    /// Recompute `visible` if rows arrived: a folder that gains a second child
    /// stops being part of a chain, which changes rows above the new one.
    fn refresh(&mut self) {
        if std::mem::take(&mut self.stale) {
            self.refold();
        }
    }

    fn apply_one(&mut self, m: Msg) {
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
            self.apply_one(m);
        }
        self.refresh();
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
                    changed: 0,
                });
                self.open_dirs.push(r);
            }
        }
        self.open_dirs.truncate(dirs);
        let differs = self.entries[e].status.differs();
        for &d in &self.open_dirs {
            if let Node::Dir { files, changed, .. } = &mut self.rows[d].node {
                *files += 1;
                *changed += differs as usize;
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
        if let Some(p) = parent {
            self.rows[p].children += 1;
        }
        self.rows.push(Row {
            node,
            depth: self.open_dirs.len(),
            parent,
            last: true,
            last_child: None,
            children: 0,
        });
        self.stale = true;
        r
    }

    /// A folder with one child below the top level shares that child's row:
    /// a chain of such folders reads as one path (spec §6.3).
    fn is_merged(&self, row: usize) -> bool {
        let r = &self.rows[row];
        r.dir_rel().is_some() && r.parent.is_some() && r.children == 1 && !self.is_folded(row)
    }

    /// The nearest folder above a row that has a row of its own.
    fn shown_parent(&self, row: usize) -> Option<usize> {
        let mut p = self.rows[row].parent;
        while let Some(a) = p {
            if !self.is_merged(a) {
                return Some(a);
            }
            p = self.rows[a].parent;
        }
        None
    }

    /// How far a row is indented: the folders above it with rows of their own.
    fn shown_depth(&self, row: usize) -> usize {
        let mut n = 0;
        let mut p = self.shown_parent(row);
        while let Some(a) = p {
            n += 1;
            p = self.shown_parent(a);
        }
        n
    }

    /// The folders merged into a row, as a path ending in `/`.
    fn merged_prefix(&self, row: usize) -> String {
        let mut parts = Vec::new();
        let mut p = self.rows[row].parent;
        while let Some(a) = p.filter(|&a| self.is_merged(a)) {
            parts.push(
                self.rows[a]
                    .dir_rel()
                    .map(|r| leaf_name(r))
                    .unwrap_or_default(),
            );
            p = self.rows[a].parent;
        }
        parts.iter().rev().map(|s| format!("{s}/")).collect()
    }

    fn is_folded(&self, row: usize) -> bool {
        self.rows[row]
            .dir_rel()
            .is_some_and(|rel| self.folded.contains(rel))
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

    /// Put the cursor on a row, or the nearest visible one before it (after
    /// it, for a folder merged into the row below).
    fn cursor_to(&mut self, row: usize) {
        let merged = row < self.rows.len() && self.is_merged(row);
        self.cursor = match self.visible.binary_search(&row) {
            Ok(p) => p,
            Err(p) if merged => p,
            Err(p) => p.saturating_sub(1),
        };
        self.cursor = self.cursor.min(self.visible.len().saturating_sub(1));
        self.ensure_margin();
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
            if self.is_merged(i) {
                continue;
            }
            visible.push(i);
            if self.is_folded(i) {
                skip = Some(r.depth);
            }
        }
        self.visible = visible;
        self.stale = false;
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
        self.refold();
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

    /// Rows of context kept above and below the selection when it moves.
    const MARGIN: usize = 3;

    /// Scroll so the selection has `MARGIN` rows on each side, where the
    /// list allows it.
    fn ensure_margin(&mut self) {
        let h = self.content_height();
        let m = Self::MARGIN.min(h.saturating_sub(1) / 2);
        let max_top = self.visible.len().saturating_sub(h);
        if self.cursor < self.top + m {
            self.top = self.cursor.saturating_sub(m);
        } else if self.cursor + m >= self.top + h {
            self.top = (self.cursor + m + 1).saturating_sub(h).min(max_top);
        }
        self.ensure_visible();
    }

    fn move_by(&mut self, d: isize) {
        let last = self.visible.len().saturating_sub(1) as isize;
        self.cursor = (self.cursor as isize + d).clamp(0, last.max(0)) as usize;
        self.ensure_margin();
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
            self.reveal(row);
        }
    }

    /// Unfold whatever hides a row, and put the cursor on it.
    fn reveal(&mut self, row: usize) {
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

    /// The row of a listed entry.
    fn row_of_entry(&self, entry: usize) -> Option<usize> {
        self.rows.iter().position(|r| r.node == Node::File(entry))
    }

    /// The next (or previous) listed file that differs, after row `from`.
    fn change_from(&self, from: usize, forward: bool) -> Option<usize> {
        let differs = |r: &usize| match self.rows[*r].node {
            Node::File(e) => self.entries[e].status.differs(),
            Node::Dir { .. } => false,
        };
        if forward {
            (from + 1..self.rows.len()).find(differs)
        } else {
            (0..from).rev().find(differs)
        }
    }

    /// `]f` / `[f`: select the next or previous changed file.
    fn step_change(&mut self, forward: bool) {
        let found = self
            .current_row()
            .and_then(|from| self.change_from(from, forward));
        match found {
            Some(r) => self.reveal(r),
            None => {
                self.message = Some(if forward {
                    "no more changes below".into()
                } else {
                    "no more changes above".into()
                })
            }
        }
    }

    /// The changed entry after (or before) this one in the list, for `]f` in a file.
    pub fn change_after(&self, entry: usize, forward: bool) -> Option<usize> {
        let row = self.row_of_entry(entry)?;
        match self.rows[self.change_from(row, forward)?].node {
            Node::File(e) => Some(e),
            Node::Dir { .. } => None,
        }
    }

    /// Select a listed entry, unfolding what hides it.
    pub fn select_entry(&mut self, entry: usize) {
        if let Some(r) = self.row_of_entry(entry) {
            self.reveal(r);
        }
    }

    /// An entry's place among the listed files, from 0, and how many there are.
    pub fn file_position(&self, entry: usize) -> Option<(usize, usize)> {
        let mut at = None;
        let mut n = 0;
        for r in &self.rows {
            if let Node::File(e) = r.node {
                if e == entry {
                    at = Some(n);
                }
                n += 1;
            }
        }
        at.map(|a| (a, n))
    }

    /// The selection's place among the listed files: the selected file, or for
    /// a folder the first file under it.
    fn selection_position(&self) -> Option<(usize, usize)> {
        let row = self.current_row()?;
        let mut at = None;
        let mut n = 0;
        for (i, r) in self.rows.iter().enumerate() {
            if let Node::File(_) = r.node {
                if i >= row && at.is_none() {
                    at = Some(n);
                }
                n += 1;
            }
        }
        at.map(|a| (a, n))
    }

    /// `h`: fold the folder under the cursor, or go up to the one holding it.
    fn fold_or_up(&mut self) {
        let Some(row) = self.current_row() else {
            return;
        };
        if self.rows[row].dir_rel().is_some() && !self.is_folded(row) {
            self.set_folded(row, true);
        } else if let Some(p) = self.shown_parent(row) {
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
        match (self.pending.take(), key.code) {
            (Some('g'), KeyCode::Char('g')) => {
                self.cursor = 0;
                self.ensure_visible();
                return DirAction::None;
            }
            (Some(']'), KeyCode::Char('f')) => {
                self.step_change(true);
                return DirAction::None;
            }
            (Some('['), KeyCode::Char('f')) => {
                self.step_change(false);
                return DirAction::None;
            }
            _ => {}
        }
        match key.code {
            KeyCode::Char('j') | KeyCode::Down => self.move_by(1),
            KeyCode::Char('k') | KeyCode::Up => self.move_by(-1),
            KeyCode::Char('d') if ctrl => self.move_by((self.content_height() / 2).max(1) as isize),
            KeyCode::Char('u') if ctrl => {
                self.move_by(-((self.content_height() / 2).max(1) as isize))
            }
            KeyCode::Char(c @ ('g' | '[' | ']')) => self.pending = Some(c),
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
        self.pending = None;
    }

    /// Pasted text goes into the name search prompt and nowhere else.
    pub fn on_paste(&mut self, text: &str) -> bool {
        if self.help {
            return false;
        }
        self.pending = None;
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
        if too_small(self.width, self.height) || y < BODY_Y {
            return None;
        }
        let offset = (y - BODY_Y) as usize;
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
        if event.row == TOOLBAR_Y && !too_small(self.width, self.height) {
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

/// What a status is called in the status bar.
fn status_word(s: &Status) -> String {
    match s {
        Status::Modified => "modified".into(),
        Status::Identical => "identical".into(),
        s => s.detail(),
    }
}

const SPINNER: [&str; 8] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧"];

/// Width of the status letter column: " M  ".
const LETTER_W: u16 = 4;

pub fn draw_dir(buf: &mut Buffer, dir: &mut DirView, t: &Theme, area: Rect) {
    dir.width = area.width;
    dir.height = area.height;
    dir.ensure_visible();
    if too_small(area.width, area.height) {
        buf.set_string(0, 0, "terminal too small", Style::default());
        return;
    }
    draw_title(buf, dir, t, area);
    draw_toolbar(buf, dir, t, area);
    if dir.help {
        draw_dir_help(buf, t, area);
    } else {
        draw_rows(buf, dir, t, area);
    }
    draw_dir_status(buf, dir, t, area);
}

/// The title bar: the two roots, with only their difference in colour and the
/// path they share after it once, and the selection's place in the list.
fn draw_title(buf: &mut Buffer, dir: &DirView, t: &Theme, area: Rect) {
    let bg = title_style(t);
    fill(buf, TITLE_Y, area.width, bg);
    let mut right = Spans::default();
    if let Some((i, n)) = dir.selection_position() {
        right.push(
            format!("{} of {}", thousands(i + 1), thousands(n)),
            bg.fg(t.muted),
        );
    }
    let end = right
        .draw_right(buf, TITLE_Y, 0, area.width)
        .unwrap_or(area.width);
    let roots = Roots::new(
        &dir.left.display().to_string(),
        &dir.right.display().to_string(),
    );
    let mut left = Spans::default();
    left.push(" wenas", bold(bg.fg(t.bright)));
    if tier(area.width) != Tier::Narrow {
        left.push("    ", bg);
        left.0.extend(roots.spans(t, bg, false).0);
        let room = end.saturating_sub(left.width() + 5) as usize;
        if !roots.tail.is_empty() {
            left.push("   ", bg)
                .push(clip_left(&roots.tail, room), bg.fg(t.dim));
        }
    }
    left.draw(buf, 0, TITLE_Y, end.saturating_sub(2));
}

/// The toolbar: the filter chips and find, and on the right how many files
/// are shown.
fn draw_toolbar(buf: &mut Buffer, dir: &DirView, t: &Theme, area: Rect) {
    let bg = Style::default().bg(t.panel_bg);
    fill(buf, TOOLBAR_Y, area.width, bg);
    let tier = tier(area.width);
    let mut x = 1;
    put(
        buf,
        &mut x,
        TOOLBAR_Y,
        area.width,
        FILTER_LABEL,
        bg.fg(t.dim),
    );
    for (f, cx, _) in chip_spans(area.width) {
        let on = dir.filters.shows_kind(f);
        let text = chip_text(f, on, tier);
        let (key, rest) = text.split_at(1);
        x = cx;
        put(
            buf,
            &mut x,
            TOOLBAR_Y,
            area.width,
            key,
            key_style(t).bg(t.panel_bg),
        );
        let style = if on { bg.fg(t.bright) } else { bg.fg(t.dim) };
        put(buf, &mut x, TOOLBAR_Y, area.width, rest, style);
    }
    let gap = if tier == Tier::Full { 6 } else { 3 };
    let mut find = Spans::default();
    find.push("/", key_style(t).bg(t.panel_bg))
        .push(" find", bg.fg(t.muted));
    if let Some(s) = &dir.search {
        find.push(format!(" {}", s.query), bg.fg(t.bright));
    }
    let fx = x + gap;
    let mut end_left = x;
    if fx + find.width() + 1 < area.width {
        end_left = find.draw(buf, fx, TOOLBAR_Y, area.width);
    }
    let listed = dir
        .rows
        .iter()
        .filter(|r| matches!(r.node, Node::File(_)))
        .count();
    let mut right = Spans::default();
    if dir.scanning {
        let frame = SPINNER[(dir.started.elapsed().as_millis() / 100) as usize % SPINNER.len()];
        right.push(frame, bg.fg(t.amber)).push(
            format!(
                " scanning  {} files  ·  {} differ so far",
                thousands(dir.files_seen.max(dir.entries.len())),
                thousands(dir.differ)
            ),
            bg.fg(t.muted),
        );
    } else {
        right.push(
            format!(
                "{} of {} files",
                thousands(listed),
                thousands(dir.entries.len())
            ),
            bg.fg(t.muted),
        );
        let out = dir.filtered_out();
        if out > 0 {
            let only_identical = !dir.filters.identical
                && dir.filters.missing
                && dir.filters.binary
                && dir.filters.hidden;
            let what = if only_identical {
                format!("{} identical hidden", thousands(out))
            } else {
                format!("{} filtered out", thousands(out))
            };
            right.push(format!("  ·  {what}"), bg.fg(t.dim));
        }
    }
    if right
        .draw_right(buf, TOOLBAR_Y, end_left + 2, area.width)
        .is_none()
    {
        // Too long beside the chips: the short form.
        let mut short = Spans::default();
        if dir.scanning {
            let frame = SPINNER[(dir.started.elapsed().as_millis() / 100) as usize % SPINNER.len()];
            short.push(frame, bg.fg(t.amber)).push(
                format!(
                    " scanning  {} files",
                    thousands(dir.files_seen.max(dir.entries.len()))
                ),
                bg.fg(t.muted),
            );
        } else {
            short.push(
                format!("{} of {}", thousands(listed), thousands(dir.entries.len())),
                bg.fg(t.muted),
            );
        }
        short.draw_right(buf, TOOLBAR_Y, end_left + 2, area.width);
    }
}

fn draw_rows(buf: &mut Buffer, dir: &DirView, t: &Theme, area: Rect) {
    let h = dir.content_height();
    let tier = tier(area.width);
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
            BODY_Y,
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
        let y = r as u16 + BODY_Y;
        let selected = i == dir.cursor;
        let base = if !selected {
            Style::default()
        } else if t.no_color {
            Style::default().add_modifier(Modifier::REVERSED)
        } else {
            Style::default().bg(t.select_bg)
        };
        if selected {
            buf.set_style(Rect::new(0, y, area.width, 1), base);
        }
        let node = &dir.rows[row].node;
        let status = match node {
            Node::File(e) => Some(&dir.entries[*e].status),
            Node::Dir { .. } => None,
        };
        let letter = status.map_or(' ', Status::letter);
        let mut ls = status.map_or(base, |s| letter_style(t, s).patch(base));
        if selected && status.is_some() {
            ls = if t.no_color {
                bold(Style::default())
            } else {
                bold(Style::default().fg(t.on_amber).bg(t.amber))
            };
        }
        buf.set_stringn(0, y, format!(" {letter}  "), LETTER_W as usize, ls);

        // The note on the right: a folder's count, a file's status.
        let (note, note_style) = match node {
            Node::Dir { files, changed, .. } => {
                let text = match (tier, *changed) {
                    (Tier::Full, 0) => format!("{} files", thousands(*files)),
                    (Tier::Full, c) => format!("{} changed", thousands(c)),
                    (_, 0) => thousands(*files),
                    (_, c) => thousands(c),
                };
                (text, base.fg(t.dim))
            }
            Node::File(e) if dir.unsaved.contains(e) => {
                ("unsaved changes [+]".to_string(), bold(base.fg(t.amber)))
            }
            Node::File(e) => match &dir.entries[*e].status {
                Status::Error(m) => (m.clone(), base.fg(t.removed_fg)),
                Status::Modified => (String::new(), base),
                s => (s.detail(), base.fg(t.dim)),
            },
        };
        let note_w = (note.width() as u16).min(area.width / 2);
        let note_x = area.width.saturating_sub(note_w + 2);
        if note_w > 0 {
            buf.set_stringn(note_x, y, &note, note_w as usize, note_style);
        }

        // The name: indented by depth, folders marked open or folded.
        let max_x = if note_w > 0 {
            note_x.saturating_sub(2)
        } else {
            area.width.saturating_sub(1)
        };
        let depth = dir.shown_depth(row);
        let mark = match node {
            Node::Dir { .. } if dir.is_folded(row) => "▸ ",
            Node::Dir { .. } => "▾ ",
            Node::File(_) => "  ",
        };
        let indent = format!("{}{mark}", "  ".repeat(depth));
        let mut x = LETTER_W;
        let mark_style = if t.no_color { base } else { base.fg(t.dim) };
        put(buf, &mut x, y, max_x, &indent, mark_style);
        let prefix = dir.merged_prefix(row);
        let room = max_x.saturating_sub(x) as usize;
        match node {
            Node::Dir { rel, .. } => {
                let text = format!("{prefix}{}/", leaf_name(rel));
                put(
                    buf,
                    &mut x,
                    y,
                    max_x,
                    &clip_left(&text, room),
                    base.fg(t.muted),
                );
            }
            Node::File(e) => {
                let entry = &dir.entries[*e];
                let name = leaf_name(&entry.rel);
                let fg = if entry.status.differs() {
                    t.bright
                } else {
                    t.dim
                };
                let pre = middle_cut(&prefix, room.saturating_sub(name.width()));
                put(buf, &mut x, y, max_x, &pre, base.fg(t.dim));
                let room = max_x.saturating_sub(x) as usize;
                put(buf, &mut x, y, max_x, &clip_left(&name, room), base.fg(fg));
            }
        }
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
        fill(buf, y, area.width, style);
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
    let bg = Style::default().bg(t.panel_bg);
    fill(buf, y, area.width, bg);
    let items: Vec<(&str, &str, bool)> = if dir.help {
        vec![("F1 Esc", "close", false)]
    } else if dir.prompt.is_some() {
        vec![("Enter", "search", false), ("Esc", "cancel", false)]
    } else if dir.search.is_some() {
        vec![
            ("n N", "next, previous match", false),
            ("Esc", "clear", false),
            ("F1", "help", false),
        ]
    } else {
        let on_folder = dir
            .current_row()
            .is_some_and(|r| dir.rows[r].dir_rel().is_some());
        let mut v = vec![
            ("Enter", "open", on_folder),
            ("h l", "fold folder", false),
            ("]f [f", "next, previous change", false),
            ("F1", "help", false),
        ];
        v.push(if dir.scanning {
            ("Esc", "stop scan", false)
        } else {
            ("q", "quit", false)
        });
        v
    };
    let mut left = status_left(dir, t, end.saturating_sub(2));
    // The left side is what the bar is for; the hints take what is left, `F1
    // help` and `q quit` last of all.
    let min_x = (left.width() + 1).min(end / 2);
    let right = fit_hints(t, end, min_x, items, None, 2);
    let start = right.draw_right(buf, y, 0, end).unwrap_or(end);
    let room = start.saturating_sub(2);
    if left.width() > room {
        left = status_left(dir, t, room);
    }
    left.draw(buf, 0, y, room.max(1));
}

/// The status bar's left side: the prompt, a message, or the selection's
/// path and state, cut to `room` columns.
fn status_left<'a>(dir: &'a DirView, t: &Theme, room: u16) -> Spans<'a> {
    let bg = Style::default().bg(t.panel_bg);
    let mut left = Spans::default();
    if let Some(p) = &dir.prompt {
        left.push(" /", key_style(t).bg(t.panel_bg))
            .push(p.as_str(), bg.fg(t.bright))
            .push("█", bg.fg(t.amber));
    } else if let Some(m) = &dir.message {
        left.push(format!(" {m}"), bg.fg(t.bright));
    } else if let Some(s) = &dir.search {
        let pos = s.current.map_or("no matches".to_string(), |c| {
            format!("match {} of {}", c + 1, s.matches.len())
        });
        left.push(format!(" {pos}"), bg.fg(t.bright));
    } else if let Some(row) = dir.current_row() {
        let (path, word) = match &dir.rows[row].node {
            Node::File(e) => {
                let entry = &dir.entries[*e];
                let word = if dir.unsaved.contains(e) {
                    "unsaved changes".to_string()
                } else {
                    status_word(&entry.status)
                };
                (entry.rel.display().to_string(), word)
            }
            Node::Dir { rel, changed, .. } => (
                format!("{}/", rel.display()),
                format!("{} changed", thousands(*changed)),
            ),
        };
        let (dirs, name) = split_name(&path);
        let word = format!("   {word}");
        let fixed = 1 + name.width() + word.width();
        let dirs = middle_cut(dirs, (room as usize).saturating_sub(fixed));
        left.push(" ", bg)
            .push(dirs, bg.fg(t.muted))
            .push(name.to_string(), bg.fg(t.bright))
            .push(word, bg.fg(t.dim));
    }
    left
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
                ("]f [f", "next, previous change"),
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
    let mut y = BODY_Y;
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
    fn chips_that_do_not_fit_are_left_out() {
        assert_eq!(chip_spans(200).len(), 4);
        let narrow = chip_spans(40);
        assert!(!narrow.is_empty() && narrow.len() < 4);
        assert!(narrow.iter().all(|&(_, x, w)| x + w < 40));
        assert!(chip_spans(10).is_empty());
    }
}
