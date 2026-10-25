//! Wayland permission checks and diagnostics reporting.
//!
//! This module handles the permission model for Wayland, which often
//! involves asynchronous portal requests or checking for XWayland
//! compatibility.

use anyhow::{Result, bail};
#[cfg(feature = "linux-wayland")]
use std::sync::Arc;

use crate::platform::{DisplayManager, PermissionChecker};

use super::display;
use super::session;

/// Wayland implementation of [`PermissionChecker`].
///
/// When the `linux-wayland` feature is enabled, this holds an `Arc<PortalSession>`
/// and uses it for real portal permission checking. Otherwise, it falls back to
/// the stub implementation in `session.rs`.
pub(crate) struct WaylandPermissionChecker {
    #[cfg(feature = "linux-wayland")]
    portal: Option<Arc<super::portal::PortalSession>>,
}

impl WaylandPermissionChecker {
    /// Create a new permission checker without a portal session.
    #[allow(dead_code)]
    pub(crate) fn new() -> Self {
        Self {
            #[cfg(feature = "linux-wayland")]
            portal: None,
        }
    }

    /// Create a new permission checker with an existing portal session.
    #[cfg(feature = "linux-wayland")]
    pub(crate) fn with_portal(portal: Arc<super::portal::PortalSession>) -> Self {
        Self {
            portal: Some(portal),
        }
    }
}

impl PermissionChecker for WaylandPermissionChecker {
    fn check_accessibility_permission(&self) -> bool {
        #[cfg(feature = "linux-wayland")]
        if let Some(ref portal) = self.portal {
            // If we have a portal session, check if RD is active.
            if portal.rd_active() {
                return true;
            }
        }

        session::probe_wayland_support().permission_granted()
    }

    fn prompt_accessibility_permission(&self) -> Result<()> {
        #[cfg(feature = "linux-wayland")]
        {
            // Get or create the global portal session and create an RD session,
            // which triggers the user-facing permission dialog.
            let portal = match self.portal {
                Some(ref p) => Arc::clone(p),
                None => super::portal::get_or_create_portal_session()?,
            };
            portal.create_rd_session()?;
            return Ok(());
        }

        #[cfg(not(feature = "linux-wayland"))]
        {
            session::request_portal_permission()
        }
    }
}

pub(crate) fn run_doctor_report() -> Result<()> {
    let probe = session::probe_wayland_support();

    println!(
        "doctor: wayland.socket_access={}",
        if probe.wayland_socket_reachable {
            "pass"
        } else {
            "fail"
        }
    );
    println!(
        "doctor: wayland.portal.available={}",
        if probe.portal_service_available {
            "pass"
        } else {
            "fail"
        }
    );
    println!(
        "doctor: wayland.portal.permission={}",
        if probe.portal_permission_granted {
            "granted"
        } else {
            "interactive-required"
        }
    );
    println!(
        "doctor: wayland.libei.ready={}",
        if probe.libei_ready { "pass" } else { "fail" }
    );

    // Capture/injection backend selection.
    let capture_backend = if cfg!(feature = "linux-wayland") && probe.libei_ready {
        "NativeLibei"
    } else if probe.xwayland_fallback_available {
        "XWayland"
    } else {
        "Unavailable"
    };
    println!("doctor: wayland.capture_backend={capture_backend}");

    // Output protocol path.
    println!(
        "doctor: wayland.output.path={}",
        display::output_protocol_path()
    );
    println!(
        "doctor: wayland.xwayland_fallback={}",
        if probe.xwayland_fallback_available {
            if probe.using_xwayland_fallback() {
                "active"
            } else {
                "available"
            }
        } else {
            "unavailable"
        }
    );

    // Display topology.
    let display_manager = display::WaylandDisplayManager::new();
    match display_manager.discover_displays() {
        Ok(displays) => {
            println!("doctor: wayland.displays.count={}", displays.len());
            for (i, b) in displays.iter().enumerate() {
                println!(
                    "doctor: wayland.display[{i}].bounds={}x{}+{}+{}",
                    b.width, b.height, b.x, b.y
                );
            }
        }
        Err(e) => {
            println!("doctor: wayland.displays=error ({e})");
        }
    }

    if probe.permission_granted() {
        return Ok(());
    }

    if !probe.portal_service_available {
        bail!(
            "Wayland RemoteDesktop portal is unavailable. {}",
            session::required_portal_remediation()
        );
    }

    bail!(
        "Wayland RemoteDesktop permission is required. {}",
        session::permission_remediation()
    )
}

/// Log the selected capture, injection, and display backends at startup.
///
/// Call this once during initialisation after the probe has been run, so
/// that the chosen backends are visible in the application log.
#[allow(dead_code)]
pub(crate) fn log_backend_selection() {
    let probe = session::probe_wayland_support();

    let capture_backend = if cfg!(feature = "linux-wayland") && probe.libei_ready {
        "NativeLibei"
    } else if probe.xwayland_fallback_available {
        "XWayland"
    } else {
        "Unavailable"
    };

    let output_path = display::output_protocol_path();

    tracing::info!(
        capture_backend,
        output_path,
        xwayland_active = probe.using_xwayland_fallback(),
        "Wayland backends selected"
    );
}

pub(crate) fn ensure_accessibility_trusted(command_name: &str) -> Result<()> {
    let probe = session::probe_wayland_support();
    if probe.permission_granted() {
        return Ok(());
    }

    if probe.portal_service_available {
        session::request_portal_permission()?;
        if session::probe_wayland_support().permission_granted() {
            return Ok(());
        }
    }

    bail!(
        "Wayland input permission is required before running `mouse-thing {command_name}`. {}",
        session::permission_remediation()
    )
}
