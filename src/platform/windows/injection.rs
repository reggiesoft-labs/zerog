//! Windows event injection via SendInput.
//!
//! This module implements input injection using the Windows SendInput API.
//! All injected events are marked with a custom `dwExtraInfo` value to
//! prevent loopback (capturing our own injected events).
//!
//! # Coordinate System
//!
//! Mouse positions use normalized absolute coordinates (0-65535) mapped
//! to the virtual desktop. This ensures correct positioning across
//! multi-monitor configurations.

use anyhow::{Result, anyhow, bail};
use std::mem::size_of;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU16, Ordering};

use crate::platform::{DisplayManager, EventInjection};

use super::display::{WindowsDisplayManager, virtual_rect_from_metrics};
use super::marker::local_mouse_marker;
use windows_sys::Win32::Foundation::RECT;
use windows_sys::Win32::System::Threading::GetCurrentProcessId;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    GetKeyState, INPUT, INPUT_0, INPUT_KEYBOARD, INPUT_MOUSE, KEYBDINPUT, KEYEVENTF_EXTENDEDKEY,
    KEYEVENTF_KEYUP, KEYEVENTF_SCANCODE, MOUSEEVENTF_ABSOLUTE, MOUSEEVENTF_HWHEEL,
    MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP, MOUSEEVENTF_MIDDLEDOWN, MOUSEEVENTF_MIDDLEUP,
    MOUSEEVENTF_MOVE, MOUSEEVENTF_RIGHTDOWN, MOUSEEVENTF_RIGHTUP, MOUSEEVENTF_VIRTUALDESK,
    MOUSEEVENTF_WHEEL, MOUSEEVENTF_XDOWN, MOUSEEVENTF_XUP, MOUSEINPUT, SendInput, VK_CAPITAL,
    VK_LCONTROL, VK_LMENU, VK_LSHIFT, VK_LWIN, VK_RCONTROL, VK_RMENU, VK_RSHIFT, VK_RWIN,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    ClipCursor, GetDesktopWindow, GetForegroundWindow, GetShellWindow, GetWindowThreadProcessId,
    MSG, PM_NOREMOVE, PeekMessageW, SetCursorPos, SetForegroundWindow, ShowCursor,
};

const XBUTTON1: u32 = 0x0001;
const XBUTTON2: u32 = 0x0002;

/// HID usage for Caps Lock; Windows toggles the system lock on this key event.
const HID_KEY_CAPS_LOCK: u32 = 0x39;

/// Maximum value for normalized absolute coordinates (16-bit).
const ABSOLUTE_AXIS_MAX: i32 = 65_535;

/// Windows event injection implementation using SendInput.
pub(crate) struct WindowsEventInjection {
    dry_run: bool,
    /// Tracks cursor visibility state to avoid redundant ShowCursor calls.
    cursor_hidden: AtomicBool,
    marker: usize,
    /// Tracks currently pressed modifier keys (bitmask).
    modifier_state: AtomicU16,
    /// Tracks currently pressed mouse buttons (bitmask).
    pressed_buttons: AtomicU8,
}

impl WindowsEventInjection {
    pub(crate) fn new() -> Self {
        Self::with_marker(local_mouse_marker())
    }

    pub(crate) fn with_marker(marker: usize) -> Self {
        Self::with_marker_and_dry_run(marker, false)
    }

    #[cfg(test)]
    pub(crate) fn new_test() -> Self {
        Self::with_marker_and_dry_run(0, true)
    }

    fn with_marker_and_dry_run(marker: usize, dry_run: bool) -> Self {
        if !dry_run {
            unsafe {
                ClipCursor(std::ptr::null());
                ShowCursor(1);
            }
        }
        Self {
            dry_run,
            marker,
            cursor_hidden: AtomicBool::new(false),
            modifier_state: AtomicU16::new(0),
            pressed_buttons: AtomicU8::new(0),
        }
    }

    fn send_mouse_input(&self, mut input: INPUT) -> Result<()> {
        if self.dry_run {
            return Ok(());
        }
        // SAFETY: INPUT is fully initialized and points to a valid single-entry buffer.
        let sent = unsafe { SendInput(1, &mut input as *mut INPUT, size_of::<INPUT>() as i32) };
        if sent == 1 {
            Ok(())
        } else {
            Err(anyhow!(
                "SendInput failed: {}",
                std::io::Error::last_os_error()
            ))
        }
    }

