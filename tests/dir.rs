//! Directory view and shell (spec §6.3, §7.2, M4).

use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use wenas::diff::Whitespace;
use wenas::dircmp::Scan;
use wenas::theme::{Depth, Theme};
use wenas::ui::dir::DirView;
use wenas::ui::shell::{Shell, ShellOptions};
use wenas::ui::term::Ui;
use wenas::ui::{Outcome, ViewConfig};

struct Tree {
    _tmp: tempfile::TempDir,
    left: PathBuf,
    right: PathBuf,
}

fn write(root: &Path, rel: &str, content: &[u8]) {
    let p = root.join(rel);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, content).unwrap();
}

fn read(root: &Path, rel: &str) -> String {
    std::fs::read_to_string(root.join(rel)).unwrap()
}

/// Shaped like the mockup: modified, one-sided, binary, type mismatch, identical.
fn tree() -> Tree {
    let tmp = tempfile::tempdir().unwrap();
    let (l, r) = (tmp.path().join("left"), tmp.path().join("right"));
    for (rel, a, b) in [
        ("Cargo.toml", "same\n", "same\n"),
        ("docs/architecture.md", "same\n", "same\n"),
        ("src/main.rs", "same\n", "same\n"),
        ("src/auth/session.rs", "a\nb\n", "a\nB\n"),
        (
            "src/auth/token.rs",
            "1\n2\n3\n4\n5\n6\n7\n",
            "1\nX\n3\n4\n5\nY\n7\n",
        ),
    ] {
        write(&l, rel, a.as_bytes());
        write(&r, rel, b.as_bytes());
    }
    write(&l, "docs/old-flow.md", b"old\nflow\n");
    write(&r, "src/auth/refresh.rs", b"fn refresh() {}\n");
    write(&l, "config", b"a file");
    write(&r, "config/inner", b"x");
    write(&l, "assets/logo.png", &[0, 1, 2, 3]);
    write(&r, "assets/logo.png", &[0, 1, 9, 3]);
    Tree {
        _tmp: tmp,
        left: l,
        right: r,
    }
}

fn theme() -> Theme {
    Theme::new(true, Depth::True, false)
}

fn shell_with(t: &Tree, show_identical: bool, readonly_left: bool, size: (u16, u16)) -> Shell {
    let scan = Scan::spawn(t.left.clone(), t.right.clone(), vec![]);
    let dir = DirView::new(
        t.left.clone(),
        t.right.clone(),
        show_identical,
        scan,
        vec![],
    );
    let opts = ShellOptions {
        theme: theme(),
        view: ViewConfig::default(),
        ws: Whitespace::Off,
        readonly_left,
        readonly_right: false,
        large_file_bytes: wenas::input::LARGE_FILE_BYTES,
    };
    let mut s = Shell::new(dir, opts, size);
    let deadline = Instant::now() + Duration::from_secs(5);
    while s.dir.scanning {
        assert!(Instant::now() < deadline, "scan did not finish");
        s.tick();
        std::thread::sleep(Duration::from_millis(2));
    }
    s
}

fn shell(t: &Tree) -> Shell {
    shell_with(t, false, false, (110, 20))
}

fn screen(s: &mut Shell, size: (u16, u16)) -> String {
    let mut term = Terminal::new(TestBackend::new(size.0, size.1)).unwrap();
    term.draw(|f| s.draw(f)).unwrap();
    let buf = term.backend().buffer().clone();
    (0..buf.area.height)
        .map(|y| {
            let l: String = (0..buf.area.width)
                .map(|x| buf[(x, y)].symbol().to_string())
                .collect();
            l.trim_end().to_string()
        })
        .collect::<Vec<_>>()
        .join("\n")
        + "\n"
}

/// The screen without the header line, whose paths are random temp dirs.
fn body(s: &mut Shell) -> String {
    let full = screen(s, (110, 20));
    full.split_once('\n').unwrap().1.to_string()
}

