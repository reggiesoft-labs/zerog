//! mouse-thing platform implementation library crate.
//!
//! This crate provides platform implementations (macOS, Windows, Linux), the
//! in-tree core domain module, and shared runtime wiring.
//!
//! # Module Structure
//!
//! - [`platform`]: The entry point for registering platform-specific engines
//!   and creating host-native instances.
//! - Re-exports: [`discovery`] and [`events`] from [`crate::core`].

#![allow(unexpected_cfgs)]

pub use crate::core::discovery;
pub use crate::core::events;

pub mod core;
pub mod logging;
pub(crate) mod sys;

pub mod quic {
    pub use crate::platform::shared::quic::{
        CertificateStore, PeerConnection, QuicEndpoint, cert_fingerprint, max_datagram_size,
    };
}

pub mod platform;
pub use platform::release_gui_focus;

/// Installs the process-wide Rustls crypto provider.
///
/// QUIC setup requires a provider to be installed before any TLS configuration
/// is constructed on worker threads.
pub fn install_crypto_provider() {
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
}
