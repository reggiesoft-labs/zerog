//! Wayland event-injection implementation with native libei and XWayland fallback.
//!
//! This module provides input synthesis for Wayland sessions. When the
//! `linux-wayland` feature is enabled, native libei injection via the
//! xdg-desktop-portal RemoteDesktop interface is attempted first. If libei
//! is unavailable (no portal, or portal denied) the implementation falls back
//! to XWayland using X11's XTEST extension.
//!
//! # Architecture
//!
//! - [`WaylandEventInjection`] dispatches to one of:
//!   - `NativeLibei` — native libei via the RemoteDesktop portal (feature-gated)
//!   - `XWayland`    — X11 XTEST through the XWayland compatibility layer
//!   - `Unavailable` — neither path is active; all calls return an error

use anyhow::Result;

use crate::platform::EventInjection;

use super::super::x11::injection::X11EventInjection;
use super::session;

#[cfg(feature = "linux-wayland")]
use super::libei_injection::LibeiInjection;

enum InjectionBackend {
    #[cfg(feature = "linux-wayland")]
    NativeLibei(LibeiInjection),
    XWayland(X11EventInjection),
    Unavailable,
}

/// Wayland event injection implementation.
pub(crate) struct WaylandEventInjection {
    backend: InjectionBackend,
}

impl WaylandEventInjection {
    pub(crate) fn new() -> Self {
        // When the linux-wayland feature is available, try native libei first.
        #[cfg(feature = "linux-wayland")]
        {
            match LibeiInjection::new() {
                Ok(libei) => {
                    tracing::info!("Wayland injection: using native libei (RemoteDesktop portal)");
                    return Self {
                        backend: InjectionBackend::NativeLibei(libei),
                    };
                }
                Err(e) => {
                    tracing::warn!(
                        "Wayland injection: native libei unavailable ({e:#}); trying XWayland fallback"
                    );
                }
            }
        }

        // Fall back to XWayland if available.
        let probe = session::probe_wayland_support();
        if probe.xwayland_fallback_available {
            tracing::info!("Wayland injection: using XWayland fallback");
            return Self {
                backend: InjectionBackend::XWayland(X11EventInjection::new()),
            };
        }

        tracing::warn!("Wayland injection: no backend available");
        Self {
            backend: InjectionBackend::Unavailable,
        }
    }

    fn unavailable_error() -> anyhow::Error {
        anyhow::anyhow!(
            "Wayland injection is unavailable: portal+libei injection path is not active and XWayland fallback is unavailable. {}",
            session::required_libei_remediation()
        )
    }
}

impl EventInjection for WaylandEventInjection {
    fn inject_mouse_move(&self, x: i32, y: i32) -> Result<()> {
        match &self.backend {
            #[cfg(feature = "linux-wayland")]
            InjectionBackend::NativeLibei(inner) => inner.inject_mouse_move(x, y),
            InjectionBackend::XWayland(inner) => inner.inject_mouse_move(x, y),
            InjectionBackend::Unavailable => Err(Self::unavailable_error()),
        }
    }

    fn inject_mouse_move_relative(&self, dx: i32, dy: i32) -> Result<()> {
        match &self.backend {
            #[cfg(feature = "linux-wayland")]
            InjectionBackend::NativeLibei(inner) => inner.inject_mouse_move_relative(dx, dy),
            InjectionBackend::XWayland(inner) => inner.inject_mouse_move_relative(dx, dy),
            InjectionBackend::Unavailable => Err(Self::unavailable_error()),
        }
    }

    fn inject_mouse_button(&self, button: u8, pressed: bool) -> Result<()> {
        match &self.backend {
            #[cfg(feature = "linux-wayland")]
            InjectionBackend::NativeLibei(inner) => inner.inject_mouse_button(button, pressed),
            InjectionBackend::XWayland(inner) => inner.inject_mouse_button(button, pressed),
            InjectionBackend::Unavailable => Err(Self::unavailable_error()),
        }
    }

