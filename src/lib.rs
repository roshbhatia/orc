pub mod animation;
pub mod cli;
pub mod config;
pub mod control;
pub mod control_plane;
pub mod daemon;
pub mod domain;
pub mod mcp;
pub mod preferences;
pub mod provider;
pub mod state;
pub mod tui;
pub mod workflow;

#[cfg(test)]
pub(crate) mod test_support;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");
