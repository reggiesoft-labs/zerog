//! Linux platform implementation with support for X11 and Wayland.
//!
//! This module coordinates runtime detection of the display server and
//! dispatches work to either the X11 or Wayland backend implementations.
//!
//! # Backend Selection
//!
//! On Linux, backends are selected based on the `XDG_SESSION_TYPE`,
//! `WAYLAND_DISPLAY`, and `DISPLAY` environment variables. If detection
//! is incorrect, the `MOUSE_THING_LINUX_BACKEND` environment variable
//! can override the choice to `x11` or `wayland`.
//!
//! # Runtime Requirements
//!
//! Backends are checked for required capabilities (capture, injection,
//! display discovery) and permissions (access to X server or portal
//! services) before starting the engine.

use anyhow::{Result, anyhow, bail};
use std::path::PathBuf;
use std::thread;
use std::time::Duration;
use tracing::{info, warn};

use crate::platform::PeerEdge;

mod backend;
mod detect;
mod error;
#[cfg(test)]
pub(crate) mod live_test;
pub(crate) mod keycodes;
pub(crate) mod wayland;
pub(crate) mod x11;

use backend::{CapabilityStatus, LinuxBackendKind, PermissionState, select_linux_backend};

const TRANSIENT_RETRY_ATTEMPTS: usize = 3;
const TRANSIENT_RETRY_DELAY_MS: u64 = 150;

/// Required capabilities for specific CLI commands.
#[derive(Clone, Copy)]
struct CommandRequirements {
    /// Whether the command needs to capture local input.
    require_capture: bool,
    /// Whether the command needs to inject synthetic events.
    require_injection: bool,
    /// Whether the command needs display topology information.
    require_display_topology: bool,
    /// Whether the command needs to check for permissions.
    require_permission: bool,
}

impl CommandRequirements {
    const LOOPBACK: Self = Self {
        require_capture: true,
        require_injection: true,
        require_display_topology: false,
        require_permission: true,
    };

    #[allow(dead_code)]
    const START: Self = Self {
        require_capture: true,
        require_injection: true,
        require_display_topology: true,
        require_permission: true,
    };

    #[allow(dead_code)]
    const LIST: Self = Self {
        require_capture: false,
        require_injection: false,
        require_display_topology: true,
        require_permission: false,
    };

    const PERMISSION_CHECK: Self = Self {
        require_capture: false,
        require_injection: false,
        require_display_topology: false,
        require_permission: false,
    };

    fn validate(self, selected: &backend::SelectedLinuxBackend) -> Result<()> {
        if self.require_capture {
            ensure_capability("capture", &selected.capabilities.capture)?;
        }
        if self.require_injection {
            ensure_capability("injection", &selected.capabilities.injection)?;
        }
        if self.require_display_topology {
            ensure_capability("display topology", &selected.capabilities.display_topology)?;
        }
        if self.require_permission {
            ensure_permission(&selected.capabilities.permission)?;
        }
        Ok(())
    }
}

/// Creates separate platform and session engines sharing the same display handle.
pub fn create_engine_pair(
    peer_edge: PeerEdge,
) -> Result<(std::sync::Arc<dyn crate::platform::PlatformEngine>, std::sync::Arc<dyn crate::platform::SessionEngine>)> {
    let selected =
        select_backend_with_requirements("create_engine_pair", CommandRequirements::PERMISSION_CHECK)?;
    match selected.kind {
        LinuxBackendKind::X11 => Ok(crate::sys::os::create_x11_engine_pair(peer_edge)),
        LinuxBackendKind::Wayland => crate::sys::os::create_wayland_engine_pair(peer_edge),
    }
}

/// Runs a local loopback test (capture → injection) for debugging.
pub fn run_phase1_loopback() -> Result<()> {
    let selected = select_backend_with_requirements("loopback", CommandRequirements::LOOPBACK)?;
    match selected.kind {
        LinuxBackendKind::X11 => x11::run_loopback(),
        LinuxBackendKind::Wayland => wayland::run_loopback(),
    }
}

/// Runs diagnostics for the current Linux environment.
pub fn run_doctor() -> Result<()> {
    let selected = select_backend_with_retry("doctor", || {
        select_linux_backend().map_err(|err| anyhow!(err))
    })?;
    log_selected_backend("doctor", &selected);

    println!(
        "doctor: linux backend={} detection_source={}",
        selected.kind.as_str(),
        selected.detection.source.as_str()
    );

    print_capability("capture", &selected.capabilities.capture);
    print_capability("injection", &selected.capabilities.injection);
    print_capability("display_topology", &selected.capabilities.display_topology);
    print_permission(&selected.capabilities.permission);

    match selected.kind {
        LinuxBackendKind::X11 => x11::permission::run_doctor_report(),
        LinuxBackendKind::Wayland => wayland::permission::run_doctor_report(),
    }
}