    fn inject_mouse_wheel(&self, delta_x: i32, delta_y: i32) -> Result<()> {
        match &self.backend {
            #[cfg(feature = "linux-wayland")]
            InjectionBackend::NativeLibei(inner) => inner.inject_mouse_wheel(delta_x, delta_y),
            InjectionBackend::XWayland(inner) => inner.inject_mouse_wheel(delta_x, delta_y),
            InjectionBackend::Unavailable => Err(Self::unavailable_error()),
        }
    }

    fn inject_key_press(&self, keycode: u32) -> Result<()> {
        match &self.backend {
            #[cfg(feature = "linux-wayland")]
            InjectionBackend::NativeLibei(inner) => inner.inject_key_press(keycode),
            InjectionBackend::XWayland(inner) => inner.inject_key_press(keycode),
            InjectionBackend::Unavailable => Err(Self::unavailable_error()),
        }
    }

    fn inject_key_release(&self, keycode: u32) -> Result<()> {
        match &self.backend {
            #[cfg(feature = "linux-wayland")]
            InjectionBackend::NativeLibei(inner) => inner.inject_key_release(keycode),
            InjectionBackend::XWayland(inner) => inner.inject_key_release(keycode),
            InjectionBackend::Unavailable => Err(Self::unavailable_error()),
        }
    }

    fn set_cursor_hidden(&self, hidden: bool) -> Result<()> {
        match &self.backend {
            #[cfg(feature = "linux-wayland")]
            InjectionBackend::NativeLibei(inner) => inner.set_cursor_hidden(hidden),
            InjectionBackend::XWayland(inner) => inner.set_cursor_hidden(hidden),
            InjectionBackend::Unavailable => Ok(()),
        }
    }

    fn warp_cursor(&self, x: i32, y: i32) -> Result<()> {
        match &self.backend {
            #[cfg(feature = "linux-wayland")]
            InjectionBackend::NativeLibei(inner) => inner.warp_cursor(x, y),
            InjectionBackend::XWayland(inner) => inner.warp_cursor(x, y),
            InjectionBackend::Unavailable => Err(Self::unavailable_error()),
        }
    }

    fn release_all_keys(&self) -> Result<()> {
        match &self.backend {
            #[cfg(feature = "linux-wayland")]
            InjectionBackend::NativeLibei(inner) => inner.release_all_keys(),
            InjectionBackend::XWayland(inner) => inner.release_all_keys(),
            InjectionBackend::Unavailable => Err(Self::unavailable_error()),
        }
    }

    fn declare_user_activity(&self) -> Result<()> {
        match &self.backend {
            #[cfg(feature = "linux-wayland")]
            InjectionBackend::NativeLibei(inner) => inner.declare_user_activity(),
            InjectionBackend::XWayland(inner) => inner.declare_user_activity(),
            InjectionBackend::Unavailable => Ok(()),
        }
    }

    fn lock_cursor_position(&self) -> Result<()> {
        match &self.backend {
            #[cfg(feature = "linux-wayland")]
            InjectionBackend::NativeLibei(inner) => inner.lock_cursor_position(),
            InjectionBackend::XWayland(inner) => inner.lock_cursor_position(),
            InjectionBackend::Unavailable => Ok(()),
        }
    }

    fn unlock_cursor_position(&self) -> Result<()> {
        match &self.backend {
            #[cfg(feature = "linux-wayland")]
            InjectionBackend::NativeLibei(inner) => inner.unlock_cursor_position(),
            InjectionBackend::XWayland(inner) => inner.unlock_cursor_position(),
            InjectionBackend::Unavailable => Ok(()),
        }
    }

    fn restore_cursor_position(&self, x: i32, y: i32) -> Result<()> {
        match &self.backend {
            #[cfg(feature = "linux-wayland")]
            InjectionBackend::NativeLibei(inner) => inner.restore_cursor_position(x, y),
            InjectionBackend::XWayland(inner) => inner.restore_cursor_position(x, y),
            InjectionBackend::Unavailable => Ok(()),
        }
    }
}
