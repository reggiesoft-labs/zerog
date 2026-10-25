//! Wayland event-capture implementation with native libei and XWayland fallback.
//!
//! This module provides input capture for Wayland sessions. It supports two
//! backends:
//!
//! - **NativeLibei**: Uses the xdg-desktop-portal InputCapture interface with
//!   libei (via the `reis` crate) for native Wayland capture. This is the
//!   preferred backend on compositors that support it (GNOME 45+, KDE 6+).
//!
//! - **XWayland**: Falls back to X11-based capture via XWayland when native
//!   libei is not available (e.g., wlroots-based compositors like sway).

use anyhow::{Result, bail};
use std::sync::Arc;
use std::sync::atomic::AtomicU8;
use tokio::sync::mpsc;

use crate::platform::{CapturePolicy, CapturedInputEvent, EventCapture, PeerEdge};

use super::super::x11::capture::X11EventCapture;
use super::session;

#[cfg(feature = "linux-wayland")]
use super::libei_capture;

enum CaptureBackend {
    #[cfg(feature = "linux-wayland")]
    NativeLibei(libei_capture::LibeiCapture),
    XWayland(X11EventCapture),
    Unavailable,
}

/// Wayland event capture implementation.
pub(crate) struct WaylandEventCapture {
    backend: CaptureBackend,
}

impl WaylandEventCapture {
    pub(crate) fn new() -> Self {
        Self::with_peer_edge(
            PeerEdge::Right,
            Arc::new(AtomicU8::new(CapturePolicy::DeliverLocally as u8)),
        )
    }

    #[allow(dead_code)]
    pub(crate) fn with_capture_policy(capture_policy: Arc<AtomicU8>) -> Self {
        Self::with_peer_edge(PeerEdge::Right, capture_policy)
    }

    pub(crate) fn with_peer_edge(_peer_edge: PeerEdge, capture_policy: Arc<AtomicU8>) -> Self {
        let probe = session::probe_wayland_support();

        // Try native libei first if the portal + libei stack is ready.
        #[cfg(feature = "linux-wayland")]
        if probe.libei_ready {
            match libei_capture::LibeiCapture::with_peer_edge(_peer_edge, Arc::clone(&capture_policy)) {
                Ok(capture) => {
                    tracing::info!("Using native libei capture");
                    return Self {
                        backend: CaptureBackend::NativeLibei(capture),
                    };
                }
                Err(e) => {
                    tracing::warn!("Failed to create libei capture: {e}, falling back");
                }
            }
        }

        // Fall back to XWayland.
        if probe.xwayland_fallback_available {
            tracing::info!("Using XWayland fallback capture");
            return Self {
                backend: CaptureBackend::XWayland(X11EventCapture::with_capture_policy(
                    Arc::clone(&capture_policy),
                )),
            };
        }

        Self {
            backend: CaptureBackend::Unavailable,
        }
    }

    pub(crate) fn take_event_receiver(&self) -> Option<mpsc::Receiver<CapturedInputEvent>> {
        match &self.backend {
            #[cfg(feature = "linux-wayland")]
            CaptureBackend::NativeLibei(inner) => inner.take_event_receiver(),
            CaptureBackend::XWayland(inner) => inner.take_event_receiver(),
            CaptureBackend::Unavailable => None,
        }
    }
}

// Only run capture-backend tests on Linux where the probe actually works.
#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;

    /// When libei is reported not ready and XWayland fallback is available,
    /// the XWayland backend should be selected.
    #[test]
    #[serial_test::serial]
    fn backend_selection_xwayland_when_libei_not_ready() {
        // Disable libei so NativeLibei path is skipped, but leave XWayland
        // available (MOUSE_THING_WAYLAND_XWAYLAND_FALLBACK not set, so real
        // X11 probe runs — on a CI box without X11 that returns false, so
        // we force it too).
        unsafe {
            std::env::set_var("MOUSE_THING_WAYLAND_LIBEI_READY", "false");
            std::env::set_var("MOUSE_THING_WAYLAND_XWAYLAND_FALLBACK", "true");
        }
        let capture = WaylandEventCapture::new();
        // Verify the selected backend is XWayland (not NativeLibei or Unavailable).
        match &capture.backend {
            CaptureBackend::XWayland(_) => {}
            #[cfg(feature = "linux-wayland")]
            CaptureBackend::NativeLibei(_) => {
                panic!("expected XWayland backend but got NativeLibei")
            }
            CaptureBackend::Unavailable => {
                panic!("expected XWayland backend but got Unavailable")
            }
        }
        unsafe {
            std::env::remove_var("MOUSE_THING_WAYLAND_LIBEI_READY");
            std::env::remove_var("MOUSE_THING_WAYLAND_XWAYLAND_FALLBACK");
        }
    }

    /// When both libei and XWayland fallback are unavailable,
    /// the Unavailable backend should be selected.
    #[test]
    #[serial_test::serial]
    fn backend_selection_unavailable_when_no_fallback() {
        unsafe {
            std::env::set_var("MOUSE_THING_WAYLAND_LIBEI_READY", "false");
            std::env::set_var("MOUSE_THING_WAYLAND_XWAYLAND_FALLBACK", "false");
        }
        let capture = WaylandEventCapture::new();
        match &capture.backend {
            CaptureBackend::Unavailable => {}
            #[cfg(feature = "linux-wayland")]
            CaptureBackend::NativeLibei(_) => {
                panic!("expected Unavailable backend but got NativeLibei")
            }
            CaptureBackend::XWayland(_) => {
                panic!("expected Unavailable backend but got XWayland")
            }
        }
        unsafe {
            std::env::remove_var("MOUSE_THING_WAYLAND_LIBEI_READY");
            std::env::remove_var("MOUSE_THING_WAYLAND_XWAYLAND_FALLBACK");
        }
    }
}

impl EventCapture for WaylandEventCapture {
    fn install_tap(&self) -> Result<()> {
        match &self.backend {
            #[cfg(feature = "linux-wayland")]
            CaptureBackend::NativeLibei(inner) => inner.install_tap(),
            CaptureBackend::XWayland(inner) => inner.install_tap(),
            CaptureBackend::Unavailable => bail!(
                "Wayland capture is unavailable: portal+libei capture path is not active and XWayland fallback is unavailable. {}",
                session::required_libei_remediation()
            ),
        }
    }

    fn uninstall_tap(&self) -> Result<()> {
        match &self.backend {
            #[cfg(feature = "linux-wayland")]
            CaptureBackend::NativeLibei(inner) => inner.uninstall_tap(),
            CaptureBackend::XWayland(inner) => inner.uninstall_tap(),
            CaptureBackend::Unavailable => Ok(()),
        }
    }

    fn is_installed(&self) -> bool {
        match &self.backend {
            #[cfg(feature = "linux-wayland")]
            CaptureBackend::NativeLibei(inner) => inner.is_installed(),
            CaptureBackend::XWayland(inner) => inner.is_installed(),
            CaptureBackend::Unavailable => false,
        }
    }
}
