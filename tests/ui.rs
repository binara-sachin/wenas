//! Screen snapshots (spec §12): rendered through ratatui's test backend.

use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::style::Color;
use wenas::diff::Whitespace;
use wenas::document::Document;
use wenas::ops::{Session, Side};
use wenas::theme::{Depth, Theme};
use wenas::ui::render::draw;
use wenas::ui::{App, BinaryInfo, Outcome, View, ViewConfig};

const LEFT: &str = "impl Token {\n    pub fn is_expired(&self) -> bool {\n        self.issued + self.ttl < now()\n    }\n\n    /// Refresh the token if it has expired.\n    fn refresh(&self) -> Result {\n        let ttl = self.ttl;\n        self.fetch(ttl)?;\n        self.cache.clear();\n        Ok(())\n    }\n}\n";
const RIGHT: &str = "impl Token {\n    pub fn is_expired(&self) -> bool {\n        self.issued + self.ttl <= now()\n    }\n\n    /// Refresh the token before it expires.\n    fn refresh(&self) -> Result {\n        let ttl = self.ttl.max(60);\n        log::debug!(\"ttl={ttl}\");\n        self.fetch(ttl)?;\n        Ok(())\n    }\n}\n";

fn theme() -> Theme {
    Theme::new(true, Depth::True, false)
}

fn app_with(left: &str, right: &str, size: (u16, u16), cfg: ViewConfig) -> App {
    let mut l = Document::from_bytes(left.into());
    let mut r = Document::from_bytes(right.into());
    l.label = Some("a/src/auth/token.rs".into());
    l.readonly = true;
    r.label = Some("src/auth/token.rs".into());
    App::new(Session::new(l, r, Whitespace::Off), theme(), cfg, size)
}

fn app(size: (u16, u16)) -> App {
    app_with(LEFT, RIGHT, size, ViewConfig::default())
}

fn render(app: &mut App) -> (Terminal<TestBackend>, String) {
    let mut term = Terminal::new(TestBackend::new(app.width, app.height)).unwrap();
    term.draw(|f| draw(f, app)).unwrap();
    let buf = term.backend().buffer().clone();
    let mut out = String::new();
    for y in 0..buf.area.height {
        let line: String = (0..buf.area.width)
            .map(|x| buf[(x, y)].symbol().to_string())
            .collect();
        out.push_str(line.trim_end());
        out.push('\n');
    }
    (term, out)
}

fn text(app: &mut App) -> String {
    render(app).1
}

fn key(app: &mut App, c: char) {
    app.on_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
}
fn press(app: &mut App, code: KeyCode) {
    app.on_key(KeyEvent::new(code, KeyModifiers::NONE));
}

#[test]
fn side_by_side() {
    insta::assert_snapshot!(text(&mut app((100, 20))));
}

#[test]
fn unified_when_narrow() {
    let mut a = app((80, 20));
    assert_eq!(a.view, View::Unified);
    insta::assert_snapshot!(text(&mut a));
}

#[test]
fn switching_view_with_v_and_resize_threshold() {
    let mut a = app((120, 20));
    assert_eq!(a.view, View::SideBySide);
    key(&mut a, 'v');
    text(&mut a);
    assert_eq!(a.view, View::Unified);
    key(&mut a, 'v');
    text(&mut a);
    assert_eq!(a.view, View::SideBySide);
    // Resizing across the threshold switches views (no explicit override on a fresh app).
    let mut a = app((120, 20));
    a.sync_size(80, 20);
    assert_eq!(a.view, View::Unified);
    a.sync_size(120, 20);
    assert_eq!(a.view, View::SideBySide);
}

#[test]
fn help_screen() {
    let mut a = app((100, 20));
    press(&mut a, KeyCode::F(1));
    insta::assert_snapshot!(text(&mut a));
    press(&mut a, KeyCode::Esc);
    assert!(!a.help);
}

