//! X11 event injection via XTEST and cursor visibility via XFIXES.
//!
//! This module implements input injection using the XTEST extension, which
//! allows synthesizing input events that are processed by the X server as
//! if they came from physical devices.
//!
//! # Loopback Prevention
//!
//! Injected events are tracked via the marker module to prevent capturing
//! our own injected events in an infinite loop. Each injection call records
//! the event details for later comparison during capture.

use anyhow::{Result, anyhow, bail};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU16, Ordering};

use crate::platform::{DisplayManager, EventInjection};
use x11rb::connection::Connection;
use x11rb::protocol::xfixes::ConnectionExt as XfixesConnectionExt;
use x11rb::protocol::xproto::ConnectionExt as XprotoConnectionExt;
use x11rb::protocol::xproto::{
    BUTTON_PRESS_EVENT, BUTTON_RELEASE_EVENT, KEY_PRESS_EVENT, KEY_RELEASE_EVENT,
    MOTION_NOTIFY_EVENT,
};
use x11rb::protocol::xtest::ConnectionExt as XtestConnectionExt;
use x11rb::rust_connection::RustConnection;

use super::display::X11DisplayManager;
use super::marker;

/// X11 event injection implementation using XTEST extension.
pub(crate) struct X11EventInjection {
    /// Tracks cursor visibility state to avoid redundant XFIXES calls.
    cursor_hidden: AtomicBool,
    hide_conn: Mutex<Option<(RustConnection, u32)>>,
    /// Tracks currently pressed modifier keys (bitmask).
    modifier_state: AtomicU16,
    /// Tracks currently pressed mouse buttons (bitmask).
    pressed_buttons: AtomicU8,
}

impl X11EventInjection {
    pub(crate) fn new() -> Self {
        Self {
            cursor_hidden: AtomicBool::new(false),
            hide_conn: Mutex::new(None),
            modifier_state: AtomicU16::new(0),
            pressed_buttons: AtomicU8::new(0),
        }
    }
}

impl EventInjection for X11EventInjection {
    fn inject_mouse_move(&self, x: i32, y: i32) -> Result<()> {
        if self.cursor_hidden.load(Ordering::SeqCst) {
            return Ok(());
        }
        let (conn, screen_num) = x11rb::connect(None)
            .map_err(|err| anyhow!("failed to connect to X11 display server: {err}"))?;
        let root = conn
            .setup()
            .roots
            .get(screen_num)
            .ok_or_else(|| anyhow!("X11 screen index out of bounds"))?
            .root;

        let x_i16 = x.clamp(i16::MIN as i32, i16::MAX as i32) as i16;
        let y_i16 = y.clamp(i16::MIN as i32, i16::MAX as i32) as i16;
        marker::note_injected_move(x, y);

        conn.xtest_fake_input(MOTION_NOTIFY_EVENT, 0, 0, root, x_i16, y_i16, 0)?;
        conn.flush()?;
        Ok(())
    }

    fn inject_mouse_move_relative(&self, dx: i32, dy: i32) -> Result<()> {
        if self.cursor_hidden.load(Ordering::SeqCst) {
            return Ok(());
        }
        let (conn, screen_num) = x11rb::connect(None)
            .map_err(|err| anyhow!("failed to connect to X11 display server: {err}"))?;
        let root = conn
            .setup()
            .roots
            .get(screen_num)
            .ok_or_else(|| anyhow!("X11 screen index out of bounds"))?
            .root;

        let rx = dx.clamp(i16::MIN as i32, i16::MAX as i32) as i16;
        let ry = dy.clamp(i16::MIN as i32, i16::MAX as i32) as i16;

        conn.xtest_fake_input(MOTION_NOTIFY_EVENT, 1, 0, root, rx, ry, 0)?;
        conn.flush()?;
        Ok(())
    }