    fn send_keyboard_input(&self, mut input: INPUT) -> Result<()> {
        if self.dry_run {
            return Ok(());
        }
        // SAFETY: INPUT is fully initialized and points to a valid single-entry buffer.
        let sent = unsafe { SendInput(1, &mut input as *mut INPUT, size_of::<INPUT>() as i32) };
        if sent == 1 {
            Ok(())
        } else {
            Err(anyhow!(
                "SendInput keyboard event failed: {}",
                std::io::Error::last_os_error()
            ))
        }
    }

    fn normalized_absolute_coords(&self, x: i32, y: i32) -> Result<(i32, i32)> {
        let virtual_rect = virtual_rect_from_metrics()?;
        let width = i64::from((virtual_rect.right - virtual_rect.left).max(1));
        let height = i64::from((virtual_rect.bottom - virtual_rect.top).max(1));

        let rel_x = i64::from(x) - i64::from(virtual_rect.left);
        let rel_y = i64::from(y) - i64::from(virtual_rect.top);

        let denom_x = (width - 1).max(1);
        let denom_y = (height - 1).max(1);

        let norm_x = ((rel_x * i64::from(ABSOLUTE_AXIS_MAX)) / denom_x)
            .clamp(0, i64::from(ABSOLUTE_AXIS_MAX)) as i32;
        let norm_y = ((rel_y * i64::from(ABSOLUTE_AXIS_MAX)) / denom_y)
            .clamp(0, i64::from(ABSOLUTE_AXIS_MAX)) as i32;

        Ok((norm_x, norm_y))
    }
}

impl EventInjection for WindowsEventInjection {
    fn inject_mouse_move(&self, x: i32, y: i32) -> Result<()> {
        let (dx, dy) = self.normalized_absolute_coords(x, y)?;

        let input = INPUT {
            r#type: INPUT_MOUSE,
            Anonymous: INPUT_0 {
                mi: MOUSEINPUT {
                    dx,
                    dy,
                    mouseData: 0,
                    dwFlags: MOUSEEVENTF_MOVE | MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_VIRTUALDESK,
                    time: 0,
                    dwExtraInfo: self.marker,
                },
            },
        };

        self.send_mouse_input(input)
    }

    fn inject_mouse_move_relative(&self, dx: i32, dy: i32) -> Result<()> {
        let input = INPUT {
            r#type: INPUT_MOUSE,
            Anonymous: INPUT_0 {
                mi: MOUSEINPUT {
                    dx,
                    dy,
                    mouseData: 0,
                    dwFlags: MOUSEEVENTF_MOVE,
                    time: 0,
                    dwExtraInfo: self.marker,
                },
            },
        };

        self.send_mouse_input(input)
    }

    fn inject_mouse_button(&self, button: u8, pressed: bool) -> Result<()> {
        let (flags, mouse_data): (u32, u32) = match (button, pressed) {
            (0, true) => (MOUSEEVENTF_LEFTDOWN, 0),
            (0, false) => (MOUSEEVENTF_LEFTUP, 0),
            (1, true) => (MOUSEEVENTF_RIGHTDOWN, 0),
            (1, false) => (MOUSEEVENTF_RIGHTUP, 0),
            (2, true) => (MOUSEEVENTF_MIDDLEDOWN, 0),
            (2, false) => (MOUSEEVENTF_MIDDLEUP, 0),
            (3, true) => (MOUSEEVENTF_XDOWN, XBUTTON1),
            (3, false) => (MOUSEEVENTF_XUP, XBUTTON1),
            (4, true) => (MOUSEEVENTF_XDOWN, XBUTTON2),
            (4, false) => (MOUSEEVENTF_XUP, XBUTTON2),
            _ => bail!("Unsupported mouse button index: {button}"),
        };

        if button < 8 {
            let mask = 1u8 << button;
            if pressed {
                self.pressed_buttons.fetch_or(mask, Ordering::Relaxed);
            } else {
                self.pressed_buttons.fetch_and(!mask, Ordering::Relaxed);
            }
        }

        let input = INPUT {
            r#type: INPUT_MOUSE,
            Anonymous: INPUT_0 {
                mi: MOUSEINPUT {
                    dx: 0,
                    dy: 0,
                    mouseData: mouse_data,
                    dwFlags: flags,
                    time: 0,
                    dwExtraInfo: self.marker,
                },
            },
        };

        self.send_mouse_input(input)
    }

