//! Command line (spec §8).

use clap::Parser;
use std::path::PathBuf;

#[derive(Parser, Debug)]
#[command(name = "wenas", version, about = "A fast, minimal terminal diff tool")]
pub struct Cli {
    /// Left file or directory (`-` reads stdin)
    #[arg(required_unless_present = "git_config")]
    pub left: Option<PathBuf>,
    /// Right file or directory (`-` reads stdin)
    #[arg(required_unless_present = "git_config")]
    pub right: Option<PathBuf>,

    /// Pane title for the left side instead of its path
    #[arg(long, value_name = "TEXT")]
    pub label_left: Option<String>,
    /// Pane title for the right side instead of its path
    #[arg(long, value_name = "TEXT")]
    pub label_right: Option<String>,
    /// Force the left side read-only
    #[arg(long)]
    pub readonly_left: bool,
    /// Force the right side read-only
    #[arg(long)]
    pub readonly_right: bool,
    /// Ignore all whitespace when comparing
    #[arg(short = 'w', long, conflicts_with = "ignore_space_change")]
    pub ignore_all_space: bool,
    /// Ignore changes in the amount of whitespace
    #[arg(short = 'b', long)]
    pub ignore_space_change: bool,
    /// Start in unified view
    #[arg(long)]
    pub unified: bool,
    /// Directory view: show identical files from the start
    #[arg(long)]
    pub show_identical: bool,
    /// Use this config file instead of the default
    #[arg(long, value_name = "PATH")]
    pub config: Option<PathBuf>,
    /// Print the git configuration snippet and exit
    #[arg(long)]
    pub git_config: bool,
}

impl Cli {
    pub fn whitespace(&self) -> crate::diff::Whitespace {
        use crate::diff::Whitespace::*;
        if self.ignore_all_space {
            All
        } else if self.ignore_space_change {
            Change
        } else {
            Off
        }
    }
}

/// Exit codes (spec §8.3).
pub const EXIT_OK: u8 = 0;
pub const EXIT_ABORT: u8 = 1;
pub const EXIT_ERROR: u8 = 2;

/// Map how the UI ended to an exit code.
pub fn exit_code(outcome: crate::ui::Outcome) -> u8 {
    use crate::ui::Outcome;
    match outcome {
        Outcome::Abort => EXIT_ABORT,
        // `Back` and `QuitAsk` are handled inside the directory shell.
        Outcome::Quit
        | Outcome::Back
        | Outcome::QuitAsk
        | Outcome::Identical
        | Outcome::NextFile
        | Outcome::PrevFile => EXIT_OK,
    }
}

/// Turn a run's result into an exit code, reporting an error on stderr.
pub fn finish(result: anyhow::Result<u8>) -> u8 {
    match result {
        Ok(code) => code,
        Err(e) => {
            eprintln!("wenas: {e:#}");
            EXIT_ERROR
        }
    }
}

pub const GIT_CONFIG: &str = r#"[diff]
    tool = wenas
[difftool]
    prompt = false
    trustExitCode = true
[difftool "wenas"]
    cmd = wenas "$LOCAL" "$REMOTE"
[alias]
    dd = difftool --dir-diff
"#;
