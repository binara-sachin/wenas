//! Renders real wenas screens to HTML files, one per scenario, for the README
//! screenshots. `scripts/screenshots.sh` turns them into PNGs.
//!
//!     cargo run --example screenshots -- <output-dir>

use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::style::{Color, Modifier};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use unicode_width::UnicodeWidthStr;
use wenas::diff::Whitespace;
use wenas::dircmp::Scan;
use wenas::document::{Document, ReadOnlyReason};
use wenas::ops::Session;
use wenas::theme::{Depth, Theme};
use wenas::ui::dir::{DirView, Filters};
use wenas::ui::shell::{Shell, ShellOptions};
use wenas::ui::term::Ui;
use wenas::ui::{App, ViewConfig};

const TERM_BG: &str = "#1b1f27";
const TERM_FG: &str = "#cdd3de";

const OLD: &str = r#"impl Token {
    pub fn is_expired(&self) -> bool {
        self.issued + self.ttl < now()
    }

    /// Refresh the token if it has expired.
    fn refresh(&self) -> Result {
        let ttl = self.ttl;
        self.fetch(ttl)?;
        self.cache.clear();
        Ok(())
    }
}

impl Drop for Token {
    fn drop(&mut self) {
        self.cache.clear();
    }
}
"#;

fn new_version() -> String {
    OLD.replace("< now()", "<= now()")
        .replace("if it has expired", "before it expires")
        .replace("let ttl = self.ttl;\n        self.fetch(ttl)?;\n        self.cache.clear();", "let ttl = self.ttl.max(60);\n        log::debug!(\"ttl={ttl}\");\n        self.fetch(ttl)?;")
}

fn css(c: Color, default: &str) -> String {
    match c {
        Color::Rgb(r, g, b) => format!("#{r:02x}{g:02x}{b:02x}"),
        _ => default.to_string(),
    }
}

fn html(buf: &Buffer, title: &str) -> String {
    let mut out = String::new();
    write!(
        out,
        "<!doctype html><meta charset=utf-8><title>{title}</title><style>\
         body{{margin:0;background:#0d0f14;padding:24px;display:inline-block}}\
         pre{{margin:0;padding:14px 10px;background:{TERM_BG};color:{TERM_FG};border-radius:10px;\
         font:15px/1.3 'JetBrains Mono','DejaVu Sans Mono','Menlo',monospace;white-space:pre}}\
         </style><pre id=term>"
    )
    .unwrap();
    for y in 0..buf.area.height {
        let mut x = 0;
        while x < buf.area.width {
            let cell = &buf[(x, y)];
            let sym = cell.symbol();
            let (mut fg, mut bg) = (css(cell.fg, TERM_FG), css(cell.bg, TERM_BG));
            if cell.modifier.contains(Modifier::REVERSED) {
                std::mem::swap(&mut fg, &mut bg);
            }
            let mut style = format!("color:{fg};background:{bg};");
            if cell.modifier.contains(Modifier::BOLD) {
                style.push_str("font-weight:700;");
            }
            if cell.modifier.contains(Modifier::UNDERLINED) {
                style.push_str("text-decoration:underline;");
            }
            let esc = match sym {
                "<" => "&lt;".to_string(),
                ">" => "&gt;".to_string(),
                "&" => "&amp;".to_string(),
                s => s.to_string(),
            };
            write!(out, "<span style=\"{style}\">{esc}</span>").unwrap();
            // A wide glyph covers the next cell too.
            x += if sym.width() >= 2 { 2 } else { 1 };
        }
        out.push('\n');
    }
    out.push_str("</pre>");
    out
}

fn snapshot(ui: &mut impl Ui, size: (u16, u16)) -> Buffer {
    let mut term = Terminal::new(TestBackend::new(size.0, size.1)).unwrap();
    term.draw(|f| ui.draw(f)).unwrap();
    term.backend().buffer().clone()
}

fn theme() -> Theme {
    Theme::new(true, Depth::True, false)
}

