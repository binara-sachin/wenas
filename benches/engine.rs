//! Measurements for the targets in docs/wenas-spec.md §9. They are goals to
//! track, not gates: CI runs these and reports, but never fails on the numbers.

use criterion::{BatchSize, Criterion, criterion_group, criterion_main};
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use std::hint::black_box;
use std::path::Path;
use std::time::Duration;
use wenas::diff::Whitespace;
use wenas::dircmp::{Msg, Scan};
use wenas::document::Document;
use wenas::ops::Session;
use wenas::theme::{Depth, Theme};
use wenas::ui::render::draw;
use wenas::ui::{App, ViewConfig};

/// Code-like text; every `every`th line differs between the two versions.
fn texts(lines: usize, every: usize) -> (Vec<u8>, Vec<u8>) {
    let (mut a, mut b) = (String::new(), String::new());
    for i in 0..lines {
        let line = format!(
            "    let value_{i} = compute(input_{}, {i}) + offset;\n",
            i % 97
        );
        a.push_str(&line);
        if i % every == 0 {
            b.push_str(&format!(
                "    let value_{i} = compute_v2(input_{}, {i}) + offset;\n",
                i % 97
            ));
        } else {
            b.push_str(&line);
        }
    }
    (a.into_bytes(), b.into_bytes())
}

fn theme() -> Theme {
    Theme::new(true, Depth::True, false)
}

fn render(term: &mut Terminal<TestBackend>, app: &mut App) {
    term.draw(|f| draw(f, app)).unwrap();
}

fn first_screen(c: &mut Criterion) {
    let mut g = c.benchmark_group("first_screen");
    g.sample_size(20);
    for (name, lines) in [("10k_lines", 10_000), ("100k_lines", 100_000)] {
        let (a, b) = texts(lines, 50);
        g.bench_function(name, |bench| {
            bench.iter(|| {
                // Everything between "bytes in memory" and "first frame drawn".
                let (l, r) = (
                    Document::from_bytes(a.clone()),
                    Document::from_bytes(b.clone()),
                );
                let session = Session::new(l, r, Whitespace::Off);
                let mut app = App::new(session, theme(), ViewConfig::default(), (200, 50));
                let mut term = Terminal::new(TestBackend::new(200, 50)).unwrap();
                render(&mut term, &mut app);
                black_box(app.rows.len())
            })
        });
    }
    g.finish();
}

fn rediff_after_copy(c: &mut Criterion) {
    let (a, b) = texts(10_000, 50);
    let (l, r) = (Document::from_bytes(a), Document::from_bytes(b));
    c.bench_function("hunk_copy_and_rediff_10k_lines", |bench| {
        bench.iter_batched(
            || {
                let session = Session::new(l.clone(), r.clone(), Whitespace::Off);
                App::new(session, theme(), ViewConfig::default(), (200, 50))
            },
            |mut app| {
                // `>` copies the current hunk, re-diffs, and rebuilds the rows.
                app.on_key(KeyEvent::new(KeyCode::Char('>'), KeyModifiers::NONE));
                black_box(app.session.hunks.len())
            },
            BatchSize::LargeInput,
        )
    });
}

fn scrolling_frame(c: &mut Criterion) {
    let (a, b) = texts(100_000, 50);
    let session = Session::new(
        Document::from_bytes(a),
        Document::from_bytes(b),
        Whitespace::Off,
    );
    let mut app = App::new(session, theme(), ViewConfig::default(), (200, 50));
    let mut term = Terminal::new(TestBackend::new(200, 50)).unwrap();
    c.bench_function("scroll_one_frame_100k_lines", |bench| {
        bench.iter(|| {
            app.on_key(KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE));
            render(&mut term, &mut app);
        })
    });
}

/// Two trees of `n` small files each, one file in 500 different.
fn make_trees(root: &Path, n: usize) {
    for side in ["a", "b"] {
        for d in 0..(n / 200) {
            let dir = root.join(side).join(format!("d{d:03}"));
            std::fs::create_dir_all(&dir).unwrap();
            for f in 0..200 {
                let differs = side == "b" && (d * 200 + f) % 500 == 0;
                let body = format!("fn f{f}() {{}}\n").repeat(20)
                    + if differs { "// changed\n" } else { "" };
                std::fs::write(dir.join(format!("f{f:03}.rs")), body).unwrap();
            }
        }
    }
}

fn directory_compare(c: &mut Criterion) {
    let tmp = tempfile::tempdir().unwrap();
    make_trees(tmp.path(), 50_000);
    let (l, r) = (tmp.path().join("a"), tmp.path().join("b"));
    let mut g = c.benchmark_group("directory_compare_50k_files");
    g.sample_size(10).measurement_time(Duration::from_secs(20));
    g.bench_function("time_to_first_result", |bench| {
        bench.iter(|| {
            let scan = Scan::spawn(l.clone(), r.clone(), vec![]);
            loop {
                if let Ok(Msg::Entry(e)) = scan.rx.recv() {
                    break black_box(e);
                }
            }
        })
    });
    g.bench_function("full_scan", |bench| {
        bench.iter(|| {
            let scan = Scan::spawn(l.clone(), r.clone(), vec![]);
            let mut out = Vec::new();
            scan.drain_for(Duration::from_secs(120), &mut out);
            black_box(out.len())
        })
    });
    g.finish();
}

criterion_group!(
    benches,
    first_screen,
    rediff_after_copy,
    scrolling_frame,
    directory_compare
);
criterion_main!(benches);
