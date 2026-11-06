//! Bracketed paste (spec §7.7): pasted text never runs as keys.

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use std::time::{Duration, Instant};
use wenas::diff::Whitespace;
use wenas::dircmp::Scan;
use wenas::document::Document;
use wenas::ops::Session;
use wenas::theme::{Depth, Theme};
use wenas::ui::app::Modal;
use wenas::ui::dir::DirView;
use wenas::ui::shell::{Shell, ShellOptions};
use wenas::ui::term::Ui;
use wenas::ui::{App, Outcome, ViewConfig};

const SIZE: (u16, u16) = (100, 20);

fn theme() -> Theme {
    Theme::new(true, Depth::True, false)
}

fn app() -> App {
    let left: String = (0..30).map(|i| format!("line {i}\n")).collect();
    let right = left.replace("line 5\n", "changed 5\n");
    App::new(
        Session::new(
            Document::from_bytes(left.into()),
            Document::from_bytes(right.into()),
            Whitespace::Off,
        ),
        theme(),
        ViewConfig::default(),
        SIZE,
    )
}

fn key(a: &mut App, code: KeyCode) {
    a.on_key(KeyEvent::new(code, KeyModifiers::NONE));
}

#[test]
fn pasted_commands_do_nothing_in_the_file_view() {
    let mut a = app();
    let before = (a.cursor, a.top);
    // Every one of these would copy, save or quit if it were typed.
    assert!(a.on_paste(">w<QQ:wq\n"));
    assert_eq!(a.outcome, None);
    assert!(!a.session.is_dirty());
    assert_eq!((a.cursor, a.top), before);
    assert!(a.message.as_deref().unwrap().contains("paste ignored"));
    assert!(a.message_is_error);
}

#[test]
fn a_paste_ends_a_half_typed_key_sequence() {
    let mut a = app();
    key(&mut a, KeyCode::Char('G'));
    let bottom = a.cursor;
    key(&mut a, KeyCode::Char('g'));
    a.on_paste("x");
    key(&mut a, KeyCode::Char('g'));
    assert_eq!(a.cursor, bottom, "g, paste, g is not gg");
}

#[test]
fn the_search_prompt_takes_the_first_line_of_a_paste() {
    let mut a = app();
    key(&mut a, KeyCode::Char('/'));
    assert!(a.on_paste("chan\tged 5\r\nsecond line\x1b[31m"));
    assert_eq!(a.prompt.as_ref().unwrap().text, "chan ged 5");
    // It adds to what is typed, and Enter still runs the search.
    a.prompt.as_mut().unwrap().text.clear();
    a.on_paste("changed");
    key(&mut a, KeyCode::Char(' '));
    a.on_paste("5");
    key(&mut a, KeyCode::Enter);
    assert!(a.prompt.is_none());
    assert_eq!(a.cursor, 5);
}

#[test]
fn a_paste_never_answers_a_question_or_closes_help() {
    let mut a = app();
    a.modal = Some(Modal::QuitUnsaved);
    assert!(!a.on_paste("y"));
    assert_eq!(a.modal, Some(Modal::QuitUnsaved));
    assert_eq!(a.outcome, None);
    a.modal = None;
    key(&mut a, KeyCode::F(1));
    assert!(!a.on_paste("q"));
    assert!(a.help);
}

fn shell(dir: &std::path::Path) -> Shell {
    let (left, right) = (dir.join("left"), dir.join("right"));
    for (side, text) in [(&left, "a\nb\n"), (&right, "a\nc\n")] {
        std::fs::create_dir_all(side).unwrap();
        for name in ["one.txt", "two.txt"] {
            std::fs::write(side.join(name), text).unwrap();
        }
    }
    let scan = Scan::spawn(left.clone(), right.clone(), vec![]);
    let dir = DirView::new(left, right, false, scan, vec![]);
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
    s
}

#[test]
fn the_directory_list_ignores_pasted_keys_and_its_prompt_takes_text() {
    let tmp = tempfile::tempdir().unwrap();
    let mut s = shell(tmp.path());
    // Typed, `j` then Enter would open the second file, and `q` would quit.
    assert!(s.on_paste("j\nq"));
    assert_eq!(s.current, None);
    assert!(!s.quit_modal);
    assert_eq!(s.outcome, None);
    assert_eq!(s.dir.cursor, 0);
    assert!(s.dir.message.as_deref().unwrap().contains("paste ignored"));

    s.on_key(KeyEvent::new(KeyCode::Char('/'), KeyModifiers::NONE));
    assert!(s.on_paste("two\nignored"));
    assert_eq!(s.dir.prompt.as_deref(), Some("two"));
}

#[test]
fn the_shell_sends_a_paste_to_the_open_file_and_never_to_a_question() {
    let tmp = tempfile::tempdir().unwrap();
    let mut s = shell(tmp.path());
    s.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    let idx = s.current.expect("Enter opens the file");
    assert!(s.on_paste("<w"));
    assert!(!s.open[&idx].session.is_dirty());
    assert_eq!(s.outcome, None);

    s.on_key(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE));
    s.quit_modal = true;
    assert!(!s.on_paste("y"));
    assert!(s.quit_modal);
    assert_ne!(s.outcome, Some(Outcome::Quit));
}