    fn inject_mouse_wheel(&self, delta_x: i32, delta_y: i32) -> Result<()> {
        if delta_y != 0 {
            let input = INPUT {
                r#type: INPUT_MOUSE,
                Anonymous: INPUT_0 {
                    mi: MOUSEINPUT {
                        dx: 0,
                        dy: 0,
                        mouseData: delta_y as u32,
                        dwFlags: MOUSEEVENTF_WHEEL,
                        time: 0,
                        dwExtraInfo: self.marker,
                    },
                },
            };
            self.send_mouse_input(input)?;
        }
        if delta_x != 0 {
            let input = INPUT {
                r#type: INPUT_MOUSE,
                Anonymous: INPUT_0 {
                    mi: MOUSEINPUT {
                        dx: 0,
                        dy: 0,
                        mouseData: delta_x as u32,
                        dwFlags: MOUSEEVENTF_HWHEEL,
                        time: 0,
                        dwExtraInfo: self.marker,
                    },
                },
            };
            self.send_mouse_input(input)?;
        }
        Ok(())
    }

    fn inject_key_press(&self, keycode: u32) -> Result<()> {
        if keycode == 0x19E {
            if !self.dry_run {
                unsafe {
                    windows_sys::Win32::System::Shutdown::LockWorkStation();
                }
            }
            return Ok(());
        }

        let mut input = keyboard_input_for_hid(keycode, false)?;
        input.Anonymous.ki.dwExtraInfo = self.marker;

        self.send_keyboard_input(input)?;

        if let Some(bit) = modifier_hid_to_bit(keycode) {
            self.modifier_state.fetch_or(bit, Ordering::Relaxed);
        }

        Ok(())
    }

    fn inject_key_release(&self, keycode: u32) -> Result<()> {
        if keycode == 0x19E {
            return Ok(());
        }

        let mut input = keyboard_input_for_hid(keycode, true)?;
        input.Anonymous.ki.dwExtraInfo = self.marker;

        self.send_keyboard_input(input)?;

        if let Some(bit) = modifier_hid_to_bit(keycode) {
            self.modifier_state.fetch_and(!bit, Ordering::Relaxed);
        }

        Ok(())
    }

    fn caps_lock_active(&self) -> Option<bool> {
        // SAFETY: GetKeyState/PeekMessageW have no memory safety preconditions.
        unsafe {
            // GetKeyState reads toggle state from the calling thread's input
            // queue. Worker threads may not have one yet, so create it (without
            // removing anything) before reading; the queue is seeded from the
            // current system keyboard state.
            let mut message: MSG = std::mem::zeroed();
            PeekMessageW(&mut message, std::ptr::null_mut(), 0, 0, PM_NOREMOVE);
            // The low bit of the Caps Lock state is the toggle state.
            let state = GetKeyState(VK_CAPITAL as i32);
            Some(state & 1 != 0)
        }
    }

    fn set_caps_lock(&self, active: bool) -> Result<()> {
        if self.caps_lock_active() == Some(active) {
            return Ok(());
        }
        // The key event is what makes Windows update the toggle state and the
        // physical LED; there is no user-mode way to set the LED directly.
        self.inject_key_press(HID_KEY_CAPS_LOCK)?;
        self.inject_key_release(HID_KEY_CAPS_LOCK)
    }

    fn set_cursor_hidden(&self, hidden: bool) -> Result<()> {
        let previous = self.cursor_hidden.swap(hidden, Ordering::SeqCst);
        if previous == hidden || self.dry_run {
            return Ok(());
        }

        // SAFETY: ShowCursor adjusts process cursor display count; we gate calls via local state.
        let _ = unsafe { ShowCursor(if hidden { 0 } else { 1 }) };
        Ok(())
    }

    fn warp_cursor(&self, x: i32, y: i32) -> Result<()> {
        if self.dry_run {
            return Ok(());
        }
        let manager = WindowsDisplayManager::new();
        let (clamped_x, clamped_y) = manager.clamp_to_active_display(x, y)?;
        // SAFETY: Coordinates are clamped into active display bounds before calling SetCursorPos.
        let ok = unsafe { SetCursorPos(clamped_x, clamped_y) };
        if ok != 0 {
            Ok(())
        } else {
            Err(anyhow!(
                "SetCursorPos failed: {}",
                std::io::Error::last_os_error()
            ))
        }
    }

