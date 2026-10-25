//! Linux session detection for X11 and Wayland environments.
//!
//! This module uses environment variables and runtime signals to determine
//! whether to use the X11 or Wayland backend implementation.
//!
//! # Signals
//!
//! - `MOUSE_THING_LINUX_BACKEND` (override)
//! - `XDG_SESSION_TYPE` (environment variable)
//! - `WAYLAND_DISPLAY` (Wayland signal)
//! - `DISPLAY` (X11 signal)

use std::env;

use super::backend::LinuxBackendKind;
use super::error::LinuxPlatformError;

const BACKEND_OVERRIDE_ENV: &str = "MOUSE_THING_LINUX_BACKEND";

/// Source of session detection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DetectionSource {
    /// Detection was overridden by `MOUSE_THING_LINUX_BACKEND`.
    Override,
    /// Detection was based on Wayland signals.
    WaylandEnv,
    /// Detection was based on X11 signals.
    X11Env,
}

impl DetectionSource {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Override => "override",
            Self::WaylandEnv => "environment:wayland",
            Self::X11Env => "environment:x11",
        }
    }
}

/// Detected session configuration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SessionDetection {
    /// Detected backend kind (X11 or Wayland).
    pub(crate) backend: LinuxBackendKind,
    /// Source of detection.
    pub(crate) source: DetectionSource,
}

/// Detects the active Linux session by inspecting environment variables.
pub(crate) fn detect_session() -> Result<SessionDetection, LinuxPlatformError> {
    let raw_override = read_non_empty_env(BACKEND_OVERRIDE_ENV);
    let session_type =
        read_non_empty_env("XDG_SESSION_TYPE").map(|value| value.trim().to_ascii_lowercase());
    let has_wayland_display = read_non_empty_env("WAYLAND_DISPLAY").is_some();
    let has_x_display = read_non_empty_env("DISPLAY").is_some();

    detect_session_from_signals(
        raw_override.as_deref(),
        session_type.as_deref(),
        has_wayland_display,
        has_x_display,
    )
}

fn detect_session_from_signals(
    backend_override: Option<&str>,
    session_type: Option<&str>,
    has_wayland_display: bool,
    has_x_display: bool,
) -> Result<SessionDetection, LinuxPlatformError> {
    if let Some(raw_override) = backend_override {
        return parse_backend_override(raw_override).map(|backend| SessionDetection {
            backend,
            source: DetectionSource::Override,
        });
    }

    let is_wayland_session = matches!(session_type, Some("wayland"));
    if has_wayland_display || is_wayland_session {
        return Ok(SessionDetection {
            backend: LinuxBackendKind::Wayland,
            source: DetectionSource::WaylandEnv,
        });
    }

    let is_x11_session = matches!(session_type, Some("x11"));
    if has_x_display || is_x11_session {
        return Ok(SessionDetection {
            backend: LinuxBackendKind::X11,
            source: DetectionSource::X11Env,
        });
    }

    Err(LinuxPlatformError::NoDisplayServerDetected)
}

fn parse_backend_override(raw_override: &str) -> Result<LinuxBackendKind, LinuxPlatformError> {
    match raw_override.trim().to_ascii_lowercase().as_str() {
        "wayland" => Ok(LinuxBackendKind::Wayland),
        "x11" | "xorg" => Ok(LinuxBackendKind::X11),
        _ => Err(LinuxPlatformError::InvalidBackendOverride {
            value: raw_override.to_string(),
            env_var: BACKEND_OVERRIDE_ENV,
        }),
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

    #[test]
    fn override_selects_wayland() {
        let detection = detect_session_from_signals(Some("wayland"), None, false, true).unwrap();
        assert_eq!(detection.backend, LinuxBackendKind::Wayland);
        assert_eq!(detection.source, DetectionSource::Override);
    }

    #[test]
    fn override_accepts_xorg_alias() {
        let detection =
            detect_session_from_signals(Some("xorg"), Some("wayland"), true, true).unwrap();
        assert_eq!(detection.backend, LinuxBackendKind::X11);
        assert_eq!(detection.source, DetectionSource::Override);
    }

    #[test]
    fn override_rejects_invalid_value() {
        let err = detect_session_from_signals(Some("unknown"), Some("x11"), true, true)
            .expect_err("invalid override should fail");
        match err {
            LinuxPlatformError::InvalidBackendOverride { value, env_var } => {
                assert_eq!(value, "unknown");
                assert_eq!(env_var, BACKEND_OVERRIDE_ENV);
            }
            other => panic!("unexpected error: {other}"),
        }
    }

    #[test]
    fn wayland_signals_win_over_x11() {
        let detection = detect_session_from_signals(None, Some("x11"), true, true).unwrap();
        assert_eq!(detection.backend, LinuxBackendKind::Wayland);
        assert_eq!(detection.source, DetectionSource::WaylandEnv);
    }

    #[test]
    fn x11_selected_when_only_x11_signals_present() {
        let detection = detect_session_from_signals(None, Some("x11"), false, false).unwrap();
        assert_eq!(detection.backend, LinuxBackendKind::X11);
        assert_eq!(detection.source, DetectionSource::X11Env);
    }

    #[test]
    fn no_display_server_returns_error() {
        let err = detect_session_from_signals(None, None, false, false)
            .expect_err("no display server should fail");
        assert!(matches!(err, LinuxPlatformError::NoDisplayServerDetected));
    }

    #[test]
    #[serial_test::serial]
    fn override_env_selects_backend_through_process_env() {
        let saved = std::env::var(BACKEND_OVERRIDE_ENV).ok();
        unsafe { std::env::set_var(BACKEND_OVERRIDE_ENV, "wayland") };
        let detection = detect_session();
        match saved {
            Some(previous) => unsafe { std::env::set_var(BACKEND_OVERRIDE_ENV, previous) },
            None => unsafe { std::env::remove_var(BACKEND_OVERRIDE_ENV) },
        }
        let detection = detection.expect("override should select a backend");
        assert_eq!(detection.backend, LinuxBackendKind::Wayland);
        assert_eq!(detection.source, DetectionSource::Override);
    }
}