fn key(s: &mut Shell, c: char) {
    s.on_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
}
fn press(s: &mut Shell, c: KeyCode) {
    s.on_key(KeyEvent::new(c, KeyModifiers::NONE));
}

fn select(s: &mut Shell, rel: &str) {
    let pos = s
        .dir
        .shown
        .iter()
        .position(|&i| s.dir.entries[i].rel == Path::new(rel))
        .unwrap_or_else(|| panic!("{rel} not shown"));
    s.dir.cursor = pos;
}

fn open(s: &mut Shell, rel: &str) {
    select(s, rel);
    press(s, KeyCode::Enter);
}

#[test]
fn list_shows_differences_and_hides_identical() {
    let t = tree();
    let mut s = shell(&t);
    insta::assert_snapshot!(body(&mut s));
    assert!(
        screen(&mut s, (110, 20))
            .lines()
            .next()
            .unwrap()
            .contains("identical hidden")
    );
}

#[test]
fn i_toggles_identical_files_and_keeps_the_selection() {
    let t = tree();
    let mut s = shell(&t);
    select(&mut s, "src/auth/session.rs");
    key(&mut s, 'i');
    assert!(s.dir.show_identical);
    assert_eq!(
        s.dir.entries[s.dir.selected().unwrap()].rel,
        Path::new("src/auth/session.rs")
    );
    insta::assert_snapshot!(body(&mut s));
    assert!(
        screen(&mut s, (110, 20))
            .lines()
            .next()
            .unwrap()
            .contains("identical shown")
    );
    key(&mut s, 'i');
    assert_eq!(
        s.dir.entries[s.dir.selected().unwrap()].rel,
        Path::new("src/auth/session.rs")
    );
}

#[test]
fn counts_in_the_status_line() {
    let t = tree();
    let mut s = shell(&t);
    let screen = screen(&mut s, (110, 20));
    let last = screen.lines().rev().find(|l| !l.is_empty()).unwrap();
    assert!(
        last.contains("9 files") && last.contains("6 differ") && last.contains("3 identical"),
        "{last}"
    );
    assert!(
        last.contains("Enter open") && last.contains("i show identical") && last.contains("q quit"),
        "{last}"
    );
}

#[test]
fn navigation_keys() {
    let t = tree();
    let mut s = shell(&t);
    let n = s.dir.shown.len();
    assert_eq!(s.dir.cursor, 0);
    key(&mut s, 'j');
    key(&mut s, 'j');
    assert_eq!(s.dir.cursor, 2);
    key(&mut s, 'k');
    assert_eq!(s.dir.cursor, 1);
    key(&mut s, 'G');
    assert_eq!(s.dir.cursor, n - 1);
    key(&mut s, 'j');
    assert_eq!(s.dir.cursor, n - 1, "clamped at the end");
    key(&mut s, 'g');
    key(&mut s, 'g');
    assert_eq!(s.dir.cursor, 0);
    s.on_key(KeyEvent::new(KeyCode::Char('d'), KeyModifiers::CONTROL));
    assert!(s.dir.cursor > 0);
}

#[test]
fn search_jumps_between_matching_names() {
    let t = tree();
    let mut s = shell(&t);
    key(&mut s, '/');
    for c in "auth".chars() {
        key(&mut s, c);
    }
    press(&mut s, KeyCode::Enter);
    let first = s.dir.cursor;
    assert!(
        s.dir.entries[s.dir.selected().unwrap()]
            .rel
            .starts_with("src/auth")
    );
    key(&mut s, 'n');
    assert_ne!(s.dir.cursor, first);
    key(&mut s, 'N');
    assert_eq!(s.dir.cursor, first);
    assert!(screen(&mut s, (110, 20)).contains("1 of 3"));
}

