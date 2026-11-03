//! Running wenas: decide what the two arguments are and start the right screen
//! (spec §5.2 `cli`). `main` only parses arguments and maps the result to an
//! exit code.

use crate::cli::{Cli, EXIT_OK, exit_code};
use crate::config::{self, Config};
use crate::diff::{Whitespace, diff};
use crate::dircmp::Scan;
use crate::document::{Document, ReadOnlyReason};
use crate::gitenv::mark_readonly;
use crate::input::{self, read_input};
use crate::ops::Session;
use crate::ui::dir::DirView;
use crate::ui::loading::Loading;
use crate::ui::shell::{Shell, ShellOptions};
use crate::ui::term::{confirm_large_files, run_interactive};
use crate::ui::{App, BinaryInfo, ViewConfig};
use std::io::IsTerminal;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

/// Above this many lines (both sides together) the first diff runs in the background.
const BACKGROUND_DIFF_LINES: usize = 50_000;

/// The view and comparison settings shared by file and directory mode:
/// command-line flags win over the config file.
struct Settings {
    view: ViewConfig,
    ws: Whitespace,
}

impl Settings {
    fn new(cli: &Cli, cfg: &Config) -> Settings {
        Settings {
            view: ViewConfig {
                tab_width: cfg.tab_width,
                unified_below_columns: cfg.unified_below_columns,
                force_unified: cli.unified,
            },
            ws: if cli.ignore_all_space || cli.ignore_space_change {
                cli.whitespace()
            } else {
                cfg.whitespace.into()
            },
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

pub fn run(cli: &Cli) -> anyhow::Result<u8> {
    let (left, right) = (cli.left.as_deref().unwrap(), cli.right.as_deref().unwrap());
    if left.is_dir() != right.is_dir() {
        anyhow::bail!(
            "{} and {} must both be files or both be directories",
            left.display(),
            right.display()
        );
    }
    let cfg = config::load(cli.config.as_deref())?;
    if left.is_dir() {
        return run_dir(cli, &cfg, left, right);
    }
    if left.as_os_str() == "-" && right.as_os_str() == "-" {
        anyhow::bail!("only one side may read from stdin");
    }
    run_files(cli, &cfg, left, right)
}

fn load_document(
    path: &Path,
    loaded: input::Loaded,
    label: &Option<String>,
    forced: Option<ReadOnlyReason>,
) -> Document {
    let mut doc = Document::from_loaded(loaded, Some(path));
    doc.label = label.clone();
    mark_readonly(&mut doc, path, forced);
    doc
}

fn run_files(cli: &Cli, cfg: &Config, left: &Path, right: &Path) -> anyhow::Result<u8> {
    if !confirm_large_files(&[left, right])? {
        eprintln!("wenas: skipped");
        return Ok(EXIT_OK);
    }
    let (ll, rl) = (read_input(left)?, read_input(right)?);
    if input::looks_binary(&ll.bytes) || input::looks_binary(&rl.bytes) {
        let Some(_) = input::first_difference(&ll.bytes, &rl.bytes) else {
            println!("identical");
            return Ok(EXIT_OK);
        };
        require_terminal()?;
        let name = cli
            .label_right
            .clone()
            .unwrap_or_else(|| right.display().to_string());
        let info = BinaryInfo::from_bytes(name, &ll.bytes, &rl.bytes);
        let outcome = run_interactive(cfg, |theme, size| App::binary_screen(info, theme, size))?;
        return Ok(exit_code(outcome));
    }

    let l = load_document(
        left,
        ll,
        &cli.label_left,
        cli.readonly_left.then_some(ReadOnlyReason::FlagLeft),
    );
    let r = load_document(
        right,
        rl,
        &cli.label_right,
        cli.readonly_right.then_some(ReadOnlyReason::FlagRight),
    );
    let Settings { view, ws } = Settings::new(cli, cfg);

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
                let outcome = run_interactive(cfg, move |theme, size| {
                    Loading::new(rx, (l, r), ws, theme, view, size, flag)
                })?;
                if identical.load(Ordering::Relaxed) {
                    println!("identical");
                }
                return Ok(exit_code(outcome));
            }
        };
        if hunks.is_empty() {
            println!("identical");
            return Ok(EXIT_OK);
        }
        let outcome = run_interactive(cfg, move |theme, size| {
            App::new(Session::from_parts(l, r, ws, hunks), theme, view, size)
        })?;
        return Ok(exit_code(outcome));
    }

    // No hunks (identical, or only line-ending/ignored-whitespace differences)
    // never opens the UI.
    if diff(&l, &r, ws).is_empty() {
        println!("identical");
        return Ok(EXIT_OK);
    }
    require_terminal()?;
    let outcome = run_interactive(cfg, move |theme, size| {
        App::new(Session::new(l, r, ws), theme, view, size)
    })?;
    Ok(exit_code(outcome))
}

/// Directory mode (spec §6.3). The list always opens, even when nothing differs:
/// identical files can be shown with `i`, and results stream in while the scan
/// runs. Without a terminal there is nothing to show, so it exits 2 right away
/// (§8.3) rather than scanning first.
fn run_dir(cli: &Cli, cfg: &Config, left: &Path, right: &Path) -> anyhow::Result<u8> {
    require_terminal()?;
    let scan = Scan::spawn(
        left.to_path_buf(),
        right.to_path_buf(),
        cfg.directory.exclude.clone(),
    );
    let Settings { view, ws } = Settings::new(cli, cfg);
    let show_identical = cli.show_identical || cfg.directory.show_identical;
    let (l, r) = (left.to_path_buf(), right.to_path_buf());
    let (readonly_left, readonly_right) = (cli.readonly_left, cli.readonly_right);
    let outcome = run_interactive(cfg, move |theme, size| {
        let dir = DirView::new(l, r, show_identical, scan, Vec::new());
        let opts = ShellOptions {
            theme,
            view,
            ws,
            readonly_left,
            readonly_right,
            large_file_bytes: input::LARGE_FILE_BYTES,
        };
        Shell::new(dir, opts, size)
    })?;
    Ok(exit_code(outcome))
}