    fn release_all_keys(&self) -> Result<()> {
        for hid in 0xE0..=0xE7 {
            if let Ok(mut input) = keyboard_input_for_hid(hid, true) {
                input.Anonymous.ki.dwExtraInfo = self.marker;
                let _ = self.send_keyboard_input(input);
            }
        }

        self.modifier_state.store(0, Ordering::Relaxed);

        let pressed_buttons = self.pressed_buttons.swap(0, Ordering::Relaxed);
        for button in 0..8 {
            if (pressed_buttons & (1 << button)) != 0 {
                let _ = self.inject_mouse_button(button, false);
            }
        }

        Ok(())
    }

    fn lock_cursor_position(&self) -> Result<()> {
        if self.dry_run {
            return Ok(());
        }
        let manager = WindowsDisplayManager::new();
        if let Ok(bounds) = manager.get_virtual_bounds() {
            let rect = RECT {
                left: bounds.x,
                top: bounds.y,
                right: bounds.x + 1,
                bottom: bounds.y + 1,
            };
            unsafe {
                ClipCursor(&rect);
            }
        }
        release_gui_focus();
        Ok(())
    }

    fn unlock_cursor_position(&self) -> Result<()> {
        if !self.dry_run {
            unsafe {
                ClipCursor(std::ptr::null());
            }
        }
        Ok(())
    }

    fn declare_user_activity(&self) -> Result<()> {
        if self.dry_run {
            return Ok(());
        }
        unsafe {
            windows_sys::Win32::System::Power::SetThreadExecutionState(
                windows_sys::Win32::System::Power::ES_SYSTEM_REQUIRED
                    | windows_sys::Win32::System::Power::ES_DISPLAY_REQUIRED,
            );
        }
        Ok(())
    }
}

impl Drop for WindowsEventInjection {
    fn drop(&mut self) {
        let _ = self.release_all_keys();
        if !self.dry_run {
            unsafe {
                ClipCursor(std::ptr::null());
                ShowCursor(1);
            }
        }
        self.cursor_hidden.store(false, Ordering::SeqCst);
    }
}

pub(crate) fn release_gui_focus() {
    unsafe {
        let foreground = GetForegroundWindow();
        if !foreground.is_null() {
            let mut process_id = 0;
            GetWindowThreadProcessId(foreground, &mut process_id);
            if process_id == GetCurrentProcessId() {
                let shell = GetShellWindow();
                let target = if !shell.is_null() {
                    shell
                } else {
                    GetDesktopWindow()
                };
                if !target.is_null() {
                    SetForegroundWindow(target);
                }
            }
        }
    }
}

fn keyboard_input_for_hid(keycode: u32, key_up: bool) -> Result<INPUT> {
    let (w_vk, w_scan, mut dw_flags) = if let Some(virtual_key) =
        modifier_hid_to_virtual_key(keycode)
    {
        (virtual_key, 0, 0)
    } else {
        let (scancode, extra_flags) = hid_to_windows_scancode(keycode)
            .ok_or_else(|| anyhow!("unsupported HID keycode for Windows injection: {keycode}"))?;
        (0, scancode, KEYEVENTF_SCANCODE | extra_flags)
    };

    if key_up {
        dw_flags |= KEYEVENTF_KEYUP;
    }

    Ok(INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: w_vk,
                wScan: w_scan,
                dwFlags: dw_flags,
                time: 0,
                dwExtraInfo: local_mouse_marker(),
            },
        },
    })
}

fn modifier_hid_to_virtual_key(hid: u32) -> Option<u16> {
    match hid {
        0xE0 => Some(VK_LCONTROL),
        0xE1 => Some(VK_LSHIFT),
        0xE2 => Some(VK_LMENU),
        0xE3 => Some(VK_LWIN),
        0xE4 => Some(VK_RCONTROL),
        0xE5 => Some(VK_RSHIFT),
        0xE6 => Some(VK_RMENU),
        0xE7 => Some(VK_RWIN),
        _ => None,
    }
}