#[test]
fn enter_opens_the_file_and_backspace_returns() {
    let t = tree();
    let mut s = shell(&t);
    open(&mut s, "src/auth/session.rs");
    assert!(s.current.is_some());
    let view = screen(&mut s, (110, 20));
    assert!(
        view.lines().next().unwrap().contains("src/auth/session.rs"),
        "{view}"
    );
    press(&mut s, KeyCode::Backspace);
    assert!(s.current.is_none());
    assert_eq!(
        s.dir.entries[s.dir.selected().unwrap()].rel,
        Path::new("src/auth/session.rs"),
        "selection kept"
    );
    assert_eq!(s.outcome(), None);
}

#[test]
fn finishing_every_hunk_marks_the_file_identical() {
    let t = tree();
    let mut s = shell_with(&t, false, true, (110, 20));
    let before = s.dir.differ;
    open(&mut s, "src/auth/session.rs");
    key(&mut s, '>');
    key(&mut s, 'w');
    press(&mut s, KeyCode::Backspace);
    assert_eq!(read(&t.right, "src/auth/session.rs"), "a\nb\n");
    assert_eq!(s.dir.differ, before - 1);
    assert!(
        !s.dir
            .shown
            .iter()
            .any(|&i| s.dir.entries[i].rel == Path::new("src/auth/session.rs"))
    );
}

#[test]
fn unsaved_files_are_flagged_and_quit_asks_about_them() {
    let t = tree();
    let mut s = shell_with(&t, false, true, (110, 20));
    open(&mut s, "src/auth/token.rs");
    key(&mut s, '>');
    press(&mut s, KeyCode::Backspace);
    assert!(screen(&mut s, (110, 20)).contains("unsaved changes [+]"));
    // Another file, unchanged: no prompt for it.
    open(&mut s, "src/auth/session.rs");
    key(&mut s, '>');
    press(&mut s, KeyCode::Backspace);
    key(&mut s, 'q');
    assert!(s.quit_modal);
    assert_eq!(s.outcome(), None);
    insta::assert_snapshot!(body(&mut s));
    press(&mut s, KeyCode::Esc);
    assert!(!s.quit_modal);
}

#[test]
fn save_both_and_quit_writes_every_dirty_file() {
    let t = tree();
    let mut s = shell_with(&t, false, true, (110, 20));
    open(&mut s, "src/auth/token.rs");
    key(&mut s, '>');
    press(&mut s, KeyCode::Backspace);
    open(&mut s, "src/auth/session.rs");
    key(&mut s, '>');
    press(&mut s, KeyCode::Backspace);
    key(&mut s, 'q');
    key(&mut s, 'w');
    assert_eq!(s.outcome(), Some(Outcome::Quit));
    assert_eq!(read(&t.right, "src/auth/session.rs"), "a\nb\n");
    assert!(read(&t.right, "src/auth/token.rs").starts_with("1\n2\n"));
}

#[test]
fn q_from_inside_a_file_goes_through_the_shell() {
    let t = tree();
    let mut s = shell_with(&t, false, true, (110, 20));
    open(&mut s, "src/auth/session.rs");
    key(&mut s, '>');
    key(&mut s, 'q');
    assert!(
        s.current.is_none() && s.quit_modal,
        "back at the list with the question open"
    );
    key(&mut s, 'Q');
    assert_eq!(s.outcome(), Some(Outcome::Quit));
    assert_eq!(
        read(&t.right, "src/auth/session.rs"),
        "a\nB\n",
        "nothing saved"
    );
}

#[test]
fn q_with_nothing_unsaved_quits_at_once() {
    let t = tree();
    let mut s = shell(&t);
    key(&mut s, 'q');
    assert_eq!(s.outcome(), Some(Outcome::Quit));
}

#[test]
fn save_and_quit_stops_at_a_conflict_then_continues() {
    let t = tree();
    let mut s = shell_with(&t, false, true, (110, 20));
    open(&mut s, "src/auth/session.rs");
    key(&mut s, '>');
    press(&mut s, KeyCode::Backspace);
    open(&mut s, "src/auth/token.rs");
    key(&mut s, '>');
    press(&mut s, KeyCode::Backspace);
    std::fs::write(t.right.join("src/auth/session.rs"), "changed elsewhere\n").unwrap();
    key(&mut s, 'q');
    key(&mut s, 'w');
    assert_eq!(s.outcome(), None, "waiting on the conflict");
    assert!(
        s.current.is_some(),
        "the conflicting file is shown with its question"
    );
    key(&mut s, 'o');
    assert_eq!(s.outcome(), Some(Outcome::Quit));
    assert_eq!(read(&t.right, "src/auth/session.rs"), "a\nb\n");
    assert!(
        read(&t.right, "src/auth/token.rs").starts_with("1\n2\n"),
        "the other file was saved too"
    );
}

