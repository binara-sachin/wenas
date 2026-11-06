//! Drawing (spec §6). Only visible rows are touched.

use super::app::{App, View};
use super::chrome::{
    BACK, BODY_Y, CHROME_ROWS, RULER_W, Roots, Spans, TITLE_Y, TOOLBAR_Y, Tier, back_style, fill,
    hint_spans, middle_cut, split_name, tier, title_style,
};
use crate::align::{Row, RowKind};
use crate::inline::Inline;
use crate::ops::{Dir, Side};
use crate::theme::Theme;
use ratatui::Frame;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use std::collections::HashMap;
use std::ops::Range;
use std::rc::Rc;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

/// One screen cell's worth of a line.
struct Cell {
    ch: char,
    w: usize,
    /// Byte offset in the line this cell came from (tabs and escapes share one).
    byte: usize,
}

/// Expand tabs, escape invalid UTF-8 as `\xNN`, replace control characters.
/// Zero-width characters are dropped. Lazy, so drawing a 1 MB line only pays
/// for the part that reaches the screen.
struct Cells<'a> {
    chunks: std::str::Utf8Chunks<'a>,
    valid: std::str::CharIndices<'a>,
    invalid: std::slice::Iter<'a, u8>,
    /// Byte offset where the current chunk starts.
    chunk_start: usize,
    valid_len: usize,
    invalid_len: usize,
    invalid_seen: usize,
    col: usize,
    tab: usize,
    pending: std::collections::VecDeque<Cell>,
}

impl<'a> Cells<'a> {
    fn new(line: &'a [u8], tab: usize) -> Self {
        Cells {
            chunks: line.utf8_chunks(),
            valid: "".char_indices(),
            invalid: [].iter(),
            chunk_start: 0,
            valid_len: 0,
            invalid_len: 0,
            invalid_seen: 0,
            col: 0,
            tab: tab.max(1),
            pending: Default::default(),
        }
    }

    fn push(&mut self, ch: char, w: usize, byte: usize) {
        self.pending.push_back(Cell { ch, w, byte });
        self.col += w;
    }
}

impl Iterator for Cells<'_> {
    type Item = Cell;

    fn next(&mut self) -> Option<Cell> {
        loop {
            if let Some(c) = self.pending.pop_front() {
                return Some(c);
            }
            if let Some((i, ch)) = self.valid.next() {
                let byte = self.chunk_start + i;
                if ch == '\t' {
                    for _ in 0..(self.tab - self.col % self.tab) {
                        self.push(' ', 1, byte);
                    }
                } else if ch.is_control() {
                    self.push('·', 1, byte);
                } else {
                    let w = ch.width().unwrap_or(0);
                    if w > 0 {
                        self.push(ch, w, byte);
                    }
                }
                continue;
            }
            if let Some(&b) = self.invalid.next() {
                let byte = self.chunk_start + self.valid_len + self.invalid_seen;
                self.invalid_seen += 1;
                for ch in format!("\\x{b:02X}").chars() {
                    self.push(ch, 1, byte);
                }
                continue;
            }
            self.chunk_start += self.valid_len + self.invalid_len;
            let chunk = self.chunks.next()?;
            self.valid_len = chunk.valid().len();
            self.invalid_len = chunk.invalid().len();
            self.invalid_seen = 0;
            self.valid = chunk.valid().char_indices();
            self.invalid = chunk.invalid().iter();
        }
    }
}

fn build_cells(line: &[u8], tab: usize) -> Vec<Cell> {
    Cells::new(line, tab).collect()
}

pub fn display_width(line: &[u8], tab: usize) -> usize {
    build_cells(line, tab).iter().map(|c| c.w).sum()
}

fn digits(n: usize) -> usize {
    n.max(1).to_string().len()
}

fn number_width(app: &App) -> usize {
    digits(app.session.left.len().max(app.session.right.len())).max(3)
}

fn pane_widths(width: u16) -> (usize, usize) {
    let left = (width as usize).saturating_sub(3) / 2;
    (left, (width as usize).saturating_sub(3 + left))
}

/// Columns the content may use: the overview ruler keeps the right edge.
fn body_width(app: &App) -> u16 {
    app.width.saturating_sub(RULER_W)
}

/// The screen row of the first content row. Side by side, the pane headers
/// take the row above it.
pub fn content_y(app: &App) -> u16 {
    match app.view {
        View::SideBySide => BODY_Y + 1,
        View::Unified => BODY_Y,
    }
}

/// Below this the screens show "terminal too small" and nothing can be clicked.
pub(crate) fn too_small(width: u16, height: u16) -> bool {
    width < 30 || height < CHROME_ROWS + 2
}

/// The column where the 3-column gutter starts.
fn gutter_x(app: &App) -> u16 {
    match app.view {
        View::SideBySide => pane_widths(body_width(app)).0 as u16,
        View::Unified => {
            let n = number_width(app) as u16;
            1 + n + 1 + n + 1
        }
    }
}

/// What is under a screen position in the file view.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Hit {
    /// A content row, and which pane when that's side by side.
    Row { row: usize, side: Option<Side> },
    /// A copy arrow in the current hunk's gutter (`gutter_cells`).
    Arrow(Dir),
    /// The title bar's back control.
    Back,
    /// A toolbar control, by its key.
    Tool(char),
}

