//! One test per row of the edge-case table in docs/wenas-spec.md §10.
//!
//! Rows also covered elsewhere (kept there because they need that file's setup):
//! - Symlinks in directory compare, permission-only changes, unreadable entries
//!   in a walk: `src/dircmp.rs` unit tests.
//! - File changed on disk while open: `tests/ui.rs` (`changed_on_disk_*`).
//! - Terminal resize: `tests/ui.rs` (`switching_view_with_v_and_resize_threshold`).
//! - Missing file (one side) in directory view: `tests/dir.rs`.

use assert_cmd::Command;
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use std::time::{Duration, Instant};
use wenas::diff::{Whitespace, diff};
use wenas::document::{Document, ReadOnlyReason};
use wenas::ops::{Dir, Session, Side};
use wenas::theme::{Depth, Theme};
use wenas::ui::term::Ui;
use wenas::ui::{App, BinaryInfo, ViewConfig};

fn wenas() -> Command {
    Command::cargo_bin("wenas").unwrap()
}

fn theme() -> Theme {
    Theme::new(true, Depth::True, false)
}

fn doc(b: &[u8]) -> Document {
    Document::from_bytes(b.to_vec())
}

fn app(left: &[u8], right: &[u8], size: (u16, u16), cfg: ViewConfig) -> App {
    App::new(
        Session::new(doc(left), doc(right), Whitespace::Off),
        theme(),
        cfg,
        size,
    )
}

fn wide_cfg() -> ViewConfig {
    ViewConfig {
        unified_below_columns: 0,
        ..ViewConfig::default()
    }
}