#[test]
fn only_on_left_file_opens_against_an_empty_pane_and_is_created_on_save() {
    let t = tree();
    let mut s = shell(&t);
    open(&mut s, "docs/old-flow.md");
    assert!(s.current.is_some());
    key(&mut s, '>');
    key(&mut s, 'w');
    assert_eq!(
        read(&t.right, "docs/old-flow.md"),
        "old\nflow\n",
        "created, with its folder"
    );
    press(&mut s, KeyCode::Backspace);
    assert!(
        !s.dir
            .shown
            .iter()
            .any(|&i| s.dir.entries[i].rel == Path::new("docs/old-flow.md"))
    );
}

#[test]
fn only_on_right_file_can_be_copied_left() {
    let t = tree();
    let mut s = shell(&t);
    open(&mut s, "src/auth/refresh.rs");
    key(&mut s, '<');
    key(&mut s, 'w');
    assert_eq!(read(&t.left, "src/auth/refresh.rs"), "fn refresh() {}\n");
}

#[test]
fn binary_entries_show_the_binary_screen_and_go_back() {
    let t = tree();
    let mut s = shell(&t);
    open(&mut s, "assets/logo.png");
    let v = screen(&mut s, (80, 12));
    assert!(
        v.contains("(binary)") && v.contains("first difference at byte 2"),
        "{v}"
    );
    key(&mut s, 'q');
    assert!(
        s.current.is_none(),
        "q returns to the list instead of quitting"
    );
    assert_eq!(s.outcome(), None);
}

#[test]
fn type_mismatch_cannot_be_opened() {
    let t = tree();
    let mut s = shell(&t);
    open(&mut s, "config");
    assert!(s.current.is_none());
    assert!(s.dir.message_is_error);
    let v = screen(&mut s, (110, 20));
    assert!(
        v.contains("× can't compare: file on left, directory on right"),
        "{v}"
    );
}

#[test]
fn git_difftool_left_side_is_read_only_in_directory_mode() {
    let tmp = tempfile::tempdir().unwrap();
    let (l, r) = (
        tmp.path().join("git-difftool.Xa12/left"),
        tmp.path().join("git-difftool.Xa12/right"),
    );
    write(&l, "a.rs", b"old\n");
    write(&r, "a.rs", b"new\n");
    let t = Tree {
        _tmp: tmp,
        left: l,
        right: r,
    };
    let mut s = shell(&t);
    open(&mut s, "a.rs");
    key(&mut s, '<');
    let v = screen(&mut s, (110, 20));
    assert!(v.contains("git's old version, so it's read-only"), "{v}");
    key(&mut s, '>');
    key(&mut s, 'w');
    assert_eq!(read(&t.right, "a.rs"), "old\n");
}

#[test]
fn symlinked_working_tree_files_are_saved_through_the_link() {
    // git difftool --dir-diff: the right side holds symlinks into the working tree.
    let tmp = tempfile::tempdir().unwrap();
    let (l, r) = (
        tmp.path().join("git-difftool.Xa12/left"),
        tmp.path().join("git-difftool.Xa12/right"),
    );
    let work = tmp.path().join("work");
    write(&l, "a.rs", b"old\n");
    write(&work, "a.rs", b"new\n");
    std::fs::create_dir_all(&r).unwrap();
    std::os::unix::fs::symlink(work.join("a.rs"), r.join("a.rs")).unwrap();
    let t = Tree {
        _tmp: tmp,
        left: l,
        right: r.clone(),
    };
    let mut s = shell(&t);
    assert_eq!(s.dir.entries.len(), 1);
    open(&mut s, "a.rs");
    key(&mut s, '>');
    key(&mut s, 'w');
    assert_eq!(
        read(&work, "a.rs"),
        "old\n",
        "the working-tree file was edited"
    );
    assert!(
        std::fs::symlink_metadata(r.join("a.rs"))
            .unwrap()
            .file_type()
            .is_symlink()
    );
}

