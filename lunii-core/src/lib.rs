//! # lunii-core
//!
//! Lunii business logic: management of Lunii
//! (v1, v2, v3) and Flam storytellers, story import / export, STUdio
//! conversion, story databases and firmware download.
//!
//! The library is UI agnostic: progress and logs go through
//! [`report::Reporter`], and every long operation can be aborted through
//! [`report::Context`].

pub mod album;
pub mod archive;
pub mod constants;
pub mod convert;
pub mod crypto;
pub mod db;
pub mod device;
pub mod firmware;
pub mod format;
mod http;
pub mod mp3;
pub mod podcast;
pub mod report;
pub mod settings;
pub mod story;
pub mod tools;

pub use constants::{ArchiveType, DeviceVersion};
pub use device::{Device, FlamDevice, LuniiDevice, find_devices, open_device};
pub use report::{Aborted, Context, Level, Reporter};
pub use story::{Story, StoryList, StudioStory};

/// Version of the application.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
