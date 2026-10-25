//! Windows permission checks and diagnostics.
//!
//! On Windows, global hooks and input injection (`SendInput`) generally do
//! not require specialized Accessibility permissions, unlike macOS.
//! However, running with Administrator privileges might be required if
//! the target application is also elevated.

use anyhow::Result;

use crate::platform::PermissionChecker;

/// Windows implementation of [`PermissionChecker`].
///
/// On Windows, this is mostly a no-op as standard user accounts
/// can usually install low-level hooks.
pub(crate) struct WindowsPermissionChecker;

impl WindowsPermissionChecker {
    pub(crate) fn new() -> Self {
        Self
    }
}

impl PermissionChecker for WindowsPermissionChecker {
    fn check_accessibility_permission(&self) -> bool {
        true
    }

    fn prompt_accessibility_permission(&self) -> Result<()> {
        Ok(())
    }
}

/// Runs the `doctor` command diagnostics for Windows permissions.
pub(super) fn run_doctor() -> Result<()> {
    println!("doctor: accessibility permission is granted on Windows");
    println!("doctor: admin rights are not required for baseline WH_MOUSE_LL + SendInput flow");
    let status = super::service::client::query_status();
    println!("doctor: windows.elevated={}", status.elevated);
    println!(
        "doctor: windows.input_service={}",
        match (status.service_installed, status.service_running) {
            (true, true) => "installed,running",
            (true, false) => "installed,stopped",
            (false, _) => "missing",
        }
    );
    println!(
        "doctor: windows.service_reachable={}",
        status.service_reachable
    );
    println!(
        "doctor: windows.secure_desktop={}",
        match status.secure_desktop_enabled {
            Some(true) => "enabled",
            Some(false) => "disabled",
            None => "unknown",
        }
    );
    if !status.service_running {
        println!("doctor: windows.uac_note=UAC prompts on the secure desktop need the input service; standard SendInput cannot reach them");
    }
    Ok(())
}

pub(super) fn check_accessibility_permission() -> Result<()> {
    Ok(())
}
