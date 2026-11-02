use clap::Parser;
use std::io::IsTerminal;
use std::path::Path;
use std::process::ExitCode;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use wenas::cli::{Cli, GIT_CONFIG};
use wenas::config;
use wenas::diff::diff;
use wenas::dircmp::{Msg, Scan};
use wenas::gitenv::mark_readonly;
use wenas::input::{self, read_input};
use wenas::ops::Session;
use wenas::save::document_from_loaded;
use wenas::theme::{Depth, Theme, resolve_dark};
use wenas::ui::dir::DirView;
use wenas::ui::loading::Loading;
use wenas::ui::shell::{Shell, ShellOptions};
use wenas::ui::term::Ui;
use wenas::ui::term::{TerminalGuard, run_loop, terminal_size};
use wenas::ui::{App, BinaryInfo, Outcome, ViewConfig};

/// Exit codes (spec §8.3).
const EXIT_OK: u8 = 0;
const EXIT_ABORT: u8 = 1;
const EXIT_ERROR: u8 = 2;

fn main() -> ExitCode {
    let cli = Cli::parse();
    if cli.git_config {
        print!("{GIT_CONFIG}");
        return ExitCode::from(EXIT_OK);
    }
    match run(&cli) {
        Ok(code) => ExitCode::from(code),
        Err(e) => {
            eprintln!("wenas: {e:#}");
            ExitCode::from(EXIT_ERROR)
        }
    }
}

fn require_terminal() -> anyhow::Result<()> {
    if !std::io::stdout().is_terminal() {
        anyhow::bail!(
            "output is not a terminal; use `git diff --no-index` for non-interactive diffs"
        );
    }
    Ok(())
}

fn run(cli: &Cli) -> anyhow::Result<u8> {
    let (left, right) = (cli.left.as_deref().unwrap(), cli.right.as_deref().unwrap());
    if left.is_dir() != right.is_dir() {
        anyhow::bail!(
            "{} and {} must both be files or both be directories",
            left.display(),
            right.display()
        );
    }
    if left.is_dir() {
        return run_dir(cli, left, right);
    }
    if left.as_os_str() == "-" && right.as_os_str() == "-" {
        anyhow::bail!("only one side may read from stdin");
    }
    let cfg = config::load(cli.config.as_deref())?;

    if !confirm_large_files(&[left, right])? {
        eprintln!("wenas: skipped");
        return Ok(EXIT_OK);
    }
    let (ll, rl) = (read_input(left)?, read_input(right)?);
    let is_bin = |b: &[u8]| input::is_binary(b) || input::has_utf16_bom(b);
    if is_bin(&ll.bytes) || is_bin(&rl.bytes) {
        let Some(first) = input::first_difference(&ll.bytes, &rl.bytes) else {
            println!("identical");
            return Ok(EXIT_OK);
        };
        require_terminal()?;
        let info = BinaryInfo {
            name: display_name(right, cli.label_right.as_deref()),
            left_size: ll.bytes.len(),
            right_size: rl.bytes.len(),
            first_difference: Some(first),
            utf16: input::has_utf16_bom(&ll.bytes) || input::has_utf16_bom(&rl.bytes),
        };
        return interactive(&cfg, |theme, size| App::binary_screen(info, theme, size));
    }

    let mut l = document_from_loaded(ll, Some(left));
    let mut r = document_from_loaded(rl, Some(right));
    l.label = cli.label_left.clone();
    r.label = cli.label_right.clone();
    mark_readonly(
        &mut l,
        left,
        cli.readonly_left.then_some("marked --readonly-left"),
    );
    mark_readonly(
        &mut r,
        right,
        cli.readonly_right.then_some("marked --readonly-right"),
    );
    let ws = if cli.ignore_all_space || cli.ignore_space_change {
        cli.whitespace()
    } else {
        cfg.whitespace.into()
    };

    let view_cfg = ViewConfig {
        tab_width: cfg.tab_width,
        unified_below_columns: cfg.unified_below_columns,
        force_unified: cli.unified,
    };

    // Small inputs diff inline (microseconds). Large ones run on a worker thread
    // so the screen can show progress and `Esc` can cancel (spec §10).
    if l.len() + r.len() > BACKGROUND_DIFF_LINES && std::io::stdout().is_terminal() {
        let (tx, rx) = std::sync::mpsc::channel();
        let (lc, rc) = (l.clone(), r.clone());
        std::thread::spawn(move || {
            let _ = tx.send(diff(&lc, &rc, ws));
        });
        let hunks = match rx.recv_timeout(std::time::Duration::from_millis(300)) {
            Ok(h) => h,
            Err(_) => {
                let identical = Arc::new(AtomicBool::new(false));
                let flag = identical.clone();
                let code = interactive(&cfg, move |theme, size| {
                    Loading::new(rx, (l, r), ws, theme, view_cfg, size, flag)
                })?;
                if identical.load(Ordering::Relaxed) {
                    println!("identical");
                }
                return Ok(code);
            }
        };
        if hunks.is_empty() {
            println!("identical");
            return Ok(EXIT_OK);
        }
        return interactive(&cfg, move |theme, size| {
            App::new(Session::from_parts(l, r, ws, hunks), theme, view_cfg, size)
        });
    }

    // No hunks (identical, or only line-ending/ignored-whitespace differences)
    // never opens the UI.
    if diff(&l, &r, ws).is_empty() {
        println!("identical");
        return Ok(EXIT_OK);
    }
    require_terminal()?;
    interactive(&cfg, move |theme, size| {
        App::new(Session::new(l, r, ws), theme, view_cfg, size)
    })
}

