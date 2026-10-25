//! Linux backend selection and capability reporting.

use super::detect::{self, SessionDetection};
use super::error::LinuxPlatformError;
use super::wayland;
use super::x11::permission;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LinuxBackendKind {
    X11,
    Wayland,
}

impl LinuxBackendKind {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::X11 => "x11",
            Self::Wayland => "wayland",
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) enum CapabilityStatus {
    Available,
    Unavailable {
        reason: &'static str,
        remediation: &'static str,
    },
}

impl CapabilityStatus {
    fn from_probe(available: bool, reason: &'static str, remediation: &'static str) -> Self {
        if available {
            Self::Available
        } else {
            Self::Unavailable {
                reason,
                remediation,
            }
        }
    }

    pub(crate) fn summary(&self) -> &'static str {
        match self {
            Self::Available => "available",
            Self::Unavailable { .. } => "unavailable",
        }
    }

    pub(crate) fn reason(&self) -> Option<&'static str> {
        match self {
            Self::Available => None,
            Self::Unavailable { reason, .. } => Some(*reason),
        }
    }

    pub(crate) fn remediation(&self) -> Option<&'static str> {
        match self {
            Self::Available => None,
            Self::Unavailable { remediation, .. } => Some(*remediation),
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) enum PermissionState {
    Granted,
    Denied { remediation: &'static str },
    InteractiveRequired { remediation: &'static str },
}

impl PermissionState {
    pub(crate) fn summary(&self) -> &'static str {
        match self {
            Self::Granted => "granted",
            Self::Denied { .. } => "denied",
            Self::InteractiveRequired { .. } => "interactive-required",
        }
    }

    pub(crate) fn remediation(&self) -> Option<&'static str> {
        match self {
            Self::Granted => None,
            Self::Denied { remediation } | Self::InteractiveRequired { remediation } => {
                Some(*remediation)
            }
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct LinuxBackendCapabilities {
    pub(crate) capture: CapabilityStatus,
    pub(crate) injection: CapabilityStatus,
    pub(crate) display_topology: CapabilityStatus,
    pub(crate) permission: PermissionState,
}

#[derive(Debug, Clone)]
pub(crate) struct SelectedLinuxBackend {
    pub(crate) kind: LinuxBackendKind,
    pub(crate) detection: SessionDetection,
    pub(crate) capabilities: LinuxBackendCapabilities,
}

pub(crate) fn select_linux_backend() -> Result<SelectedLinuxBackend, LinuxPlatformError> {
    let detection = detect::detect_session()?;
    let capabilities = match detection.backend {
        LinuxBackendKind::X11 => x11_capabilities(),
        LinuxBackendKind::Wayland => wayland_capabilities(),
    };

    Ok(SelectedLinuxBackend {
        kind: detection.backend,
        detection,
        capabilities,
    })
}

fn x11_capabilities() -> LinuxBackendCapabilities {
    let probe = permission::probe_x11_support();
    let display_remediation = permission::display_access_remediation();

    LinuxBackendCapabilities {
        capture: CapabilityStatus::from_probe(
            probe.can_capture(),
            "X11 display access is unavailable for capture",
            display_remediation,
        ),
        injection: CapabilityStatus::from_probe(
            probe.can_inject(),
            "X11 XTEST extension is unavailable for injection",
            permission::required_extension_remediation(),
        ),
        display_topology: CapabilityStatus::from_probe(
            probe.can_discover_topology(),
            "X11 display topology discovery cannot access X server",
            display_remediation,
        ),
        permission: if probe.permission_granted() {
            PermissionState::Granted
        } else {
            PermissionState::Denied {
                remediation: display_remediation,
            }
        },
    }
}

fn wayland_capabilities() -> LinuxBackendCapabilities {
    let probe = wayland::session::probe_wayland_support();
    let libei_remediation = wayland::session::required_libei_remediation();
    let portal_remediation = wayland::session::required_portal_remediation();

    LinuxBackendCapabilities {
        capture: CapabilityStatus::from_probe(
            probe.can_capture(),
            "Wayland capture is unavailable (portal/libei not ready and XWayland fallback is unavailable)",
            libei_remediation,
        ),
        injection: CapabilityStatus::from_probe(
            probe.can_inject(),
            "Wayland injection is unavailable (portal/libei not ready and XWayland fallback is unavailable)",
            libei_remediation,
        ),
        display_topology: CapabilityStatus::from_probe(
            probe.can_discover_topology(),
            "Wayland display topology discovery could not access session outputs",
            portal_remediation,
        ),
        permission: wayland_permission(probe.permission_granted(), probe.portal_service_available),
    }
}

fn wayland_permission(granted: bool, portal_available: bool) -> PermissionState {
    if granted {
        PermissionState::Granted
    } else if portal_available {
        PermissionState::InteractiveRequired {
            remediation: wayland::session::permission_remediation(),
        }
    } else {
        PermissionState::Denied {
            remediation: wayland::session::required_portal_remediation(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;
    use std::env;

    const BACKEND_OVERRIDE_ENV: &str = "MOUSE_THING_LINUX_BACKEND";
    const WAYLAND_KEYS: &[&str] = &[
        "WAYLAND_DISPLAY",
        "XDG_RUNTIME_DIR",
        "XDG_CURRENT_DESKTOP",
        "MOUSE_THING_WAYLAND_PORTAL_AVAILABLE",
        "MOUSE_THING_WAYLAND_PORTAL_GRANTED",
        "MOUSE_THING_WAYLAND_LIBEI_READY",
        "MOUSE_THING_WAYLAND_XWAYLAND_FALLBACK",
        "MOUSE_THING_WAYLAND_OUTPUT_PROTOCOL",
    ];

    fn wait_for_display() -> bool {
        let has_display = env::var("DISPLAY")
            .ok()
            .filter(|value| !value.trim().is_empty())
            .is_some();
        if !has_display {
            return false;
        }
        let started = std::time::Instant::now();
        while started.elapsed() < std::time::Duration::from_secs(20) {
            if x11rb::connect(None).is_ok() {
                return true;
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        false
    }

    struct SavedEnv {
        backend: Option<String>,
        wayland: Vec<(String, Option<String>)>,
    }

    impl SavedEnv {
        fn capture() -> Self {
            let wayland = WAYLAND_KEYS
                .iter()
                .map(|key| (key.to_string(), env::var(key).ok()))
                .collect();
            Self {
                backend: env::var(BACKEND_OVERRIDE_ENV).ok(),
                wayland,
            }
        }

        fn clear_wayland(&self) {
            for (key, _) in &self.wayland {
                unsafe { env::remove_var(key) };
            }
        }
    }

    impl Drop for SavedEnv {
        fn drop(&mut self) {
            match &self.backend {
                Some(previous) => unsafe { env::set_var(BACKEND_OVERRIDE_ENV, previous) },
                None => unsafe { env::remove_var(BACKEND_OVERRIDE_ENV) },
            }
            for (key, value) in &self.wayland {
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

    #[test]
    #[serial]
    fn x11_override_reports_live_capabilities() {
        let _saved = SavedEnv::capture();
        set(BACKEND_OVERRIDE_ENV, "x11");

        if !wait_for_display() {
            eprintln!("SKIP x11_override_reports_live_capabilities: no live X display");
            return;
        }

        let selected = select_linux_backend().expect("x11 selection should succeed");
        assert_eq!(selected.kind, LinuxBackendKind::X11);
        assert!(matches!(
            selected.capabilities.capture,
            CapabilityStatus::Available
        ));
        assert!(matches!(
            selected.capabilities.injection,
            CapabilityStatus::Available
        ));
        assert!(matches!(
            selected.capabilities.display_topology,
            CapabilityStatus::Available
        ));
        assert!(matches!(
            selected.capabilities.permission,
            PermissionState::Granted
        ));
    }

    #[test]
    #[serial]
    fn wayland_override_without_compositor_reports_remediation() {
        let saved = SavedEnv::capture();
        saved.clear_wayland();
        set(BACKEND_OVERRIDE_ENV, "wayland");
        set("MOUSE_THING_WAYLAND_PORTAL_AVAILABLE", "0");
        set("MOUSE_THING_WAYLAND_PORTAL_GRANTED", "0");
        set("MOUSE_THING_WAYLAND_LIBEI_READY", "0");
        set("MOUSE_THING_WAYLAND_XWAYLAND_FALLBACK", "0");

        let selected = select_linux_backend().expect("wayland selection should succeed");
        assert_eq!(selected.kind, LinuxBackendKind::Wayland);
        let capture = &selected.capabilities.capture;
        assert!(matches!(
            capture,
            CapabilityStatus::Unavailable { .. }
        ));
        assert!(capture.remediation().is_some());
        assert!(matches!(
            selected.capabilities.permission,
            PermissionState::Denied { .. }
        ));
    }

    #[test]
    #[serial]
    fn invalid_override_fails_selection() {
        let _saved = SavedEnv::capture();
        set(BACKEND_OVERRIDE_ENV, "not-a-backend");

        assert!(select_linux_backend().is_err());
    }
}