/// Map a screen position to what `draw` put there.
pub fn hit_test(app: &App, column: u16, y: u16) -> Option<Hit> {
    if too_small(app.width, app.height) {
        return None;
    }
    if y == TITLE_Y {
        let back = BACK.width() as u16;
        return (app.in_directory && column < back).then_some(Hit::Back);
    }
    if y == TOOLBAR_Y {
        return toolbar_items(app)
            .into_iter()
            .find(|i| (i.x..i.x + i.spans.width()).contains(&column))
            .map(|i| Hit::Tool(i.key));
    }
    let top = content_y(app);
    if y < top || column >= body_width(app) {
        return None;
    }
    let offset = (y - top) as usize;
    if offset >= app.content_height() {
        return None;
    }
    let row = app.top + offset;
    let shown = app.rows.get(row)?;
    let gx = gutter_x(app);
    if shown.hunk.is_some() && shown.hunk == app.current_hunk() {
        if column == gx && !app.session.left.is_readonly() {
            return Some(Hit::Arrow(Dir::RightToLeft));
        }
        if column == gx + 2 && !app.session.right.is_readonly() {
            return Some(Hit::Arrow(Dir::LeftToRight));
        }
    }
    let side = match app.view {
        View::SideBySide if column < gx => Some(Side::Left),
        View::SideBySide if column >= gx + 3 => Some(Side::Right),
        _ => None,
    };
    Some(Hit::Row { row, side })
}

/// Columns available for text in one pane (side by side) or the whole line (unified).
pub fn text_width(app: &App) -> usize {
    let numw = number_width(app);
    let w = body_width(app);
    match app.view {
        View::SideBySide => pane_widths(w).0.saturating_sub(1 + numw + 2),
        View::Unified => (w as usize).saturating_sub(1 + numw + 1 + numw + 1 + 3 + 1),
    }
}

pub(crate) fn put(buf: &mut Buffer, x: &mut u16, y: u16, max_x: u16, s: &str, style: Style) {
    if *x >= max_x {
        return;
    }
    let (nx, _) = buf.set_stringn(*x, y, s, (max_x - *x) as usize, style);
    *x = nx;
}

pub(crate) fn bold(style: Style) -> Style {
    style.add_modifier(Modifier::BOLD)
}

pub(crate) fn key_style(t: &Theme) -> Style {
    bold(Style::default().fg(t.amber))
}

fn match_style(t: &Theme, current: bool) -> Style {
    if t.no_color {
        if current {
            Style::default().add_modifier(Modifier::REVERSED | Modifier::BOLD)
        } else {
            Style::default().add_modifier(Modifier::UNDERLINED | Modifier::BOLD)
        }
    } else if current {
        bold(Style::default().fg(t.on_amber).bg(t.amber))
    } else {
        Style::default().fg(t.match_fg).bg(t.match_bg)
    }
}

pub(crate) fn badge_style(t: &Theme) -> Style {
    if t.no_color {
        Style::default().add_modifier(Modifier::REVERSED | Modifier::BOLD)
    } else {
        bold(Style::default().fg(t.on_amber).bg(t.amber))
    }
}

/// Changes narrower than this get bold text and a stronger tint, so a
/// one-character change can't be missed.
const SHORT_CHANGE: usize = 3;

/// Draw one line of text with row tint, word emphasis and search highlights.
#[allow(clippy::too_many_arguments)]
fn draw_text(
    buf: &mut Buffer,
    x: u16,
    y: u16,
    width: usize,
    line: &[u8],
    app: &App,
    base: Style,
    word: (Style, Style),
    ranges: &[Range<usize>],
    matches: &[(Range<usize>, bool)],
    no_eol: bool,
) {
    if width == 0 {
        return;
    }
    let t = &app.theme;
    let hscroll = app.hscroll;
    let mut col = 0usize;
    let mut more = false;
    let mut tmp = [0u8; 4];
    for c in Cells::new(line, app.cfg.tab_width) {
        let start = col;
        col += c.w;
        if col <= hscroll {
            continue;
        }
        let (sx, ch) = if start < hscroll {
            (0, ' ')
        } else {
            (start - hscroll, c.ch)
        };
        if sx >= width {
            more = true;
            break;
        }
        if sx + c.w > width && start >= hscroll {
            buf.set_stringn(x + sx as u16, y, " ", 1, base);
            more = true;
            break;
        }
        let mut style = base;
        if let Some(r) = ranges.iter().find(|r| r.contains(&c.byte)) {
            style = style.patch(if r.len() < SHORT_CHANGE {
                word.1
            } else {
                word.0
            });
        }
        if let Some((_, cur)) = matches.iter().find(|(r, _)| r.contains(&c.byte)) {
            style = style.patch(match_style(t, *cur));
        }
        buf.set_stringn(
            x + sx as u16,
            y,
            ch.encode_utf8(&mut tmp),
            c.w.max(1),
            style,
        );
    }
    // `⏎` marks a last line with no newline (spec §10).
    if no_eol && !more && col >= hscroll && col - hscroll < width {
        let style = if t.no_color { base } else { base.fg(t.dim) };
        buf.set_stringn(x + (col - hscroll) as u16, y, "⏎", 1, style);
    }
    if more {
        let style = base.fg(t.amber);
        buf.set_stringn(
            x + width as u16 - 1,
            y,
            "›",
            1,
            if t.no_color { base } else { style },
        );
    }
}

/// A row's tint, and the tints for its changed words: (normal, short).
fn row_style(t: &Theme, kind: RowKind, cursor: bool) -> (Style, (Style, Style)) {
    let pair = |bg, word, short| {
        (
            t.bg(bg),
            (t.emphasis(t.bg(word)), bold(t.emphasis(t.bg(short)))),
        )
    };
    match kind {
        RowKind::Changed => {
            let (b, (w, s)) = pair(t.changed_bg, t.changed_word_bg, t.changed_short_bg);
            let s = if t.no_color { s } else { s.fg(t.bright) };
            (b, (w, s))
        }
        RowKind::Added => pair(t.added_bg, t.added_word_bg, t.added_word_bg),
        RowKind::Removed => pair(t.removed_bg, t.removed_word_bg, t.removed_word_bg),
        RowKind::Same if cursor => (t.bg(t.cursor_bg), Default::default()),
        RowKind::Same => (Style::default(), Default::default()),
    }
}

