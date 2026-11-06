//! Renders real wenas screens to HTML files, one per scenario, for the README
//! screenshots. `scripts/screenshots.sh` turns them into PNGs.
//!
//!     cargo run --example screenshots -- <output-dir>
//!
//! With `--design`, it renders the screens of the UI design instead, at the
//! design's sizes and font, so they can be compared with its pictures.

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
    page(buf, title, false)
}

/// The design's pictures use 13px JetBrains Mono on 18px rows, 12px above and
/// below the screen, and no frame around it.
fn page(buf: &Buffer, title: &str, design: bool) -> String {
    let mut out = String::new();
    let (body, pre) = if design {
        (
            "margin:0;background:#14171d;display:inline-block".to_string(),
            format!(
                "margin:0;padding:12px;background:{TERM_BG};color:{TERM_FG};border-radius:10px;\
                 font:13px/18px 'JetBrains Mono',monospace;white-space:pre"
            ),
        )
    } else {
        (
            "margin:0;background:#0d0f14;padding:24px;display:inline-block".to_string(),
            format!(
                "margin:0;padding:14px 10px;background:{TERM_BG};color:{TERM_FG};border-radius:10px;\
                 font:15px/1.3 'JetBrains Mono','DejaVu Sans Mono','Menlo',monospace;white-space:pre"
            ),
        )
    };
    write!(
        out,
        "<!doctype html><meta charset=utf-8><title>{title}</title><style>\
         body{{{body}}}pre{{{pre}}}span{{display:inline-block;height:18px}}</style><pre id=term>"
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

/// The pom.xml the design shows, with version 2.4.2.3 on line 21.
fn pom(version: &str) -> String {
    let mut lines: Vec<String> = vec![
        r#"<?xml version="1.0" encoding="UTF-8"?>"#.into(),
        "<!--".into(),
        "  ~ Copyright (c) 2019, WSO2 Inc. (http://www.wso2.org) All Rights Reserved.".into(),
        "  ~".into(),
        "  ~ WSO2 Inc. licenses this file to you under the Apache License,".into(),
        "  ~ Version 2.0 (the \"License\"); you may not use this file except".into(),
        "  ~ in compliance with the License.".into(),
        "  ~ You may obtain a copy of the License at".into(),
        "  ~".into(),
        "  ~ http://www.apache.org/licenses/LICENSE-2.0".into(),
        "  -->".into(),
        "".into(),
        r#"<project xmlns="http://maven.apache.org/POM/4.0.0""#.into(),
        r#"         xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance""#.into(),
        r#"         xsi:schemaLocation="http://maven.apache.org/POM/4.0.0">"#.into(),
        "".into(),
        "    <!-- parent -->".into(),
        "    <parent>".into(),
        "        <artifactId>identity-local-auth-api</artifactId>".into(),
        "        <groupId>org.wso2.carbon.identity.local.auth.api</groupId>".into(),
        format!("        <version>{version}</version>"),
        "        <relativePath>../../pom.xml</relativePath>".into(),
        "    </parent>".into(),
        "    <modelVersion>4.0.0</modelVersion>".into(),
        "".into(),
        "    <artifactId>org.wso2.carbon.identity.local.auth.api.endpoint</artifactId>".into(),
        "    <packaging>war</packaging>".into(),
        "    <name>WSO2 Identity -  Authentication REST API</name>".into(),
        "    <description>WSO2 Identity - Authentication REST API</description>".into(),
        "".into(),
        "    <build>".into(),
        "        <plugins>".into(),
        "            <plugin>".into(),
        "                <groupId>org.apache.maven.plugins</groupId>".into(),
        "                <artifactId>maven-compiler-plugin</artifactId>".into(),
        "                <configuration>".into(),
        "                    <source>1.8</source>".into(),
        "                    <target>1.8</target>".into(),
        "                </configuration>".into(),
        "            </plugin>".into(),
        "            <plugin>".into(),
        "                <groupId>org.apache.maven.plugins</groupId>".into(),
        "                <artifactId>maven-war-plugin</artifactId>".into(),
        "                <version>${maven.war.plugin.version}</version>".into(),
        "                <configuration>".into(),
        "                    <webResources>".into(),
        "                        <resource>".into(),
        "                        </resource>".into(),
        "                    </webResources>".into(),
        "                </configuration>".into(),
        "            </plugin>".into(),
        "        </plugins>".into(),
        "    </build>".into(),
    ];
    for i in 0..60 {
        lines.push(format!("    <!-- dependency {i} -->"));
    }
    lines.push("</project>".into());
    lines.join("\n") + "\n"
}

const POM: &str = "api#identity#auth#v1.1/META-INF/maven/org.wso2.carbon.identity.local.auth.api/org.wso2.carbon.identity.local.auth.api.endpoint/pom.xml";

/// The directory the design shows: two wso2is releases' webapps.
fn design_shell(size: (u16, u16)) -> (tempfile::TempDir, Shell) {
    let tmp = tempfile::tempdir().unwrap();
    let tail = "repository/deployment/server/webapps";
    let l = tmp.path().join("wso2is-5.11.0.396").join(tail);
    let r = tmp.path().join("wso2is-5.11.0.445").join(tail);
    let modified = [
        "STRATOS_ROOT/index.html",
        "accountrecoveryendpoint/META-INF/maven/org.wso2.identity.apps/recovery-portal/pom.properties",
        "accountrecoveryendpoint/META-INF/maven/org.wso2.identity.apps/recovery-portal/pom.xml",
        "accountrecoveryendpoint/password-recovery-notify.jsp",
        "accountrecoveryendpoint/password-recovery-username-request.jsp",
        "accountrecoveryendpoint/password-recovery-with-claims-notify.jsp",
        "accountrecoveryendpoint/password-recovery.jsp",
        "accountrecoveryendpoint/self-registration-process.jsp",
        "accountrecoveryendpoint/self-registration-with-verification.jsp",
        "api#identity#auth#v1.1/META-INF/maven/org.wso2.carbon.identity.local.auth.api/org.wso2.carbon.identity.local.auth.api.endpoint/pom.properties",
        "api#identity#auth#v1.1/WEB-INF/web.xml",
        "api#identity#recovery#v0.9/META-INF/maven/org.wso2.carbon.identity.governance/org.wso2.carbon.identity.recovery.endpoint/pom.properties",
        "api#identity#recovery#v0.9/META-INF/maven/org.wso2.carbon.identity.governance/org.wso2.carbon.identity.recovery.endpoint/pom.xml",
        "authenticationendpoint/META-INF/maven/org.wso2.identity.apps/authentication-portal/pom.properties",
        "authenticationendpoint/META-INF/maven/org.wso2.identity.apps/authentication-portal/pom.xml",
        "authenticationendpoint/WEB-INF/classes/org/wso2/carbon/identity/application/authentication/endpoint/i18n/Resources.properties",
        "authenticationendpoint/WEB-INF/classes/org/wso2/carbon/identity/application/authentication/endpoint/i18n/Resources_fr_FR.properties",
        "authenticationendpoint/WEB-INF/classes/org/wso2/carbon/identity/application/authentication/endpoint/i18n/Resources_ja_JP.properties",
        "authenticationendpoint/WEB-INF/classes/org/wso2/carbon/identity/application/authentication/endpoint/i18n/Resources_zh_CN.properties",
        "console/WEB-INF/web.xml",
        "console/index.jsp",
    ];
    for f in modified {
        write(&l, f, "a\nb\n");
        write(&r, f, "a\nB\n");
    }
    write(&l, POM, &pom("2.4.2.3"));
    write(&r, POM, &pom("2.4.2.8"));
    write(
        &r,
        "authenticationendpoint/WEB-INF/classes/org/wso2/carbon/identity/application/authentication/endpoint/i18n/Resources_pt_BR.properties",
        "x\n",
    );
    write(
        &l,
        "authenticationendpoint/WEB-INF/lib/legacy-taglib.jar",
        "x\n",
    );
    let mut png = vec![0x89u8, b'P', b'N', b'G', 0];
    png.resize(4096, 7);
    std::fs::create_dir_all(l.join("authenticationendpoint/images")).unwrap();
    std::fs::create_dir_all(r.join("authenticationendpoint/images")).unwrap();
    std::fs::write(l.join("authenticationendpoint/images/logo.png"), &png).unwrap();
    png[2048] = 9;
    std::fs::write(r.join("authenticationendpoint/images/logo.png"), &png).unwrap();
    for i in 0..40 {
        let f = format!("console/js/lib-{i:02}.js");
        write(&l, &f, "same\n");
        write(&r, &f, "same\n");
    }
    let scan = Scan::spawn(l.clone(), r.clone(), vec![]);
    let dir = DirView::new(l, r, Filters::default(), scan, vec![]);
    let opts = ShellOptions {
        theme: theme(),
        view: ViewConfig::default(),
        ws: Whitespace::Off,
        readonly_left: false,
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
    (tmp, shell)
}

fn shell_key(s: &mut Shell, code: KeyCode) {
    s.on_key(KeyEvent::new(code, KeyModifiers::NONE));
}

fn shell_keys(s: &mut Shell, keys: &str) {
    for c in keys.chars() {
        shell_key(s, KeyCode::Char(c));
    }
}

/// Select a file in the list by name.
fn select(s: &mut Shell, name: &str) {
    let pos = (0..s.dir.visible.len())
        .find(|&p| {
            s.dir
                .entry_at(p)
                .is_some_and(|e| s.dir.entries[e].rel.ends_with(name))
        })
        .expect("file in the list");
    s.dir.cursor = pos;
}

fn design(out: &Path) {
    let save = |name: &str, buf: &Buffer| {
        std::fs::write(out.join(format!("{name}.html")), page(buf, name, true)).unwrap()
    };
    let wide = (161, 43);
    let (_tmp, mut s) = design_shell(wide);
    select(&mut s, "Resources_fr_FR.properties");
    save("after-directory", &snapshot(&mut s, wide));

    select(&mut s, "pom.xml");
    // The second pom.xml is the one under api#identity#auth#v1.1.
    let pos = (0..s.dir.visible.len())
        .find(|&p| {
            s.dir
                .entry_at(p)
                .is_some_and(|e| s.dir.entries[e].rel.to_string_lossy() == POM)
        })
        .unwrap();
    s.dir.cursor = pos;
    snapshot(&mut s, wide);
    shell_key(&mut s, KeyCode::Enter);
    shell_keys(&mut s, "z");
    save("after-file", &snapshot(&mut s, wide));

    let narrow = (89, 29);
    let (_tmp, mut s) = design_shell(narrow);
    s.dir.cursor = 0;
    snapshot(&mut s, narrow);
    let pos = (0..s.dir.visible.len())
        .find(|&p| {
            s.dir
                .entry_at(p)
                .is_some_and(|e| s.dir.entries[e].rel.to_string_lossy() == POM)
        })
        .unwrap();
    s.dir.cursor = pos;
    shell_key(&mut s, KeyCode::Enter);
    snapshot(&mut s, narrow);
    shell_keys(&mut s, "jjjjjjjjjjjjjjjj");
    save("after-file-80", &snapshot(&mut s, narrow));

    // Other states, to check they keep to the same zones.
    let (_tmp, mut s) = design_shell(narrow);
    select(&mut s, "Resources_fr_FR.properties");
    save("state-directory-80", &snapshot(&mut s, narrow));
    shell_key(&mut s, KeyCode::F(1));
    save("state-directory-help", &snapshot(&mut s, narrow));
    shell_key(&mut s, KeyCode::Esc);
    select(&mut s, "logo.png");
    shell_key(&mut s, KeyCode::Enter);
    save("state-binary", &snapshot(&mut s, narrow));

    let mut a = file_app((120, 24));
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
    save("state-search", &snapshot(&mut a, (120, 24)));
    let mut a = file_app((120, 24));
    press(&mut a, '<');
    save("state-error", &snapshot(&mut a, (120, 24)));
    press(&mut a, 'F');
    a.on_key(KeyEvent::new(KeyCode::F(1), KeyModifiers::NONE));
    save("state-help", &snapshot(&mut a, (120, 24)));

    for (name, theme) in [
        ("state-light", Theme::new(false, Depth::True, false)),
        ("state-no-color", Theme::new(true, Depth::True, true)),
    ] {
        let light = !theme.dark;
        // A light theme goes on a light terminal.
        let save = |n: &str, buf: &Buffer| {
            let mut html = page(buf, n, true);
            if light {
                html = html.replace(TERM_BG, "#faf9f5").replace(TERM_FG, "#1f1e1d");
            }
            std::fs::write(out.join(format!("{n}.html")), html).unwrap()
        };
        let (_tmp, mut s) = design_shell(wide);
        s.set_theme(theme);
        select(&mut s, "Resources_fr_FR.properties");
        save(&format!("{name}-directory"), &snapshot(&mut s, wide));
        let pos = (0..s.dir.visible.len())
            .find(|&p| {
                s.dir
                    .entry_at(p)
                    .is_some_and(|e| s.dir.entries[e].rel.to_string_lossy() == POM)
            })
            .unwrap();
        s.dir.cursor = pos;
        shell_key(&mut s, KeyCode::Enter);
        save(&format!("{name}-file"), &snapshot(&mut s, wide));
    }
}

fn main() {
    let out = PathBuf::from(
        std::env::args()
            .nth(1)
            .expect("usage: screenshots <output-dir> [--design]"),
    );
    std::fs::create_dir_all(&out).unwrap();
    if std::env::args().nth(2).as_deref() == Some("--design") {
        design(&out);
        return;
    }
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
