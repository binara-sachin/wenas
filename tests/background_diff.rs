//! Diffs on a worker, with progress and cancel (spec §9, §10, D35).
//!
//! The diff function is replaced by one that waits for a gate, so every case is
//! deterministic: the diff is "slow" until the test opens the gate.

use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};
use wenas::diff::{Hunk, Whitespace, diff};
use wenas::document::Document;
use wenas::ops::{DiffPolicy, Dir, Session, Side};
use wenas::theme::{Depth, Theme};
use wenas::ui::term::Ui;
use wenas::ui::{App, Outcome, ViewConfig};

static GATE: AtomicBool = AtomicBool::new(false);
/// The gate is process-wide, so the tests that use it take turns.
static SERIAL: Mutex<()> = Mutex::new(());

fn gated(l: &Document, r: &Document, ws: Whitespace) -> Vec<Hunk> {
    while !GATE.load(Ordering::SeqCst) {
        std::thread::sleep(Duration::from_millis(1));
    }
    diff(l, r, ws)
}

fn slow() -> DiffPolicy {
    DiffPolicy {
        inline_lines: 0,
        fast_wait: Duration::ZERO,
        compute: gated,
    }
}

/// Holds the turn lock, starts with the gate closed, and opens it on exit so no
/// abandoned worker is left spinning.
struct Gate<'a>(#[allow(dead_code)] std::sync::MutexGuard<'a, ()>);

impl Gate<'_> {
    fn closed() -> Gate<'static> {
        let guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        GATE.store(false, Ordering::SeqCst);
        Gate(guard)
    }
    fn open(&self) {
        GATE.store(true, Ordering::SeqCst);
    }
}

impl Drop for Gate<'_> {
    fn drop(&mut self) {
        GATE.store(true, Ordering::SeqCst);
    }
}

fn theme() -> Theme {
    Theme::new(true, Depth::True, false)
}

fn doc(s: &str) -> Document {
    Document::from_bytes(s.as_bytes().to_vec())
}

fn cfg() -> ViewConfig {
    ViewConfig {
        unified_below_columns: 0,
        ..ViewConfig::default()
    }
}

fn key(a: &mut App, c: char) {
    a.on_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
}
fn esc(a: &mut App) {
    a.on_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
}

