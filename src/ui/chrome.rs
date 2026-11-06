//! The zones every screen shares (spec §6): title bar, toolbar, content and
//! status bar, always in that order. A zone with nothing to say keeps its row,
//! so nothing shifts between views.

use super::render::{bold, key_style};
use crate::theme::Theme;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use std::borrow::Cow;
use unicode_width::UnicodeWidthStr;

pub const TITLE_Y: u16 = 0;
pub const TOOLBAR_Y: u16 = 1;
/// The first content row. A blank row separates it from the toolbar.
pub const BODY_Y: u16 = 3;
/// Rows the other zones take: title, toolbar, the blank row and the status bar.
pub const CHROME_ROWS: u16 = 4;
/// Columns kept free at the right edge for the file view's overview ruler.
pub const RULER_W: u16 = 2;

/// How much room the labels get (spec §6, narrow terminals).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Tier {
    /// Under 100 columns: key and value only.
    Narrow,
    /// 100 to 139 columns: short labels.
    Short,
    /// 140 columns and more: full labels.
    Full,
}

pub fn tier(width: u16) -> Tier {
    match width {
        140.. => Tier::Full,
        100..=139 => Tier::Short,
        _ => Tier::Narrow,
    }
}

/// A run of styled text.
#[derive(Default)]
pub struct Spans<'a>(pub Vec<(Cow<'a, str>, Style)>);

impl<'a> Spans<'a> {
    pub fn push(&mut self, text: impl Into<Cow<'a, str>>, style: Style) -> &mut Self {
        self.0.push((text.into(), style));
        self
    }

    pub fn width(&self) -> u16 {
        self.0.iter().map(|(s, _)| s.width()).sum::<usize>() as u16
    }

    /// Draw from `x`, stopping at `max_x`. Returns where the text ended.
    pub fn draw(&self, buf: &mut Buffer, x: u16, y: u16, max_x: u16) -> u16 {
        let mut x = x;
        for (s, style) in &self.0 {
            if x >= max_x {
                break;
            }
            x = buf.set_stringn(x, y, s, (max_x - x) as usize, *style).0;
        }
        x
    }

    /// Draw so the text ends one column before `end`, if it fits right of
    /// `min_x`. Returns where it starts.
    pub fn draw_right(&self, buf: &mut Buffer, y: u16, min_x: u16, end: u16) -> Option<u16> {
        let x = end.checked_sub(self.width() + 1)?;
        if x < min_x {
            return None;
        }
        self.draw(buf, x, y, end);
        Some(x)
    }
}

/// Paint a bar's background across the whole row.
pub fn fill(buf: &mut Buffer, y: u16, width: u16, style: Style) {
    buf.set_style(Rect::new(0, y, width, 1), style);
}

/// The surface of the title bar.
pub fn title_style(t: &Theme) -> Style {
    if t.no_color {
        bold(Style::default())
    } else {
        Style::default().bg(t.title_bg)
    }
}

/// The back control a file opened from the list starts its title bar with.
pub const BACK: &str = " ‹ files ";

pub fn back_style(t: &Theme) -> Style {
    if t.no_color {
        Style::default().add_modifier(Modifier::REVERSED | Modifier::BOLD)
    } else {
        bold(Style::default().fg(t.bright).bg(t.back_bg))
    }
}

/// A key hint: the key in amber, then what it does. `off` dims both, for
/// actions that don't apply right now, so the bar never jumps.
pub fn hint<'a>(
    spans: &mut Spans<'a>,
    t: &Theme,
    bg: Style,
    key: &'a str,
    what: &'a str,
    off: bool,
) {
    if off {
        let s = if t.no_color { bg } else { bg.fg(t.off) };
        spans.push(key, s).push(format!(" {what}"), s);
    } else {
        spans
            .push(key, key_style(t).patch(bg_only(bg)))
            .push(format!(" {what}"), bg.fg(t.muted));
    }
}

fn bg_only(s: Style) -> Style {
    match s.bg {
        Some(c) => Style::default().bg(c),
        None => Style::default(),
    }
}