fn file_app(size: (u16, u16)) -> App {
    let mut l = Document::from_bytes(OLD.into());
    let mut r = Document::from_bytes(new_version().into());
    l.label = Some("a/src/auth/token.rs".into());
    l.make_readonly(ReadOnlyReason::GitOldVersion);
    r.label = Some("src/auth/token.rs".into());
    App::new(
        Session::new(l, r, Whitespace::Off),
        theme(),
        ViewConfig::default(),
        size,
    )
}

fn press(a: &mut App, c: char) {
    a.on_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
}

fn write(root: &Path, rel: &str, body: &str) {
    let p = root.join(rel);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, body).unwrap();
}

fn directory_shell(size: (u16, u16)) -> (tempfile::TempDir, Shell) {
    let tmp = tempfile::tempdir().unwrap();
    let (l, r) = (tmp.path().join("l"), tmp.path().join("r"));
    for f in [
        "Cargo.toml",
        "docs/architecture.md",
        "src/main.rs",
        "src/auth/mod.rs",
        "src/cli/args.rs",
        "tests/fixtures.rs",
    ] {
        write(&l, f, "same\n");
        write(&r, f, "same\n");
    }
    for f in ["src/auth/session.rs", "src/auth/token.rs", "src/cli/run.rs"] {
        write(&l, f, "a\nb\n");
        write(&r, f, "a\nB\n");
    }
    write(&l, "docs/old-flow.md", "old\n");
    write(&r, "src/auth/refresh.rs", "fn refresh() {}\n");
    write(&l, "config", "x");
    write(&r, "config/inner", "x");
    std::fs::create_dir_all(l.join("assets")).unwrap();
    std::fs::create_dir_all(r.join("assets")).unwrap();
    std::fs::write(l.join("assets/logo.png"), [0u8, 1, 2, 3]).unwrap();
    std::fs::write(r.join("assets/logo.png"), [0u8, 1, 9, 3]).unwrap();
    let scan = Scan::spawn(l.clone(), r.clone(), vec![]);
    let dir = DirView::new(l, r, Filters::default(), scan, vec![]);
    let opts = ShellOptions {
        theme: theme(),
        view: ViewConfig::default(),
        ws: Whitespace::Off,
        readonly_left: true,
        readonly_right: false,
        large_file_bytes: wenas::input::LARGE_FILE_BYTES,
        diff: wenas::ops::DiffPolicy::default(),
    };
    let mut shell = Shell::new(dir, opts, size);
    let deadline = Instant::now() + Duration::from_secs(10);
    while shell.dir.scanning && Instant::now() < deadline {
        shell.tick();
        std::thread::sleep(Duration::from_millis(5));
    }
    shell.dir.left = PathBuf::from("/tmp/git-difftool.Xa12/left");
    shell.dir.right = PathBuf::from("~/code/app");
    shell.dir.cursor = 4;
    (tmp, shell)
}

fn main() {
    let out = PathBuf::from(
        std::env::args()
            .nth(1)
            .expect("usage: screenshots <output-dir>"),
    );
    std::fs::create_dir_all(&out).unwrap();
    let save = |name: &str, buf: &Buffer| {
        std::fs::write(out.join(format!("{name}.html")), html(buf, name)).unwrap()
    };

    let mut a = file_app((100, 21));
    press(&mut a, 'J');
    press(&mut a, 'K');
    save("side-by-side", &snapshot(&mut a, (100, 21)));

    let mut a = file_app((80, 21));
    save("unified", &snapshot(&mut a, (80, 21)));

    let mut a = file_app((100, 21));
    for c in "/ttl\r".chars() {
        a.on_key(KeyEvent::new(
            if c == '\r' {
                KeyCode::Enter
            } else {
                KeyCode::Char(c)
            },
            KeyModifiers::NONE,
        ));
    }
    save("search", &snapshot(&mut a, (100, 21)));

    let (_tmp, mut shell) = directory_shell((100, 16));
    save("directory", &snapshot(&mut shell, (100, 16)));
}