fn screen(a: &mut App) -> String {
    let mut term = Terminal::new(TestBackend::new(100, 12)).unwrap();
    term.draw(|f| a.draw(f)).unwrap();
    let buf = term.backend().buffer();
    (0..buf.area.height)
        .map(|y| {
            let l: String = (0..buf.area.width)
                .map(|x| buf[(x, y)].symbol().to_string())
                .collect();
            l.trim_end().to_string()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn wait_idle(a: &mut App) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while a.session.is_busy() {
        assert!(Instant::now() < deadline, "diff never finished");
        a.tick();
        std::thread::sleep(Duration::from_millis(2));
    }
}

/// A finished session with the slow policy installed for later diffs.
fn app_then_slow(left: &str, right: &str) -> App {
    let mut s = Session::new(doc(left), doc(right), Whitespace::Off);
    s.set_policy(slow());
    App::new(s, theme(), cfg(), (100, 12))
}

#[test]
fn the_first_diff_shows_progress_after_300ms_and_esc_leaves() {
    let gate = Gate::closed();
    let s = Session::start(doc("a\nb\n"), doc("a\nB\n"), Whitespace::Off, slow());
    assert!(s.is_busy());
    let mut a = App::new(s, theme(), cfg(), (100, 12));
    assert!(
        !screen(&mut a).contains("Comparing"),
        "no flash of progress for a quick diff"
    );
    std::thread::sleep(Duration::from_millis(350));
    let v = screen(&mut a);
    assert!(
        v.contains("Comparing") && v.contains("4 lines") && v.contains("Esc cancel"),
        "{v}"
    );
    key(&mut a, '>'); // ignored while comparing
    key(&mut a, 'q');
    assert_eq!(a.outcome, None);
    esc(&mut a);
    assert_eq!(a.outcome, Some(Outcome::Quit));
    assert!(a.diff_cancelled);
    gate.open();
}

#[test]
fn cancelling_the_first_diff_from_a_directory_goes_back_to_the_list() {
    let gate = Gate::closed();
    let s = Session::start(doc("a\n"), doc("b\n"), Whitespace::Off, slow());
    let mut a = App::new(s, theme(), cfg(), (100, 12));
    a.in_directory = true;
    esc(&mut a);
    assert_eq!(a.outcome, Some(Outcome::Back));
    gate.open();
}

#[test]
fn the_first_diff_becomes_the_normal_view_when_it_arrives() {
    let gate = Gate::closed();
    let s = Session::start(doc("a\nb\n"), doc("a\nB\n"), Whitespace::Off, slow());
    let mut a = App::new(s, theme(), cfg(), (100, 12));
    gate.open();
    wait_idle(&mut a);
    let v = screen(&mut a);
    assert!(v.contains("1/1") && v.contains("copy to right"), "{v}");
    assert_eq!(a.outcome, None);
}

#[test]
fn a_first_diff_that_finds_nothing_reports_identical() {
    let gate = Gate::closed();
    let s = Session::start(doc("a\r\n"), doc("a\n"), Whitespace::Off, slow());
    let mut a = App::new(s, theme(), cfg(), (100, 12));
    gate.open();
    wait_idle(&mut a);
    assert_eq!(a.outcome, Some(Outcome::Identical));
}

#[test]
fn a_slow_re_diff_after_a_copy_blocks_edits_then_completes() {
    let gate = Gate::closed();
    let mut a = app_then_slow("a\nB\nc\n", "a\nb\nc\n");
    key(&mut a, '>');
    assert!(a.session.is_busy());
    key(&mut a, '>');
    key(&mut a, 'u');
    key(&mut a, 'W');
    assert_eq!(
        a.session.edit_count(Side::Right),
        1,
        "keys are ignored while comparing"
    );
    gate.open();
    wait_idle(&mut a);
    assert!(a.session.hunks.is_empty());
    assert_eq!(a.message.as_deref(), Some("hunk 1 copied to right"));
    assert_eq!(a.session.right.to_bytes(), b"a\nB\nc\n");
    assert!(screen(&mut a).contains("-/0"));
}

#[test]
fn esc_during_a_copy_re_diff_undoes_the_copy() {
    let gate = Gate::closed();
    let mut a = app_then_slow("a\nB\nc\n", "a\nb\nc\n");
    let hunks_before = a.session.hunks.clone();
    key(&mut a, '>');
    assert!(a.session.is_busy());
    esc(&mut a);
    assert!(!a.session.is_busy());
    assert_eq!(
        a.session.right.to_bytes(),
        b"a\nb\nc\n",
        "the copy was reverted"
    );
    assert!(!a.session.is_dirty() && !a.session.can_undo() && !a.session.can_redo());
    assert_eq!(
        a.session.hunks, hunks_before,
        "hunks match the documents again"
    );
    assert!(a.message.as_deref().unwrap().contains("cancelled"));
    gate.open();
    // Everything works afterwards.
    a.session.set_policy(DiffPolicy::default());
    key(&mut a, '>');
    assert_eq!(a.session.right.to_bytes(), b"a\nB\nc\n");
}

#[test]
fn esc_during_an_undo_re_diff_puts_the_edit_back() {
    let gate = Gate::closed();
    let mut s = Session::new(doc("a\nB\n"), doc("a\nb\n"), Whitespace::Off);
    s.copy_hunk(0, Dir::LeftToRight).unwrap();
    s.set_policy(slow());
    let mut a = App::new(s, theme(), cfg(), (100, 12));
    key(&mut a, 'u');
    assert!(a.session.is_busy());
    esc(&mut a);
    assert_eq!(a.session.right.to_bytes(), b"a\nB\n", "still copied");
    assert!(a.session.right.is_dirty() && a.session.can_undo() && !a.session.can_redo());
    gate.open();
    a.session.set_policy(DiffPolicy::default());
    key(&mut a, 'u');
    assert_eq!(
        a.session.right.to_bytes(),
        b"a\nb\n",
        "and undoable as before"
    );
}

#[test]
fn esc_during_a_redo_re_diff_keeps_it_undone() {
    let gate = Gate::closed();
    let mut s = Session::new(doc("a\nB\n"), doc("a\nb\n"), Whitespace::Off);
    s.copy_hunk(0, Dir::LeftToRight).unwrap();
    s.undo();
    s.set_policy(slow());
    let mut a = App::new(s, theme(), cfg(), (100, 12));
    a.on_key(KeyEvent::new(KeyCode::Char('r'), KeyModifiers::CONTROL));
    assert!(a.session.is_busy());
    esc(&mut a);
    assert_eq!(a.session.right.to_bytes(), b"a\nb\n");
    assert!(a.session.can_redo() && !a.session.can_undo());
    gate.open();
}

#[test]
fn esc_during_a_whitespace_re_diff_restores_the_mode() {
    let gate = Gate::closed();
    let mut a = app_then_slow("a  b\n", "a b\n");
    assert_eq!(a.session.hunks.len(), 1);
    key(&mut a, 'W');
    assert!(a.session.is_busy());
    esc(&mut a);
    assert_eq!(a.session.ws, Whitespace::Off);
    assert_eq!(a.session.hunks.len(), 1, "the hunk is back");
    gate.open();
}

#[test]
fn cancelling_an_editor_reload_restores_content_history_and_dirty_state() {
    use wenas::document::DiskId;
    let gate = Gate::closed();
    let mut s = Session::new(doc("1\nX\n2\n3\n"), doc("1\nx\n2\ny\n"), Whitespace::Off);
    s.copy_hunk(0, Dir::LeftToRight).unwrap();
    s.copy_hunk(0, Dir::LeftToRight).unwrap();
    s.undo();
    let before = s.right.to_bytes();
    let (dirty, redo) = (s.right.is_dirty(), s.can_redo());
    s.set_policy(slow());
    assert!(s.replace_content(
        Side::Right,
        b"from disk\n",
        DiskId::of_bytes(b"from disk\n", None)
    ));
    assert!(s.is_busy());
    assert!(!s.can_redo(), "a new edit clears redo while it is applied");
    assert_eq!(s.cancel(), Some(wenas::ops::Cancelled::Reverted));
    assert_eq!(s.right.to_bytes(), before);
    assert_eq!((s.right.is_dirty(), s.can_redo()), (dirty, redo));
    gate.open();
}

#[test]
fn ctrl_c_still_aborts_while_comparing() {
    let gate = Gate::closed();
    let mut a = app_then_slow("a\n", "b\n");
    key(&mut a, '>');
    assert!(a.session.is_busy());
    a.on_key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL));
    assert_eq!(a.outcome, Some(Outcome::Abort));
    gate.open();
}

