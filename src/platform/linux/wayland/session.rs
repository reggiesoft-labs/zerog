//! Wayland session probing and runtime portal permission state.
//!
//! This module detects Wayland-specific capabilities such as portal
//! availability, libei support, and the presence of a Wayland socket.
//! It also manages the runtime state of portal permissions.

use anyhow::{Result, bail};
use std::env;
use std::path::Path;
use std::sync::{Mutex, OnceLock};

use super::super::x11;

const PORTAL_AVAILABLE_ENV: &str = "MOUSE_THING_WAYLAND_PORTAL_AVAILABLE";
const PORTAL_GRANTED_ENV: &str = "MOUSE_THING_WAYLAND_PORTAL_GRANTED";
const LIBEI_READY_ENV: &str = "MOUSE_THING_WAYLAND_LIBEI_READY";
const XWAYLAND_FALLBACK_ENV: &str = "MOUSE_THING_WAYLAND_XWAYLAND_FALLBACK";
const OUTPUT_PROTOCOL_ENV: &str = "MOUSE_THING_WAYLAND_OUTPUT_PROTOCOL";

/// Path used for display topology discovery in Wayland.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OutputProtocolPath {
    /// Use `wlr-output-management` (Sway, Hyprland, etc.)
    WlrOutputManagement,
    /// Use `xdg-output` (GNOME, Plasma, etc.)
    XdgOutput,
    /// Fallback to a single 1080p display if no protocol is available.
    SingleDisplayFallback,
}

impl OutputProtocolPath {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::WlrOutputManagement => "wlr-output-management",
            Self::XdgOutput => "xdg-output",
            Self::SingleDisplayFallback => "single-display-fallback",
        }
    }
}

/// Summary of Wayland-related capabilities and permissions detected at runtime.
#[derive(Debug, Clone)]
pub(crate) struct WaylandSupportProbe {
    /// Whether a Wayland socket (e.g., `wayland-0`) is reachable.
    pub(crate) wayland_socket_reachable: bool,
    /// Whether the `xdg-desktop-portal` service is available via DBus.
    pub(crate) portal_service_available: bool,
    /// Whether the user has granted RemoteDesktop permission via the portal.
    pub(crate) portal_permission_granted: bool,
    /// Whether the compositor supports the `libei` protocol for input.
    pub(crate) libei_ready: bool,
    /// Whether XWayland-based capture/injection is available as a fallback.
    pub(crate) xwayland_fallback_available: bool,
    /// The protocol used for display discovery.
    pub(crate) output_protocol: OutputProtocolPath,
}

impl WaylandSupportProbe {
    pub(crate) fn can_capture(&self) -> bool {
        (self.wayland_socket_reachable && self.portal_permission_granted && self.libei_ready)
            || self.xwayland_fallback_available
    }

    pub(crate) fn can_inject(&self) -> bool {
        self.can_capture()
    }

    pub(crate) fn can_discover_topology(&self) -> bool {
        self.wayland_socket_reachable || self.xwayland_fallback_available
    }

    pub(crate) fn permission_granted(&self) -> bool {
        self.portal_permission_granted || self.xwayland_fallback_available
    }

    pub(crate) fn using_xwayland_fallback(&self) -> bool {
        self.xwayland_fallback_available && (!self.portal_permission_granted || !self.libei_ready)
    }
}

#[derive(Debug, Default)]
struct RuntimePermissionState {
    portal_permission_granted: bool,
}

fn permission_state() -> &'static Mutex<RuntimePermissionState> {
    static STATE: OnceLock<Mutex<RuntimePermissionState>> = OnceLock::new();
    STATE.get_or_init(|| Mutex::new(RuntimePermissionState::default()))
}

pub(crate) fn probe_wayland_support() -> WaylandSupportProbe {
    let wayland_socket_reachable = wayland_socket_reachable();
    let portal_service_available = portal_service_available();
    let portal_permission_granted = portal_permission_granted();
    let libei_ready = libei_ready(wayland_socket_reachable, portal_service_available);
    let xwayland_fallback_available = xwayland_fallback_available();

    WaylandSupportProbe {
        wayland_socket_reachable,
        portal_service_available,
        portal_permission_granted,
        libei_ready,
        xwayland_fallback_available,
        output_protocol: select_output_protocol(wayland_socket_reachable),
    }
}

fn xwayland_fallback_available() -> bool {
    if let Some(value) = parse_bool_env(XWAYLAND_FALLBACK_ENV) {
        return value;
    }

    let x11_probe = x11::permission::probe_x11_support();
    x11_probe.can_capture() && x11_probe.can_inject()
}

pub(crate) fn request_portal_permission() -> Result<()> {
    if !portal_service_available() {
        bail!("RemoteDesktop portal service is unavailable")
    }

    if let Ok(mut state) = permission_state().lock() {
        state.portal_permission_granted = true;
    }
    Ok(())
}

pub(crate) fn required_portal_remediation() -> &'static str {
    "Install and start xdg-desktop-portal for your desktop session, then retry the command."
}