/// Key hints separated by three spaces.
pub fn hint_spans<'a>(t: &Theme, bg: Style, items: &[(&'a str, &'a str, bool)]) -> Spans<'a> {
    let mut s = Spans::default();
    for (i, &(k, d, off)) in items.iter().enumerate() {
        if i > 0 {
            s.push("   ", bg);
        }
        hint(&mut s, t, bg, k, d, off);
    }
    s
}

/// Cut a directory part from the middle so it fits `width` columns: keep the
/// first folder and as many of the last ones as fit, with `…/` for the rest.
/// `dirs` ends with `/` (or is empty). Too narrow even for that, it keeps the
/// end, starting with `…`.
pub fn middle_cut(dirs: &str, width: usize) -> String {
    if dirs.width() <= width {
        return dirs.to_string();
    }
    let parts: Vec<&str> = dirs.split('/').filter(|p| !p.is_empty()).collect();
    let lead = if dirs.starts_with('/') { "/" } else { "" };
    let w = |p: &str| p.width() + 1;
    let ell = "…/".width();
    if parts.len() >= 2 && lead.width() + w(parts[0]) + ell <= width {
        let (mut front, mut back) = (1usize, 0usize);
        let mut used = lead.width() + w(parts[0]) + ell;
        // Alternate between the end and the start; a side that doesn't fit
        // stops growing, the other may still.
        let (mut back_ok, mut front_ok) = (true, true);
        while front + back < parts.len() && (back_ok || front_ok) {
            if back_ok {
                let p = parts[parts.len() - 1 - back];
                if used + w(p) <= width {
                    used += w(p);
                    back += 1;
                    if front + back >= parts.len() {
                        break;
                    }
                } else {
                    back_ok = false;
                }
            }
            if front_ok {
                let p = parts[front];
                if front + back < parts.len() && used + w(p) <= width {
                    used += w(p);
                    front += 1;
                } else {
                    front_ok = false;
                }
            }
        }
        if front + back >= parts.len() {
            return dirs.to_string();
        }
        let mut out = String::from(lead);
        for p in &parts[..front] {
            out.push_str(p);
            out.push('/');
        }
        out.push_str("…/");
        for p in &parts[parts.len() - back..] {
            out.push_str(p);
            out.push('/');
        }
        return out;
    }
    clip_left(dirs, width)
}

/// Keep the end of `s` in `width` columns, marking the cut with `…`.
pub fn clip_left(s: &str, width: usize) -> String {
    if s.width() <= width {
        return s.to_string();
    }
    if width == 0 {
        return String::new();
    }
    let mut out: Vec<char> = Vec::new();
    let mut used = 1;
    for c in s.chars().rev() {
        let cw = unicode_width::UnicodeWidthChar::width(c).unwrap_or(0);
        if used + cw > width {
            break;
        }
        used += cw;
        out.push(c);
    }
    out.push('…');
    out.into_iter().rev().collect()
}

/// Split a path into its folders (ending in `/`) and its last part.
pub fn split_name(path: &str) -> (&str, &str) {
    match path.rfind('/') {
        Some(i) if i + 1 < path.len() => (&path[..=i], &path[i + 1..]),
        _ => ("", path),
    }
}

/// What tells two sides apart, for the title bar: the last part of each that
/// differs, the characters those share, and the path both have after it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Roots {
    /// Characters both names start with (`wso2is-5.11.0.`).
    pub shared: String,
    /// What follows `shared` in each name (`396`, `445`).
    pub left: String,
    pub right: String,
    /// The path both sides end with, from its `/` (`/repository/deployment`).
    pub tail: String,
}

