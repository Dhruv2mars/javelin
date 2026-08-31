pub mod cli;
pub mod commands;
pub mod config;
pub mod error;
pub mod fault;
pub mod model;
pub mod objects;
pub mod paths;
mod process;
pub mod store;
pub mod view;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");