pub(crate) fn required_libei_remediation() -> &'static str {
    "Install compositor support for libei/RemoteDesktop input emulation or enable XWayland fallback."
}

pub(crate) fn permission_remediation() -> &'static str {
    "Grant RemoteDesktop input control in the portal prompt, or set MOUSE_THING_LINUX_BACKEND=x11."
}

fn wayland_socket_reachable() -> bool {
    let Some(display) = read_non_empty_env("WAYLAND_DISPLAY") else {
        return false;
    };

    let runtime_dir =
        read_non_empty_env("XDG_RUNTIME_DIR").unwrap_or_else(|| "/run/user".to_string());
    let socket_path = Path::new(&runtime_dir).join(display);
    socket_path.exists()
}

fn portal_service_available() -> bool {
    if let Some(value) = parse_bool_env(PORTAL_AVAILABLE_ENV) {
        return value;
    }

    // When the linux-wayland feature is enabled, use ashpd to probe
    // for the portal service directly (more reliable than dbus-send).
    #[cfg(feature = "linux-wayland")]
    {
        return super::portal::ashpd_portal_available();
    }

    // Fallback: use dbus-send when the feature is not enabled.
    #[cfg(not(feature = "linux-wayland"))]
    {
        let output = std::process::Command::new("dbus-send")
            .args([
                "--session",
                "--dest=org.freedesktop.DBus",
                "--type=method_call",
                "--print-reply",
                "/org/freedesktop/DBus",
                "org.freedesktop.DBus.NameHasOwner",
                "string:org.freedesktop.portal.Desktop",
            ])
            .output();

        let Ok(output) = output else {
            return false;
        };

        if !output.status.success() {
            return false;
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        stdout.contains("boolean true")
    }
}

fn portal_permission_granted() -> bool {
    if let Some(value) = parse_bool_env(PORTAL_GRANTED_ENV) {
        return value;
    }

    permission_state()
        .lock()
        .map(|state| state.portal_permission_granted)
        .unwrap_or(false)
}

fn libei_ready(wayland_socket_reachable: bool, portal_service_available: bool) -> bool {
    if let Some(value) = parse_bool_env(LIBEI_READY_ENV) {
        return value;
    }

    wayland_socket_reachable && portal_service_available
}

fn select_output_protocol(wayland_socket_reachable: bool) -> OutputProtocolPath {
    if let Some(value) = read_non_empty_env(OUTPUT_PROTOCOL_ENV) {
        match value.to_ascii_lowercase().as_str() {
            "wlr" | "wlr-output-management" => return OutputProtocolPath::WlrOutputManagement,
            "xdg" | "xdg-output" => return OutputProtocolPath::XdgOutput,
            "single" | "single-display" => return OutputProtocolPath::SingleDisplayFallback,
            _ => {}
        }
    }

    let desktop = read_non_empty_env("XDG_CURRENT_DESKTOP")
        .unwrap_or_default()
        .to_ascii_lowercase();
    if desktop.contains("sway")
        || desktop.contains("hypr")
        || desktop.contains("river")
        || desktop.contains("wayfire")
    {
        return OutputProtocolPath::WlrOutputManagement;
    }

    if wayland_socket_reachable {
        OutputProtocolPath::XdgOutput
    } else {
        OutputProtocolPath::SingleDisplayFallback
    }
}

fn parse_bool_env(key: &str) -> Option<bool> {
    let value = read_non_empty_env(key)?;
    match value.to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Some(true),
        "0" | "false" | "no" | "off" => Some(false),
        _ => None,
    }
}

