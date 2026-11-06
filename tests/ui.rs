//! Screen snapshots (spec §12): rendered through ratatui's test backend.

use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::style::Color;
use wenas::diff::Whitespace;
use wenas::document::{Document, ReadOnlyReason};
use wenas::ops::{Session, Side};
use wenas::theme::{Depth, Theme};
use wenas::ui::app::Modal;
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
    l.make_readonly(ReadOnlyReason::GitOldVersion);
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
fn crlf_against_lf_screen() {
    // CRLF vs LF is ignored when comparing but flagged in the status line.
    let mut l = Document::from_bytes(b"fn a() {\r\n    one();\r\n    two();\r\n}\r\n".to_vec());
    let mut r = Document::from_bytes(b"fn a() {\n    one();\n    TWO();\n}\n".to_vec());
    l.label = Some("a/windows.rs".into());
    r.label = Some("windows.rs".into());
    let mut a = App::new(
        Session::new(l, r, Whitespace::Off),
        theme(),
        ViewConfig::default(),
        (100, 10),
    );
    assert_eq!(
        a.session.hunks.len(),
        1,
        "only the real change, not every line"
    );
    insta::assert_snapshot!(text(&mut a));
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
    let l = Document::from_bytes(LEFT.into());
    let mut a = app((100, 20));
    a.session.left = l;
    a.session.rediff();
    a.rebuild(None);
    assert!(text(&mut a).contains("←~→"));
    assert!(!a.session.doc(Side::Left).is_readonly());
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

// ---- editing (M3) ----

/// A session over two real files in a temp dir, both writable.
fn files_app(left: &str, right: &str) -> (tempfile::TempDir, App) {
    let dir = tempfile::tempdir().unwrap();
    let (lp, rp) = (dir.path().join("left.rs"), dir.path().join("right.rs"));
    std::fs::write(&lp, left).unwrap();
    std::fs::write(&rp, right).unwrap();
    let mut l = Document::open(&lp).unwrap();
    let mut r = Document::open(&rp).unwrap();
    l.label = Some("left.rs".into());
    r.label = Some("right.rs".into());
    let app = App::new(
        Session::new(l, r, Whitespace::Off),
        theme(),
        ViewConfig::default(),
        (100, 20),
    );
    (dir, app)
}

fn read(dir: &tempfile::TempDir, name: &str) -> String {
    std::fs::read_to_string(dir.path().join(name)).unwrap()
}

#[test]
fn copy_hunk_edits_the_target_and_marks_it_dirty() {
    let mut a = app((100, 20));
    let before = a.session.hunks.len();
    key(&mut a, '>');
    assert_eq!(a.session.hunks.len(), before - 1);
    assert!(a.session.right.is_dirty() && !a.session.left.is_dirty());
    assert_eq!(a.message.as_deref(), Some("hunk 1 copied to right"));
    let s = text(&mut a);
    // The right pane's header, on the row under the toolbar's blank row.
    assert!(s.lines().nth(3).unwrap().contains("[+]"), "{s}");
}

#[test]
fn copy_acts_on_the_hunk_under_or_below_the_cursor() {
    let mut a = app((100, 20));
    key(&mut a, 'J');
    key(&mut a, 'J');
    let h = a.current_hunk().unwrap();
    let expected = a.session.hunks[h].clone();
    key(&mut a, '>');
    // Only that hunk was copied: the right side now matches the left there.
    let r = a.session.right.to_bytes();
    let left_lines: Vec<&[u8]> = (expected.left.clone())
        .map(|i| a.session.left.line(i))
        .collect();
    for l in left_lines {
        assert!(r.windows(l.len().max(1)).any(|w| w == l));
    }
    assert_eq!(a.session.hunks.len(), 3);
}

#[test]
fn cursor_stays_on_the_same_logical_line_after_a_copy() {
    let mut a = app((100, 20));
    key(&mut a, 'J');
    key(&mut a, 'J');
    let src_line = a.rows[a.cursor].left;
    key(&mut a, '>');
    assert_eq!(a.rows[a.cursor].left, src_line);
}

#[test]
fn copy_into_readonly_side_is_refused_with_a_hint() {
    let mut a = app((100, 20));
    a.session.left.make_readonly(ReadOnlyReason::GitOldVersion);
    key(&mut a, '<');
    assert!(a.message_is_error);
    assert_eq!(
        a.message.as_deref(),
        Some(
            "The left side is git's old version, so it's read-only. Press > to copy right instead."
        )
    );
    assert!(!a.session.is_dirty());
    insta::assert_snapshot!(text(&mut a));
}

#[test]
fn copy_with_both_sides_readonly_gives_no_alternative() {
    let mut a = app((100, 20));
    a.session.right.make_readonly(ReadOnlyReason::FlagRight);
    key(&mut a, '>');
    assert_eq!(
        a.message.as_deref(),
        Some("The right side is marked --readonly-right, so it's read-only.")
    );
}

#[test]
fn undo_redo_move_the_cursor_to_the_change() {
    let mut a = app((100, 12));
    key(&mut a, 'J');
    key(&mut a, 'J');
    key(&mut a, '>');
    key(&mut a, 'g');
    key(&mut a, 'g');
    key(&mut a, 'u');
    assert_eq!(a.message.as_deref(), Some("undone"));
    assert!(!a.session.is_dirty());
    assert!(a.cursor > 0, "cursor jumped to the restored hunk");
    app_ctrl(&mut a, 'r');
    assert!(a.session.right.is_dirty());
    key(&mut a, 'u');
    key(&mut a, 'u');
    assert_eq!(a.message.as_deref(), Some("nothing to undo"));
}

#[test]
fn save_writes_the_file_atomically_and_clears_dirty() {
    let (dir, mut a) = files_app("a\nB\nc\n", "a\nb\nc\n");
    key(&mut a, '>');
    assert_eq!(
        read(&dir, "right.rs"),
        "a\nb\nc\n",
        "nothing written until saved"
    );
    key(&mut a, 'w');
    assert_eq!(read(&dir, "right.rs"), "a\nB\nc\n");
    assert!(!a.session.is_dirty());
    assert_eq!(a.message.as_deref(), Some("saved right.rs"));
    key(&mut a, 'w');
    assert_eq!(a.message.as_deref(), Some("nothing to save"));
    // Undo still works after saving, and makes the file dirty again.
    key(&mut a, 'u');
    assert!(a.session.right.is_dirty());
}

#[test]
fn changed_on_disk_asks_and_overwrite_wins() {
    let (dir, mut a) = files_app("a\nB\nc\n", "a\nb\nc\n");
    key(&mut a, '>');
    std::fs::write(dir.path().join("right.rs"), "someone else\n").unwrap();
    key(&mut a, 'w');
    assert!(matches!(
        a.modal,
        Some(Modal::ChangedOnDisk {
            side: Side::Right,
            quit_after: false
        })
    ));
    assert_eq!(read(&dir, "right.rs"), "someone else\n");
    insta::assert_snapshot!(text(&mut a));
    key(&mut a, 'o');
    assert!(a.modal.is_none());
    assert_eq!(read(&dir, "right.rs"), "a\nB\nc\n");
    assert!(!a.session.is_dirty());
}

#[test]
fn changed_on_disk_reload_drops_changes_but_undo_brings_them_back() {
    let (dir, mut a) = files_app("a\nB\nc\n", "a\nb\nc\n");
    key(&mut a, '>');
    std::fs::write(dir.path().join("right.rs"), "a\nb\nc\nd\n").unwrap();
    key(&mut a, 'w');
    key(&mut a, 'r');
    assert!(a.modal.is_none());
    assert_eq!(a.session.right.to_bytes(), b"a\nb\nc\nd\n");
    assert!(!a.session.is_dirty());
    assert_eq!(
        read(&dir, "right.rs"),
        "a\nb\nc\nd\n",
        "disk untouched by a reload"
    );
    key(&mut a, 'u');
    assert_eq!(a.session.right.to_bytes(), b"a\nB\nc\n");
}

#[test]
fn changed_on_disk_cancel_keeps_everything() {
    let (dir, mut a) = files_app("a\nB\n", "a\nb\n");
    key(&mut a, '>');
    std::fs::write(dir.path().join("right.rs"), "x\n").unwrap();
    key(&mut a, 'w');
    press(&mut a, KeyCode::Esc);
    assert!(a.modal.is_none() && a.session.right.is_dirty());
    assert_eq!(read(&dir, "right.rs"), "x\n");
}

#[test]
fn quit_with_unsaved_changes_asks_first() {
    let (dir, mut a) = files_app("a\nB\n", "a\nb\n");
    key(&mut a, '>');
    key(&mut a, 'q');
    assert_eq!(a.modal, Some(Modal::QuitUnsaved));
    assert_eq!(a.outcome, None);
    insta::assert_snapshot!(text(&mut a));
    press(&mut a, KeyCode::Esc);
    assert!(a.modal.is_none() && a.outcome.is_none());
    key(&mut a, 'q');
    key(&mut a, 'w');
    assert_eq!(a.outcome, Some(Outcome::Quit));
    assert_eq!(read(&dir, "right.rs"), "a\nB\n");
}

#[test]
fn quit_without_saving_discards() {
    let (dir, mut a) = files_app("a\nB\n", "a\nb\n");
    key(&mut a, '>');
    key(&mut a, 'q');
    key(&mut a, 'Q');
    assert_eq!(a.outcome, Some(Outcome::Quit));
    assert_eq!(read(&dir, "right.rs"), "a\nb\n");
    // Capital Q quits straight away, without asking.
    let (_d, mut a) = files_app("a\nB\n", "a\nb\n");
    key(&mut a, '>');
    key(&mut a, 'Q');
    assert_eq!(a.outcome, Some(Outcome::Quit));
}

#[test]
fn save_and_quit_stops_at_a_conflict() {
    let (dir, mut a) = files_app("a\nB\n", "a\nb\n");
    key(&mut a, '>');
    std::fs::write(dir.path().join("right.rs"), "x\n").unwrap();
    key(&mut a, 'q');
    key(&mut a, 'w');
    assert!(matches!(
        a.modal,
        Some(Modal::ChangedOnDisk {
            quit_after: true,
            ..
        })
    ));
    assert_eq!(a.outcome, None);
    key(&mut a, 'o');
    assert_eq!(a.outcome, Some(Outcome::Quit));
    assert_eq!(read(&dir, "right.rs"), "a\nB\n");
}

#[test]
fn both_sides_can_be_edited_and_saved_together() {
    let (dir, mut a) = files_app("1\n2\nL\n", "1\nR\n2\n");
    let n = a.session.hunks.len();
    assert!(n >= 2);
    key(&mut a, '>');
    key(&mut a, 'J');
    key(&mut a, '<');
    let dirty = a.session.dirty_sides();
    key(&mut a, 'w');
    assert!(!a.session.is_dirty());
    assert!(dirty.len() <= 2);
    let (l, r) = (read(&dir, "left.rs"), read(&dir, "right.rs"));
    assert!(!l.is_empty() && !r.is_empty());
}

#[test]
fn editor_request_targets_the_focused_side_and_cursor_line() {
    let (dir, mut a) = files_app("a\nB\nc\n", "a\nb\nc\n");
    key(&mut a, 'J');
    key(&mut a, 'e');
    let req = a.editor_request.take().expect("editor requested");
    assert_eq!(req.side, Side::Right);
    assert_eq!(req.line, 2);
    assert_eq!(req.path, dir.path().join("right.rs"));
    press(&mut a, KeyCode::Tab);
    key(&mut a, 'e');
    assert_eq!(a.editor_request.take().unwrap().side, Side::Left);
}

#[test]
fn editor_is_refused_on_readonly_sides_and_waits_for_saves() {
    let mut a = app((100, 20));
    press(&mut a, KeyCode::Tab);
    assert_eq!(a.focus, Side::Left);
    key(&mut a, 'e');
    assert!(a.message_is_error && a.editor_request.is_none());

    let (_d, mut a) = files_app("a\nB\n", "a\nb\n");
    key(&mut a, '>');
    key(&mut a, 'e');
    assert_eq!(a.modal, Some(Modal::SaveFirst { side: Side::Right }));
    assert!(a.editor_request.is_none());
    key(&mut a, 'w');
    assert!(a.modal.is_none() && !a.session.is_dirty());
    assert!(a.editor_request.is_some(), "after saving, the editor opens");
}

#[test]
fn after_the_editor_the_file_is_reloaded_as_one_undo_step() {
    let (dir, mut a) = files_app("a\nB\n", "a\nb\n");
    key(&mut a, 'e');
    let req = a.editor_request.take().unwrap();
    std::fs::write(&req.path, "a\nB\nextra\n").unwrap();
    a.finish_editor(req.side, Ok(true));
    assert_eq!(a.session.right.to_bytes(), b"a\nB\nextra\n");
    assert!(!a.session.is_dirty(), "the editor wrote it, so it is saved");
    assert_eq!(
        a.message.as_deref(),
        Some("reloaded right.rs after editing")
    );
    key(&mut a, 'u');
    assert_eq!(a.session.right.to_bytes(), b"a\nb\n");
    assert!(a.session.right.is_dirty());
    assert_eq!(read(&dir, "right.rs"), "a\nB\nextra\n");
    // An editor that changed nothing, and one that failed.
    let (_d, mut a) = files_app("x\n", "y\n");
    a.finish_editor(Side::Right, Ok(true));
    assert_eq!(a.message.as_deref(), Some("no changes"));
    a.finish_editor(Side::Right, Ok(false));
    assert!(a.message_is_error);
    a.finish_editor(Side::Right, Err("boom".into()));
    assert!(a.message.as_deref().unwrap().contains("boom"));
}

#[test]
fn help_lists_the_editing_keys() {
    let mut a = app((120, 24));
    press(&mut a, KeyCode::F(1));
    let s = text(&mut a);
    for needle in [
        "copy to right",
        "copy to left",
        "undo",
        "redo",
        "save",
        "$EDITOR",
    ] {
        assert!(s.contains(needle), "{needle}\n{s}");
    }
}

/// The colour of the first cell on the status bar showing `needle`.
fn status_fg(a: &mut App, needle: &str) -> Color {
    let (term, s) = render(a);
    let y = a.height - 1;
    let line = s.lines().nth(y as usize).unwrap();
    let col = line[..line
        .find(needle)
        .unwrap_or_else(|| panic!("{needle}: {line}"))]
        .chars()
        .count() as u16;
    term.backend().buffer()[(col, y)].fg
}

#[test]
fn status_hints_reflect_which_sides_are_writable() {
    // Actions that can't be taken are dimmed, not hidden, so the bar never jumps.
    let t = theme();
    let mut a = app((100, 20));
    assert_eq!(status_fg(&mut a, "> copy right"), t.amber);
    assert_eq!(status_fg(&mut a, "< copy left"), t.off);
    let (_d, mut a) = files_app("a\n", "b\n");
    assert_eq!(status_fg(&mut a, "> copy right"), t.amber);
    assert_eq!(status_fg(&mut a, "< copy left"), t.amber);
}

// ---- Folding, zones and narrow terminals ----

/// 60 lines with changes on lines 10 and 40 (counting from 1).
fn long_app(size: (u16, u16)) -> App {
    let left: String = (1..=60).map(|i| format!("line {i}\n")).collect();
    let right = left
        .replace("line 10\n", "LINE 10\n")
        .replace("line 40\n", "LINE 40\n");
    app_with(&left, &right, size, ViewConfig::default())
}

#[test]
fn z_folds_unchanged_runs_keeping_three_lines_around_each_hunk() {
    let mut a = long_app((100, 30));
    key(&mut a, 'z');
    assert!(a.fold);
    let s = text(&mut a);
    for needle in [
        "lines 1 to 6 unchanged",
        "lines 14 to 36 unchanged",
        "line 44 to end of file unchanged",
        "z to unfold",
        "unchanged folded",
    ] {
        assert!(s.contains(needle), "{needle}\n{s}");
    }
    // 1 fold + 3 + hunk + 3 + fold + 3 + hunk + 3 + fold.
    assert_eq!(a.rows.len(), 17);
    // Hunks still step, and the cursor stays on the hunk across the toggle.
    key(&mut a, 'J');
    assert_eq!(a.cursor_state(), "hunk 2 of 2");
    key(&mut a, 'z');
    assert!(!a.fold);
    assert_eq!(a.rows.len(), 60);
    assert_eq!(a.cursor_state(), "hunk 2 of 2");
    assert_eq!(a.cursor_line(), 40);
}

#[test]
fn search_unfolds_when_a_match_is_hidden() {
    let mut a = long_app((100, 30));
    key(&mut a, 'z');
    for c in "/line 25".chars() {
        key(&mut a, c);
    }
    press(&mut a, KeyCode::Enter);
    assert!(!a.fold, "the match was in folded lines");
    assert_eq!(a.cursor_line(), 25);
}

#[test]
fn the_status_bar_says_where_the_cursor_is_in_words() {
    let mut a = long_app((100, 30));
    assert!(text(&mut a).contains("hunk 1 of 2   line 10"));
    key(&mut a, 'G');
    let s = text(&mut a);
    assert!(s.contains("after the last hunk  K back to it"), "{s}");
    assert!(!s.contains("-/2"));
}

#[test]
fn the_overview_ruler_shows_hunks_and_the_screen_when_unfolded() {
    let mut a = long_app((100, 20));
    let s = text(&mut a);
    let ruler: String = s
        .lines()
        .skip(4)
        .take(a.content_height())
        .map(|l| l.chars().last().unwrap_or(' '))
        .collect();
    assert!(
        ruler.contains('▬') && ruler.contains('┃') && ruler.contains('│'),
        "{ruler}"
    );
    key(&mut a, 'z');
    assert!(!text(&mut a).contains('▬'), "no ruler while folded");
}

#[test]
fn the_title_bar_shows_what_differs_and_the_toolbar_how_it_is_shown() {
    let s = text(&mut app((140, 20)));
    let mut lines = s.lines();
    assert_eq!(lines.next().unwrap(), " a/src ↔ src   › auth/token.rs");
    let toolbar = lines.next().unwrap();
    assert!(
        toolbar.starts_with(
            " v view split     W whitespace off     z unchanged lines shown     / find"
        ),
        "{toolbar}"
    );
    assert!(toolbar.ends_with("4 hunks · 5 changed lines"), "{toolbar}");
    // 100 to 139 columns: short labels; under 100: key and value only.
    let s = text(&mut app((110, 20)));
    assert!(s.contains(" v view split   W ws off   z unchanged shown   / find"));
    let s = text(&mut app((80, 20)));
    assert!(s.contains(" v unified   W ws off   z all lines"), "{s}");
}

#[test]
fn status_hints_shrink_to_help_on_narrow_terminals() {
    let s = text(&mut app((58, 20)));
    let last = s.lines().rev().find(|l| !l.is_empty()).unwrap();
    assert!(
        last.ends_with("F1 help") && !last.contains("copy"),
        "{last}"
    );
}

#[test]
fn f_steps_ask_the_list_or_say_there_is_none() {
    let mut a = app((100, 20));
    key(&mut a, ']');
    key(&mut a, 'f');
    assert_eq!(a.outcome, None);
    assert_eq!(a.message.as_deref(), Some("only one file to compare"));
    a.in_directory = true;
    key(&mut a, ']');
    key(&mut a, 'f');
    assert_eq!(a.outcome, Some(Outcome::NextFile));
}