#[test]
fn the_event_loop_keeps_polling_while_a_diff_runs() {
    let gate = Gate::closed();
    let mut a = app_then_slow("a\nB\n", "a\nb\n");
    assert!(!a.polling());
    key(&mut a, '>');
    assert!(a.polling(), "the loop must wake up to notice the result");
    gate.open();
    wait_idle(&mut a);
    assert!(!a.polling());
}

// ---- from the directory list ----

mod directory {
    use super::*;
    use wenas::dircmp::Scan;
    use wenas::ui::dir::{DirView, Filters};
    use wenas::ui::shell::{Shell, ShellOptions};

    fn shell_in(dir: &tempfile::TempDir) -> Shell {
        let (l, r) = (dir.path().join("l"), dir.path().join("r"));
        for (root, body) in [(&l, "a\nb\n"), (&r, "a\nB\n")] {
            std::fs::create_dir_all(root).unwrap();
            std::fs::write(root.join("f.rs"), body).unwrap();
        }
        let scan = Scan::spawn(l.clone(), r.clone(), vec![]);
        let view = DirView::new(l, r, Filters::default(), scan, vec![]);
        let opts = ShellOptions {
            theme: theme(),
            view: cfg(),
            ws: Whitespace::Off,
            readonly_left: false,
            readonly_right: false,
            large_file_bytes: u64::MAX,
            diff: slow(),
        };
        let mut s = Shell::new(view, opts, (100, 12));
        let deadline = Instant::now() + Duration::from_secs(5);
        while s.dir.scanning {
            assert!(Instant::now() < deadline);
            s.tick();
            std::thread::sleep(Duration::from_millis(2));
        }
        s
    }

    fn press(s: &mut Shell, code: KeyCode) {
        s.on_key(KeyEvent::new(code, KeyModifiers::NONE));
    }

    #[test]
    fn opening_a_slow_file_shows_progress_and_esc_returns_to_the_list() {
        let gate = Gate::closed();
        let tmp = tempfile::tempdir().unwrap();
        let mut s = shell_in(&tmp);
        press(&mut s, KeyCode::Enter);
        assert!(
            s.current.is_some() && s.polling(),
            "busy, so the loop polls"
        );
        press(&mut s, KeyCode::Esc);
        assert!(s.current.is_none(), "back at the list");
        assert!(
            s.open.is_empty(),
            "a cancelled first diff leaves no half-built session"
        );
        assert_eq!(s.outcome(), None);
        // Opening it again starts over and works once the diff can finish.
        press(&mut s, KeyCode::Enter);
        gate.open();
        let deadline = Instant::now() + Duration::from_secs(5);
        while s.polling() {
            assert!(Instant::now() < deadline);
            s.tick();
            std::thread::sleep(Duration::from_millis(2));
        }
        let app = &s.open[&s.current.unwrap()];
        assert_eq!(app.session.hunks.len(), 1);
    }
}

// ---- command line, no terminal ----

#[test]
fn large_files_without_a_terminal_wait_for_the_diff() {
    use assert_cmd::Command;
    let d = tempfile::tempdir().unwrap();
    let (a, b) = (d.path().join("a"), d.path().join("b"));
    // More lines than the inline threshold, so this takes the worker path.
    let body: String = (0..8_000).map(|i| format!("line {i}\n")).collect();
    std::fs::write(&a, &body).unwrap();
    std::fs::write(&b, body.replace('\n', "\r\n")).unwrap();
    Command::cargo_bin("wenas")
        .unwrap()
        .arg(&a)
        .arg(&b)
        .assert()
        .success()
        .stdout("identical\n");
    std::fs::write(&b, body.replace("line 4000", "changed")).unwrap();
    Command::cargo_bin("wenas")
        .unwrap()
        .arg(&a)
        .arg(&b)
        .assert()
        .code(2)
        .stderr(predicates::str::contains("git diff --no-index"));
}
