//! macOS accessibility permission checks and user guidance helpers.
//!
//! On macOS, input capture and injection require Accessibility permission.
//! This module provides utilities to check permission status and guide users
//! through the System Settings permission flow.
//!
//! # Permission Model
//!
//! macOS requires explicit user approval for Accessibility access. The permission
//! is per-application and must be granted through:
//! System Settings → Privacy & Security → Accessibility

use anyhow::{Result, bail};

use crate::platform::PermissionChecker;

use super::ffi;

/// macOS permission checker using the Accessibility API.
///
/// Delegates to FFI bindings that call `AXIsProcessTrusted` and related APIs.
pub(crate) struct MacosPermissionChecker;

impl MacosPermissionChecker {
    pub(crate) fn new() -> Self {
        Self
    }
}

impl PermissionChecker for MacosPermissionChecker {
    fn check_accessibility_permission(&self) -> bool {
        ffi::is_accessibility_trusted()
    }

    fn prompt_accessibility_permission(&self) -> Result<()> {
        ffi::open_accessibility_settings();
        print_accessibility_instructions();
        bail!("Accessibility permission is required")
    }
}

/// Ensures accessibility permission before running a command.
///
/// If permission is not granted, opens System Settings and prints instructions.
pub(super) fn ensure_accessibility_trusted(command_name: &str) -> Result<()> {
    if ffi::is_accessibility_trusted() {
        Ok(())
    } else {
        ffi::open_accessibility_settings();
        print_accessibility_instructions();
        bail!("Accessibility permission is required before running `mouse-thing {command_name}`")
    }
}

/// Runs the `doctor` command diagnostics for accessibility permission.
///
/// Reports permission status and provides remediation steps if denied.
pub(super) fn run_doctor() -> Result<()> {
    if ffi::is_accessibility_trusted() {
        println!("doctor: accessibility permission is granted");
        Ok(())
    } else {
        ffi::open_accessibility_settings();
        println!("doctor: accessibility permission is NOT granted");
        print_accessibility_instructions();
        bail!("Accessibility permission is required")
    }
}

/// Checks accessibility permission without prompting.
///
/// Returns `Ok(())` if granted, error otherwise.
pub(super) fn check_accessibility_permission() -> Result<()> {
    if ffi::is_accessibility_trusted() {
        Ok(())
    } else {
        bail!("Accessibility permission is required")
    }
}

/// Prints step-by-step instructions for granting Accessibility permission.
fn print_accessibility_instructions() {
    eprintln!("Accessibility permission is required for event tap capture/injection.");
    eprintln!("1) Open System Settings -> Privacy & Security -> Accessibility");
    eprintln!("2) Add and enable your terminal (or this binary)");
    eprintln!("3) Restart the command after permission is granted");
    eprintln!("Tip: run `mouse-thing doctor` to re-check permission status");
}