#[test]
fn ctrl_c_aborts_from_the_list_and_from_a_file() {
    let t = tree();
    let mut s = shell(&t);
    s.on_key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL));
    assert_eq!(s.outcome(), Some(Outcome::Abort));
    let mut s = shell(&t);
    open(&mut s, "src/auth/session.rs");
    s.on_key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL));
    assert_eq!(s.outcome(), Some(Outcome::Abort));
}

#[test]
fn scanning_state_is_shown_while_results_stream_in() {
    let t = tree();
    let scan = Scan::spawn(t.left.clone(), t.right.clone(), vec![]);
    // Not polled, so the view is still "scanning" with whatever was passed in.
    let mut dir = DirView::new(t.left.clone(), t.right.clone(), false, scan, vec![]);
    dir.apply(wenas::dircmp::Msg::Entry(wenas::dircmp::Entry {
        rel: "src/auth/token.rs".into(),
        status: wenas::dircmp::Status::Modified,
    }));
    dir.apply(wenas::dircmp::Msg::Scanned(1204));
    let opts = ShellOptions {
        theme: theme(),
        view: ViewConfig::default(),
        ws: Whitespace::Off,
        readonly_left: false,
        readonly_right: false,
        large_file_bytes: wenas::input::LARGE_FILE_BYTES,
    };
    let mut s = Shell::new(dir, opts, (110, 20));
    assert!(s.polling());
    let v = screen(&mut s, (110, 20));
    assert!(
        v.contains("scanning") && v.contains("1,204 files") && v.contains("1 differ so far"),
        "{v}"
    );
    assert!(v.contains("Esc stop scan"));
}

#[test]
fn esc_stops_a_running_scan() {
    let t = tree();
    let scan = Scan::spawn(t.left.clone(), t.right.clone(), vec![]);
    let dir = DirView::new(t.left.clone(), t.right.clone(), false, scan, vec![]);
    let opts = ShellOptions {
        theme: theme(),
        view: ViewConfig::default(),
        ws: Whitespace::Off,
        readonly_left: false,
        readonly_right: false,
        large_file_bytes: wenas::input::LARGE_FILE_BYTES,
    };
    let mut s = Shell::new(dir, opts, (110, 20));
    press(&mut s, KeyCode::Esc);
    let deadline = Instant::now() + Duration::from_secs(5);
    while s.dir.scanning {
        assert!(Instant::now() < deadline);
        s.tick();
        std::thread::sleep(Duration::from_millis(2));
    }
    assert!(!s.polling());
}

#[test]
fn empty_comparison_says_so() {
    let tmp = tempfile::tempdir().unwrap();
    let (l, r) = (tmp.path().join("l"), tmp.path().join("r"));
    write(&l, "a", b"1");
    write(&r, "a", b"1");
    let t = Tree {
        _tmp: tmp,
        left: l,
        right: r,
    };
    let mut s = shell(&t);
    assert!(screen(&mut s, (110, 20)).contains("No differences"));
}

#[test]
fn tiny_terminals_do_not_panic() {
    let t = tree();
    for size in [(1, 1), (10, 3), (29, 4), (30, 5), (40, 6)] {
        let mut s = shell_with(&t, true, false, size);
        screen(&mut s, size);
        key(&mut s, 'G');
        key(&mut s, '/');
        screen(&mut s, size);
        press(&mut s, KeyCode::Esc);
        press(&mut s, KeyCode::F(1));
        screen(&mut s, size);
    }
}