    fn inject_mouse_button(&self, button: u8, pressed: bool) -> Result<()> {
        let detail = logical_to_x11_button(button)?;
        marker::note_injected_button(button, pressed);

        if button < 8 {
            let mask = 1u8 << button;
            if pressed {
                self.pressed_buttons.fetch_or(mask, Ordering::Relaxed);
            } else {
                self.pressed_buttons.fetch_and(!mask, Ordering::Relaxed);
            }
        }

        let event_type = if pressed {
            BUTTON_PRESS_EVENT
        } else {
            BUTTON_RELEASE_EVENT
        };

        let (conn, screen_num) = x11rb::connect(None)
            .map_err(|err| anyhow!("failed to connect to X11 display server: {err}"))?;
        let root = conn
            .setup()
            .roots
            .get(screen_num)
            .ok_or_else(|| anyhow!("X11 screen index out of bounds"))?
            .root;

        conn.xtest_fake_input(event_type, detail, 0, root, 0, 0, 0)?;
        conn.flush()?;
        Ok(())
    }

    fn inject_mouse_wheel(&self, delta_x: i32, delta_y: i32) -> Result<()> {
        if delta_x == 0 && delta_y == 0 {
            return Ok(());
        }

        let (conn, screen_num) = x11rb::connect(None)
            .map_err(|err| anyhow!("failed to connect to X11 display server: {err}"))?;
        let root = conn
            .setup()
            .roots
            .get(screen_num)
            .ok_or_else(|| anyhow!("X11 screen index out of bounds"))?
            .root;

        if delta_y != 0 {
            let wheel_button = if delta_y > 0 { 4 } else { 5 };
            let steps = (delta_y.abs() / 120).max(1);
            for _ in 0..steps {
                conn.xtest_fake_input(BUTTON_PRESS_EVENT, wheel_button, 0, root, 0, 0, 0)?;
                conn.xtest_fake_input(BUTTON_RELEASE_EVENT, wheel_button, 0, root, 0, 0, 0)?;
            }
        }
        if delta_x != 0 {
            let wheel_button = if delta_x > 0 { 7 } else { 6 };
            let steps = (delta_x.abs() / 120).max(1);
            for _ in 0..steps {
                conn.xtest_fake_input(BUTTON_PRESS_EVENT, wheel_button, 0, root, 0, 0, 0)?;
                conn.xtest_fake_input(BUTTON_RELEASE_EVENT, wheel_button, 0, root, 0, 0, 0)?;
            }
        }
        conn.flush()?;
        Ok(())
    }

    fn inject_key_press(&self, keycode: u32) -> Result<()> {
        let x11_keycode = hid_to_x11_keycode(keycode)
            .ok_or_else(|| anyhow!("unsupported HID keycode for X11 injection: {keycode}"))?;
        marker::note_injected_key(keycode, true);

        let (conn, screen_num) = x11rb::connect(None)
            .map_err(|err| anyhow!("failed to connect to X11 display server: {err}"))?;
        let root = conn
            .setup()
            .roots
            .get(screen_num)
            .ok_or_else(|| anyhow!("X11 screen index out of bounds"))?
            .root;

        conn.xtest_fake_input(KEY_PRESS_EVENT, x11_keycode, 0, root, 0, 0, 0)?;
        conn.flush()?;

        if let Some(bit) = modifier_hid_to_bit(keycode) {
            self.modifier_state.fetch_or(bit, Ordering::Relaxed);
        }

        Ok(())
    }

    fn inject_key_release(&self, keycode: u32) -> Result<()> {
        let x11_keycode = hid_to_x11_keycode(keycode)
            .ok_or_else(|| anyhow!("unsupported HID keycode for X11 injection: {keycode}"))?;
        marker::note_injected_key(keycode, false);

        let (conn, screen_num) = x11rb::connect(None)
            .map_err(|err| anyhow!("failed to connect to X11 display server: {err}"))?;
        let root = conn
            .setup()
            .roots
            .get(screen_num)
            .ok_or_else(|| anyhow!("X11 screen index out of bounds"))?
            .root;

        conn.xtest_fake_input(KEY_RELEASE_EVENT, x11_keycode, 0, root, 0, 0, 0)?;
        conn.flush()?;

        if let Some(bit) = modifier_hid_to_bit(keycode) {
            self.modifier_state.fetch_and(!bit, Ordering::Relaxed);
        }

        Ok(())
    }