fn hid_to_windows_scancode(hid: u32) -> Option<(u16, u32)> {
    match hid {
        0x29 => Some((0x01, 0)),                     // Esc
        0x1E => Some((0x02, 0)),                     // 1
        0x1F => Some((0x03, 0)),                     // 2
        0x20 => Some((0x04, 0)),                     // 3
        0x21 => Some((0x05, 0)),                     // 4
        0x22 => Some((0x06, 0)),                     // 5
        0x23 => Some((0x07, 0)),                     // 6
        0x24 => Some((0x08, 0)),                     // 7
        0x25 => Some((0x09, 0)),                     // 8
        0x26 => Some((0x0A, 0)),                     // 9
        0x27 => Some((0x0B, 0)),                     // 0
        0x2D => Some((0x0C, 0)),                     // -
        0x2E => Some((0x0D, 0)),                     // =
        0x2A => Some((0x0E, 0)),                     // Backspace
        0x2B => Some((0x0F, 0)),                     // Tab
        0x14 => Some((0x10, 0)),                     // Q
        0x1A => Some((0x11, 0)),                     // W
        0x08 => Some((0x12, 0)),                     // E
        0x15 => Some((0x13, 0)),                     // R
        0x17 => Some((0x14, 0)),                     // T
        0x1C => Some((0x15, 0)),                     // Y
        0x18 => Some((0x16, 0)),                     // U
        0x0C => Some((0x17, 0)),                     // I
        0x12 => Some((0x18, 0)),                     // O
        0x13 => Some((0x19, 0)),                     // P
        0x2F => Some((0x1A, 0)),                     // [
        0x30 => Some((0x1B, 0)),                     // ]
        0x28 => Some((0x1C, 0)),                     // Enter
        0xE0 => Some((0x1D, 0)),                     // Left Ctrl
        0xE4 => Some((0x1D, KEYEVENTF_EXTENDEDKEY)), // Right Ctrl
        0x04 => Some((0x1E, 0)),                     // A
        0x16 => Some((0x1F, 0)),                     // S
        0x07 => Some((0x20, 0)),                     // D
        0x09 => Some((0x21, 0)),                     // F
        0x0A => Some((0x22, 0)),                     // G
        0x0B => Some((0x23, 0)),                     // H
        0x0D => Some((0x24, 0)),                     // J
        0x0E => Some((0x25, 0)),                     // K
        0x0F => Some((0x26, 0)),                     // L
        0x33 => Some((0x27, 0)),                     // ;
        0x34 => Some((0x28, 0)),                     // '
        0x35 => Some((0x29, 0)),                     // `
        0xE1 => Some((0x2A, 0)),                     // Left Shift
        0x31 => Some((0x2B, 0)),                     // \
        0x1D => Some((0x2C, 0)),                     // Z
        0x1B => Some((0x2D, 0)),                     // X
        0x06 => Some((0x2E, 0)),                     // C
        0x19 => Some((0x2F, 0)),                     // V
        0x05 => Some((0x30, 0)),                     // B
        0x11 => Some((0x31, 0)),                     // N
        0x10 => Some((0x32, 0)),                     // M
        0x36 => Some((0x33, 0)),                     // ,
        0x37 => Some((0x34, 0)),                     // .
        0x38 => Some((0x35, 0)),                     // /
        0x54 => Some((0x35, KEYEVENTF_EXTENDEDKEY)), // Keypad /
        0xE5 => Some((0x36, 0)),                     // Right Shift
        0x55 => Some((0x37, 0)),                     // Keypad *
        0xE2 => Some((0x38, 0)),                     // Left Alt
        0xE6 => Some((0x38, KEYEVENTF_EXTENDEDKEY)), // Right Alt
        0x2C => Some((0x39, 0)),                     // Space
        0x39 => Some((0x3A, 0)),                     // Caps Lock
        0x3A => Some((0x3B, 0)),                     // F1
        0x3B => Some((0x3C, 0)),                     // F2
        0x3C => Some((0x3D, 0)),                     // F3
        0x3D => Some((0x3E, 0)),                     // F4
        0x3E => Some((0x3F, 0)),                     // F5
        0x3F => Some((0x40, 0)),                     // F6
        0x40 => Some((0x41, 0)),                     // F7
        0x41 => Some((0x42, 0)),                     // F8
        0x42 => Some((0x43, 0)),                     // F9
        0x43 => Some((0x44, 0)),                     // F10
        0x53 => Some((0x45, 0)),                     // Num Lock
        0x47 => Some((0x46, 0)),                     // Scroll Lock
        0x5F => Some((0x47, 0)),                     // Keypad 7
        0x4A => Some((0x47, KEYEVENTF_EXTENDEDKEY)), // Home
        0x60 => Some((0x48, 0)),                     // Keypad 8
        0x52 => Some((0x48, KEYEVENTF_EXTENDEDKEY)), // Up
        0x61 => Some((0x49, 0)),                     // Keypad 9
        0x4B => Some((0x49, KEYEVENTF_EXTENDEDKEY)), // Page Up
        0x56 => Some((0x4A, 0)),                     // Keypad -
        0x5C => Some((0x4B, 0)),                     // Keypad 4
        0x50 => Some((0x4B, KEYEVENTF_EXTENDEDKEY)), // Left
        0x5D => Some((0x4C, 0)),                     // Keypad 5
        0x5E => Some((0x4D, 0)),                     // Keypad 6
        0x4F => Some((0x4D, KEYEVENTF_EXTENDEDKEY)), // Right
        0x57 => Some((0x4E, 0)),                     // Keypad +
        0x59 => Some((0x4F, 0)),                     // Keypad 1
        0x4D => Some((0x4F, KEYEVENTF_EXTENDEDKEY)), // End
        0x5A => Some((0x50, 0)),                     // Keypad 2
        0x51 => Some((0x50, KEYEVENTF_EXTENDEDKEY)), // Down
        0x5B => Some((0x51, 0)),                     // Keypad 3
        0x4E => Some((0x51, KEYEVENTF_EXTENDEDKEY)), // Page Down
        0x62 => Some((0x52, 0)),                     // Keypad 0
        0x49 => Some((0x52, KEYEVENTF_EXTENDEDKEY)), // Insert
        0x63 => Some((0x53, 0)),                     // Keypad .
        0x4C => Some((0x53, KEYEVENTF_EXTENDEDKEY)), // Delete
        0x58 => Some((0x1C, KEYEVENTF_EXTENDEDKEY)), // Keypad Enter
        0x46 => Some((0x37, KEYEVENTF_EXTENDEDKEY)), // PrintScreen
        0x48 => Some((0x45, KEYEVENTF_EXTENDEDKEY)), // Pause
        0x64 => Some((0x56, 0)),                     // Non-US \ / ISO
        0x44 => Some((0x57, 0)),                     // F11
        0x45 => Some((0x58, 0)),                     // F12
        0x67 => Some((0x59, 0)),                     // Keypad =
        0xE3 => Some((0x5B, KEYEVENTF_EXTENDEDKEY)), // Left GUI
        0xE7 => Some((0x5C, KEYEVENTF_EXTENDEDKEY)), // Right GUI
        0x65 => Some((0x5D, KEYEVENTF_EXTENDEDKEY)), // Menu
        0x68 => Some((0x64, 0)),                     // F13
        0x69 => Some((0x65, 0)),                     // F14
        0x6A => Some((0x66, 0)),                     // F15
        0x6B => Some((0x67, 0)),                     // F16
        0x6C => Some((0x68, 0)),                     // F17
        0x6D => Some((0x69, 0)),                     // F18
        0x6E => Some((0x6A, 0)),                     // F19
        0x6F => Some((0x6B, 0)),                     // F20
        0x70 => Some((0x6C, 0)),                     // F21
        0x71 => Some((0x6D, 0)),                     // F22
        0x72 => Some((0x6E, 0)),                     // F23
        0x73 => Some((0x76, 0)),                     // F24
        0x7E => Some((0x76, 0)),                     // F24 alias
        0x7F => Some((0x20, KEYEVENTF_EXTENDEDKEY)), // Mute
        0x80 => Some((0x30, KEYEVENTF_EXTENDEDKEY)), // Volume Up
        0x81 => Some((0x2E, KEYEVENTF_EXTENDEDKEY)), // Volume Down
        0x82 => Some((0x5F, KEYEVENTF_EXTENDEDKEY)),
        0x85 => Some((0x7E, 0)),                     // Keypad ,
        0x87 => Some((0x73, 0)),
        0x88 => Some((0x70, 0)),
        0x89 => Some((0x7D, 0)),
        0x8A => Some((0x79, 0)),
        0x8B => Some((0x7B, 0)),
        0x90 => Some((0xF2, 0)),
        0x91 => Some((0xF1, 0)),
        0xB5 => Some((0x19, KEYEVENTF_EXTENDEDKEY)), // Next Track
        0xB6 => Some((0x10, KEYEVENTF_EXTENDEDKEY)), // Prev Track
        0xB7 => Some((0x24, KEYEVENTF_EXTENDEDKEY)), // Stop
        0xCD => Some((0x22, KEYEVENTF_EXTENDEDKEY)), // Play/Pause
        0x182 => Some((0x66, KEYEVENTF_EXTENDEDKEY)),
        0x18A => Some((0x6C, KEYEVENTF_EXTENDEDKEY)),
        0x192 => Some((0x21, KEYEVENTF_EXTENDEDKEY)),
        0x193 => Some((0x6D, KEYEVENTF_EXTENDEDKEY)),
        0x194 => Some((0x6B, KEYEVENTF_EXTENDEDKEY)),
        0x221 => Some((0x65, KEYEVENTF_EXTENDEDKEY)),
        0x223 => Some((0x32, KEYEVENTF_EXTENDEDKEY)),
        0x224 => Some((0x6A, KEYEVENTF_EXTENDEDKEY)),
        0x225 => Some((0x69, KEYEVENTF_EXTENDEDKEY)),
        0x227 => Some((0x67, KEYEVENTF_EXTENDEDKEY)),
        _ => None,
    }
}

