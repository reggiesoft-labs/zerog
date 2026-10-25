//! Platform entrypoints and runtime factory for OS-specific implementations.

use anyhow::Result;
use std::path::PathBuf;
use std::sync::Arc;

// Re-export core platform types so that submodules can use `crate::platform::PeerEdge` etc.
pub use crate::core::platform::{
    CapturePolicy, CapturedInputEvent, DisplayBounds, DisplayManager, EventCapture, EventInjection,
    LocalCoords, PeerEdge, PermissionChecker, PlatformEngine, PlatformEngineTestExt, Point,
    RuntimeEngine, SessionEngine, TestPlatformEngine, TransferMode, VirtualCoords,
};

pub mod shared;

#[cfg(all(test, not(target_os = "macos")))]
#[path = "platform/macos/click_state.rs"]
mod macos_click_state;

#[cfg(windows)]
pub use windows::service;

#[cfg(target_os = "macos")]
pub(crate) mod macos;
#[cfg(target_os = "macos")]
use macos as platform_impl;

#[cfg(target_os = "linux")]
pub(crate) mod linux;
#[cfg(target_os = "linux")]
use linux as platform_impl;

#[cfg(target_os = "windows")]
pub(crate) mod windows;
#[cfg(target_os = "windows")]
use windows as platform_impl;

/// Creates separate platform and session engines sharing the same display handle.
pub fn create_engine_pair(
    peer_edge: PeerEdge,
) -> Result<(Arc<dyn PlatformEngine>, Arc<dyn SessionEngine>)> {
    platform_impl::create_engine_pair(peer_edge)
}

pub fn begin_shutdown() {
    #[cfg(all(target_os = "linux", feature = "linux-wayland"))]
    linux::wayland::portal::begin_shutdown();
}

/// Runs the local loopback smoke test.
pub fn run_phase1_loopback() -> Result<()> {
    platform_impl::run_phase1_loopback()
}

/// Runs the platform doctor command.
pub fn run_doctor() -> Result<()> {
    platform_impl::run_doctor()
}

/// Verifies required accessibility/input permissions.
pub fn check_accessibility_permission() -> Result<()> {
    platform_impl::check_accessibility_permission()
}

/// Releases foreground focus from the GUI window to prevent hook starvation.
pub fn release_gui_focus() {
    #[cfg(target_os = "windows")]
    platform_impl::release_gui_focus();
}

/// Runs the manual peer connection command.
pub fn run_phase3_manual(peer: String, listen_port: u16, peer_edge: PeerEdge) -> Result<()> {
    platform_impl::run_phase3_manual(peer, listen_port, peer_edge)
}

/// Starts phase 4 runtime with networking and event settings.
pub fn run_phase4_start(
    listen_port: u16,
    peer_edge: PeerEdge,
    preferred_peer_name: Option<String>,
    batch_window_us: u64,
    max_events_per_packet: usize,
    movement_cadence_us: u64,
    metrics_log_path: Option<PathBuf>,
) -> Result<()> {
    platform_impl::run_phase4_start(
        listen_port,
        peer_edge,
        preferred_peer_name,
        batch_window_us,
        max_events_per_packet,
        movement_cadence_us,
        metrics_log_path,
    )
}

/// Lists discovered peers for phase 4 networking.
pub fn run_phase4_list(listen_port: u16) -> Result<()> {
    platform_impl::run_phase4_list(listen_port)
}

#[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
mod platform_impl {
    use anyhow::{Result, bail};
    use std::path::PathBuf;
    use std::sync::Arc;

    use super::{PeerEdge, PlatformEngine, SessionEngine};

    const UNSUPPORTED_PLATFORM: &str = "This OS is not supported by mouse-thing yet";

    pub fn run_phase1_loopback() -> Result<()> {
        bail!(UNSUPPORTED_PLATFORM)
    }

    pub fn run_doctor() -> Result<()> {
        bail!(UNSUPPORTED_PLATFORM)
    }

    pub fn check_accessibility_permission() -> Result<()> {
        bail!(UNSUPPORTED_PLATFORM)
    }

    pub fn run_phase3_manual(_peer: String, _listen_port: u16, _peer_edge: PeerEdge) -> Result<()> {
        bail!(UNSUPPORTED_PLATFORM)
    }

    pub fn run_phase4_start(
        _listen_port: u16,
        _peer_edge: PeerEdge,
        _preferred_peer_name: Option<String>,
        _batch_window_us: u64,
        _max_events_per_packet: usize,
        _movement_cadence_us: u64,
        _metrics_log_path: Option<PathBuf>,
    ) -> Result<()> {
        bail!(UNSUPPORTED_PLATFORM)
    }

    pub fn run_phase4_list(_listen_port: u16) -> Result<()> {
        bail!(UNSUPPORTED_PLATFORM)
    }

    pub fn create_engine_pair(
        _peer_edge: PeerEdge,
    ) -> Result<(Arc<dyn PlatformEngine>, Arc<dyn SessionEngine>)> {
        bail!(UNSUPPORTED_PLATFORM)
    }
}