fn gutter_cells(app: &App, kind: RowKind, current: bool) -> (String, Style) {
    let t = &app.theme;
    let mark = match kind {
        RowKind::Changed => '~',
        RowKind::Added => '+',
        RowKind::Removed => '-',
        RowKind::Same => return ("   ".into(), Style::default()),
    };
    if !current {
        let fg = match kind {
            RowKind::Changed => t.changed_fg,
            RowKind::Added => t.added_fg,
            _ => t.removed_fg,
        };
        return (format!(" {mark} "), Style::default().fg(fg));
    }
    let l = if app.session.left.is_readonly() {
        ' '
    } else {
        '←'
    };
    let r = if app.session.right.is_readonly() {
        ' '
    } else {
        '→'
    };
    (format!("{l}{mark}{r}"), badge_style(t))
}

struct Frame1<'a> {
    app: &'a App,
    inline: &'a HashMap<usize, Rc<Inline>>,
    numw: usize,
}

impl Frame1<'_> {
    fn ranges(&self, row: &Row, side: Side) -> &[Range<usize>] {
        let (Some(h), Some(inl)) = (row.hunk, row.hunk.and_then(|h| self.inline.get(&h))) else {
            return &[];
        };
        let hunk = &self.app.session.hunks[h];
        let (list, idx) = match side {
            Side::Left => (&inl.left, row.left.map(|l| l.wrapping_sub(hunk.left.start))),
            Side::Right => (
                &inl.right,
                row.right.map(|r| r.wrapping_sub(hunk.right.start)),
            ),
        };
        idx.and_then(|i| list.get(i)).map_or(&[], Vec::as_slice)
    }

    fn matches(&self, ri: usize, side: Side) -> Vec<(Range<usize>, bool)> {
        let Some(s) = &self.app.search else {
            return vec![];
        };
        let from = s.matches.partition_point(|m| m.row < ri);
        s.matches[from..]
            .iter()
            .take_while(|m| m.row == ri)
            .enumerate()
            .filter(|(_, m)| m.side == side || self.app.view == View::Unified)
            .map(|(k, m)| (m.range.clone(), s.current == Some(from + k)))
            .collect()
    }

    /// Only the cursor's line number stands out, and side by side only in the
    /// focused pane.
    fn number_style(&self, cursor: bool) -> Style {
        let t = &self.app.theme;
        if cursor {
            bold(Style::default().fg(t.amber))
        } else {
            Style::default().fg(t.dim)
        }
    }
}

pub fn draw(f: &mut Frame, app: &mut App) {
    let area = f.area();
    app.sync_size(area.width, area.height);
    // Compute inline highlights for visible hunks before borrowing immutably.
    let mut inline = HashMap::new();
    if app.binary.is_none() && !app.help && !app.session.is_busy() {
        let end = (app.top + app.content_height()).min(app.rows.len());
        let hunks: Vec<usize> = app.rows[app.top.min(end)..end]
            .iter()
            .filter_map(|r| r.hunk)
            .collect();
        for h in hunks {
            inline.entry(h).or_insert_with(|| app.inline_for(h));
        }
    }
    let app: &App = app;
    let buf = f.buffer_mut();
    if too_small(area.width, area.height) {
        buf.set_string(0, 0, "terminal too small", Style::default());
        return;
    }
    if app.binary.is_some() {
        draw_binary(buf, app, area);
        return;
    }
    if app.session.is_busy() {
        draw_comparing(buf, app, area);
        return;
    }
    draw_title(buf, app, area);
    draw_toolbar(buf, app, area);
    if app.help {
        draw_help(buf, app, area);
    } else {
        if app.view == View::SideBySide {
            draw_pane_headers(buf, app);
        }
        let fr = Frame1 {
            app,
            inline: &inline,
            numw: number_width(app),
        };
        let top = content_y(app);
        for y in 0..app.content_height() {
            let ri = app.top + y;
            let Some(row) = app.rows.get(ri) else { break };
            let y = y as u16 + top;
            if app.is_fold(ri) {
                draw_fold_row(buf, app, area, y, ri);
                continue;
            }
            match app.view {
                View::SideBySide => draw_side_row(buf, &fr, area, y, ri, row),
                View::Unified => draw_unified_row(buf, &fr, area, y, ri, row),
            }
        }
        if !app.fold {
            draw_ruler(buf, app, area);
        }
    }
    if app.modal.is_some() {
        draw_modal(buf, app, area);
    } else {
        draw_status(buf, app, area);
    }
}

pub fn side_name(app: &App, side: Side) -> String {
    let d = app.session.doc(side);
    d.label.clone().unwrap_or_else(|| match &d.path {
        Some(p) if p.as_os_str() == "-" => "<stdin>".into(),
        Some(p) => p.display().to_string(),
        None => String::new(),
    })
}

/// What tells the two sides apart, and the file's path under them.
fn sides(app: &App) -> (Roots, String) {
    match &app.place {
        Some(p) => (p.roots.clone(), p.rel.clone()),
        None => {
            let r = Roots::new(&side_name(app, Side::Left), &side_name(app, Side::Right));
            let path = r.tail.trim_start_matches('/').to_string();
            (r, path)
        }
    }
}