    fn set_cursor_hidden(&self, hidden: bool) -> Result<()> {
        let previous = self.cursor_hidden.swap(hidden, Ordering::SeqCst);
        if previous == hidden {
            return Ok(());
        }

        let mut guard = self.hide_conn.lock().unwrap();
        if hidden {
            let _ = self.lock_cursor_position();
            if guard.is_none() {
                if let Ok((conn, screen_num)) = x11rb::connect(None) {
                    if let Some(screen) = conn.setup().roots.get(screen_num) {
                        let root = screen.root;
                        let has_xfixes = conn
                            .query_extension(b"XFIXES")
                            .ok()
                            .and_then(|cookie| cookie.reply().ok())
                            .map(|reply| reply.present)
                            .unwrap_or(false);
                        if has_xfixes {
                            let version_ok = conn
                                .xfixes_query_version(4, 0)
                                .ok()
                                .and_then(|cookie| cookie.reply().ok())
                                .map(|reply| reply.major_version >= 4)
                                .unwrap_or(false);
                            if version_ok {
                                if conn.xfixes_hide_cursor(root).is_ok() {
                                    let _ = conn.flush();
                                    *guard = Some((conn, root));
                                }
                            }
                        }
                    }
                }
            }
        } else if let Some((conn, root)) = guard.take() {
            let _ = conn.xfixes_show_cursor(root);
            let _ = conn.flush();
        }
        Ok(())
    }

    fn lock_cursor_position(&self) -> Result<()> {
        let manager = X11DisplayManager::new();
        let (park_x, park_y) = match manager.get_virtual_bounds() {
            Ok(bounds) => (
                bounds.x.saturating_add(bounds.width as i32),
                bounds.y.saturating_add(bounds.height as i32),
            ),
            Err(_) => (i16::MAX as i32, i16::MAX as i32),
        };
        self.warp_cursor(park_x, park_y)
    }

    fn unlock_cursor_position(&self) -> Result<()> {
        Ok(())
    }

    fn warp_cursor(&self, x: i32, y: i32) -> Result<()> {
        let manager = X11DisplayManager::new();
        let (clamped_x, clamped_y) = manager.clamp_to_active_display(x, y)?;

        let (conn, screen_num) = x11rb::connect(None)
            .map_err(|err| anyhow!("failed to connect to X11 display server: {err}"))?;
        let root = conn
            .setup()
            .roots
            .get(screen_num)
            .ok_or_else(|| anyhow!("X11 screen index out of bounds"))?
            .root;

        marker::note_injected_move(clamped_x, clamped_y);
        conn.warp_pointer(
            x11rb::NONE,
            root,
            0,
            0,
            0,
            0,
            clamped_x.clamp(i16::MIN as i32, i16::MAX as i32) as i16,
            clamped_y.clamp(i16::MIN as i32, i16::MAX as i32) as i16,
        )?;
        conn.flush()?;
        Ok(())
    }

    fn release_all_keys(&self) -> Result<()> {
        let current = self.modifier_state.load(Ordering::Relaxed);

        let (conn, screen_num) = x11rb::connect(None)
            .map_err(|err| anyhow!("failed to connect to X11 display server: {err}"))?;
        let root = conn
            .setup()
            .roots
            .get(screen_num)
            .ok_or_else(|| anyhow!("X11 screen index out of bounds"))?
            .root;

        for hid in 0xE0..=0xE7 {
            if let Some(bit) = modifier_hid_to_bit(hid) {
                if (current & bit) != 0 {
                    if let Some(x11_keycode) = hid_to_x11_keycode(hid) {
                        let _ =
                            conn.xtest_fake_input(KEY_RELEASE_EVENT, x11_keycode, 0, root, 0, 0, 0);
                    }
                }
            }
        }

        let pressed_buttons = self.pressed_buttons.swap(0, Ordering::Relaxed);
        for button in 0..8 {
            if (pressed_buttons & (1 << button)) != 0 {
                if let Ok(detail) = logical_to_x11_button(button) {
                    let _ = conn.xtest_fake_input(BUTTON_RELEASE_EVENT, detail, 0, root, 0, 0, 0);
                }
            }
        }

        conn.flush()?;
        self.modifier_state.store(0, Ordering::Relaxed);
        Ok(())
    }
}

impl Drop for X11EventInjection {
    fn drop(&mut self) {
        let _ = self.release_all_keys();
        let _ = self.set_cursor_hidden(false);
    }
}

fn logical_to_x11_button(button: u8) -> Result<u8> {
    match button {
        0 => Ok(1),
        1 => Ok(3),
        2 => Ok(2),
        3 => Ok(8),
        4 => Ok(9),
        _ => bail!("unsupported mouse button index: {button}"),
    }
}

