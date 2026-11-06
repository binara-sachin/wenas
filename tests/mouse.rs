//! Mouse and wheel input (spec §7.6), driven the way the event loop drives it.

use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::crossterm::event::{
    KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use wenas::diff::Whitespace;
use wenas::dircmp::Scan;
use wenas::document::{Document, ReadOnlyReason};
use wenas::ops::{Dir, Session, Side};
use wenas::theme::{Depth, Theme};
use wenas::ui::app::Modal;
use wenas::ui::dir::{DOUBLE_CLICK, DirAction, DirView, Filters};
use wenas::ui::render::{Hit, draw, hit_test};
use wenas::ui::shell::{Shell, ShellOptions};
use wenas::ui::term::Ui;
use wenas::ui::{App, BinaryInfo, View, ViewConfig};

const SIZE: (u16, u16) = (100, 20);
/// The screen row of the first content row.
/// The first content row: under the title bar, toolbar, a blank row and
/// the pane headers.
const TOP: u16 = 4;
/// The directory list's first row (it has no pane headers).
const LIST_TOP: u16 = 3;

fn theme() -> Theme {
    Theme::new(true, Depth::True, false)
}

/// 100 lines, with changed lines at 10, 50 and 90.
fn long_sides() -> (String, String) {
    let line = |i: usize, changed: bool| {
        if changed {
            format!("changed {i}\n")
        } else {
            format!("line {i}\n")
        }
    };
    let left: String = (0..100).map(|i| line(i, false)).collect();
    let right: String = (0..100)
        .map(|i| line(i, matches!(i, 10 | 50 | 90)))
        .collect();
    (left, right)
}

fn app_from(left: &str, right: &str, readonly_left: bool) -> App {
    let mut l = Document::from_bytes(left.into());
    let r = Document::from_bytes(right.into());
    if readonly_left {
        l.make_readonly(ReadOnlyReason::GitOldVersion);
    }
    App::new(
        Session::new(l, r, Whitespace::Off),
        theme(),
        ViewConfig::default(),
        SIZE,
    )
}

fn app() -> App {
    let (l, r) = long_sides();
    app_from(&l, &r, false)
}

fn event(kind: MouseEventKind, x: u16, y: u16) -> MouseEvent {
    MouseEvent {
        kind,
        column: x,
        row: y,
        modifiers: KeyModifiers::NONE,
    }
}

fn down(x: u16, y: u16) -> MouseEvent {
    event(MouseEventKind::Down(MouseButton::Left), x, y)
}

fn up(x: u16, y: u16) -> MouseEvent {
    event(MouseEventKind::Up(MouseButton::Left), x, y)
}

fn click(a: &mut App, x: u16, y: u16) {
    a.on_mouse(down(x, y));
    a.on_mouse(up(x, y));
}

fn key(a: &mut App, c: char) {
    a.on_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
}

/// Every position where the screen shows `symbol`.
fn find_on_screen(a: &mut App, symbol: &str) -> Vec<(u16, u16)> {
    let mut term = Terminal::new(TestBackend::new(a.width, a.height)).unwrap();
    term.draw(|f| draw(f, a)).unwrap();
    let buf = term.backend().buffer().clone();
    let mut out = Vec::new();
    for y in 0..buf.area.height {
        for x in 0..buf.area.width {
            if buf[(x, y)].symbol() == symbol {
                out.push((x, y));
            }
        }
    }
    out
}

fn right_text(a: &App) -> String {
    String::from_utf8(a.session.right.to_bytes()).unwrap()
}

// ---- wheel ----

#[test]
fn wheel_moves_the_view_and_leaves_the_cursor_alone_while_it_is_visible() {
    let mut a = app();
    a.top = 0;
    a.cursor = 8;
    assert!(a.on_scroll(3, 0));
    assert_eq!((a.top, a.cursor), (3, 8));
    assert!(a.on_scroll(-2, 0));
    assert_eq!((a.top, a.cursor), (1, 8));
}

#[test]
fn wheel_keeps_the_cursor_on_screen_so_a_copy_never_hits_an_unseen_hunk() {
    let mut a = app();
    let h = a.content_height();
    a.top = 0;
    a.cursor = 0;
    a.on_scroll(40, 0);
    assert_eq!((a.top, a.cursor), (40, 40));
    a.cursor = 40 + h - 1;
    a.on_scroll(-20, 0);
    assert_eq!(a.top, 20);
    assert_eq!(a.cursor, 20 + h - 1);
}

#[test]
fn wheel_stops_at_both_ends_and_reports_no_change_there() {
    let mut a = app();
    let h = a.content_height();
    a.top = 0;
    a.cursor = 0;
    assert!(!a.on_scroll(-5, 0), "already at the top");
    a.on_scroll(1000, 0);
    assert_eq!(a.top, 100 - h);
    assert!(!a.on_scroll(1, 0), "already at the bottom");
    assert_eq!(a.top, 100 - h);
}

#[test]
fn wheel_works_when_everything_fits_on_screen() {
    let mut a = app_from("a\nb\n", "a\nc\n", false);
    assert!(!a.on_scroll(5, 0));
    assert_eq!(a.top, 0);
}

#[test]
fn sideways_wheel_moves_by_what_it_is_given_and_stops_at_the_longest_line() {
    let long = format!("{}\nsame\n", "x".repeat(500));
    let mut a = app_from(&long, &format!("{}\nother\n", "x".repeat(500)), false);
    assert!(a.on_scroll(0, 2));
    assert_eq!(a.hscroll, 2);
    a.on_scroll(0, 100_000);
    let limit = a.hscroll;
    assert!(limit < 500, "stops once the line end reaches the pane edge");
    assert!(!a.on_scroll(0, 2));
    assert!(a.on_scroll(0, -2));
    assert_eq!(a.hscroll, limit - 2);
    a.on_scroll(0, -100_000);
    assert_eq!(a.hscroll, 0);
}

// ---- clicks ----

#[test]
fn click_moves_the_cursor_to_the_row_on_screen_and_focuses_that_pane() {
    let mut a = app();
    a.top = 30;
    a.cursor = 30;
    assert_eq!(a.focus, Side::Right);
    assert!(a.on_mouse(down(5, TOP + 4)));
    assert_eq!(a.cursor, 34);
    assert_eq!(a.focus, Side::Left);
    assert!(a.on_mouse(down(90, TOP + 2)));
    assert_eq!(a.cursor, 32);
    assert_eq!(a.focus, Side::Right);
}

#[test]
fn clicks_outside_the_rows_do_nothing() {
    let mut a = app_from("a\nb\n", "a\nc\n", false);
    let before = a.cursor;
    // The title bar (no back control here), the blank row, the pane headers,
    // below the last line and the status bar.
    for y in [0, 2, 3, TOP + 5, SIZE.1 - 1] {
        assert!(!a.on_mouse(down(5, y)), "row {y}");
    }
    assert_eq!(a.cursor, before);
}

#[test]
fn only_a_plain_left_click_counts() {
    let mut a = app();
    a.top = 0;
    a.cursor = 0;
    let right = event(MouseEventKind::Down(MouseButton::Right), 5, TOP + 3);
    assert!(!a.on_mouse(right));
    let mut with_ctrl = down(5, TOP + 3);
    with_ctrl.modifiers = KeyModifiers::CONTROL;
    assert!(!a.on_mouse(with_ctrl));
    assert!(!a.on_mouse(event(MouseEventKind::Moved, 5, TOP + 3)));
    assert_eq!(a.cursor, 0);
}

#[test]
fn a_click_ends_a_half_typed_key_sequence() {
    let mut a = app();
    a.top = 40;
    a.cursor = 45;
    key(&mut a, 'g');
    a.on_mouse(down(5, TOP + 1));
    key(&mut a, 'g');
    assert_eq!(a.cursor, 41, "the g before the click must not combine");
}

#[test]
fn a_click_clears_the_status_message() {
    let mut a = app();
    a.message = Some("saved".into());
    a.on_mouse(down(5, TOP + 1));
    assert_eq!(a.message, None);
}

// ---- copy arrows ----

#[test]
fn hit_testing_finds_the_arrows_where_they_are_drawn_in_both_views() {
    for unified in [false, true] {
        let mut a = app();
        if unified {
            key(&mut a, 'v');
            assert_eq!(a.view, View::Unified);
        }
        let (left, right) = (find_on_screen(&mut a, "←"), find_on_screen(&mut a, "→"));
        assert!(!left.is_empty() && !right.is_empty(), "unified: {unified}");
        for (x, y) in left {
            assert_eq!(
                hit_test(&a, x, y),
                Some(Hit::Arrow(Dir::RightToLeft)),
                "unified: {unified}"
            );
        }
        for (x, y) in right {
            assert_eq!(
                hit_test(&a, x, y),
                Some(Hit::Arrow(Dir::LeftToRight)),
                "unified: {unified}"
            );
        }
    }
}

#[test]
fn a_read_only_side_has_no_arrow_to_click() {
    let (l, r) = long_sides();
    let mut a = app_from(&l, &r, true);
    assert!(find_on_screen(&mut a, "←").is_empty());
    let (x, y) = find_on_screen(&mut a, "→")[0];
    assert!(matches!(hit_test(&a, x - 2, y), Some(Hit::Row { .. })));
}

#[test]
fn clicking_an_arrow_copies_the_hunk_and_undo_takes_it_back() {
    let mut a = app();
    let before = right_text(&a);
    let (x, y) = find_on_screen(&mut a, "→")[0];
    click(&mut a, x, y);
    assert_eq!(a.session.hunks.len(), 2);
    assert_ne!(right_text(&a), before);
    assert!(a.session.is_dirty(), "copied but not saved");
    key(&mut a, 'u');
    assert_eq!(right_text(&a), before);
}

#[test]
fn nothing_is_copied_until_the_release() {
    let mut a = app();
    let (x, y) = find_on_screen(&mut a, "→")[0];
    a.on_mouse(down(x, y));
    assert_eq!(a.session.hunks.len(), 3);
    a.on_mouse(up(x, y));
    assert_eq!(a.session.hunks.len(), 2);
}

#[test]
fn dragging_off_the_arrow_abandons_the_copy() {
    let mut a = app();
    let (x, y) = find_on_screen(&mut a, "→")[0];
    a.on_mouse(down(x, y));
    a.on_mouse(event(MouseEventKind::Drag(MouseButton::Left), x + 5, y));
    a.on_mouse(up(x + 5, y));
    assert_eq!(a.session.hunks.len(), 3);
    assert!(!a.session.is_dirty());
}

#[test]
fn a_wobble_inside_the_arrow_still_copies() {
    let mut a = app();
    let (x, y) = find_on_screen(&mut a, "→")[0];
    a.on_mouse(down(x, y));
    a.on_mouse(event(MouseEventKind::Drag(MouseButton::Left), x, y));
    a.on_mouse(up(x, y));
    assert_eq!(a.session.hunks.len(), 2);
}

#[test]
fn the_click_that_makes_a_hunk_current_does_not_also_copy_it() {
    // Show two hunks at once, the second not current.
    let (l, r) = long_sides();
    let r = r.replace("line 13\n", "changed 13\n");
    let mut a = app_from(&l, &r, false);
    a.top = 8;
    a.cursor = 8;
    let (x, _) = find_on_screen(&mut a, "→")[0];
    let second = TOP + 13 - 8;
    assert!(matches!(hit_test(&a, x, second), Some(Hit::Row { .. })));
    let hunks = a.session.hunks.len();
    click(&mut a, x, second);
    assert_eq!(a.session.hunks.len(), hunks, "first click only selects");
    assert_eq!(a.cursor, 13);
    assert!(!a.session.is_dirty());
    assert_eq!(hit_test(&a, x, second), Some(Hit::Arrow(Dir::LeftToRight)));
    click(&mut a, x, second);
    assert_eq!(a.session.hunks.len(), hunks - 1, "second click copies");
}

// ---- screens that stay keyboard-only ----

#[test]
fn questions_help_and_prompts_ignore_the_mouse() {
    type Setup = fn(&mut App);
    let setups: [(&str, Setup); 3] = [
        ("modal", |a| a.modal = Some(Modal::QuitUnsaved)),
        ("help", |a| a.help = true),
        ("prompt", |a| key(a, '/')),
    ];
    for (name, setup) in setups {
        let mut a = app();
        let (x, y) = find_on_screen(&mut a, "→")[0];
        setup(&mut a);
        let (top, cursor) = (a.top, a.cursor);
        assert!(!a.on_scroll(5, 5), "{name}");
        click(&mut a, x, y);
        click(&mut a, 5, TOP + 1);
        assert_eq!((a.top, a.cursor, a.hscroll), (top, cursor, 0), "{name}");
        assert!(!a.session.is_dirty(), "{name}");
    }
}

#[test]
fn the_binary_screen_ignores_the_mouse() {
    let info = BinaryInfo::from_bytes("x".into(), &[0, 1], &[0, 2]);
    let mut a = App::binary_screen(info, theme(), SIZE);
    assert!(!a.on_scroll(1, 1));
    assert!(!a.on_mouse(down(5, TOP)));
    assert_eq!(a.outcome, None);
}

#[test]
fn a_terminal_too_small_to_draw_has_nothing_to_click() {
    let mut a = app_from("a\nb\n", "a\nc\n", false);
    a.sync_size(20, 4);
    assert_eq!(hit_test(&a, 5, TOP), None);
}

// ---- directory list ----

struct Tree {
    _tmp: tempfile::TempDir,
    left: PathBuf,
    right: PathBuf,
}

fn tree(files: usize) -> Tree {
    let tmp = tempfile::tempdir().unwrap();
    let (left, right) = (tmp.path().join("left"), tmp.path().join("right"));
    std::fs::create_dir_all(&left).unwrap();
    std::fs::create_dir_all(&right).unwrap();
    for i in 0..files {
        let name = format!("f{i:03}.txt");
        std::fs::write(left.join(&name), "a\nb\n").unwrap();
        std::fs::write(right.join(&name), "a\nc\n").unwrap();
    }
    Tree {
        _tmp: tmp,
        left,
        right,
    }
}

fn shell(t: &Tree) -> Shell {
    let scan = Scan::spawn(t.left.clone(), t.right.clone(), vec![]);
    let dir = DirView::new(
        t.left.clone(),
        t.right.clone(),
        Filters::default(),
        scan,
        vec![],
    );
    let opts = ShellOptions {
        theme: theme(),
        view: ViewConfig::default(),
        ws: Whitespace::Off,
        readonly_left: false,
        readonly_right: false,
        large_file_bytes: wenas::input::LARGE_FILE_BYTES,
        diff: wenas::ops::DiffPolicy::default(),
    };
    let mut s = Shell::new(dir, opts, SIZE);
    let deadline = Instant::now() + Duration::from_secs(5);
    while s.dir.scanning {
        assert!(Instant::now() < deadline, "scan did not finish");
        s.tick();
        std::thread::sleep(Duration::from_millis(2));
    }
    s.dir.height = SIZE.1;
    s.dir.width = SIZE.0;
    s
}

fn draw_shell(s: &mut Shell) {
    let mut term = Terminal::new(TestBackend::new(SIZE.0, SIZE.1)).unwrap();
    term.draw(|f| s.draw(f)).unwrap();
}

fn rel(s: &Shell, pos: usize) -> &Path {
    &s.dir.entries[s.dir.entry_at(pos).unwrap()].rel
}

#[test]
fn the_list_scrolls_and_stays_scrolled_after_drawing() {
    let t = tree(60);
    let mut s = shell(&t);
    assert!(s.on_scroll(10, 0));
    draw_shell(&mut s);
    assert_eq!((s.dir.top, s.dir.cursor), (10, 10));
    assert!(s.on_scroll(-4, 0));
    draw_shell(&mut s);
    assert_eq!(s.dir.top, 6);
    assert!(s.dir.cursor >= 6);
    s.on_scroll(10_000, 0);
    assert_eq!(s.dir.top, 60 - (SIZE.1 as usize - 4));
    assert!(!s.on_scroll(1, 0), "already at the end");
}

#[test]
fn an_empty_list_ignores_the_wheel() {
    let t = tree(0);
    let mut s = shell(&t);
    assert!(!s.on_scroll(3, 0));
    assert!(!s.on_mouse(down(5, LIST_TOP)));
}

#[test]
fn a_click_selects_the_row_on_screen() {
    let t = tree(40);
    let mut s = shell(&t);
    s.on_scroll(5, 0);
    assert!(s.on_mouse(down(5, LIST_TOP + 2)));
    assert_eq!(s.dir.cursor, 7);
    assert_eq!(rel(&s, s.dir.cursor), Path::new("f007.txt"));
    assert_eq!(s.current, None, "one click doesn't open anything");
}

#[test]
fn a_double_click_opens_and_goes_to_the_file_view() {
    let t = tree(3);
    let mut s = shell(&t);
    s.on_mouse(down(5, LIST_TOP + 1));
    s.on_mouse(up(5, LIST_TOP + 1));
    s.on_mouse(down(5, LIST_TOP + 1));
    let opened = s.current.expect("the second click opens the entry");
    assert_eq!(s.dir.entries[opened].rel, Path::new("f001.txt"));
    // The release of that click lands on the file view and must do nothing.
    assert!(!s.on_mouse(up(5, LIST_TOP + 1)));
    assert!(!s.open[&opened].session.is_dirty());
}

#[test]
fn double_click_timing_and_target_matter() {
    let t = tree(3);
    let mut s = shell(&t);
    let t0 = Instant::now();
    let d = &mut s.dir;
    let slow = t0 + DOUBLE_CLICK + Duration::from_millis(1);
    assert_eq!(d.on_mouse(down(5, LIST_TOP), t0), Some(DirAction::None));
    assert_eq!(
        d.on_mouse(down(5, LIST_TOP), slow),
        Some(DirAction::None),
        "too slow"
    );
    let other = slow + Duration::from_millis(10);
    assert_eq!(
        d.on_mouse(down(5, LIST_TOP + 1), other),
        Some(DirAction::None),
        "a different entry"
    );
    let again = other + Duration::from_millis(10);
    assert!(matches!(
        d.on_mouse(down(5, LIST_TOP + 1), again),
        Some(DirAction::Open(_))
    ));
    let third = again + Duration::from_millis(10);
    assert_eq!(
        d.on_mouse(down(5, LIST_TOP + 1), third),
        Some(DirAction::None),
        "a third click starts over"
    );
}

#[test]
fn clicks_below_the_list_or_on_the_header_do_nothing() {
    let t = tree(2);
    let mut s = shell(&t);
    for y in [0, 1, LIST_TOP + 2, SIZE.1 - 1] {
        assert!(!s.on_mouse(down(5, y)), "row {y}");
    }
    assert_eq!(s.dir.cursor, 0);
}

#[test]
fn the_shell_passes_the_mouse_to_an_open_file_and_back() {
    let t = tree(2);
    let mut s = shell(&t);
    s.on_mouse(down(5, LIST_TOP));
    s.on_mouse(down(5, LIST_TOP));
    let idx = s.current.unwrap();
    draw_shell(&mut s);
    let before = s.dir.cursor;
    assert!(s.on_mouse(down(5, TOP)));
    assert_eq!(s.open[&idx].cursor, 0);
    assert_eq!(s.dir.cursor, before);
    s.on_key(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE));
    assert_eq!(s.current, None);
    assert!(s.on_mouse(down(5, LIST_TOP + 1)));
    assert_eq!(s.dir.cursor, 1);
}