/// The title bar: the back control, the two sides with only their difference
/// in colour, the path cut from the middle so the file name stays, and the
/// position in the list on the right.
fn draw_title(buf: &mut Buffer, app: &App, area: Rect) {
    let t = &app.theme;
    let bg = title_style(t);
    let w = area.width;
    fill(buf, TITLE_Y, w, bg);
    let tier = tier(w);
    let mut right = Spans::default();
    if let Some(p) = &app.place {
        let pos = format!("{} of {}", p.index + 1, p.total);
        if tier == Tier::Narrow {
            right.push(pos, bg.fg(t.muted));
        } else {
            let key = key_style(t).patch(bg_only(bg));
            right
                .push("[f", key)
                .push(format!("  {pos}  "), bg.fg(t.muted))
                .push("]f", key);
        }
    }
    let end = right.draw_right(buf, TITLE_Y, 0, w).unwrap_or(w);
    let (roots, path) = sides(app);
    let (dirs, name) = split_name(&path);
    let path_name = Spans(vec![(name.into(), bold(bg.fg(t.bright)))]);
    // Under 100 columns the sides give way, unless they're all there is.
    let named = !(roots.left.is_empty() && roots.right.is_empty());
    let mut with_roots = named && (tier != Tier::Narrow || path.is_empty());
    loop {
        let mut left = Spans::default();
        if app.in_directory {
            left.push(BACK, back_style(t));
            if with_roots {
                left.push(" ⌫ ", bg.fg(t.dim));
            }
        }
        if with_roots {
            if !left.0.is_empty() {
                left.push("  ", bg);
            } else {
                left.push(" ", bg);
            }
            left.0.extend(roots.spans(t, bg, true).0);
        }
        if !path.is_empty() {
            let gap = match (with_roots, left.0.is_empty()) {
                (true, _) => "   › ",
                (false, false) => "  ",
                (false, true) => " ",
            };
            left.push(gap, bg.fg(t.dim));
        }
        let room = (end.saturating_sub(1 + left.width() + path_name.width() + 2)) as usize;
        let fits = dirs.width() <= room || room >= 12;
        if with_roots && !fits && !path.is_empty() {
            with_roots = false;
            continue;
        }
        left.push(middle_cut(dirs, room), bg.fg(t.dim));
        left.0.extend(path_name.0);
        left.draw(buf, 0, TITLE_Y, end);
        break;
    }
}

fn bg_only(s: Style) -> Style {
    match s.bg {
        Some(c) => Style::default().bg(c),
        None => Style::default(),
    }
}

/// The surface of the toolbar and the status bar.
fn panel(t: &Theme) -> Style {
    Style::default().bg(t.panel_bg)
}

/// A toolbar control: its key, where it sits, and what it shows.
pub struct ToolItem {
    pub key: char,
    pub x: u16,
    pub spans: Spans<'static>,
}

/// The toolbar's controls, placed for this width. Shared by drawing and
/// clicking, so a click always hits what is drawn.
pub fn toolbar_items(app: &App) -> Vec<ToolItem> {
    let t = &app.theme;
    let bg = panel(t);
    let tier = tier(app.width);
    let key = key_style(t).patch(bg);
    let (label, value) = (bg.fg(t.dim), bg.fg(t.bright));
    let item = |k: char, l: &str, v: String| {
        let mut s = Spans::default();
        s.push(k.to_string(), key);
        if !l.is_empty() {
            s.push(format!(" {l}"), label);
        }
        s.push(format!(" {v}"), value);
        s
    };
    let view = match app.view {
        View::SideBySide => "split",
        View::Unified => "unified",
    };
    let ws = app.session.ws.label();
    let (vl, wl, wv, fl, fv) = match tier {
        Tier::Full => (
            "view",
            "whitespace",
            ws.to_string(),
            "unchanged lines",
            if app.fold { "folded" } else { "shown" },
        ),
        Tier::Short => (
            "view",
            "ws",
            ws.to_string(),
            "unchanged",
            if app.fold { "folded" } else { "shown" },
        ),
        Tier::Narrow => (
            "",
            "",
            format!("ws {ws}"),
            "",
            if app.fold { "folded" } else { "all lines" },
        ),
    };
    let mut items = vec![
        ('v', item('v', vl, view.to_string())),
        ('W', item('W', wl, wv)),
        ('z', item('z', fl, fv.to_string())),
    ];
    if tier != Tier::Narrow {
        let mut s = Spans::default();
        s.push("/", key).push(" find", bg.fg(t.muted));
        if let Some(q) = &app.search {
            s.push(format!(" {}", q.query), value);
        }
        items.push(('/', s));
    }
    let gap = if tier == Tier::Full { 5 } else { 3 };
    let mut x = 1;
    let mut out = Vec::new();
    for (k, spans) in items {
        let w = spans.width();
        if x + w + 1 > app.width {
            break;
        }
        out.push(ToolItem { key: k, x, spans });
        x += w + gap;
    }
    out
}

/// The toolbar: how the content is shown (view, whitespace, folding), and on
/// the right a count of what's shown.
fn draw_toolbar(buf: &mut Buffer, app: &App, area: Rect) {
    let t = &app.theme;
    let bg = panel(t);
    fill(buf, TOOLBAR_Y, area.width, bg);
    if app.help {
        let mut s = Spans::default();
        s.push(" Keys", bold(bg.fg(t.bright)));
        s.draw(buf, 0, TOOLBAR_Y, area.width);
        return;
    }
    let items = toolbar_items(app);
    let mut x = 1;
    for i in &items {
        x = i.spans.draw(buf, i.x, TOOLBAR_Y, area.width);
    }
    let mut right = Spans::default();
    if app.eols.0 != app.eols.1 && app.view == View::Unified {
        let amber = bg.fg(t.amber);
        right.push(
            format!("{} ↔ {}   ", app.eols.0.label(), app.eols.1.label()),
            amber,
        );
    }
    if tier(area.width) == Tier::Narrow {
        let (roots, _) = sides(app);
        right.push(format!("{} ↔ {}", roots.left, roots.right), bg.fg(t.dim));
    } else {
        let hunks = app.session.hunks.len();
        let lines: usize = app
            .session
            .hunks
            .iter()
            .map(|h| h.left.len().max(h.right.len()))
            .sum();
        let plural = |n: usize, one: &str| {
            if n == 1 {
                format!("{n} {one}")
            } else {
                format!("{} {one}s", thousands(n))
            }
        };
        right.push(
            format!(
                "{} · {}",
                plural(hunks, "hunk"),
                plural(lines, "changed line")
            ),
            bg.fg(t.muted),
        );
    }
    right.draw_right(buf, TOOLBAR_Y, x + 2, area.width);
}

