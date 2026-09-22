//! Versioned records and lossless workspace transfer.
//!
//! Product adapters retain schemas, authorization and authored file layouts.
#![forbid(unsafe_code)]

#[cfg(all(feature = "filesystem", target_os = "linux"))]
mod archive;
#[cfg(all(feature = "filesystem", target_os = "linux"))]
mod filesystem;
mod model;
mod transfer;

#[cfg(all(feature = "filesystem", target_os = "linux"))]
pub use archive::{FileArchive, restore_files};
#[cfg(all(feature = "filesystem", target_os = "linux"))]
pub use filesystem::FileStore;
pub use model::*;
pub use transfer::*;
