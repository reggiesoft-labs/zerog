//! Trait contracts for platform-specific capture, injection, display, and permission subsystems.
//!
//! This module defines the interfaces that each host platform (macOS, Windows, Linux)
//! must implement to participate in the mouse-sharing engine.

use anyhow::Result;

use crate::core::platform::types::DisplayBounds;

/// Captures local input events from the host platform.
///
/// Implementations typically use low-level OS hooks (e.g., `CGEventTap` on macOS,
/// `WH_MOUSE_LL` on Windows) to intercept hardware events before they reach applications.
pub trait EventCapture: Send + Sync {
    /// Installs the platform event tap/hook.
    fn install_tap(&self) -> Result<()>;

    /// Uninstalls the platform event tap/hook.
    fn uninstall_tap(&self) -> Result<()>;

    /// Returns true if capture hook is currently active.
    fn is_installed(&self) -> bool;
}

/// Injects synthetic input events into the host platform.
///
/// Implementations use OS-level input synthesis (e.g., `CGEventPost` on macOS,
/// `SendInput` on Windows, `XTEST` on Linux) to simulate hardware events.
pub trait EventInjection: Send + Sync {
    /// Injects a mouse move to virtual-desktop coordinates.
    ///
    /// The runtime supplies coordinates in the same space as `DisplayBounds`.
    /// Platform adapters perform the only conversion to native coordinates.
    fn inject_mouse_move(&self, x: i32, y: i32) -> Result<()>;

    /// Injects a mouse move with sub-pixel floating-point coordinates.
    fn inject_mouse_move_subpixel(&self, x: f64, y: f64) -> Result<()> {
        self.inject_mouse_move(x.round() as i32, y.round() as i32)
    }

    /// Injects relative mouse motion.
    fn inject_mouse_move_relative(&self, dx: i32, dy: i32) -> Result<()> {
        let _ = (dx, dy);
        anyhow::bail!("relative mouse move injection not implemented for this platform")
    }

    /// Injects a mouse button press/release.
    ///
    /// `button` indices: 0=Left, 1=Right, 2=Middle, 3=X1, 4=X2.
    fn inject_mouse_button(&self, button: u8, pressed: bool) -> Result<()>;

    /// Injects a mouse wheel delta event.
    ///
    /// `delta_x` is horizontal (positive = scroll right, negative = scroll left),
    /// `delta_y` is vertical (positive = scroll up/away, negative = scroll down).
    fn inject_mouse_wheel(&self, delta_x: i32, delta_y: i32) -> Result<()>;

    /// Injects a keyboard key press event represented by a platform-neutral HID keycode.
    fn inject_key_press(&self, _keycode: u32) -> Result<()> {
        anyhow::bail!("keyboard key press injection not implemented for this platform")
    }

    /// Injects a keyboard key release event represented by a platform-neutral HID keycode.
    fn inject_key_release(&self, _keycode: u32) -> Result<()> {
        anyhow::bail!("keyboard key release injection not implemented for this platform")
    }

    /// Returns the local Caps Lock toggle state when the platform can read it.
    ///
    /// The peer handing over control reports this so the receiver can match it
    /// before the session starts.
    fn caps_lock_active(&self) -> Option<bool> {
        None
    }

    /// Sets the local Caps Lock state to exactly `active`.
    ///
    /// Called on the machine receiving control so its lock state (and the
    /// physical keyboard LED) matches the peer that was just controlling it.
    fn set_caps_lock(&self, _active: bool) -> Result<()> {
        Ok(())
    }

    /// Hides or shows the local cursor.
    ///
    /// Used when the local mouse has "left" the screen to move to a peer.
    fn set_cursor_hidden(&self, hidden: bool) -> Result<()>;

    /// Warps the local cursor to absolute coordinates.
    fn warp_cursor(&self, x: i32, y: i32) -> Result<()>;

    /// Decouples the hardware mouse from the OS cursor position.
    ///
    /// On some platforms, this prevents the OS from moving the visible cursor
    /// while we are capturing and redirecting hardware events.
    fn lock_cursor_position(&self) -> Result<()> {
        Ok(())
    }

    /// Re-couples the hardware mouse with the OS cursor position.
    fn unlock_cursor_position(&self) -> Result<()> {
        Ok(())
    }

    fn restore_cursor_position(&self, x: i32, y: i32) -> Result<()> {
        self.unlock_cursor_position()?;
        self.warp_cursor(x, y)
    }

