//! Colours, colour depth, and background detection (spec §6.5).

use ratatui::style::{Color, Modifier, Style};
use serde::Deserialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ThemeMode {
    Auto,
    Dark,
    Light,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Depth {
    True,
    Ansi256,
    Ansi16,
}

impl Depth {
    pub fn detect() -> Depth {
        Self::from_env(
            std::env::var("COLORTERM").ok().as_deref(),
            std::env::var("TERM").ok().as_deref(),
        )
    }

    pub fn from_env(colorterm: Option<&str>, term: Option<&str>) -> Depth {
        if matches!(colorterm, Some("truecolor" | "24bit")) {
            return Depth::True;
        }
        match term {
            None => Depth::True,
            Some(t)
                if ["ghostty", "kitty", "alacritty", "wezterm", "direct"]
                    .iter()
                    .any(|n| t.contains(n)) =>
            {
                Depth::True
            }
            Some(t) if t.contains("256") => Depth::Ansi256,
            Some("dumb") | Some("linux") | Some("") => Depth::Ansi16,
            Some(t)
                if t.starts_with("xterm") || t.starts_with("screen") || t.starts_with("tmux") =>
            {
                Depth::Ansi256
            }
            Some(_) => Depth::Ansi16,
        }
    }
}

type Rgb = (u8, u8, u8);

fn hex(s: u32) -> Rgb {
    ((s >> 16) as u8, (s >> 8) as u8, s as u8)
}

fn dist(a: Rgb, b: Rgb) -> i32 {
    let d = |x: u8, y: u8| (x as i32 - y as i32).pow(2);
    d(a.0, b.0) + d(a.1, b.1) + d(a.2, b.2)
}

const CUBE: [u8; 6] = [0, 95, 135, 175, 215, 255];

fn to_256(c: Rgb) -> u8 {
    let near = |v: u8| {
        (0..6)
            .min_by_key(|&i| (CUBE[i] as i32 - v as i32).abs())
            .unwrap()
    };
    let (r, g, b) = (near(c.0), near(c.1), near(c.2));
    let cube = (CUBE[r], CUBE[g], CUBE[b]);
    let cube_idx = 16 + 36 * r + 6 * g + b;
    let avg = ((c.0 as u32 + c.1 as u32 + c.2 as u32) / 3) as i32;
    let gray_i = ((avg - 8).max(0) / 10).min(23);
    let gv = (8 + gray_i * 10) as u8;
    if dist(c, (gv, gv, gv)) < dist(c, cube) {
        232 + gray_i as u8
    } else {
        cube_idx as u8
    }
}

const ANSI16: [Rgb; 16] = [
    (0, 0, 0),
    (205, 0, 0),
    (0, 205, 0),
    (205, 205, 0),
    (0, 0, 238),
    (205, 0, 205),
    (0, 205, 205),
    (229, 229, 229),
    (127, 127, 127),
    (255, 0, 0),
    (0, 255, 0),
    (255, 255, 0),
    (92, 92, 255),
    (255, 0, 255),
    (0, 255, 255),
    (255, 255, 255),
];

fn to_16(c: Rgb) -> u8 {
    (0..16).min_by_key(|&i| dist(c, ANSI16[i])).unwrap() as u8
}

pub fn quantize(c: Rgb, depth: Depth) -> Color {
    match depth {
        Depth::True => Color::Rgb(c.0, c.1, c.2),
        Depth::Ansi256 => Color::Indexed(to_256(c)),
        Depth::Ansi16 => Color::Indexed(to_16(c)),
    }
}

/// Every colour the UI uses. With `NO_COLOR`, all are `Color::Reset` and the
/// renderer uses `reverse`/`bold` instead (see `Theme::no_color`).
#[derive(Debug, Clone)]
pub struct Theme {
    pub no_color: bool,
    pub dark: bool,
    pub panel_bg: Color,
    pub dim: Color,
    pub muted: Color,
    pub bright: Color,
    pub amber: Color,
    pub on_amber: Color,
    pub cursor_bg: Color,
    pub changed_bg: Color,
    pub changed_word_bg: Color,
    pub added_bg: Color,
    pub added_word_bg: Color,
    pub removed_bg: Color,
    pub removed_word_bg: Color,
    pub filler: Color,
    pub added_fg: Color,
    pub removed_fg: Color,
    pub changed_fg: Color,
    pub match_bg: Color,
    pub match_fg: Color,
}

/// Colour names accepted in `[colors]` (same as the `Theme` fields).
pub const COLOR_NAMES: &[&str] = &[
    "panel_bg",
    "dim",
    "muted",
    "bright",
    "amber",
    "on_amber",
    "cursor_bg",
    "changed_bg",
    "changed_word_bg",
    "added_bg",
    "added_word_bg",
    "removed_bg",
    "removed_word_bg",
    "filler",
    "added_fg",
    "removed_fg",
    "changed_fg",
    "match_bg",
    "match_fg",
];

/// `#rrggbb` (the `#` is optional).
pub fn parse_hex(s: &str) -> Option<(u8, u8, u8)> {
    let h = s.strip_prefix('#').unwrap_or(s);
    if h.len() != 6 || !h.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    let v = u32::from_str_radix(h, 16).ok()?;
    Some(((v >> 16) as u8, (v >> 8) as u8, v as u8))
}

impl Theme {
    /// Override colours by name. Ignored under `NO_COLOR`. Unknown names or bad
    /// values are skipped (they are reported when the config is parsed).
    pub fn apply_overrides(
        &mut self,
        colors: &std::collections::BTreeMap<String, String>,
        depth: Depth,
    ) {
        if self.no_color {
            return;
        }
        for (name, value) in colors {
            let Some(rgb) = parse_hex(value) else {
                continue;
            };
            let c = quantize(rgb, depth);
            let slot = match name.as_str() {
                "panel_bg" => &mut self.panel_bg,
                "dim" => &mut self.dim,
                "muted" => &mut self.muted,
                "bright" => &mut self.bright,
                "amber" => &mut self.amber,
                "on_amber" => &mut self.on_amber,
                "cursor_bg" => &mut self.cursor_bg,
                "changed_bg" => &mut self.changed_bg,
                "changed_word_bg" => &mut self.changed_word_bg,
                "added_bg" => &mut self.added_bg,
                "added_word_bg" => &mut self.added_word_bg,
                "removed_bg" => &mut self.removed_bg,
                "removed_word_bg" => &mut self.removed_word_bg,
                "filler" => &mut self.filler,
                "added_fg" => &mut self.added_fg,
                "removed_fg" => &mut self.removed_fg,
                "changed_fg" => &mut self.changed_fg,
                "match_bg" => &mut self.match_bg,
                "match_fg" => &mut self.match_fg,
                _ => continue,
            };
            *slot = c;
        }
    }

    pub fn new(dark: bool, depth: Depth, no_color: bool) -> Theme {
        // (dark, light) pairs; dark values come from the design mockups.
        let c = |d: u32, l: u32| {
            if no_color {
                Color::Reset
            } else {
                quantize(hex(if dark { d } else { l }), depth)
            }
        };
        Theme {
            no_color,
            dark,
            panel_bg: c(0x222833, 0xebe9e2),
            dim: c(0x5f6878, 0x8a8780),
            muted: c(0x8a93a3, 0x6b6a68),
            bright: c(0xeef2f8, 0x1f1e1d),
            amber: c(0xe7b04b, 0xb7791f),
            on_amber: c(0x1b1f27, 0xfaf9f5),
            cursor_bg: c(0x222833, 0xebe9e2),
            changed_bg: c(0x232d42, 0xe3eaf6),
            changed_word_bg: c(0x3a4f7a, 0xb9cbec),
            added_bg: c(0x1f3229, 0xe2f1e6),
            added_word_bg: c(0x2f5c43, 0xb3dcbf),
            removed_bg: c(0x3b2428, 0xf7e1e1),
            removed_word_bg: c(0x5c2b2e, 0xeeb5b5),
            filler: c(0x2c323d, 0xd6d3ca),
            added_fg: c(0x7fc08f, 0x2f7a46),
            removed_fg: c(0xe08a8a, 0xb23b3b),
            changed_fg: c(0x8fa8d8, 0x3b5fa8),
            match_bg: c(0x4b4227, 0xf6e3b0),
            match_fg: c(0xf2c56d, 0x5a3e05),
        }
    }

    /// `bg` as a style; under NO_COLOR nothing is set.
    pub fn bg(&self, color: Color) -> Style {
        Style::default().bg(color)
    }

    /// Emphasis that works with and without colour.
    pub fn emphasis(&self, style: Style) -> Style {
        if self.no_color {
            style.add_modifier(Modifier::REVERSED)
        } else {
            style
        }
    }
}

/// Luminance of an OSC 11 reply (`rgb:RRRR/GGGG/BBBB`, 1–4 hex digits per channel).
pub fn parse_osc11(reply: &str) -> Option<bool> {
    let body = reply.split("rgb:").nth(1)?;
    let body = body.trim_end_matches(|c: char| !c.is_ascii_hexdigit());
    let mut it = body.split('/');
    let mut chan = || -> Option<f32> {
        let h = it.next()?;
        let h = &h[..h.len().min(4)];
        let v = u32::from_str_radix(h, 16).ok()? as f32;
        Some(v / (16f32.powi(h.len() as i32) - 1.0))
    };
    let (r, g, b) = (chan()?, chan()?, chan()?);
    Some(0.2126 * r + 0.7152 * g + 0.0722 * b < 0.5)
}

/// What the terminal said in answer to our two queries.
#[derive(Debug, PartialEq, Eq)]
pub enum QueryReply {
    /// Still waiting for the DA1 reply that ends the exchange.
    Incomplete,
    /// The exchange is over. `Some(true)` means a dark background.
    Done(Option<bool>),
}

/// Interpret the bytes read so far. We send the background query (OSC 11)
/// followed by a primary device attributes query (`ESC [ c`). Terminals answer
/// in order, so once the DA1 reply (`ESC [ ? ... c`) has arrived, a background
/// reply has either already come or never will (tmux with nothing to forward,
/// or a terminal that ignores OSC 11). No timeout is needed to tell the two apart,
/// and no late reply is left behind to be read as key presses.
pub fn parse_query_reply(bytes: &[u8]) -> QueryReply {
    let text = String::from_utf8_lossy(bytes);
    let Some(da1) = text.find("\x1b[?") else {
        return QueryReply::Incomplete;
    };
    if !text[da1..].contains('c') {
        return QueryReply::Incomplete;
    }
    QueryReply::Done(parse_osc11(&text[..da1]))
}

/// Ask the terminal for its background colour. Raw mode must already be on.
/// Returns `Some(true)` for dark, `None` if it did not say (or on timeout).
#[cfg(unix)]
pub fn query_dark_background(timeout_ms: i32) -> Option<bool> {
    use std::io::Write;
    use std::os::fd::AsRawFd;
    let mut tty = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open("/dev/tty")
        .ok()?;
    tty.write_all(b"\x1b]11;?\x1b\\\x1b[c").ok()?;
    tty.flush().ok()?;
    let fd = tty.as_raw_fd();
    let mut reply = Vec::new();
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(timeout_ms as u64);
    loop {
        let left = deadline
            .saturating_duration_since(std::time::Instant::now())
            .as_millis() as i32;
        if left == 0 {
            return None;
        }
        if !wait_readable(fd, left) {
            return None;
        }
        let mut buf = [0u8; 64];
        // SAFETY: `buf` is valid for `buf.len()` bytes.
        let n = unsafe { libc::read(fd, buf.as_mut_ptr().cast(), buf.len()) };
        if n <= 0 {
            return None;
        }
        reply.extend_from_slice(&buf[..n as usize]);
        if let QueryReply::Done(dark) = parse_query_reply(&reply) {
            return dark;
        }
    }
}

/// Wait up to `ms` for `fd` to have input. This uses `select`, not `poll`: on
/// macOS `poll` reports `POLLNVAL` for terminal devices at once, and the `read`
/// that followed then blocked until a key came and swallowed it.
#[cfg(unix)]
fn wait_readable(fd: i32, ms: i32) -> bool {
    if !(0..libc::FD_SETSIZE as i32).contains(&fd) {
        return false;
    }
    let mut timeout = libc::timeval {
        tv_sec: (ms / 1000) as _,
        tv_usec: ((ms % 1000) * 1000) as _,
    };
    // SAFETY: `set` is initialised by FD_ZERO, `fd` is below FD_SETSIZE, and
    // every pointer passed to `select` is valid for the call.
    unsafe {
        let mut set: libc::fd_set = std::mem::zeroed();
        libc::FD_ZERO(&mut set);
        libc::FD_SET(fd, &mut set);
        libc::select(
            fd + 1,
            &mut set,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            &mut timeout,
        ) > 0
            && libc::FD_ISSET(fd, &set)
    }
}

#[cfg(not(unix))]
pub fn query_dark_background(_: i32) -> Option<bool> {
    None
}

/// Decide dark vs light (spec S2). `auto` asks the terminal, including inside
/// tmux; if the terminal (or tmux) does not answer, dark is the default.
pub fn resolve_dark(mode: ThemeMode) -> bool {
    match mode {
        ThemeMode::Dark => true,
        ThemeMode::Light => false,
        ThemeMode::Auto => query_dark_background(150).unwrap_or(true),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn osc11_parsing() {
        assert_eq!(parse_osc11("\x1b]11;rgb:1b1b/1f1f/2727\x1b\\"), Some(true));
        assert_eq!(parse_osc11("\x1b]11;rgb:fafa/f9f9/f5f5\x07"), Some(false));
        assert_eq!(parse_osc11("\x1b]11;rgb:ff/ff/ff\x07"), Some(false));
        assert_eq!(parse_osc11("garbage"), None);
    }

    #[test]
    fn the_da1_reply_ends_the_background_query() {
        let osc = "\x1b]11;rgb:fafa/f9f9/f5f5\x1b\\";
        let da1 = "\x1b[?62;4c";
        // Background answer first, then DA1: light.
        assert_eq!(
            parse_query_reply(format!("{osc}{da1}").as_bytes()),
            QueryReply::Done(Some(false))
        );
        // Terminal ignored OSC 11 (or tmux had nothing to forward): DA1 alone ends it.
        assert_eq!(parse_query_reply(da1.as_bytes()), QueryReply::Done(None));
        // Not finished until the DA1 reply is complete.
        assert_eq!(parse_query_reply(osc.as_bytes()), QueryReply::Incomplete);
        assert_eq!(parse_query_reply(b"\x1b[?62;4"), QueryReply::Incomplete);
        assert_eq!(parse_query_reply(b""), QueryReply::Incomplete);
        let dark = "\x1b]11;rgb:1b1b/1f1f/2727\x07";
        assert_eq!(
            parse_query_reply(format!("{dark}{da1}").as_bytes()),
            QueryReply::Done(Some(true))
        );
    }

    /// On macOS `poll` can't wait on a terminal device, so this runs on a real pty.
    #[cfg(target_os = "macos")]
    #[test]
    fn waiting_for_terminal_input_times_out_and_then_sees_it() {
        let (mut master, mut slave) = (0, 0);
        // SAFETY: both out-pointers are valid; the name, termios and size are optional.
        let rc = unsafe {
            libc::openpty(
                &mut master,
                &mut slave,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        };
        assert_eq!(rc, 0, "openpty failed");
        let start = std::time::Instant::now();
        assert!(!wait_readable(slave, 100), "nothing has been typed yet");
        assert!(start.elapsed() >= std::time::Duration::from_millis(90));
        // SAFETY: `master` is open and the buffer is valid.
        unsafe { libc::write(master, b"x\n".as_ptr().cast(), 2) };
        assert!(wait_readable(slave, 1000));
        // SAFETY: both descriptors came from openpty and are closed once.
        unsafe {
            libc::close(master);
            libc::close(slave);
        }
    }

    #[test]
    fn depth_detection() {
        assert_eq!(
            Depth::from_env(Some("truecolor"), Some("xterm")),
            Depth::True
        );
        assert_eq!(Depth::from_env(None, Some("xterm-ghostty")), Depth::True);
        assert_eq!(Depth::from_env(None, Some("tmux-256color")), Depth::Ansi256);
        assert_eq!(Depth::from_env(None, Some("linux")), Depth::Ansi16);
        assert_eq!(Depth::from_env(None, None), Depth::True);
    }

    #[test]
    fn quantization() {
        assert_eq!(quantize((255, 0, 0), Depth::Ansi256), Color::Indexed(196));
        assert_eq!(
            quantize((128, 128, 128), Depth::Ansi256),
            Color::Indexed(244)
        );
        assert_eq!(quantize((250, 10, 10), Depth::Ansi16), Color::Indexed(9));
        assert_eq!(quantize((1, 2, 3), Depth::True), Color::Rgb(1, 2, 3));
    }

    #[test]
    fn colour_overrides_apply_by_name_and_respect_no_color() {
        let mut o = std::collections::BTreeMap::new();
        o.insert("changed_bg".to_string(), "#102030".to_string());
        o.insert("nonsense".to_string(), "#ffffff".to_string());
        o.insert("amber".to_string(), "not-a-colour".to_string());
        let mut t = Theme::new(true, Depth::True, false);
        let amber = t.amber;
        t.apply_overrides(&o, Depth::True);
        assert_eq!(t.changed_bg, Color::Rgb(0x10, 0x20, 0x30));
        assert_eq!(t.amber, amber, "bad values leave the colour alone");
        let mut t = Theme::new(true, Depth::Ansi256, false);
        t.apply_overrides(&o, Depth::Ansi256);
        assert!(
            matches!(t.changed_bg, Color::Indexed(_)),
            "overrides follow the colour depth"
        );
        let mut t = Theme::new(true, Depth::True, true);
        t.apply_overrides(&o, Depth::True);
        assert_eq!(t.changed_bg, Color::Reset);
        assert_eq!(parse_hex("2a2f3a"), Some((0x2a, 0x2f, 0x3a)));
        assert_eq!(parse_hex("#12345"), None);
        assert_eq!(COLOR_NAMES.len(), 19);
    }

    #[test]
    fn no_color_theme_has_no_colours() {
        let t = Theme::new(true, Depth::True, true);
        assert_eq!(t.changed_bg, Color::Reset);
        assert_eq!(t.amber, Color::Reset);
    }
}