/// Pane headers describe the content, so they sit on it and stay dim; the
/// focused pane's is lit.
fn draw_pane_headers(buf: &mut Buffer, app: &App) {
    let t = &app.theme;
    let (lw, rw) = pane_widths(body_width(app));
    let (roots, _) = sides(app);
    let y = BODY_Y;
    let eol_differs = app.eols.0 != app.eols.1;
    for (side, x0, w) in [(Side::Left, 0u16, lw), (Side::Right, lw as u16 + 3, rw)] {
        let d = app.session.doc(side);
        let focused = app.focus == side;
        let base = if focused && !t.no_color {
            Style::default().bg(t.cursor_bg)
        } else {
            Style::default()
        };
        if focused {
            buf.set_style(Rect::new(x0, y, w as u16, 1), base);
        }
        let name = match (&app.place, side) {
            (Some(_), Side::Left) => roots.left_name(),
            (Some(_), Side::Right) => roots.right_name(),
            (None, _) => side_name(app, side),
        };
        let fg = if focused { t.bright } else { t.dim };
        let name_style = if focused && t.no_color {
            bold(base)
        } else {
            base.fg(fg)
        };
        let eol = if side == Side::Left {
            app.eols.0
        } else {
            app.eols.1
        };
        let mut s = Spans::default();
        s.push(format!(" {name}"), name_style);
        if d.is_readonly() {
            s.push("  read-only", base.fg(t.dim));
        }
        s.push(
            format!("  {}", eol.label()),
            if eol_differs {
                base.fg(t.amber)
            } else {
                base.fg(fg)
            },
        );
        if d.is_dirty() {
            s.push("  [+]", bold(base.fg(t.amber)));
        }
        s.draw(buf, x0, y, x0 + w as u16);
    }
}

fn draw_fold_row(buf: &mut Buffer, app: &App, area: Rect, y: u16, ri: usize) {
    let t = &app.theme;
    let Some(f) = app.folds.get(&ri) else { return };
    let w = body_width(app).min(area.width);
    let cursor = ri == app.cursor;
    let base = if t.no_color {
        Style::default()
    } else if cursor {
        Style::default().bg(t.cursor_bg).fg(t.muted)
    } else {
        Style::default().bg(t.fold_bg).fg(t.dim)
    };
    buf.set_style(Rect::new(0, y, w, 1), base);
    let range = match app.view {
        View::SideBySide if app.focus == Side::Left => &f.left,
        _ => &f.right,
    };
    let len = match app.view {
        View::SideBySide if app.focus == Side::Left => app.session.left.len(),
        _ => app.session.right.len(),
    };
    let what = if range.end >= len {
        format!("line {} to end of file unchanged", range.start + 1)
    } else {
        format!("lines {} to {} unchanged", range.start + 1, range.end)
    };
    let mut s = Spans::default();
    s.push(format!("   ┄┄┄┄┄  {what}  ┄┄┄┄┄"), base);
    let x = s.draw(buf, 0, y, w);
    let mut r = Spans::default();
    r.push("z to unfold  ", base);
    r.draw_right(buf, y, x + 2, w);
}

/// A one-column overview at the right edge: where the hunks are, and which
/// part of the file is on screen.
fn draw_ruler(buf: &mut Buffer, app: &App, area: Rect) {
    let t = &app.theme;
    let (h, n) = (app.content_height(), app.rows.len());
    // A file that fits on screen needs no overview.
    if n <= h || area.width < RULER_W {
        return;
    }
    let x = area.width - 1;
    let top = content_y(app);
    // Each cell covers a slice of the rows; a short file gets a cell per row.
    let cells = h.min(n);
    let span = |i: usize| (i * n / cells)..((i + 1) * n / cells).max(i * n / cells + 1);
    let mut kinds: Vec<Option<RowKind>> = vec![None; cells];
    for (hi, &first) in app.first_rows.iter().enumerate() {
        let hunk = &app.session.hunks[hi];
        let len = match app.view {
            View::SideBySide => hunk.left.len().max(hunk.right.len()),
            View::Unified => hunk.left.len() + hunk.right.len(),
        }
        .max(1);
        let kind = app.rows.get(first).map_or(RowKind::Changed, |r| r.kind);
        let (a, b) = (
            first * cells / n,
            ((first + len - 1) * cells / n).min(cells - 1),
        );
        for k in &mut kinds[a..=b] {
            k.get_or_insert(kind);
        }
    }
    let view = app.top..app.top + h;
    for (i, kind) in kinds.iter().enumerate() {
        let r = span(i);
        let (sym, fg) = match kind {
            Some(RowKind::Added) => ("▬", t.added_fg),
            Some(RowKind::Removed) => ("▬", t.removed_fg),
            Some(_) => ("▬", t.changed_fg),
            None if r.start < view.end && view.start < r.end => ("┃", t.dim),
            None => ("│", t.filler),
        };
        let style = if t.no_color {
            Style::default()
        } else {
            Style::default().fg(fg)
        };
        buf.set_stringn(x, top + i as u16, sym, 1, style);
    }
}

