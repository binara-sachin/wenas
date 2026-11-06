//! Terminal UI (spec §6, §7): file view, help, binary screen.

pub mod app;
pub mod chrome;
pub mod dir;
pub mod render;
pub mod shell;
pub mod term;

pub use app::{App, BinaryInfo, Outcome, View, ViewConfig};

/// What a paste adds to a one-line prompt: the first line, without control
/// characters (tabs become spaces).
pub(crate) fn paste_line(text: &str) -> String {
    text.split(['\n', '\r'])
        .next()
        .unwrap_or("")
        .chars()
        .map(|c| if c == '\t' { ' ' } else { c })
        .filter(|c| !c.is_control())
        .collect()
}

/// Shown when text is pasted where nothing takes text.
pub(crate) const PASTE_IGNORED: &str = "paste ignored (only the / search prompt takes text)";
