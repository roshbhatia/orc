pub mod cli;
pub mod config;
pub mod control;
pub mod domain;
pub mod mcp;
pub mod preferences;
pub mod provider;
pub mod state;
pub mod tui;
pub mod workflow;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");
