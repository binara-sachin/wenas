use clap::Parser;
use std::io::IsTerminal;
use std::path::Path;
use std::process::ExitCode;
use wenas::cli::{Cli, GIT_CONFIG};
use wenas::diff::diff;
use wenas::input::read_input;
use wenas::save::document_from_loaded;

/// Exit codes (spec §8.3).
const EXIT_OK: u8 = 0;
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

fn run(cli: &Cli) -> anyhow::Result<u8> {
    let (left, right) = (cli.left.as_deref().unwrap(), cli.right.as_deref().unwrap());
    let (l_dir, r_dir) = (is_dir(left), is_dir(right));
    if l_dir != r_dir {
        anyhow::bail!(
            "{} and {} must both be files or both be directories",
            left.display(),
            right.display()
        );
    }
    if l_dir {
        anyhow::bail!("directory comparison is not implemented yet (milestone M4)");
    }
    if left.as_os_str() == "-" && right.as_os_str() == "-" {
        anyhow::bail!("only one side may read from stdin");
    }

    let mut l = document_from_loaded(read_input(left)?, Some(left));
    let mut r = document_from_loaded(read_input(right)?, Some(right));
    l.label = cli.label_left.clone();
    r.label = cli.label_right.clone();
    l.readonly |= cli.readonly_left;
    r.readonly |= cli.readonly_right;

    // No hunks (identical, or only line-ending/ignored-whitespace differences)
    // never opens the UI.
    if diff(&l, &r, cli.whitespace()).is_empty() {
        println!("identical");
        return Ok(EXIT_OK);
    }
    if !std::io::stdout().is_terminal() {
        anyhow::bail!(
            "output is not a terminal; use `git diff --no-index` for non-interactive diffs"
        );
    }
    anyhow::bail!("the interactive viewer is not implemented yet (milestone M2)")
}

fn is_dir(p: &Path) -> bool {
    p.is_dir()
}
