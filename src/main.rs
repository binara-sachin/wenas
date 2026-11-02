use clap::Parser;
use std::io::IsTerminal;
use std::path::Path;
use std::process::ExitCode;
use wenas::cli::{Cli, GIT_CONFIG};
use wenas::config;
use wenas::diff::diff;
use wenas::input::{self, read_input};
use wenas::ops::Session;
use wenas::save::document_from_loaded;
use wenas::theme::{Depth, Theme, resolve_dark};
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
        anyhow::bail!("directory comparison is not implemented yet (milestone M4)");
    }
    if left.as_os_str() == "-" && right.as_os_str() == "-" {
        anyhow::bail!("only one side may read from stdin");
    }
    let cfg = config::load(cli.config.as_deref())?;

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
    l.readonly |= cli.readonly_left;
    r.readonly |= cli.readonly_right;
    let ws = if cli.ignore_all_space || cli.ignore_space_change {
        cli.whitespace()
    } else {
        cfg.whitespace.into()
    };

    // No hunks (identical, or only line-ending/ignored-whitespace differences)
    // never opens the UI.
    if diff(&l, &r, ws).is_empty() {
        println!("identical");
        return Ok(EXIT_OK);
    }
    require_terminal()?;
    let view_cfg = ViewConfig {
        tab_width: cfg.tab_width,
        unified_below_columns: cfg.unified_below_columns,
        force_unified: cli.unified,
    };
    interactive(&cfg, move |theme, size| {
        App::new(Session::new(l, r, ws), theme, view_cfg, size)
    })
}

fn display_name(path: &Path, label: Option<&str>) -> String {
    label
        .map(str::to_owned)
        .unwrap_or_else(|| path.display().to_string())
}

/// Raw mode, theme detection, alternate screen, event loop, restore.
fn interactive(
    cfg: &config::Config,
    build: impl FnOnce(Theme, (u16, u16)) -> App,
) -> anyhow::Result<u8> {
    let guard = TerminalGuard::raw()?;
    // The OSC 11 query must run in raw mode but before the alternate screen.
    let dark = resolve_dark(cfg.theme);
    let no_color = std::env::var_os("NO_COLOR").is_some_and(|v| !v.is_empty());
    let theme = Theme::new(dark, Depth::detect(), no_color);
    let mut terminal = guard.enter_screen()?;
    let mut app = build(theme, terminal_size());
    let outcome = run_loop(&mut terminal, &mut app);
    drop(terminal);
    drop(guard);
    Ok(match outcome? {
        Outcome::Quit => EXIT_OK,
        Outcome::Abort => EXIT_ABORT,
    })
}
