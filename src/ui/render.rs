//! Drawing (spec §6). Only visible rows are touched.

use super::app::{App, View};
use crate::align::{Row, RowKind};
use crate::inline::Inline;
use crate::ops::Side;
use crate::theme::Theme;
use ratatui::Frame;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use std::collections::HashMap;
use std::ops::Range;
use std::rc::Rc;
use unicode_width::UnicodeWidthChar;

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
    digits(app.session.left.len().max(app.session.right.len())).max(2)
}

fn pane_widths(width: u16) -> (usize, usize) {
    let left = (width as usize).saturating_sub(3) / 2;
    (left, (width as usize).saturating_sub(3 + left))
}

/// Columns available for text in one pane (side by side) or the whole line (unified).
pub fn text_width(app: &App) -> usize {
    let numw = number_width(app);
    match app.view {
        View::SideBySide => pane_widths(app.width).0.saturating_sub(1 + numw + 2),
        View::Unified => (app.width as usize).saturating_sub(1 + numw + 1 + numw + 1 + 3 + 1),
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
    word: Style,
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
        if ranges.iter().any(|r| r.contains(&c.byte)) {
            style = style.patch(word);
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

fn row_style(t: &Theme, kind: RowKind, cursor: bool) -> (Style, Style) {
    let pair = |bg, word| (t.bg(bg), t.emphasis(t.bg(word)));
    match kind {
        RowKind::Changed => pair(t.changed_bg, t.changed_word_bg),
        RowKind::Added => pair(t.added_bg, t.added_word_bg),
        RowKind::Removed => pair(t.removed_bg, t.removed_word_bg),
        RowKind::Same if cursor => (t.bg(t.cursor_bg), Style::default()),
        RowKind::Same => (Style::default(), Style::default()),
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
    current: Option<usize>,
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

    fn number_style(&self, in_current: bool, is_cursor: bool) -> Style {
        let t = &self.app.theme;
        if in_current {
            bold(Style::default().fg(t.amber))
        } else if is_cursor {
            bold(Style::default().fg(t.bright))
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
    if app.binary.is_none() && !app.help {
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
    if area.width < 30 || area.height < 5 {
        buf.set_string(0, 0, "terminal too small", Style::default());
        return;
    }
    if app.binary.is_some() {
        draw_binary(buf, app, area);
        return;
    }
    draw_header(buf, app, area);
    if app.help {
        draw_help(buf, app, area);
    } else {
        let fr = Frame1 {
            app,
            inline: &inline,
            numw: number_width(app),
            current: app.current_hunk(),
        };
        for y in 0..app.content_height() {
            let ri = app.top + y;
            let Some(row) = app.rows.get(ri) else { break };
            let y = (y + 2) as u16;
            match app.view {
                View::SideBySide => draw_side_row(buf, &fr, area, y, ri, row),
                View::Unified => draw_unified_row(buf, &fr, area, y, ri, row),
            }
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

fn draw_header(buf: &mut Buffer, app: &App, area: Rect) {
    let t = &app.theme;
    let bar = Style::default().bg(t.panel_bg);
    buf.set_style(
        Rect::new(0, 0, area.width, 1),
        if t.no_color {
            bold(Style::default())
        } else {
            bar
        },
    );
    if app.help {
        let mut x = 1;
        put(
            buf,
            &mut x,
            0,
            area.width,
            "Keys",
            Style::default().fg(t.bright).bg(t.panel_bg),
        );
        let s = "F1 or Esc close";
        let sx = area.width.saturating_sub(s.len() as u16 + 1);
        let mut x = sx;
        put(
            buf,
            &mut x,
            0,
            area.width,
            "F1",
            key_style(t).bg(t.panel_bg),
        );
        put(
            buf,
            &mut x,
            0,
            area.width,
            " or ",
            Style::default().fg(t.dim).bg(t.panel_bg),
        );
        put(
            buf,
            &mut x,
            0,
            area.width,
            "Esc",
            key_style(t).bg(t.panel_bg),
        );
        put(
            buf,
            &mut x,
            0,
            area.width,
            " close",
            Style::default().fg(t.dim).bg(t.panel_bg),
        );
        return;
    }
    let part = |buf: &mut Buffer, x0: u16, max_x: u16, side: Side| {
        let d = app.session.doc(side);
        let focused = app.focus == side && app.view == View::SideBySide;
        let name_style = if focused {
            bold(Style::default().fg(t.bright))
        } else {
            Style::default().fg(t.muted)
        };
        let mut x = x0;
        put(
            buf,
            &mut x,
            0,
            max_x,
            &side_name(app, side),
            name_style.bg(t.panel_bg),
        );
        if d.is_readonly() {
            put(
                buf,
                &mut x,
                0,
                max_x,
                "  read-only",
                Style::default().fg(t.dim).bg(t.panel_bg),
            );
        }
        if d.is_dirty() {
            put(
                buf,
                &mut x,
                0,
                max_x,
                "  [+]",
                bold(Style::default().fg(t.amber).bg(t.panel_bg)),
            );
        }
    };
    match app.view {
        View::SideBySide => {
            let (lw, _) = pane_widths(app.width);
            part(buf, 1, lw as u16, Side::Left);
            part(buf, lw as u16 + 4, area.width, Side::Right);
        }
        View::Unified => {
            let mut x = 1;
            let dim = Style::default().fg(t.dim).bg(t.panel_bg);
            put(
                buf,
                &mut x,
                0,
                area.width,
                &side_name(app, Side::Left),
                Style::default().fg(t.muted).bg(t.panel_bg),
            );
            if app.session.left.is_readonly() {
                put(buf, &mut x, 0, area.width, " read-only", dim);
            }
            put(buf, &mut x, 0, area.width, "  ↔  ", dim);
            put(
                buf,
                &mut x,
                0,
                area.width,
                &side_name(app, Side::Right),
                bold(Style::default().fg(t.bright).bg(t.panel_bg)),
            );
            if app.session.right.is_readonly() {
                put(buf, &mut x, 0, area.width, " read-only", dim);
            }
            if app.session.is_dirty() {
                put(
                    buf,
                    &mut x,
                    0,
                    area.width,
                    "  [+]",
                    bold(Style::default().fg(t.amber).bg(t.panel_bg)),
                );
            }
        }
    }
}

fn draw_side_row(buf: &mut Buffer, fr: &Frame1, area: Rect, y: u16, ri: usize, row: &Row) {
    let (app, t) = (fr.app, &fr.app.theme);
    let is_cursor = ri == app.cursor;
    let in_current = row.hunk.is_some() && row.hunk == fr.current;
    let (base, word) = row_style(t, row.kind, is_cursor);
    let (lw, rw) = pane_widths(area.width);
    if row.kind == RowKind::Same && is_cursor {
        buf.set_style(Rect::new(0, y, area.width, 1), base);
    }
    let panes = [
        (Side::Left, 0u16, lw, row.left),
        (Side::Right, (lw + 3) as u16, rw, row.right),
    ];
    for (side, x, w, line) in panes {
        let w = w as u16;
        match line {
            None => {
                let filler = "░".repeat(w as usize);
                buf.set_stringn(x, y, &filler, w as usize, Style::default().fg(t.filler));
            }
            Some(li) => {
                if row.kind != RowKind::Same || is_cursor {
                    // Tint only panes that have a line; the partner shows filler.
                    buf.set_style(Rect::new(x, y, w, 1), base);
                }
                if is_cursor && !t.no_color {
                    buf.set_stringn(x, y, "▎", 1, base.fg(t.amber));
                } else if is_cursor {
                    buf.set_stringn(x, y, "▎", 1, base);
                }
                let num = format!("{:>w$}", li + 1, w = fr.numw);
                let style = fr.number_style(in_current, is_cursor).patch(base);
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
    buf.set_stringn(lw as u16, y, &g, 3, gs);
}

fn draw_unified_row(buf: &mut Buffer, fr: &Frame1, area: Rect, y: u16, ri: usize, row: &Row) {
    let (app, t) = (fr.app, &fr.app.theme);
    let is_cursor = ri == app.cursor;
    let in_current = row.hunk.is_some() && row.hunk == fr.current;
    let (base, word) = row_style(t, row.kind, is_cursor);
    buf.set_style(Rect::new(0, y, area.width, 1), base);
    if is_cursor {
        buf.set_stringn(
            0,
            y,
            "▎",
            1,
            if t.no_color { base } else { base.fg(t.amber) },
        );
    }
    let nums = fr.number_style(in_current, is_cursor).patch(base);
    let n = fr.numw;
    let l = row.left.map_or(" ".repeat(n), |l| format!("{:>n$}", l + 1));
    let r = row
        .right
        .map_or(" ".repeat(n), |r| format!("{:>n$}", r + 1));
    buf.set_stringn(1, y, &l, n, nums);
    let rx = 1 + n as u16 + 1;
    buf.set_stringn(rx, y, &r, n, nums);
    let gx = rx + n as u16 + 1;
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
            put(buf, x, y, max_x, sep, Style::default());
        }
        put(buf, x, y, max_x, k, key_style(t).bg(t.panel_bg));
        put(
            buf,
            x,
            y,
            max_x,
            &format!(" {d}"),
            Style::default().fg(t.dim).bg(t.panel_bg),
        );
    }
}

pub(crate) fn hints_len(items: &[(&str, &str)]) -> u16 {
    items
        .iter()
        .map(|(k, d)| k.chars().count() + d.chars().count() + 1)
        .sum::<usize>() as u16
        + 2 * items.len().saturating_sub(1) as u16
}

/// Rough width of the status line's left part, to decide how many hints fit.
fn x_after_left(app: &App) -> u16 {
    let msg = app.message.as_deref().map_or(0, |m| m.chars().count() + 2);
    let base = 8 + app.hunk_label().len() + 22;
    (base.max(msg + 8)) as u16
}

fn draw_status(buf: &mut Buffer, app: &App, area: Rect) {
    let t = &app.theme;
    let y = area.height - 1;
    let panel = Style::default().bg(t.panel_bg);
    buf.set_style(Rect::new(0, y, area.width, 1), panel);
    let muted = Style::default().fg(t.muted).bg(t.panel_bg);
    let mut x = 0u16;
    let end = area.width;

    if app.message_is_error {
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
            &format!("× {}", app.message.as_deref().unwrap_or("")),
            style,
        );
        return;
    }

    let (lw, rw) = (
        !app.session.left.is_readonly(),
        !app.session.right.is_readonly(),
    );
    let mut right_items: Vec<(&str, &str)> = if app.help {
        vec![]
    } else if app.prompt.is_some() {
        vec![("Enter", "search"), ("Esc", "cancel")]
    } else if app.search.is_some() {
        vec![("n", "next"), ("N", "previous"), ("Esc", "clear")]
    } else {
        let mut items = Vec::new();
        if rw {
            items.push((">", "copy to right"));
        }
        if lw {
            items.push(("<", "copy to left"));
        }
        if lw || rw {
            items.push(("u", "undo"));
        } else {
            items.push(("J K", "hunks"));
            items.push((
                "v",
                if app.view == View::Unified {
                    "side by side"
                } else {
                    "unified"
                },
            ));
        }
        items.push(("F1", "help"));
        items
    };
    // Drop the least important hints until the rest fit.
    while right_items.len() > 1 && hints_len(&right_items) + 2 + x_after_left(app) > end {
        let n = right_items.len();
        right_items.remove(n - 2);
    }

    if let Some(p) = &app.prompt {
        put(
            buf,
            &mut x,
            y,
            end,
            if p.backward { " ?" } else { " /" },
            key_style(t).bg(t.panel_bg),
        );
        put(
            buf,
            &mut x,
            y,
            end,
            &p.text,
            Style::default().fg(t.bright).bg(t.panel_bg),
        );
        put(
            buf,
            &mut x,
            y,
            end,
            "█",
            Style::default().fg(t.amber).bg(t.panel_bg),
        );
    } else {
        put(
            buf,
            &mut x,
            y,
            end,
            &format!(" {} ", app.hunk_label()),
            badge_style(t),
        );
        if let Some(m) = &app.message {
            put(
                buf,
                &mut x,
                y,
                end,
                &format!("  {m}"),
                Style::default().fg(t.amber).bg(t.panel_bg),
            );
        } else {
            put(
                buf,
                &mut x,
                y,
                end,
                &format!("  whitespace {}", app.session.ws.label()),
                muted,
            );
            let (l, r) = (app.eols.0.label(), app.eols.1.label());
            let differ = if app.eols.0 != app.eols.1 {
                Style::default().fg(t.amber).bg(t.panel_bg)
            } else {
                muted
            };
            put(buf, &mut x, y, end, &format!("  {l} | {r}"), differ);
            if let Some(s) = &app.search {
                let pos = match s.current {
                    Some(c) => format!("  {} of {}", c + 1, s.matches.len()),
                    None => "  no matches".into(),
                };
                put(
                    buf,
                    &mut x,
                    y,
                    end,
                    &pos,
                    Style::default().fg(t.amber).bg(t.panel_bg),
                );
            }
        }
    }
    let need = hints_len(&right_items);
    if need + 2 <= end.saturating_sub(x) {
        let mut rx = end - need - 1;
        hints(buf, &mut rx, y, end, t, &right_items, "  ");
    }
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
        1,
        y0 + 1,
        &m.title,
        w.saturating_sub(2),
        bold(Style::default().fg(t.bright).bg(t.panel_bg)),
    );
    for (i, line) in m.body.iter().enumerate() {
        buf.set_stringn(
            1,
            y0 + 2 + i as u16,
            line,
            w.saturating_sub(2),
            Style::default().fg(t.muted).bg(t.panel_bg),
        );
    }
    let mut x = 1;
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
    ("View", &[("v", "switch view"), ("W", "whitespace mode")]),
    (
        "Files",
        &[
            ("w", "save"),
            ("e", "open in $EDITOR"),
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
    let mut ys = vec![2u16; cols];
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
    buf.set_style(
        Rect::new(0, 0, area.width, 1),
        Style::default().bg(t.panel_bg),
    );
    let mut x = 1;
    put(
        buf,
        &mut x,
        0,
        area.width,
        &info.name,
        bold(Style::default().fg(t.bright).bg(t.panel_bg)),
    );
    put(
        buf,
        &mut x,
        0,
        area.width,
        "  (binary)",
        Style::default().fg(t.dim).bg(t.panel_bg),
    );
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
        let y = 2 + i as u16;
        let mut x = 2;
        put(buf, &mut x, y, area.width, k, label);
        put(buf, &mut x, y, area.width, v, Style::default());
    }
    if info.utf16 {
        let msg = "UTF-16 text is treated as binary in this version.";
        buf.set_stringn(
            2,
            6,
            msg,
            area.width as usize - 3,
            Style::default().fg(t.muted),
        );
    }
    let y = area.height - 1;
    buf.set_style(
        Rect::new(0, y, area.width, 1),
        Style::default().bg(t.panel_bg),
    );
    let mut x = 1;
    hints(
        buf,
        &mut x,
        y,
        area.width,
        t,
        &[("q", "quit"), ("Ctrl-c", "abort")],
        "  ",
    );
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