fn draw_side_row(buf: &mut Buffer, fr: &Frame1, area: Rect, y: u16, ri: usize, row: &Row) {
    let (app, t) = (fr.app, &fr.app.theme);
    let is_cursor = ri == app.cursor;
    let in_current = row.hunk.is_some() && row.hunk == app.current_hunk();
    let (base, word) = row_style(t, row.kind, false);
    let (lw, rw) = pane_widths(body_width(app));
    let _ = area;
    let panes = [
        (Side::Left, 0u16, lw, row.left),
        (Side::Right, (lw + 3) as u16, rw, row.right),
    ];
    for (side, x, w, line) in panes {
        let w = w as u16;
        // Only the focused pane shows the cursor.
        let cursor = is_cursor && side == app.focus;
        let base = if cursor && row.kind == RowKind::Same {
            row_style(t, row.kind, true).0
        } else {
            base
        };
        match line {
            None => {
                let filler = "░".repeat(w as usize);
                buf.set_stringn(x, y, &filler, w as usize, Style::default().fg(t.filler));
            }
            Some(li) => {
                if row.kind != RowKind::Same || cursor {
                    // Tint only panes that have a line; the partner shows filler.
                    buf.set_style(Rect::new(x, y, w, 1), base);
                }
                if cursor && t.no_color {
                    buf.set_stringn(x, y, "▎", 1, base);
                }
                let num = format!("{:>w$}", li + 1, w = fr.numw);
                let style = fr.number_style(cursor).patch(base);
                buf.set_stringn(x + 1, y, &num, fr.numw, style);
                let tx = x + 1 + fr.numw as u16 + 2;
                let tw = (w as usize).saturating_sub(1 + fr.numw + 2);
                let text = app.session.doc(side).line(li);
                let (rs, ms) = (fr.ranges(row, side), fr.matches(ri, side));
                let rs = if row.kind == RowKind::Changed {
                    rs
                } else {
                    &[]
                };
                let no_eol = app.session.doc(side).eol_of(li).is_none();
                draw_text(buf, tx, y, tw, text, app, base, word, rs, &ms, no_eol);
            }
        }
    }
    let (g, gs) = gutter_cells(app, row.kind, in_current);
    buf.set_stringn(gutter_x(app), y, &g, 3, gs);
}

fn draw_unified_row(buf: &mut Buffer, fr: &Frame1, area: Rect, y: u16, ri: usize, row: &Row) {
    let (app, t) = (fr.app, &fr.app.theme);
    let is_cursor = ri == app.cursor;
    let in_current = row.hunk.is_some() && row.hunk == app.current_hunk();
    let (base, word) = row_style(t, row.kind, is_cursor);
    buf.set_style(Rect::new(0, y, body_width(app).min(area.width), 1), base);
    if is_cursor && t.no_color {
        buf.set_stringn(0, y, "▎", 1, base);
    }
    let nums = fr.number_style(is_cursor).patch(base);
    let n = fr.numw;
    let l = row.left.map_or(" ".repeat(n), |l| format!("{:>n$}", l + 1));
    let r = row
        .right
        .map_or(" ".repeat(n), |r| format!("{:>n$}", r + 1));
    buf.set_stringn(1, y, &l, n, nums);
    let rx = 1 + n as u16 + 1;
    buf.set_stringn(rx, y, &r, n, nums);
    let gx = gutter_x(app);
    let (g, gs) = gutter_cells(app, row.kind, in_current);
    buf.set_stringn(gx, y, &g, 3, gs);
    let Some((side, li)) = App::unified_line(row) else {
        return;
    };
    let text = app.session.doc(side).line(li);
    let (rs, ms) = (fr.ranges(row, side), fr.matches(ri, side));
    draw_text(
        buf,
        gx + 4,
        y,
        text_width(app),
        text,
        app,
        base,
        word,
        rs,
        &ms,
        app.session.doc(side).eol_of(li).is_none(),
    );
}

/// Key hints in the old style, for modals: two-space gaps on the panel.
pub(crate) fn hints(
    buf: &mut Buffer,
    x: &mut u16,
    y: u16,
    max_x: u16,
    t: &Theme,
    items: &[(&str, &str)],
    sep: &str,
) {
    for (i, (k, d)) in items.iter().enumerate() {
        if i > 0 {
            put(buf, x, y, max_x, sep, Style::default().bg(t.panel_bg));
        }
        put(buf, x, y, max_x, k, key_style(t).bg(t.panel_bg));
        put(
            buf,
            x,
            y,
            max_x,
            &format!(" {d}"),
            Style::default().fg(t.muted).bg(t.panel_bg),
        );
    }
}

/// Status bars narrower than this keep only the help key.
pub(crate) const HINTS_MIN_WIDTH: u16 = 60;

/// Fit key hints into the room right of `min_x`. The last `keep` hints
/// always stay (`F1 help` among them); the ones before give way from the end.
/// Under 60 columns only `F1 help` is left. `merge` names two neighbours that
/// can share one hint (`> <` copy) before anything is dropped.
pub(crate) fn fit_hints<'a>(
    t: &Theme,
    width: u16,
    min_x: u16,
    mut items: Vec<(&'a str, &'a str, bool)>,
    merge: Option<(usize, (&'a str, &'a str, bool))>,
    keep: usize,
) -> Spans<'a> {
    let bg = panel(t);
    if width < HINTS_MIN_WIDTH {
        match items.iter().position(|i| i.0 == "F1") {
            Some(p) => items = vec![items[p]],
            None => items.drain(..items.len().saturating_sub(1)).for_each(drop),
        }
    }
    let fits = |items: &[(&str, &str, bool)]| hint_spans(t, bg, items).width() + 2 + min_x <= width;
    if !fits(&items)
        && let Some((at, merged)) = merge
        && at + 1 < items.len()
    {
        items.splice(at..at + 2, [merged]);
    }
    while items.len() > keep && !fits(&items) {
        let n = items.len();
        items.remove(n - keep - 1);
    }
    if fits(&items) {
        hint_spans(t, bg, &items)
    } else {
        Spans::default()
    }
}

