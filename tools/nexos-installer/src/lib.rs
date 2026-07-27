#![allow(clippy::missing_errors_doc)]

pub mod disk;
pub mod install;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");