#[test]
fn search_highlights_and_counts() {
    let mut a = app((100, 20));
    key(&mut a, '/');
    for c in "ttl".chars() {
        key(&mut a, c);
    }
    press(&mut a, KeyCode::Enter);
    let s = a.search.as_ref().unwrap();
    assert!(s.matches.len() >= 6);
    let first = s.current;
    key(&mut a, 'n');
    assert_ne!(a.search.as_ref().unwrap().current, first);
    key(&mut a, 'N');
    assert_eq!(a.search.as_ref().unwrap().current, first);
    insta::assert_snapshot!(text(&mut a));
    press(&mut a, KeyCode::Esc);
    assert!(a.search.is_none());
}

#[test]
fn smart_case_search() {
    let mut a = app((100, 20));
    a.run_search("TOKEN".into(), false, 0, true);
    assert!(a.search.as_ref().unwrap().matches.is_empty());
    a.run_search("token".into(), false, 0, true);
    assert!(!a.search.as_ref().unwrap().matches.is_empty());
}

#[test]
fn hunk_navigation_and_labels() {
    let mut a = app((100, 20));
    let total = a.session.hunks.len();
    assert!(total >= 3);
    assert_eq!(a.hunk_label(), format!("1/{total}"));
    key(&mut a, 'J');
    assert_eq!(a.hunk_label(), format!("2/{total}"));
    key(&mut a, 'K');
    assert_eq!(a.hunk_label(), format!("1/{total}"));
    key(&mut a, 'K');
    assert_eq!(a.message.as_deref(), Some("first hunk"));
    for _ in 0..total + 2 {
        key(&mut a, 'J');
    }
    assert_eq!(a.message.as_deref(), Some("last hunk"));
    // ]c / [c work too.
    key(&mut a, '[');
    key(&mut a, 'c');
    assert_eq!(a.hunk_label(), format!("{}/{total}", total - 1));
}

#[test]
fn gg_and_g_move_cursor() {
    let mut a = app((100, 12));
    key(&mut a, 'G');
    assert_eq!(a.cursor, a.rows.len() - 1);
    key(&mut a, 'g');
    key(&mut a, 'g');
    assert_eq!((a.cursor, a.top), (0, 0));
    app_ctrl(&mut a, 'd');
    assert!(a.cursor > 0);
    app_ctrl(&mut a, 'u');
    assert_eq!(a.cursor, 0);
}

fn app_ctrl(app: &mut App, c: char) {
    app.on_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL));
}

#[test]
fn quit_and_abort_outcomes() {
    let mut a = app((100, 20));
    key(&mut a, 'q');
    assert_eq!(a.outcome, Some(Outcome::Quit));
    let mut a = app((100, 20));
    app_ctrl(&mut a, 'c');
    assert_eq!(a.outcome, Some(Outcome::Abort));
}

#[test]
fn editing_keys_report_not_available() {
    let mut a = app((100, 20));
    key(&mut a, '>');
    assert!(a.message.as_deref().unwrap().contains("M3"));
    assert!(!a.session.is_dirty());
}

#[test]
fn whitespace_mode_cycles_and_rediffs() {
    let mut a = app_with("a  b\nx\n", "a b\ny\n", (100, 12), ViewConfig::default());
    assert_eq!(a.session.hunks.len(), 1);
    key(&mut a, 'W');
    assert_eq!(a.session.ws, Whitespace::Change);
    key(&mut a, 'W');
    key(&mut a, 'W');
    assert_eq!(a.session.ws, Whitespace::Off);
}

#[test]
fn sideways_scroll_reveals_long_lines_and_marks_cut() {
    let long = format!("{}END\n", "x".repeat(150));
    let mut a = app_with(&long, "short\n", (100, 8), ViewConfig::default());
    let s = text(&mut a);
    assert!(s.contains('›'), "cut-off lines are marked");
    assert!(!s.contains("END"));
    for _ in 0..20 {
        press(&mut a, KeyCode::Right);
    }
    assert!(text(&mut a).contains("END"));
    for _ in 0..30 {
        press(&mut a, KeyCode::Left);
    }
    assert_eq!(a.hscroll, 0);
}

