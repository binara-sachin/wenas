//! Terminal UI (spec §6, §7): file view, help, binary screen.

pub mod app;
pub mod dir;
pub mod render;
pub mod shell;
pub mod term;

pub use app::{App, BinaryInfo, Outcome, View, ViewConfig};