fn dump(term: &Terminal<TestBackend>) -> String {
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

fn screen(ui: &mut impl Ui, size: (u16, u16)) -> String {
    let mut term = Terminal::new(TestBackend::new(size.0, size.1)).unwrap();
    term.draw(|f| ui.draw(f)).unwrap();
    dump(&term)
}

fn key(a: &mut App, c: char) {
    a.on_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
}

// ---- Binary file ----

#[test]
fn binary_file_shows_the_binary_screen_and_offers_no_hunk_actions() {
    let info = BinaryInfo {
        name: "logo.png".into(),
        left_size: 10,
        right_size: 12,
        first_difference: Some(3),
        utf16: false,
    };
    let mut a = App::binary_screen(info, theme(), (60, 10));
    for c in ['>', '<', 'u', 'w', 'e'] {
        key(&mut a, c);
    }
    assert_eq!(
        a.outcome, None,
        "no action key does anything on the binary screen"
    );
    assert!(!a.session.is_dirty());
    assert!(screen(&mut a, (60, 10)).contains("first difference at byte 3"));
}

#[test]
fn binary_inputs_on_the_command_line() {
    let d = tempfile::tempdir().unwrap();
    let (a, b, c) = (d.path().join("a"), d.path().join("b"), d.path().join("c"));
    std::fs::write(&a, [0u8, 1, 2]).unwrap();
    std::fs::write(&b, [0u8, 1, 2]).unwrap();
    std::fs::write(&c, [0u8, 1, 3]).unwrap();
    wenas()
        .arg(&a)
        .arg(&b)
        .assert()
        .success()
        .stdout("identical\n");
    wenas().arg(&a).arg(&c).assert().code(2); // differs, and no terminal to show it on
}

// ---- Invalid UTF-8 ----

#[test]
fn invalid_utf8_is_shown_escaped_and_written_back_unchanged() {
    let left = b"keep\nbad \xFF\xFE byte\nend\n";
    let mut a = app(left, b"keep\nother\nend\n", (100, 8), wide_cfg());
    assert!(screen(&mut a, (100, 8)).contains("bad \\xFF\\xFE byte"));
    a.session.copy_hunk(0, Dir::LeftToRight).unwrap();
    assert_eq!(
        a.session.right.to_bytes(),
        left.to_vec(),
        "raw bytes in, raw bytes out"
    );
}

// ---- UTF-16 with a byte-order mark ----

#[test]
fn utf16_is_treated_as_binary_with_a_note() {
    let d = tempfile::tempdir().unwrap();
    let (a, b) = (d.path().join("a.txt"), d.path().join("b.txt"));
    std::fs::write(&a, [0xFF, 0xFE, b'a', 0]).unwrap();
    std::fs::write(&b, [0xFF, 0xFE, b'a', 0]).unwrap();
    wenas()
        .arg(&a)
        .arg(&b)
        .assert()
        .success()
        .stdout("identical\n");
    std::fs::write(&b, [0xFF, 0xFE, b'b', 0]).unwrap();
    wenas().arg(&a).arg(&b).assert().code(2);
    let info = BinaryInfo {
        name: "a.txt".into(),
        left_size: 4,
        right_size: 4,
        first_difference: Some(2),
        utf16: true,
    };
    let mut ui = App::binary_screen(info, theme(), (70, 10));
    assert!(screen(&mut ui, (70, 10)).contains("UTF-16 text is treated as binary"));
}

// ---- Wide characters and emoji ----

#[test]
fn wide_characters_are_cut_cleanly_at_the_pane_edge() {
    // Narrow panes: left text area is 9 columns; the fifth wide glyph would
    // straddle the edge and must be replaced, never drawn in half.
    let mut a = app("ab日本語日本語\n".as_bytes(), b"x\n", (35, 6), wide_cfg());
    let s = screen(&mut a, (35, 6));
    let squeezed: String = s.chars().filter(|c| !c.is_whitespace()).collect();
    assert!(squeezed.contains("ab日本語›"), "{s}");
    assert!(!squeezed.contains("ab日本語日"), "{s}");
}

#[test]
fn emoji_take_two_columns() {
    let mut a = app("a😀b\n".as_bytes(), b"x\n", (100, 6), wide_cfg());
    let s = screen(&mut a, (100, 6));
    assert!(s.contains('😀') && s.contains('b'));
    assert_eq!(wenas::ui::render::display_width("a😀b".as_bytes(), 4), 4);
}

// ---- Tabs ----

#[test]
fn tabs_are_four_columns_by_default_and_configurable() {
    let col_of_x = |tab| {
        let cfg = ViewConfig {
            tab_width: tab,
            ..wide_cfg()
        };
        let mut a = app(b"\tx\n", b"y\n", (100, 6), cfg);
        let s = screen(&mut a, (100, 6));
        let line = s.lines().find(|l| l.contains('x')).unwrap().to_string();
        let numbers_end = line.find('1').unwrap() + 3; // "1" plus two spaces
        line[numbers_end..].chars().position(|c| c == 'x').unwrap()
    };
    assert_eq!(col_of_x(4), 4);
    assert_eq!(col_of_x(8), 8);
}

// ---- CRLF vs LF ----

#[test]
fn crlf_versus_lf_is_ignored_but_flagged() {
    let mut a = app(b"a\r\nb\r\nX\n", b"a\nb\nY\n", (100, 8), wide_cfg());
    assert_eq!(a.session.hunks.len(), 1, "only the real change shows");
    // Each pane's header names its line endings.
    let s = screen(&mut a, (100, 8));
    let headers = s.lines().nth(3).unwrap();
    assert!(
        headers.contains("mixed") && headers.contains("LF"),
        "{headers}"
    );
    let d = tempfile::tempdir().unwrap();
    let (l, r) = (d.path().join("l"), d.path().join("r"));
    std::fs::write(&l, "a\r\nb\r\n").unwrap();
    std::fs::write(&r, "a\nb\n").unwrap();
    wenas()
        .arg(&l)
        .arg(&r)
        .assert()
        .success()
        .stdout("identical\n");
    // The two sides are flagged when they differ.
    let mut a = app(b"a\r\nz\r\n", b"a\nq\n", (100, 8), wide_cfg());
    let headers = screen(&mut a, (100, 8)).lines().nth(3).unwrap().to_string();
    assert!(
        headers.contains("CRLF") && headers.contains("LF"),
        "{headers}"
    );
}

#[test]
fn copied_hunks_take_the_target_line_ending() {
    let mut s = Session::new(
        doc(b"a\nNEW1\nNEW2\n"),
        doc(b"a\r\nold\r\n"),
        Whitespace::Off,
    );
    s.copy_hunk(0, Dir::LeftToRight).unwrap();
    assert_eq!(s.right.to_bytes(), b"a\r\nNEW1\r\nNEW2\r\n");
    let mut s = Session::new(doc(b"a\r\nNEW\r\n"), doc(b"a\nold\n"), Whitespace::Off);
    s.copy_hunk(0, Dir::LeftToRight).unwrap();
    assert_eq!(s.right.to_bytes(), b"a\nNEW\n");
}

// ---- Mixed line endings in one file ----

#[test]
fn mixed_line_endings_keep_each_line_and_show_mixed() {
    let text = b"one\r\ntwo\nthree\r\nfour\r\n";
    let d = Document::from_bytes(text.to_vec());
    assert_eq!(d.style().label(), "mixed");
    assert_eq!(
        d.to_bytes(),
        text.to_vec(),
        "each line keeps its own ending"
    );
    // Copied lines take the target's most common ending (CRLF here).
    let mut s = Session::new(
        doc(b"a\nNEW\n"),
        doc(b"a\r\nb\r\nc\nd\r\n"),
        Whitespace::Off,
    );
    s.copy_hunk(0, Dir::LeftToRight).unwrap();
    let out = s.right.to_bytes();
    assert!(
        out.starts_with(b"a\r\nNEW\r\n"),
        "{:?}",
        String::from_utf8_lossy(&out)
    );
    // Untouched lines keep their own endings.
    assert!(!out.windows(2).any(|w| w == b"\r\r"));
    let mut a = app(text, b"x\n", (100, 8), wide_cfg());
    assert!(screen(&mut a, (100, 8)).contains("mixed"));
}

// ---- No newline at end of file ----

#[test]
fn missing_final_newline_is_marked_and_carried_across_copies() {
    let mut a = app(b"a\nb", b"a\nb\n", (100, 8), wide_cfg());
    let s = screen(&mut a, (100, 8));
    let marked: Vec<&str> = s.lines().filter(|l| l.contains('⏎')).collect();
    assert_eq!(
        marked.len(),
        1,
        "only the unterminated last line is marked:\n{s}"
    );
    assert!(marked[0].contains('b'));
    a.session.copy_hunk(0, Dir::LeftToRight).unwrap();
    a.rebuild(None); // the session was changed behind the app's back
    assert_eq!(
        a.session.right.to_bytes(),
        b"a\nb",
        "copying carries the missing newline"
    );
    let s = screen(&mut a, (100, 8));
    assert_eq!(s.matches('⏎').count(), 2, "both sides are now unterminated");
}

// ---- Whitespace-only differences under -w / -b ----

#[test]
fn whitespace_only_hunks_disappear_and_cannot_be_copied() {
    let mut a = app(b"a  b\nsame\n", b"a b\nsame\n", (100, 8), wide_cfg());
    assert_eq!(a.session.hunks.len(), 1);
    key(&mut a, 'W');
    assert!(a.session.hunks.is_empty());
    key(&mut a, '>');
    assert_eq!(a.message.as_deref(), Some("no hunk to copy"));
    assert!(!a.session.is_dirty());
    // The help screen explains it.
    a.on_key(KeyEvent::new(KeyCode::F(1), KeyModifiers::NONE));
    assert!(screen(&mut a, (100, 24)).contains("whitespace-only changes are hidden"));
}

// ---- Very long lines ----

#[test]
fn very_long_lines_draw_quickly_and_get_no_word_highlights() {
    let long: String = "word ".repeat(300_000); // 1.5 MB on one line
    let (l, r) = (format!("{long}A\n"), format!("{long}B\n"));
    let mut a = app(l.as_bytes(), r.as_bytes(), (100, 10), wide_cfg());
    let start = Instant::now();
    let s = screen(&mut a, (100, 10));
    for _ in 0..10 {
        a.on_key(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE));
        screen(&mut a, (100, 10));
    }
    assert!(
        start.elapsed() < Duration::from_secs(5),
        "took {:?}",
        start.elapsed()
    );
    assert!(s.contains("word") && s.contains('›'));
    let inl = wenas::inline::inline_diff(&[long.as_bytes()], &[long.as_bytes()]);
    assert!(
        inl.left[0].is_empty(),
        "no intra-line work above ~2,000 characters"
    );
}