/// Ensures required input permissions are granted for the current session.
pub fn check_accessibility_permission() -> Result<()> {
    let selected = select_backend_with_requirements(
        "permission-check",
        CommandRequirements::PERMISSION_CHECK,
    )?;
    match selected.kind {
        LinuxBackendKind::X11 => x11::permission::ensure_accessibility_trusted("start"),
        LinuxBackendKind::Wayland => wayland::permission::ensure_accessibility_trusted("start"),
    }
}

pub fn run_phase3_manual(_peer: String, _listen_port: u16, _peer_edge: PeerEdge) -> Result<()> {
    anyhow::bail!("Manual peer mode is removed; use `mouse-thing start` with mDNS discovery")
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
    anyhow::bail!("Phase 4 runtime is deprecated; use the GUI instead")
}

pub fn run_phase4_list(_listen_port: u16) -> Result<()> {
    anyhow::bail!("Phase 4 peer listing is deprecated; use the GUI instead")
}

fn select_backend_with_requirements(
    command_name: &'static str,
    requirements: CommandRequirements,
) -> Result<backend::SelectedLinuxBackend> {
    let selected = select_backend_with_retry(command_name, || {
        select_linux_backend().map_err(|err| anyhow!(err))
    })?;
    log_selected_backend(command_name, &selected);
    requirements.validate(&selected)?;
    Ok(selected)
}

fn log_selected_backend(command_name: &'static str, selected: &backend::SelectedLinuxBackend) {
    info!(
        command = command_name,
        backend = selected.kind.as_str(),
        detection_source = selected.detection.source.as_str(),
        capture = selected.capabilities.capture.summary(),
        injection = selected.capabilities.injection.summary(),
        display_topology = selected.capabilities.display_topology.summary(),
        permission = selected.capabilities.permission.summary(),
        "linux backend selected"
    );
}

fn select_backend_with_retry<T>(
    operation: &'static str,
    mut f: impl FnMut() -> Result<T>,
) -> Result<T> {
    for attempt in 1..=TRANSIENT_RETRY_ATTEMPTS {
        match f() {
            Ok(value) => {
                if attempt > 1 {
                    info!(operation, attempt, "linux backend operation recovered");
                }
                return Ok(value);
            }
            Err(err) if is_transient_error(&err) && attempt < TRANSIENT_RETRY_ATTEMPTS => {
                warn!(
                    operation,
                    attempt,
                    error = %err,
                    "transient linux backend error; retrying"
                );
                thread::sleep(Duration::from_millis(
                    TRANSIENT_RETRY_DELAY_MS * attempt as u64,
                ));
            }
            Err(err) => return Err(err),
        }
    }

    unreachable!("retry loop exits on success or final error")
}

fn is_transient_error(err: &anyhow::Error) -> bool {
    let message = err.to_string().to_ascii_lowercase();
    [
        "timed out",
        "timeout",
        "temporarily unavailable",
        "resource busy",
        "connection reset",
        "connection aborted",
        "broken pipe",
        "would block",
    ]
    .iter()
    .any(|marker| message.contains(marker))
}

fn ensure_capability(name: &str, status: &CapabilityStatus) -> Result<()> {
    match status {
        CapabilityStatus::Available => Ok(()),
        CapabilityStatus::Unavailable { remediation, .. } => {
            bail!("Linux capability `{name}` unavailable. {remediation}")
        }
    }
}

fn ensure_permission(permission: &PermissionState) -> Result<()> {
    match permission {
        PermissionState::Granted => Ok(()),
        PermissionState::Denied { remediation }
        | PermissionState::InteractiveRequired { remediation } => {
            bail!("Linux input permission unavailable. {remediation}")
        }
    }
}

fn print_capability(name: &str, status: &CapabilityStatus) {
    println!("doctor: capability.{name}={}", status.summary());
    if let Some(reason) = status.reason() {
        println!("doctor: capability.{name}.reason={reason}");
    }
    if let Some(remediation) = status.remediation() {
        println!("doctor: capability.{name}.remediation={remediation}");
    }
}

fn print_permission(permission: &PermissionState) {
    println!("doctor: permission={}", permission.summary());
    if let Some(remediation) = permission.remediation() {
        println!("doctor: permission.remediation={remediation}");
    }
}