impl Roots {
    pub fn new(left: &str, right: &str) -> Roots {
        let l: Vec<&str> = left.split('/').filter(|p| !p.is_empty()).collect();
        let r: Vec<&str> = right.split('/').filter(|p| !p.is_empty()).collect();
        let mut k = l
            .iter()
            .rev()
            .zip(r.iter().rev())
            .take_while(|(a, b)| a == b)
            .count();
        // Each side keeps at least one part of its own, unless they're the same.
        while k > 0 && (k == l.len() || k == r.len()) && l != r {
            k -= 1;
        }
        let (lh, rh) = (&l[..l.len() - k], &r[..r.len() - k]);
        let tail: String = l[l.len() - k..].iter().map(|p| format!("/{p}")).collect();
        let (mut a, mut b) = (
            lh.last().copied().unwrap_or_default().to_string(),
            rh.last().copied().unwrap_or_default().to_string(),
        );
        // `x/src` against `src`: the last parts alone would read the same.
        if a == b && lh != rh {
            a = lh.join("/");
            b = rh.join("/");
        }
        let n = a.chars().zip(b.chars()).take_while(|(x, y)| x == y).count();
        // Only a shared start that leaves both something of their own.
        let n = if n == a.chars().count() || n == b.chars().count() {
            0
        } else {
            n
        };
        let byte = |s: &str| s.char_indices().nth(n).map_or(s.len(), |(i, _)| i);
        let (ai, bi) = (byte(&a), byte(&b));
        Roots {
            shared: a[..ai].to_string(),
            left: a[ai..].to_string(),
            right: b[bi..].to_string(),
            tail,
        }
    }

    /// `wso2is-5.11.0.396 ↔ wso2is-5.11.0.445`, or with `compact` the shared
    /// part once: `wso2is-5.11.0.396 ↔ 445`. Only what differs is coloured.
    pub fn spans<'a>(&'a self, t: &Theme, bg: Style, compact: bool) -> Spans<'a> {
        let mut s = Spans::default();
        let muted = bg.fg(t.muted);
        let (lc, rc) = if t.no_color {
            (bold(bg), bold(bg))
        } else {
            (bold(bg.fg(t.removed_fg)), bold(bg.fg(t.added_fg)))
        };
        s.push(self.shared.as_str(), muted)
            .push(self.left.as_str(), lc)
            .push(" ↔ ", muted);
        if !compact {
            s.push(self.shared.as_str(), muted);
        }
        s.push(self.right.as_str(), rc);
        s
    }

    /// The left name in full.
    pub fn left_name(&self) -> String {
        format!("{}{}", self.shared, self.left)
    }

    pub fn right_name(&self) -> String {
        format!("{}{}", self.shared, self.right)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roots_show_what_differs_and_the_shared_tail() {
        let r = Roots::new(
            "/a/wso2is-5.11.0.396/repository/webapps",
            "/b/c/wso2is-5.11.0.445/repository/webapps",
        );
        assert_eq!(
            (
                r.shared.as_str(),
                r.left.as_str(),
                r.right.as_str(),
                r.tail.as_str()
            ),
            ("wso2is-5.11.0.", "396", "445", "/repository/webapps")
        );
        let r = Roots::new("/tmp/git-difftool.Xa12/left", "~/code/app");
        assert_eq!(
            (r.left.as_str(), r.right.as_str(), r.tail.as_str()),
            ("left", "app", "")
        );
        let r = Roots::new("a/src/auth/token.rs", "src/auth/token.rs");
        assert_eq!((r.left.as_str(), r.right.as_str()), ("a/src", "src"));
        assert_eq!(r.tail, "/auth/token.rs");
        let r = Roots::new("v1.2", "v1.20");
        assert_eq!((r.shared.as_str(), r.left.as_str()), ("", "v1.2"));
    }

    #[test]
    fn middle_cut_keeps_the_first_folder_and_the_last_ones() {
        let d = "api#identity#auth#v1.1/META-INF/maven/org.wso2.carbon/org.wso2.endpoint/";
        assert_eq!(middle_cut(d, 200), d);
        let cut = middle_cut(d, 50);
        assert!(cut.starts_with("api#identity#auth#v1.1/") && cut.contains("…/"));
        assert!(
            cut.ends_with("org.wso2.endpoint/") && cut.width() <= 50,
            "{cut}"
        );
        assert_eq!(middle_cut(d, 12), "…2.endpoint/");
        assert_eq!(middle_cut("", 0), "");
    }

    #[test]
    fn clip_left_keeps_the_end() {
        assert_eq!(clip_left("src/a.rs", 20), "src/a.rs");
        assert_eq!(clip_left("some/long/path/file.rs", 10), "…h/file.rs");
        assert_eq!(clip_left("abc", 1), "…");
    }
}