fn hid_to_x11_keycode(hid: u32) -> Option<u8> {
    let evdev = super::super::keycodes::hid_to_evdev(hid)?;
    Some(super::super::keycodes::evdev_to_x11(evdev) as u8)
}

fn modifier_hid_to_bit(hid: u32) -> Option<u16> {
    match hid {
        0xE1 => Some(1 << 0), // Left Shift
        0xE5 => Some(1 << 1), // Right Shift
        0xE0 => Some(1 << 2), // Left Control
        0xE4 => Some(1 << 3), // Right Control
        0xE2 => Some(1 << 4), // Left Alt
        0xE6 => Some(1 << 5), // Right Alt
        0xE3 => Some(1 << 6), // Left Meta
        0xE7 => Some(1 << 7), // Right Meta
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::super::super::keycodes::hid_to_evdev;
    use super::hid_to_x11_keycode;

    #[test]
    fn hid_to_x11_keycode_maps_common_keys() {
        assert_eq!(hid_to_x11_keycode(0x04), Some(38));
        assert_eq!(hid_to_x11_keycode(0x28), Some(36));
        assert_eq!(hid_to_x11_keycode(0x2C), Some(65));
        assert_eq!(hid_to_x11_keycode(0x50), Some(113));
    }

    #[test]
    fn hid_to_evdev_maps_modifier_keys() {
        assert_eq!(hid_to_evdev(0xE0), Some(29));
        assert_eq!(hid_to_evdev(0xE1), Some(42));
        assert_eq!(hid_to_evdev(0xE3), Some(125));
    }

    #[test]
    fn hid_to_x11_keycode_returns_none_for_unmapped_key() {
        assert_eq!(hid_to_x11_keycode(0xFFFF), None);
    }
}

#[cfg(test)]
mod live_tests {
    use super::X11EventInjection;
    use crate::platform::linux::live_test;
    use crate::platform::linux::x11::display::X11DisplayManager;
    use crate::platform::{DisplayManager, EventInjection};
    use serial_test::serial;
    use std::time::{Duration, Instant};

    fn wait_for_display() -> bool {
        let has_display = std::env::var("DISPLAY")
            .ok()
            .filter(|value| !value.trim().is_empty())
            .is_some();
        if !has_display {
            return false;
        }
        let started = Instant::now();
        while started.elapsed() < Duration::from_secs(20) {
            if x11rb::connect(None).is_ok() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        false
    }

    fn server_responds() {
        X11DisplayManager::new()
            .cursor_position()
            .expect("server should answer after injection");
    }

    #[test]
    #[serial]
    fn absolute_move_is_accepted() {
        if !live_test::require_live(
            "absolute_move_is_accepted",
            wait_for_display(),
            "no live X display",
        ) {
            return;
        }
        let injection = X11EventInjection::new();
        injection.inject_mouse_move(100, 100).expect("move should inject");
        server_responds();
        injection.inject_mouse_move(200, 150).expect("move should inject");
        server_responds();
    }

    #[test]
    #[serial]
    fn relative_move_is_accepted() {
        if !live_test::require_live(
            "relative_move_is_accepted",
            wait_for_display(),
            "no live X display",
        ) {
            return;
        }
        let injection = X11EventInjection::new();
        injection
            .inject_mouse_move_relative(20, 10)
            .expect("relative move should inject");
        server_responds();
    }

    #[test]
    #[serial]
    fn button_press_release_is_accepted() {
        if !live_test::require_live(
            "button_press_release_is_accepted",
            wait_for_display(),
            "no live X display",
        ) {
            return;
        }
        let injection = X11EventInjection::new();
        injection
            .inject_mouse_button(0, true)
            .expect("button press should inject");
        injection
            .inject_mouse_button(0, false)
            .expect("button release should inject");
        server_responds();
    }

    #[test]
    #[serial]
    fn key_press_release_is_accepted() {
        if !live_test::require_live(
            "key_press_release_is_accepted",
            wait_for_display(),
            "no live X display",
        ) {
            return;
        }
        let injection = X11EventInjection::new();
        injection.inject_key_press(0x04).expect("key press should inject");
        injection
            .inject_key_release(0x04)
            .expect("key release should inject");
        server_responds();
    }
}