#[test]
fn the_quit_question_stays_keyboard_only() {
    let t = tree(5);
    let mut s = shell(&t);
    s.quit_modal = true;
    assert!(!s.on_scroll(1, 0));
    assert!(!s.on_mouse(down(5, LIST_TOP + 1)));
    assert_eq!(s.dir.cursor, 0);
    assert!(s.quit_modal);
    assert_eq!(s.outcome, None);
}

#[test]
fn help_and_the_search_prompt_ignore_the_mouse_in_the_list() {
    let t = tree(40);
    let mut s = shell(&t);
    s.on_key(KeyEvent::new(KeyCode::F(1), KeyModifiers::NONE));
    assert!(!s.on_scroll(3, 0));
    assert!(!s.on_mouse(down(5, LIST_TOP + 1)));
    s.on_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
    s.on_key(KeyEvent::new(KeyCode::Char('/'), KeyModifiers::NONE));
    assert!(!s.on_scroll(3, 0));
    assert!(!s.on_mouse(down(5, LIST_TOP + 1)));
    assert_eq!((s.dir.top, s.dir.cursor), (0, 0));
}

#[test]
fn a_double_click_on_a_folder_folds_it_and_another_unfolds_it() {
    let t = tree(0);
    for i in 0..30 {
        let name = format!("deep/f{i:02}.txt");
        std::fs::create_dir_all(t.left.join("deep")).unwrap();
        std::fs::create_dir_all(t.right.join("deep")).unwrap();
        std::fs::write(t.left.join(&name), "a\n").unwrap();
        std::fs::write(t.right.join(&name), "b\n").unwrap();
    }
    let mut s = shell(&t);
    let all = s.dir.visible.len();
    assert!(s.on_mouse(down(5, LIST_TOP)));
    assert_eq!(s.dir.visible.len(), all, "one click only selects");
    s.on_mouse(up(5, LIST_TOP));
    s.on_mouse(down(5, LIST_TOP));
    assert_eq!(s.dir.visible.len(), 1, "folded");
    assert_eq!(s.current, None, "a folder never opens");
    s.on_mouse(up(5, LIST_TOP));
    s.on_mouse(down(5, LIST_TOP));
    s.on_mouse(up(5, LIST_TOP));
    s.on_mouse(down(5, LIST_TOP));
    assert_eq!(s.dir.visible.len(), all, "unfolded");
}