#[test]
fn wide_characters_and_invalid_bytes() {
    let mut a = app_with(
        "日本語\nbad \u{fffd}\n",
        "x\n",
        (60, 8),
        ViewConfig::default(),
    );
    let s = text(&mut a);
    // A wide glyph occupies two cells; the buffer dump shows a blank after each.
    let squeezed: String = s.chars().filter(|c| !c.is_whitespace()).collect();
    assert!(squeezed.contains("日本語"), "{s}");
    let mut l = Document::from_bytes(b"a\xFFb\n".to_vec());
    l.label = Some("l".into());
    let r = Document::from_bytes(b"c\n".to_vec());
    let mut a = App::new(
        Session::new(l, r, Whitespace::Off),
        theme(),
        ViewConfig::default(),
        (100, 8),
    );
    assert!(text(&mut a).contains("a\\xFFb"));
}

#[test]
fn colours_tint_changed_rows_and_words() {
    let mut a = app((100, 20));
    let (term, _) = render(&mut a);
    let buf = term.backend().buffer();
    let t = theme();
    let find_bg = |color: Color| {
        (0..buf.area.height).any(|y| (0..buf.area.width).any(|x| buf[(x, y)].bg == color))
    };
    assert!(find_bg(t.changed_bg));
    assert!(find_bg(t.changed_word_bg));
    assert!(find_bg(t.added_bg));
    assert!(find_bg(t.removed_bg));
    assert!(find_bg(t.amber), "current hunk gutter is highlighted");
}

#[test]
fn no_color_uses_markers_and_reverse_video() {
    let mut a = app((100, 20));
    a.theme = Theme::new(true, Depth::True, true);
    let (term, s) = render(&mut a);
    let buf = term.backend().buffer();
    assert!(s.contains('~') || s.contains('+'));
    let mut reversed = false;
    for y in 0..buf.area.height {
        for x in 0..buf.area.width {
            let c = &buf[(x, y)];
            assert!(
                matches!(c.bg, Color::Reset),
                "no background colours under NO_COLOR"
            );
            assert!(
                matches!(c.fg, Color::Reset),
                "no foreground colours under NO_COLOR"
            );
            reversed |= c.modifier.contains(ratatui::style::Modifier::REVERSED);
        }
    }
    assert!(reversed);
}

#[test]
fn readonly_side_controls_gutter_arrows() {
    // Left is read-only, so only the right-pointing arrow is offered.
    let s = text(&mut app((100, 20)));
    assert!(s.contains("~→") && !s.contains('←'), "{s}");
    let mut l = Document::from_bytes(LEFT.into());
    l.readonly = false;
    let mut a = app((100, 20));
    a.session.left = l;
    a.session.rediff();
    a.rebuild(None);
    assert!(text(&mut a).contains("←~→"));
    assert!(!a.session.doc(Side::Left).readonly);
}

#[test]
fn binary_screen() {
    let info = BinaryInfo {
        name: "assets/logo.png".into(),
        left_size: 48213,
        right_size: 48301,
        first_difference: Some(2048),
        utf16: false,
    };
    let mut a = App::binary_screen(info, theme(), (60, 10));
    insta::assert_snapshot!(text(&mut a));
    key(&mut a, 'q');
    assert_eq!(a.outcome, Some(Outcome::Quit));
}

#[test]
fn tiny_terminal_does_not_panic() {
    for size in [(1, 1), (10, 3), (29, 4), (30, 5), (31, 6)] {
        let mut a = app(size);
        text(&mut a);
        key(&mut a, 'v');
        key(&mut a, 'G');
        text(&mut a);
    }
}

#[test]
fn identical_after_whitespace_mode_shows_no_hunks() {
    let mut a = app_with("a  b\n", "a b\n", (100, 10), ViewConfig::default());
    key(&mut a, 'W');
    assert_eq!(a.hunk_label(), "-/0");
    text(&mut a);
    key(&mut a, 'J');
    assert_eq!(a.message.as_deref(), Some("last hunk"));
}
