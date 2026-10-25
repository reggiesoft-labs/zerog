//! Structured error types for the core library boundary.
//!
//! This module provides a unified error type for the core library's public API.
//! All core library operations return [`CoreResult<T>`] which uses [`CoreError`]
//! for error reporting.
//!
//! # Error Categories
//!
//! - [`CoreError::PermissionDenied`]: Missing accessibility/input permissions
//! - [`CoreError::Network`]: UDP socket or network communication failures
//! - [`CoreError::Config`]: Configuration file parsing or validation errors
//! - [`CoreError::Engine`]: Platform engine initialization or runtime errors
//! - [`CoreError::Discovery`]: mDNS service discovery failures
//! - [`CoreError::Connection`]: Peer connection establishment or maintenance
//! - [`CoreError::Platform`]: Platform-specific operation failures
//! - [`CoreError::InvalidInput`]: Invalid parameters passed to API functions

use thiserror::Error;

/// Errors returned by core library operations.
///
/// Each variant includes a human-readable description of the error context.
#[derive(Debug, Clone, Error)]
pub enum CoreError {
    /// Required system permissions are not granted (e.g., accessibility on macOS).
    #[error("permission denied: {0}")]
    PermissionDenied(String),

    /// Network communication failed (socket errors, packet failures).
    #[error("network error: {0}")]
    Network(String),

    /// Configuration file could not be loaded, parsed, or validated.
    #[error("configuration error: {0}")]
    Config(String),

    /// Platform engine failed to initialize or encountered a runtime error.
    #[error("engine error: {0}")]
    Engine(String),

    /// mDNS service discovery failed to start or find peers.
    #[error("discovery error: {0}")]
    Discovery(String),

    /// Peer connection failed or was unexpectedly terminated.
    #[error("connection error: {0}")]
    Connection(String),

    /// Platform-specific operation failed (display, input capture/injection).
    #[error("platform error: {0}")]
    Platform(String),

    /// Invalid input was provided to an API function.
    #[error("invalid input: {0}")]
    InvalidInput(String),
}

/// Convenience alias for core library results.
pub type CoreResult<T> = std::result::Result<T, CoreError>;

impl From<anyhow::Error> for CoreError {
    /// Converts an anyhow error into a CoreError.
    ///
    /// If the anyhow error already wraps a CoreError, that error is returned.
    /// Otherwise, the error is wrapped in [`CoreError::Engine`].
    fn from(err: anyhow::Error) -> Self {
        if let Some(core_error) = err.downcast_ref::<CoreError>() {
            return core_error.clone();
        }

        CoreError::Engine(format!("{err:#}"))
    }
}