// ---- Large or pathological diffs ----

#[test]
fn pathological_diffs_finish() {
    // Every line different, many repeated lines: worst cases for diff heuristics.
    let a: String = (0..30_000).map(|i| format!("{}\n", i % 7)).collect();
    let b: String = (0..30_000).map(|i| format!("{}\n", (i * 3) % 11)).collect();
    let start = Instant::now();
    let hunks = diff(&doc(a.as_bytes()), &doc(b.as_bytes()), Whitespace::Off);
    assert!(!hunks.is_empty());
    assert!(
        start.elapsed() < Duration::from_secs(20),
        "took {:?}",
        start.elapsed()
    );
}

// (Background diffing, progress and cancel are covered in tests/background_diff.rs.)

// ---- Empty file vs non-empty ----

#[test]
fn empty_versus_non_empty_is_all_filler() {
    let mut a = app(b"", b"one\ntwo\n", (100, 8), wide_cfg());
    assert_eq!(a.session.hunks.len(), 1);
    let s = screen(&mut a, (100, 8));
    assert_eq!(s.lines().filter(|l| l.contains('░')).count(), 2, "{s}");
    a.session.copy_hunk(0, Dir::RightToLeft).unwrap();
    assert_eq!(a.session.left.to_bytes(), b"one\ntwo\n");
    let d = tempfile::tempdir().unwrap();
    let (e1, e2) = (d.path().join("e1"), d.path().join("e2"));
    std::fs::write(&e1, "").unwrap();
    std::fs::write(&e2, "").unwrap();
    wenas()
        .arg(&e1)
        .arg(&e2)
        .assert()
        .success()
        .stdout("identical\n");
}

// ---- Missing file (one side) ----