fn draw_status(buf: &mut Buffer, app: &App, area: Rect) {
    let t = &app.theme;
    let y = area.height - 1;
    let bg = panel(t);
    fill(buf, y, area.width, bg);
    let end = area.width;

    if app.message_is_error {
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
            &format!(" × {}", app.message.as_deref().unwrap_or("")),
            style,
        );
        return;
    }

    let mut left = Spans::default();
    let key = key_style(t).patch(bg);
    if let Some(p) = &app.prompt {
        left.push(if p.backward { " ?" } else { " /" }, key)
            .push(p.text.as_str(), bg.fg(t.bright))
            .push("█", bg.fg(t.amber));
    } else if let Some(m) = &app.message {
        left.push(format!(" {m}"), bg.fg(t.bright));
    } else if app.help {
    } else {
        let state = app.cursor_state();
        let after = app.current_hunk().is_none() && !app.session.hunks.is_empty();
        left.push(
            format!(" {state}"),
            if after {
                bg.fg(t.muted)
            } else {
                bg.fg(t.bright)
            },
        );
        if after {
            left.push("  ", bg)
                .push("K", key)
                .push(" back to it", bg.fg(t.muted));
        } else {
            left.push(format!("   line {}", app.cursor_line()), bg.fg(t.muted));
        }
        if let Some(s) = &app.search {
            let pos = match s.current {
                Some(c) => format!("   match {} of {}", c + 1, s.matches.len()),
                None => "   no matches".into(),
            };
            left.push(pos, bg.fg(t.bright));
        }
    }
    let x = left.draw(buf, 0, y, end);

    let no_hunk = app.current_hunk().is_none();
    let (lro, rro) = (
        app.session.left.is_readonly(),
        app.session.right.is_readonly(),
    );
    let (items, merge) = if app.help {
        (vec![("F1 Esc", "close", false)], None)
    } else if app.prompt.is_some() {
        (
            vec![("Enter", "search", false), ("Esc", "cancel", false)],
            None,
        )
    } else if app.search.is_some() {
        (
            vec![
                ("n", "next", false),
                ("N", "previous", false),
                ("Esc", "clear", false),
                ("F1", "help", false),
            ],
            None,
        )
    } else {
        let edit_off = app.session.doc(app.focus).is_readonly();
        (
            vec![
                (">", "copy right", no_hunk || rro),
                ("<", "copy left", no_hunk || lro),
                ("u", "undo", false),
                ("w", "save", false),
                ("e", "edit", edit_off),
                ("F1", "help", false),
            ],
            Some((0, ("> <", "copy", no_hunk || (lro && rro)))),
        )
    };
    let right = fit_hints(t, end, x, items, merge, 1);
    right.draw_right(buf, y, x + 2, end);
}

fn draw_modal(buf: &mut Buffer, app: &App, area: Rect) {
    if let Some(m) = app.modal_text() {
        draw_modal_panel(buf, &app.theme, area, &m);
    }
}

/// The bottom panel used by every modal question.
pub(crate) fn draw_modal_panel(buf: &mut Buffer, t: &Theme, area: Rect, m: &super::app::ModalText) {
    let height = (m.body.len() + 4).min(area.height as usize) as u16;
    let y0 = area.height - height;
    let panel = Style::default().bg(t.panel_bg);
    for y in y0..area.height {
        buf.set_stringn(
            0,
            y,
            " ".repeat(area.width as usize),
            area.width as usize,
            panel,
        );
    }
    let rule = if t.no_color {
        bold(Style::default())
    } else {
        Style::default().fg(t.amber).bg(t.panel_bg)
    };
    buf.set_stringn(
        0,
        y0,
        "─".repeat(area.width as usize),
        area.width as usize,
        rule,
    );
    let w = area.width as usize;
    buf.set_stringn(
        2,
        y0 + 1,
        &m.title,
        w.saturating_sub(3),
        bold(Style::default().fg(t.bright).bg(t.panel_bg)),
    );
    for (i, line) in m.body.iter().enumerate() {
        buf.set_stringn(
            2,
            y0 + 2 + i as u16,
            line,
            w.saturating_sub(3),
            Style::default().fg(t.muted).bg(t.panel_bg),
        );
    }
    let mut x = 2;
    hints(
        buf,
        &mut x,
        area.height - 1,
        area.width,
        t,
        &m.hints,
        "    ",
    );
}

const HELP: &[(&str, &[(&str, &str)])] = &[
    (
        "Move",
        &[
            ("j k", "line down, up"),
            ("Ctrl-d u", "half page"),
            ("gg G", "top, bottom"),
            ("← →", "scroll sideways"),
            ("Tab", "other pane"),
        ],
    ),
    (
        "Hunks",
        &[
            ("J K", "next, previous"),
            ("]c [c", "next, previous"),
            (">", "copy to right"),
            ("<", "copy to left"),
            ("u", "undo"),
            ("Ctrl-r", "redo"),
        ],
    ),
    (
        "Search",
        &[
            ("/ ?", "forward, back"),
            ("n N", "next, previous"),
            ("Esc", "clear matches"),
        ],
    ),
    (
        "View",
        &[
            ("v", "switch view"),
            ("W", "whitespace mode"),
            ("z", "fold unchanged lines"),
        ],
    ),
    (
        "Files",
        &[
            ("w", "save"),
            ("e", "open in $EDITOR"),
            ("]f [f", "next, previous file"),
            ("Backspace", "back to the list"),
            ("q", "quit"),
            ("Q", "quit, don't save"),
            ("Ctrl-c", "abort, exit 1"),
        ],
    ),
];

