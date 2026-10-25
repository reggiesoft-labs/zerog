//! In-tree core domain module for mouse-thing.
//!
//! This module contains the platform-neutral logic for the mouse-sharing tool, including:
//! - [`api`]: Public types for embedding.
//! - [`config`]: Application configuration and persistence.
//! - [`controller`]: Simplified application controller.
//! - [`discovery`]: mDNS peer discovery and registration.
//! - [`events`]: Platform-neutral event model and wire protocol.
//! - [`platform`]: Generic platform engine and subsystem traits.
//!
//! # Architecture
//!
//! The core is designed to be platform-neutral and transport-agnostic.
//! Platform-specific logic (for example macOS event capture) is provided by
//! modules under [`crate::platform`], which implement the traits defined in
//! [`platform`].

pub mod api;
pub mod config;

pub mod controller;
pub mod discovery;
pub mod error;
pub mod events;
pub mod handshake;
pub mod layout;
pub mod license;

pub mod pairing_code;
pub mod peer_state;
pub mod platform;
pub mod policies;
pub(crate) mod secure_fs;
pub mod transfer;

#[cfg(test)]
pub(crate) mod test_bytes;

pub use api::{ConfigSnapshot, PeerSnapshot, StatusSnapshot};
pub use controller::Controller;
pub use error::{CoreError, CoreResult};