#[test]
fn a_missing_file_is_an_error() {
    let d = tempfile::tempdir().unwrap();
    let f = d.path().join("f");
    std::fs::write(&f, "x").unwrap();
    wenas()
        .arg(&f)
        .arg(d.path().join("missing"))
        .assert()
        .code(2)
        .stderr(predicates::str::contains("cannot read"));
    wenas()
        .arg(d.path().join("missing"))
        .arg(&f)
        .assert()
        .code(2);
}

// ---- Stdin or named pipe ----

#[test]
fn stdin_and_named_pipes_are_read_in_full_and_are_read_only() {
    let d = tempfile::tempdir().unwrap();
    let f = d.path().join("f");
    std::fs::write(&f, "x\n").unwrap();
    wenas()
        .arg("-")
        .arg(&f)
        .write_stdin("x\n")
        .assert()
        .success()
        .stdout("identical\n");
    wenas().arg("-").arg(&f).write_stdin("y\n").assert().code(2);
    wenas()
        .arg("-")
        .arg("-")
        .write_stdin("x\n")
        .assert()
        .code(2)
        .stderr(predicates::str::contains("only one side"));
    // Process substitution gives named pipes.
    let out = std::process::Command::new("bash")
        .arg("-c")
        .arg(format!(
            "{} <(printf 'a\\n') <(printf 'a\\n')",
            env!("CARGO_BIN_EXE_wenas")
        ))
        .output()
        .unwrap();
    assert_eq!(String::from_utf8_lossy(&out.stdout), "identical\n");
    // Read-only, and copying into such a side is refused.
    let loaded = wenas::input::Loaded {
        bytes: b"a\n".to_vec(),
        source: wenas::input::Source::Stream,
    };
    let streamed = Document::from_loaded(loaded, Some(std::path::Path::new("-")));
    assert_eq!(streamed.readonly_reason(), Some(ReadOnlyReason::Stream));
    assert!(streamed.disk_id().is_none());
    let mut s = Session::new(streamed, doc(b"b\n"), Whitespace::Off);
    assert!(s.copy_hunk(0, Dir::RightToLeft).is_err());
    assert!(s.save(Side::Left, false).is_err());
}

// ---- Size limits ----

#[test]
fn files_over_one_gigabyte_are_refused() {
    let d = tempfile::tempdir().unwrap();
    let (big, small) = (d.path().join("big"), d.path().join("small"));
    std::fs::File::create(&big)
        .unwrap()
        .set_len(wenas::input::MAX_FILE_BYTES + 1)
        .unwrap(); // sparse
    std::fs::write(&small, "x").unwrap();
    wenas()
        .arg(&big)
        .arg(&small)
        .assert()
        .code(2)
        .stderr(predicates::str::contains("limit"));
}

// ---- Config ----

#[test]
fn config_file_is_honoured_and_unknown_keys_only_warn() {
    let d = tempfile::tempdir().unwrap();
    let (a, b, cfg) = (
        d.path().join("a"),
        d.path().join("b"),
        d.path().join("c.toml"),
    );
    std::fs::write(&a, "x\n").unwrap();
    std::fs::write(&b, "x\n").unwrap();
    std::fs::write(
        &cfg,
        "bogus = 1\ntab_width = 2\n[colors]\nchanged_bg = \"#102030\"\nnope = \"#fff000\"\n",
    )
    .unwrap();
    wenas()
        .arg("--config")
        .arg(&cfg)
        .arg(&a)
        .arg(&b)
        .assert()
        .success()
        .stdout("identical\n")
        .stderr(predicates::str::contains("unknown config key `bogus`"))
        .stderr(predicates::str::contains("unknown colour `nope`"));
    // A missing explicit config is an error; a broken one falls back to defaults.
    wenas()
        .arg("--config")
        .arg(d.path().join("none.toml"))
        .arg(&a)
        .arg(&b)
        .assert()
        .code(2);
    std::fs::write(&cfg, "theme = \"purple\"\n").unwrap();
    wenas()
        .arg("--config")
        .arg(&cfg)
        .arg(&a)
        .arg(&b)
        .assert()
        .success()
        .stderr(predicates::str::contains("using defaults"));
}

#[test]
fn whitespace_from_config_applies_when_no_flag_is_given() {
    let d = tempfile::tempdir().unwrap();
    let (a, b, cfg) = (
        d.path().join("a"),
        d.path().join("b"),
        d.path().join("c.toml"),
    );
    std::fs::write(&a, "a  b\n").unwrap();
    std::fs::write(&b, "a b\n").unwrap();
    std::fs::write(&cfg, "whitespace = \"change\"\n").unwrap();
    wenas()
        .arg("--config")
        .arg(&cfg)
        .arg(&a)
        .arg(&b)
        .assert()
        .success()
        .stdout("identical\n");
    wenas()
        .arg(&a)
        .arg(&b)
        .arg("--config")
        .arg(d.path().join("empty.toml"))
        .assert()
        .code(2);
}
