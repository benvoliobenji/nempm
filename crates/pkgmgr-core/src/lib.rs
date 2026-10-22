//! The package manager's core: everything that touches the network or a drive.
//!
//! This crate has no UI dependencies. The Tauri app calls into it from its
//! commands and forwards [`progress::Progress`] events to the frontend, and the
//! integration tests drive it against a temporary folder posing as a drive.

pub mod cache;
pub mod check;
pub mod drives;
pub mod eject;
pub mod error;
pub mod fsutil;
pub mod http;
pub mod index;
pub mod install;
pub mod manifest;
pub mod model;
pub mod paths;
pub mod profile;
pub mod progress;
pub mod uninstall;

pub use error::{Error, Result};

/// Recorded in each drive manifest's `writtenBy`.
pub const WRITTEN_BY: &str = concat!("pkgmgr ", env!("CARGO_PKG_VERSION"));