fn draw_help(buf: &mut Buffer, app: &App, area: Rect) {
    let t = &app.theme;
    // Sections: Move, Hunks, Search, View, Files. Three columns as in the mockup
    // keep everything, and the whitespace note, on a 24-row terminal.
    let (cols, layout): (usize, [usize; 5]) = match area.width {
        96.. => (3, [0, 1, 0, 1, 2]),
        64..=95 => (2, [0, 1, 0, 1, 1]),
        _ => (1, [0; 5]),
    };
    let col_w = (area.width as usize - 2) / cols;
    let mut ys = vec![BODY_Y; cols];
    for (i, (title, items)) in HELP.iter().enumerate() {
        let c = layout[i];
        let x0 = 2 + (c * col_w) as u16;
        let y = &mut ys[c];
        if *y + 1 >= area.height - 1 {
            continue;
        }
        buf.set_stringn(x0, *y, title, col_w, bold(Style::default().fg(t.bright)));
        *y += 1;
        for (k, d) in *items {
            if *y >= area.height - 1 {
                break;
            }
            buf.set_stringn(x0, *y, k, 10, key_style(t));
            buf.set_stringn(x0 + 11, *y, d, col_w.saturating_sub(12), Style::default());
            *y += 1;
        }
        *y += 1;
    }
    let y = area.height - 3;
    if y <= ys.iter().copied().max().unwrap_or(0) {
        return;
    }
    let note =
        "When whitespace is ignored, whitespace-only changes are hidden and can't be copied.";
    buf.set_stringn(
        2,
        y,
        note,
        area.width as usize - 3,
        Style::default().fg(t.dim),
    );
    let mouse = "Mouse: wheel scrolls, click moves, click ← → copies. Shift-drag selects text.";
    buf.set_stringn(
        2,
        y + 1,
        mouse,
        area.width as usize - 3,
        Style::default().fg(t.dim),
    );
}

const SPINNER: [&str; 8] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧"];

/// Progress is only shown once a diff has taken longer than this (spec §10).
const PROGRESS_AFTER: std::time::Duration = std::time::Duration::from_millis(300);

/// The "Comparing…" screen shown while a background diff runs. `Esc` cancels.
fn draw_comparing(buf: &mut Buffer, app: &App, area: Rect) {
    let t = &app.theme;
    let elapsed = app.session.busy_for().unwrap_or_default();
    if elapsed < PROGRESS_AFTER {
        return; // most diffs finish before this; avoid a flash of progress
    }
    draw_title(buf, app, area);
    fill(buf, TOOLBAR_Y, area.width, panel(t));
    let frame = SPINNER[(elapsed.as_millis() / 100) as usize % SPINNER.len()];
    let mut x = 2;
    put(
        buf,
        &mut x,
        BODY_Y,
        area.width,
        frame,
        Style::default().fg(t.amber),
    );
    put(
        buf,
        &mut x,
        BODY_Y,
        area.width,
        " Comparing",
        bold(Style::default().fg(t.bright)),
    );
    let lines = app.session.left.len() + app.session.right.len();
    let detail = format!("{} lines, {:.1}s", thousands(lines), elapsed.as_secs_f32());
    buf.set_stringn(
        2,
        BODY_Y + 1,
        detail,
        (area.width as usize).saturating_sub(3),
        Style::default().fg(t.muted),
    );
    let y = area.height - 1;
    fill(buf, y, area.width, panel(t));
    let right = fit_hints(t, area.width, 0, vec![("Esc", "cancel", false)], None, 1);
    right.draw_right(buf, y, 0, area.width);
}

pub fn thousands(n: usize) -> String {
    let s = n.to_string();
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

fn draw_binary(buf: &mut Buffer, app: &App, area: Rect) {
    let t = &app.theme;
    let info = app.binary.as_ref().unwrap();
    let bg = title_style(t);
    fill(buf, TITLE_Y, area.width, bg);
    let mut title = Spans::default();
    if app.in_directory {
        title.push(BACK, back_style(t)).push("  ", bg);
    } else {
        title.push(" ", bg);
    }
    let (dirs, name) = split_name(&info.name);
    title
        .push(dirs, bg.fg(t.dim))
        .push(name, bold(bg.fg(t.bright)))
        .push("  (binary)", bg.fg(t.dim));
    title.draw(buf, 0, TITLE_Y, area.width);
    fill(buf, TOOLBAR_Y, area.width, panel(t));
    let label = Style::default().fg(t.dim);
    let mut lines = vec![
        ("left: ", format!("  {} bytes", thousands(info.left_size))),
        ("right:", format!("  {} bytes", thousands(info.right_size))),
    ];
    match info.first_difference {
        Some(b) => lines.push(("first difference at byte", format!(" {}", thousands(b)))),
        None => lines.push(("", "contents are identical".into())),
    }
    for (i, (k, v)) in lines.iter().enumerate() {
        let y = BODY_Y + i as u16;
        let mut x = 2;
        put(buf, &mut x, y, area.width, k, label);
        put(buf, &mut x, y, area.width, v, Style::default());
    }
    if info.utf16 {
        let msg = "UTF-16 text is treated as binary in this version.";
        buf.set_stringn(
            2,
            BODY_Y + 4,
            msg,
            area.width as usize - 3,
            Style::default().fg(t.muted),
        );
    }
    let y = area.height - 1;
    fill(buf, y, area.width, panel(t));
    let items = if app.in_directory {
        vec![("q", "back", false), ("Ctrl-c", "abort", false)]
    } else {
        vec![("q", "quit", false), ("Ctrl-c", "abort", false)]
    };
    let right = fit_hints(t, area.width, 0, items, None, 1);
    right.draw_right(buf, y, 0, area.width);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cells_expand_tabs_and_escape_bytes() {
        let c = build_cells(b"a\tb", 4);
        assert_eq!(c.iter().map(|c| c.ch).collect::<String>(), "a   b");
        let c = build_cells(b"x\xFFy", 4);
        assert_eq!(c.iter().map(|c| c.ch).collect::<String>(), "x\\xFFy");
        assert_eq!(c[1].byte, 1);
        assert_eq!(c[5].byte, 2);
        assert_eq!(display_width("日本".as_bytes(), 4), 4);
        let c = build_cells(b"\x1b[31m", 4);
        assert_eq!(c[0].ch, '·');
    }

    #[test]
    fn thousands_separators() {
        assert_eq!(thousands(48213), "48,213");
        assert_eq!(thousands(2048), "2,048");
        assert_eq!(thousands(12), "12");
        assert_eq!(thousands(1_000_000), "1,000,000");
    }
}
