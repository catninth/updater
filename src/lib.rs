//! GitHub release checks, update orchestration, and signed installer tooling.
//!
//! The core has no GUI dependency. Enable `tauri` for the native installer adapter.
//! See the README and examples for lifecycle hooks and packaging.

#![doc = include_str!("../README.md")]

mod config;
mod error;
pub mod github;
pub mod installer;
mod model;
mod updater;

#[cfg(feature = "tauri")]
pub mod tauri;

pub use config::{Config, Repository};
pub use error::{Error, Result};
pub use model::{DownloadProgress, Event, Operation, Phase, Release, UpdateState};
pub use semver::Version;
pub use updater::{
    DownloadCallback, InstallBackend, InstallHooks, InstallPermit, PollingHandle, PreparedUpdate,
    ReleaseSource, Updater, UpdaterBuilder,
};