#[test]
fn long_paths_are_clipped_not_overflowing() {
    let tmp = tempfile::tempdir().unwrap();
    let (l, r) = (tmp.path().join("l"), tmp.path().join("r"));
    let deep = format!("{}/file.rs", "very-long-directory-name/".repeat(8));
    write(&l, &deep, b"1");
    write(&r, &deep, b"2");
    let t = Tree {
        _tmp: tmp,
        left: l,
        right: r,
    };
    let mut s = shell(&t);
    let v = screen(&mut s, (80, 12));
    assert!(v.contains("…") && v.contains("file.rs"), "{v}");
    assert!(v.lines().all(|l| l.chars().count() <= 80));
}

fn shell_with_limit(t: &Tree, large_file_bytes: u64) -> Shell {
    let scan = Scan::spawn(t.left.clone(), t.right.clone(), vec![]);
    let dir = DirView::new(t.left.clone(), t.right.clone(), false, scan, vec![]);
    let opts = ShellOptions {
        theme: theme(),
        view: ViewConfig::default(),
        ws: Whitespace::Off,
        readonly_left: false,
        readonly_right: false,
        large_file_bytes,
    };
    let mut s = Shell::new(dir, opts, (110, 20));
    let deadline = Instant::now() + Duration::from_secs(5);
    while s.dir.scanning {
        assert!(Instant::now() < deadline);
        s.tick();
        std::thread::sleep(Duration::from_millis(2));
    }
    s
}

#[test]
fn large_files_ask_before_they_are_opened_from_the_list() {
    let t = tree();
    // Pretend anything over 10 bytes is large: token.rs is 14 bytes per side.
    let mut s = shell_with_limit(&t, 10);
    open(&mut s, "src/auth/token.rs");
    assert!(s.current.is_none());
    assert!(s.large_pending.is_some());
    let v = screen(&mut s, (110, 20));
    assert!(
        v.contains("src/auth/token.rs is 1 KB")
            && v.contains("y Open anyway")
            && v.contains("Esc Cancel"),
        "{v}"
    );
    press(&mut s, KeyCode::Esc);
    assert!(s.large_pending.is_none() && s.current.is_none());
    // Small files open without asking, and so does a large one once approved.
    open(&mut s, "src/auth/session.rs");
    assert!(s.current.is_some(), "6 bytes is under the limit");
    press(&mut s, KeyCode::Backspace);
    open(&mut s, "src/auth/token.rs");
    key(&mut s, 'y');
    assert!(s.current.is_some() && s.large_pending.is_none());
    press(&mut s, KeyCode::Backspace);
    open(&mut s, "src/auth/token.rs");
    assert!(
        s.current.is_some(),
        "approved files are not asked about again"
    );
}

#[test]
fn binary_entries_are_never_asked_about_because_they_are_not_loaded() {
    let t = tree();
    let mut s = shell_with_limit(&t, 1);
    open(&mut s, "assets/logo.png");
    assert!(s.current.is_some() && s.large_pending.is_none());
}

#[test]
fn unreadable_entries_show_a_bang_and_the_reason() {
    let tmp = tempfile::tempdir().unwrap();
    let (l, r) = (tmp.path().join("l"), tmp.path().join("r"));
    write(&l, "fixtures/server.pem", b"secret");
    std::fs::create_dir_all(r.join("fixtures")).unwrap();
    std::os::unix::fs::symlink("/nonexistent/target", r.join("fixtures/server.pem")).unwrap();
    write(&l, "ok.txt", b"1");
    write(&r, "ok.txt", b"2");
    let t = Tree {
        _tmp: tmp,
        left: l,
        right: r,
    };
    let mut s = shell(&t);
    let v = body(&mut s);
    assert!(
        v.contains("!  fixtures/server.pem") && v.contains("broken symlink"),
        "{v}"
    );
    assert!(
        v.contains("M  ok.txt"),
        "the walk continued past the error:\n{v}"
    );
    open(&mut s, "fixtures/server.pem");
    assert!(s.current.is_none() && s.dir.message_is_error);
}