/// Above this many lines (both sides together) the first diff runs in the background.
const BACKGROUND_DIFF_LINES: usize = 50_000;

fn display_name(path: &Path, label: Option<&str>) -> String {
    label
        .map(str::to_owned)
        .unwrap_or_else(|| path.display().to_string())
}

/// Raw mode, theme detection, alternate screen, event loop, restore.
fn interactive<U: Ui>(
    cfg: &config::Config,
    build: impl FnOnce(Theme, (u16, u16)) -> U,
) -> anyhow::Result<u8> {
    let guard = TerminalGuard::raw()?;
    // The OSC 11 query must run in raw mode but before the alternate screen.
    let dark = resolve_dark(cfg.theme);
    let no_color = std::env::var_os("NO_COLOR").is_some_and(|v| !v.is_empty());
    let depth = Depth::detect();
    let mut theme = Theme::new(dark, depth, no_color);
    theme.apply_overrides(&cfg.colors, depth);
    let mut terminal = guard.enter_screen()?;
    let mut app = build(theme, terminal_size());
    let outcome = run_loop(&mut terminal, &mut app);
    drop(terminal);
    drop(guard);
    Ok(match outcome? {
        Outcome::Quit => EXIT_OK,
        Outcome::Abort => EXIT_ABORT,
        // Directory-only outcomes are handled inside the shell.
        Outcome::Back | Outcome::QuitAsk => EXIT_OK,
    })
}

/// Directory mode. The scan starts immediately; if it finishes within a moment
/// and found nothing, behave like identical files and never open the UI.
fn run_dir(cli: &Cli, left: &Path, right: &Path) -> anyhow::Result<u8> {
    let cfg = config::load(cli.config.as_deref())?;
    let scan = Scan::spawn(
        left.to_path_buf(),
        right.to_path_buf(),
        cfg.directory.exclude.clone(),
    );
    let mut first = Vec::new();
    // With a terminal, don't hold the UI back for long; without one there is no UI
    // to open, so wait for the whole scan to learn whether anything differs.
    let wait = if std::io::stdout().is_terminal() {
        std::time::Duration::from_millis(150)
    } else {
        std::time::Duration::from_secs(60 * 60 * 24)
    };
    let finished = scan.drain_for(wait, &mut first);
    let any_difference = first
        .iter()
        .any(|m| matches!(m, Msg::Entry(e) if e.status.differs()));
    if finished && !any_difference {
        println!("identical");
        return Ok(EXIT_OK);
    }
    require_terminal()?;
    let view = ViewConfig {
        tab_width: cfg.tab_width,
        unified_below_columns: cfg.unified_below_columns,
        force_unified: cli.unified,
    };
    let ws = if cli.ignore_all_space || cli.ignore_space_change {
        cli.whitespace()
    } else {
        cfg.whitespace.into()
    };
    let show_identical = cli.show_identical || cfg.directory.show_identical;
    let (l, r) = (left.to_path_buf(), right.to_path_buf());
    let (ro_l, ro_r) = (cli.readonly_left, cli.readonly_right);
    interactive(&cfg, move |theme, size| {
        let dir = DirView::new(l, r, show_identical, scan, first);
        let opts = ShellOptions {
            theme,
            view,
            ws,
            readonly_left: ro_l,
            readonly_right: ro_r,
            large_file_bytes: input::LARGE_FILE_BYTES,
        };
        Shell::new(dir, opts, size)
    })
}

/// Ask before loading anything over 100 MB (spec §9). Files over 1 GB are
/// refused outright. Returns false when the user declines.
fn confirm_large_files(paths: &[&Path]) -> anyhow::Result<bool> {
    let mut large = Vec::new();
    for p in paths {
        if let input::SizeCheck::Large(n) =
            input::check_size(p, input::LARGE_FILE_BYTES, input::MAX_FILE_BYTES)?
        {
            large.push((p, n));
        }
    }
    if large.is_empty() {
        return Ok(true);
    }
    if !std::io::stdout().is_terminal() {
        // Nothing to ask and nothing to show; scripts get the plain behaviour.
        return Ok(true);
    }
    let list: Vec<String> = large
        .iter()
        .map(|(p, n)| format!("{} ({})", p.display(), input::human_size(*n)))
        .collect();
    let prompt = format!(
        "wenas: {} is large; loading may be slow. Load it anyway? [y/N] ",
        list.join(", ")
    );
    let mut tty = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open("/dev/tty")
        .map_err(|_| {
            anyhow::anyhow!(
                "{} is large and there is no terminal to ask on",
                list.join(", ")
            )
        })?;
    use std::io::{BufRead, Write};
    tty.write_all(prompt.as_bytes())?;
    let mut answer = String::new();
    std::io::BufReader::new(&tty).read_line(&mut answer)?;
    Ok(matches!(
        answer.trim().to_ascii_lowercase().as_str(),
        "y" | "yes"
    ))
}