    /// Releases all currently held modifier keys.
    ///
    /// This should be called when transitioning control back to the local system
    /// (e.g., on timeout, ReleaseControl, or engine shutdown) to prevent modifier
    /// keys from getting stuck in a pressed state.
    fn release_all_keys(&self) -> Result<()> {
        Ok(())
    }

    fn declare_user_activity(&self) -> Result<()> {
        Ok(())
    }
}

/// Manages display topology and coordinate conversion concerns.
pub trait DisplayManager: Send + Sync {
    /// Discovers all active displays and their boundaries.
    fn discover_displays(&self) -> Result<Vec<DisplayBounds>>;

    /// Returns virtual desktop bounds that encompass all active displays.
    fn get_virtual_bounds(&self) -> Result<DisplayBounds>;

    /// Returns true if the given point is inside any active display.
    fn contains_point(&self, x: i32, y: i32) -> Result<bool>;

    /// Clamps a point to the boundary of the nearest active display.
    fn clamp_to_active_display(&self, x: i32, y: i32) -> Result<(i32, i32)>;

    /// Returns a stable signature (hash) representing the current display topology.
    ///
    /// Used to detect monitor reconfiguration at runtime.
    fn get_signature(&self) -> Result<u64>;

    /// Returns the current cursor position in virtual desktop coordinates.
    fn cursor_position(&self) -> Result<(i32, i32)> {
        anyhow::bail!("cursor position query not implemented for this platform")
    }
}

/// Checks and prompts for required host permissions.
pub trait PermissionChecker: Send + Sync {
    /// Returns true when the required accessibility/input control permission is granted.
    fn check_accessibility_permission(&self) -> bool;

    /// Prompts the system permission UI or prints instructions to the user.
    fn prompt_accessibility_permission(&self) -> Result<()>;
}

/// Throughput metrics snapshot for network monitoring.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct ThroughputMetrics {
    /// Outbound events per second.
    pub out_events_per_sec: u64,
    /// Inbound events per second.
    pub in_events_per_sec: u64,
    /// Total packets sent since last reset.
    pub packets_sent: u64,
    /// Total packets received since last reset.
    pub packets_received: u64,
    /// Mouse moves coalesced per second (sender-side batching).
    pub moves_coalesced_per_sec: u64,
    /// Percentage of smoother ticks that injected a position (receiver-side).
    pub smoother_inject_pct: f64,
}

/// Errors from clipboard operations.
#[derive(Debug, thiserror::Error)]
pub enum ClipboardError {
    #[error("Platform clipboard API error: {0}")]
    PlatformError(String),
    #[error("Clipboard busy (another app has it locked)")]
    Busy,
    #[error("Feature not supported on this platform")]
    Unsupported,
    #[error("Clipboard operation timed out")]
    Timeout,
}

/// Manages platform clipboard read/write operations.
///
/// Thread Safety: Implementations MUST be thread-safe (Send + Sync)
/// because ClipboardSync calls these from multiple tokio tasks.
pub trait ClipboardManager: Send + Sync {
    /// Reads current clipboard text content.
    ///
    /// Returns:
    /// - Ok(Some(String)) if clipboard has text
    /// - Ok(None) if clipboard is empty or not text
    /// - Err(ClipboardError) on platform failure
    fn get_clipboard_text(&self) -> Result<Option<String>, ClipboardError>;

    /// Sets the clipboard text content.
    ///
    /// Behavior:
    /// - Replaces entire clipboard contents
    /// - Does NOT clear clipboard if content is empty (use clear_clipboard())
    ///
    /// Returns:
    /// - Ok(()) on success
    /// - Err(ClipboardError) on platform failure
    fn set_clipboard_text(&self, content: &str) -> Result<(), ClipboardError>;

    /// Clears the clipboard entirely.
    fn clear_clipboard(&self) -> Result<(), ClipboardError>;

    /// Checks if clipboard is currently available (not all platforms support it).
    /// Wayland native: returns false (no standardized clipboard API).
    fn is_available(&self) -> bool {
        true
    }
}

/// Network state interface for clipboard synchronization.
///
/// This trait abstracts the network operations needed by ClipboardSync,
/// allowing for mock implementations in tests.
pub trait NetworkState: Send + Sync {
    /// Broadcasts a packet of events to all connected peers.
    fn broadcast_packet(&self, events: Vec<crate::core::events::Event>) -> anyhow::Result<()>;

    /// Returns the currently active target address, if any.
    fn active_target(&self) -> Option<std::net::SocketAddr>;

    /// Returns true if there are any connected peers.
    fn has_any_peer(&self) -> bool;
}
