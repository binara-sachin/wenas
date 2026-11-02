//! Terminal UI (spec §6, §7): file view, help, binary screen.

pub mod app;
pub mod render;
pub mod term;

pub use app::{App, BinaryInfo, Outcome, View, ViewConfig};
