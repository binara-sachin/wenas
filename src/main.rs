use clap::Parser;
use std::process::ExitCode;
use wenas::cli::{Cli, EXIT_OK, GIT_CONFIG, finish};

fn main() -> ExitCode {
    let cli = Cli::parse();
    if cli.git_config {
        print!("{GIT_CONFIG}");
        return ExitCode::from(EXIT_OK);
    }
    ExitCode::from(finish(wenas::run::run(&cli)))
}