#[test]
fn folding_near_the_end_scrolls_back_to_fill_the_screen() {
    let t = tree(0);
    for i in 0..40 {
        let name = format!("deep/f{i:02}.txt");
        std::fs::create_dir_all(t.left.join("deep")).unwrap();
        std::fs::create_dir_all(t.right.join("deep")).unwrap();
        std::fs::write(t.left.join(&name), "a\n").unwrap();
        std::fs::write(t.right.join(&name), "b\n").unwrap();
    }
    let mut s = shell(&t);
    let key = |s: &mut Shell, c| s.on_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
    key(&mut s, 'G');
    draw_shell(&mut s);
    assert!(s.dir.top > 0);
    key(&mut s, 'h');
    key(&mut s, 'h');
    draw_shell(&mut s);
    assert_eq!((s.dir.visible.len(), s.dir.top, s.dir.cursor), (1, 0, 0));
}

#[test]
fn a_click_on_a_filter_chip_flips_it() {
    let t = tree(0);
    let mut s = shell(&t);
    draw_shell(&mut s);
    let bar = 1;
    // " show  i ○ identical   m ● one side   b ● binary   . ● hidden"
    assert!(s.on_mouse(down(10, bar)));
    assert!(s.dir.filters.identical);
    assert!(s.on_mouse(down(30, bar)));
    assert!(!s.dir.filters.missing);
    assert!(
        !s.on_mouse(down(21, bar)),
        "the gap between chips does nothing"
    );
    assert!(!s.on_mouse(down(3, bar)), "nor does the label");
    s.on_key(KeyEvent::new(KeyCode::F(1), KeyModifiers::NONE));
    assert!(!s.on_mouse(down(10, bar)), "help ignores the bar");
    assert!(s.dir.filters.identical);
}

#[test]
fn the_back_control_and_toolbar_respond_to_clicks() {
    let t = tree(2);
    let mut s = shell(&t);
    s.on_mouse(down(5, LIST_TOP));
    s.on_mouse(down(5, LIST_TOP));
    let idx = s.current.unwrap();
    draw_shell(&mut s);
    // The toolbar's `v` control switches the view, like the key.
    assert_eq!(s.open[&idx].view, View::SideBySide);
    assert!(s.on_mouse(down(3, 1)));
    assert_eq!(s.open[&idx].view, View::Unified);
    // " ‹ files " goes back to the list.
    assert!(s.on_mouse(down(3, 0)));
    assert_eq!(s.current, None);
}