fn modifier_hid_to_bit(hid: u32) -> Option<u16> {
    match hid {
        0xE1 => Some(1 << 0), // Left Shift
        0xE5 => Some(1 << 1), // Right Shift
        0xE0 => Some(1 << 2), // Left Control
        0xE4 => Some(1 << 3), // Right Control
        0xE2 => Some(1 << 4), // Left Alt
        0xE6 => Some(1 << 5), // Right Alt
        0xE3 => Some(1 << 6), // Left GUI
        0xE7 => Some(1 << 7), // Right GUI
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::{hid_to_windows_scancode, modifier_hid_to_virtual_key};
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::KEYEVENTF_EXTENDEDKEY;

    #[test]
    fn hid_to_windows_scancode_maps_standard_keys() {
        assert_eq!(hid_to_windows_scancode(0x04), Some((0x1E, 0)));
        assert_eq!(hid_to_windows_scancode(0x28), Some((0x1C, 0)));
        assert_eq!(hid_to_windows_scancode(0x2C), Some((0x39, 0)));
        assert_eq!(hid_to_windows_scancode(0x64), Some((0x56, 0)));
        assert_eq!(hid_to_windows_scancode(0x67), Some((0x59, 0)));
        assert_eq!(hid_to_windows_scancode(0x68), Some((0x64, 0)));
        assert_eq!(hid_to_windows_scancode(0x85), Some((0x7E, 0)));
        assert_eq!(hid_to_windows_scancode(0x87), Some((0x73, 0)));
        assert_eq!(hid_to_windows_scancode(0x88), Some((0x70, 0)));
        assert_eq!(hid_to_windows_scancode(0x89), Some((0x7D, 0)));
        assert_eq!(hid_to_windows_scancode(0x8A), Some((0x79, 0)));
        assert_eq!(hid_to_windows_scancode(0x8B), Some((0x7B, 0)));
        assert_eq!(hid_to_windows_scancode(0x90), Some((0xF2, 0)));
        assert_eq!(hid_to_windows_scancode(0x91), Some((0xF1, 0)));
    }

    #[test]
    fn hid_to_windows_scancode_maps_extended_keys() {
        assert_eq!(
            hid_to_windows_scancode(0xE4),
            Some((0x1D, KEYEVENTF_EXTENDEDKEY))
        );
        assert_eq!(
            hid_to_windows_scancode(0xE6),
            Some((0x38, KEYEVENTF_EXTENDEDKEY))
        );
        assert_eq!(
            hid_to_windows_scancode(0x50),
            Some((0x4B, KEYEVENTF_EXTENDEDKEY))
        );
        assert_eq!(
            hid_to_windows_scancode(0x58),
            Some((0x1C, KEYEVENTF_EXTENDEDKEY))
        );
        assert_eq!(
            hid_to_windows_scancode(0x46),
            Some((0x37, KEYEVENTF_EXTENDEDKEY))
        );
        assert_eq!(
            hid_to_windows_scancode(0x48),
            Some((0x45, KEYEVENTF_EXTENDEDKEY))
        );
        assert_eq!(
            hid_to_windows_scancode(0x7F),
            Some((0x20, KEYEVENTF_EXTENDEDKEY))
        );
        assert_eq!(
            hid_to_windows_scancode(0x80),
            Some((0x30, KEYEVENTF_EXTENDEDKEY))
        );
        assert_eq!(
            hid_to_windows_scancode(0x81),
            Some((0x2E, KEYEVENTF_EXTENDEDKEY))
        );
        assert_eq!(
            hid_to_windows_scancode(0x82),
            Some((0x5F, KEYEVENTF_EXTENDEDKEY))
        );
        assert_eq!(
            hid_to_windows_scancode(0xCD),
            Some((0x22, KEYEVENTF_EXTENDEDKEY))
        );
        assert_eq!(
            hid_to_windows_scancode(0x182),
            Some((0x66, KEYEVENTF_EXTENDEDKEY))
        );
        assert_eq!(
            hid_to_windows_scancode(0x18A),
            Some((0x6C, KEYEVENTF_EXTENDEDKEY))
        );
        assert_eq!(
            hid_to_windows_scancode(0x192),
            Some((0x21, KEYEVENTF_EXTENDEDKEY))
        );
        assert_eq!(
            hid_to_windows_scancode(0x193),
            Some((0x6D, KEYEVENTF_EXTENDEDKEY))
        );
        assert_eq!(
            hid_to_windows_scancode(0x194),
            Some((0x6B, KEYEVENTF_EXTENDEDKEY))
        );
        assert_eq!(
            hid_to_windows_scancode(0x221),
            Some((0x65, KEYEVENTF_EXTENDEDKEY))
        );
        assert_eq!(
            hid_to_windows_scancode(0x223),
            Some((0x32, KEYEVENTF_EXTENDEDKEY))
        );
        assert_eq!(
            hid_to_windows_scancode(0x224),
            Some((0x6A, KEYEVENTF_EXTENDEDKEY))
        );
        assert_eq!(
            hid_to_windows_scancode(0x225),
            Some((0x69, KEYEVENTF_EXTENDEDKEY))
        );
        assert_eq!(
            hid_to_windows_scancode(0x227),
            Some((0x67, KEYEVENTF_EXTENDEDKEY))
        );
    }

    #[test]
    fn inject_screen_lock_dry_run_succeeds() {
        use crate::platform::EventInjection;
        let injection = super::WindowsEventInjection::new_test();
        assert!(injection.inject_key_press(0x19E).is_ok());
        assert!(injection.inject_key_release(0x19E).is_ok());
    }

    #[test]
    fn hid_to_windows_scancode_returns_none_for_unmapped_key() {
        assert_eq!(hid_to_windows_scancode(0xFFFF), None);
    }

    #[test]
    fn modifier_hid_codes_use_logical_windows_keys() {
        assert_eq!(modifier_hid_to_virtual_key(0xE0), Some(0xA2));
        assert_eq!(modifier_hid_to_virtual_key(0xE1), Some(0xA0));
        assert_eq!(modifier_hid_to_virtual_key(0xE2), Some(0xA4));
        assert_eq!(modifier_hid_to_virtual_key(0xE3), Some(0x5B));
        assert_eq!(modifier_hid_to_virtual_key(0xE4), Some(0xA3));
        assert_eq!(modifier_hid_to_virtual_key(0xE5), Some(0xA1));
        assert_eq!(modifier_hid_to_virtual_key(0xE6), Some(0xA5));
        assert_eq!(modifier_hid_to_virtual_key(0xE7), Some(0x5C));
        assert_eq!(modifier_hid_to_virtual_key(0x04), None);
    }

    #[test]
    fn release_all_keys_clears_mouse_states() {
        use crate::platform::EventInjection;
        use std::sync::atomic::Ordering;
        let injection = super::WindowsEventInjection::new_test();
        injection.pressed_buttons.store(0b0000_0011, Ordering::Relaxed);
        assert_eq!(injection.pressed_buttons.load(Ordering::Relaxed), 3);

        injection.release_all_keys().unwrap();

        assert_eq!(injection.pressed_buttons.load(Ordering::Relaxed), 0);
    }
}