fn read_non_empty_env(key: &str) -> Option<String> {
    env::var(key)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;

    const OVERRIDE_KEYS: &[&str] = &[
        "WAYLAND_DISPLAY",
        "XDG_RUNTIME_DIR",
        "XDG_CURRENT_DESKTOP",
        PORTAL_AVAILABLE_ENV,
        PORTAL_GRANTED_ENV,
        LIBEI_READY_ENV,
        XWAYLAND_FALLBACK_ENV,
        OUTPUT_PROTOCOL_ENV,
    ];

    struct SavedEnv {
        values: Vec<(String, Option<String>)>,
    }

    impl SavedEnv {
        fn capture() -> Self {
            let values = OVERRIDE_KEYS
                .iter()
                .map(|key| (key.to_string(), env::var(key).ok()))
                .collect();
            Self { values }
        }

        fn clear_probed(&self) {
            for (key, _) in &self.values {
                unsafe { env::remove_var(key) };
            }
        }
    }

    impl Drop for SavedEnv {
        fn drop(&mut self) {
            for (key, value) in &self.values {
                match value {
                    Some(previous) => unsafe { env::set_var(key, previous) },
                    None => unsafe { env::remove_var(key) },
                }
            }
        }
    }

    fn set(key: &str, value: &str) {
        unsafe { env::set_var(key, value) };
    }

    fn dead_overrides() {
        set(PORTAL_AVAILABLE_ENV, "0");
        set(PORTAL_GRANTED_ENV, "0");
        set(LIBEI_READY_ENV, "0");
        set(XWAYLAND_FALLBACK_ENV, "0");
    }

    fn live_overrides() {
        set(PORTAL_AVAILABLE_ENV, "1");
        set(PORTAL_GRANTED_ENV, "1");
        set(LIBEI_READY_ENV, "1");
        set(XWAYLAND_FALLBACK_ENV, "0");
    }

    fn fake_runtime_dir() -> tempfile::TempDir {
        tempfile::tempdir().expect("temp runtime dir should be created")
    }

    #[test]
    #[serial]
    fn bare_session_reports_all_unavailable() {
        let saved = SavedEnv::capture();
        saved.clear_probed();
        dead_overrides();
        let runtime = fake_runtime_dir();
        set("XDG_RUNTIME_DIR", runtime.path().to_str().unwrap());

        let probe = probe_wayland_support();
        assert!(!probe.wayland_socket_reachable);
        assert!(!probe.portal_service_available);
        assert!(!probe.portal_permission_granted);
        assert!(!probe.libei_ready);
        assert!(!probe.xwayland_fallback_available);
        assert!(!probe.can_capture());
        assert!(!probe.can_inject());
        assert!(!probe.can_discover_topology());
        assert!(!probe.permission_granted());
        assert_eq!(probe.output_protocol, OutputProtocolPath::SingleDisplayFallback);
    }

    #[test]
    #[serial]
    fn full_grant_enables_capture_and_injection() {
        let saved = SavedEnv::capture();
        saved.clear_probed();
        live_overrides();
        let runtime = fake_runtime_dir();
        let socket_path = runtime.path().join("wayland-test-0");
        std::fs::write(&socket_path, []).expect("fake socket file should be created");
        set("XDG_RUNTIME_DIR", runtime.path().to_str().unwrap());
        set("WAYLAND_DISPLAY", "wayland-test-0");

        let probe = probe_wayland_support();
        assert!(probe.wayland_socket_reachable);
        assert!(probe.can_capture());
        assert!(probe.can_inject());
        assert!(probe.can_discover_topology());
        assert!(probe.permission_granted());
        assert!(!probe.using_xwayland_fallback());
        assert_eq!(probe.output_protocol, OutputProtocolPath::XdgOutput);
    }

    #[test]
    #[serial]
    fn xwayland_fallback_covers_missing_portal_grant() {
        let saved = SavedEnv::capture();
        saved.clear_probed();
        dead_overrides();
        set(XWAYLAND_FALLBACK_ENV, "1");

        let probe = probe_wayland_support();
        assert!(probe.xwayland_fallback_available);
        assert!(probe.can_capture());
        assert!(probe.can_inject());
        assert!(probe.can_discover_topology());
        assert!(probe.permission_granted());
        assert!(probe.using_xwayland_fallback());
    }

    #[test]
    #[serial]
    fn missing_socket_file_is_unreachable() {
        let saved = SavedEnv::capture();
        saved.clear_probed();
        dead_overrides();
        let runtime = fake_runtime_dir();
        set("XDG_RUNTIME_DIR", runtime.path().to_str().unwrap());
        set("WAYLAND_DISPLAY", "wayland-test-missing");

        let probe = probe_wayland_support();
        assert!(!probe.wayland_socket_reachable);
        assert!(!probe.can_discover_topology());
    }

    #[test]
    #[serial]
    fn output_protocol_override_wins_over_desktop() {
        let saved = SavedEnv::capture();
        saved.clear_probed();
        dead_overrides();
        set("XDG_CURRENT_DESKTOP", "sway");
        set(OUTPUT_PROTOCOL_ENV, "xdg-output");

        let probe = probe_wayland_support();
        assert_eq!(probe.output_protocol, OutputProtocolPath::XdgOutput);
    }

    #[test]
    #[serial]
    fn desktop_heuristic_selects_wlr() {
        let saved = SavedEnv::capture();
        saved.clear_probed();
        dead_overrides();
        set("XDG_CURRENT_DESKTOP", "Hyprland");

        let probe = probe_wayland_support();
        assert_eq!(probe.output_protocol, OutputProtocolPath::WlrOutputManagement);
    }

    #[test]
    #[serial]
    fn portal_permission_request_flips_grant() {
        let saved = SavedEnv::capture();
        saved.clear_probed();
        set(PORTAL_AVAILABLE_ENV, "1");
        set(LIBEI_READY_ENV, "0");
        set(XWAYLAND_FALLBACK_ENV, "0");

        request_portal_permission().expect("portal request should succeed when available");
        let probe = probe_wayland_support();
        assert!(probe.portal_permission_granted);
        assert!(probe.permission_granted());
    }

    #[test]
    #[serial]
    fn portal_permission_request_fails_when_unavailable() {
        let saved = SavedEnv::capture();
        saved.clear_probed();
        set(PORTAL_AVAILABLE_ENV, "0");
        set(XWAYLAND_FALLBACK_ENV, "0");

        assert!(request_portal_permission().is_err());
    }
}
