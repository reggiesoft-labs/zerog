//! macOS event-injection subsystem backed by CoreGraphics events.

use anyhow::{Result, anyhow};
use core_graphics::event::{
    CGEvent, CGEventFlags, CGEventTapLocation, CGEventType, CGMouseButton, EventField,
};
use core_graphics::event_source::{CGEventSource, CGEventSourceStateID};
use core_graphics::geometry::CGPoint;

use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU16, Ordering};
use std::time::Instant;

use crate::platform::{DisplayManager, EventInjection};

use super::capture::{
    POINTS_PER_WHEEL_NOTCH, UNITS_PER_POINT, WHEEL_DELTA_UNIT, local_source_marker,
};
use super::click_state::ClickState;
use super::display::MacosDisplayManager;
use super::ffi;

pub(crate) struct MacosEventInjection {
    dry_run: bool,
    cursor_hidden: AtomicBool,
    pressed_buttons: AtomicU8,
    modifier_state: AtomicU16,
    last_position: Mutex<Option<CGPoint>>,
    clicks: Mutex<ClickState>,
    source: CGEventSource,
}

impl MacosEventInjection {
    pub(crate) fn new() -> Self {
        Self::with_dry_run(false)
    }

    #[cfg(test)]
    pub(crate) fn new_test() -> Self {
        Self::with_dry_run(true)
    }

    fn with_dry_run(dry_run: bool) -> Self {
        let source = CGEventSource::new(CGEventSourceStateID::HIDSystemState)
            .expect("Failed to create CGEventSource");
        if !dry_run {
            let _ = ffi::associate_mouse_and_cursor(true);
            let _ = ffi::show_cursor();
        }
        Self {
            dry_run,
            cursor_hidden: AtomicBool::new(false),
            pressed_buttons: AtomicU8::new(0),
            modifier_state: AtomicU16::new(0),
            last_position: Mutex::new(None),
            clicks: Mutex::new(ClickState::default()),
            source,
        }
    }
}

unsafe impl Send for MacosEventInjection {}
unsafe impl Sync for MacosEventInjection {}

impl MacosEventInjection {
    fn set_button_pressed(&self, button: u8, pressed: bool) {
        let mask = 1u8 << button;
        let prev = self.pressed_buttons.load(Ordering::Relaxed);
        let new = if pressed { prev | mask } else { prev & !mask };
        self.pressed_buttons.store(new, Ordering::Relaxed);
    }

    fn get_pressed_buttons(&self) -> u8 {
        self.pressed_buttons.load(Ordering::Relaxed)
    }

    fn post_line_scroll(&self, lines_x: i32, lines_y: i32) -> Result<()> {
        let event = ffi::create_line_scroll_wheel_event(&self.source, lines_x, lines_y)?;
        event.set_integer_value_field(EventField::SCROLL_WHEEL_EVENT_IS_CONTINUOUS, 0);
        event.set_integer_value_field(
            EventField::SCROLL_WHEEL_EVENT_POINT_DELTA_AXIS_1,
            i64::from(lines_y) * i64::from(POINTS_PER_WHEEL_NOTCH),
        );
        event.set_integer_value_field(
            EventField::SCROLL_WHEEL_EVENT_POINT_DELTA_AXIS_2,
            i64::from(lines_x) * i64::from(POINTS_PER_WHEEL_NOTCH),
        );
        post_tagged_scroll_event(&event);
        Ok(())
    }

    fn post_pixel_scroll(&self, px_x: i32, px_y: i32) -> Result<()> {
        let event = ffi::create_scroll_wheel_event(&self.source, px_x, px_y)?;
        event.set_integer_value_field(EventField::SCROLL_WHEEL_EVENT_IS_CONTINUOUS, 1);
        post_tagged_scroll_event(&event);
        Ok(())
    }

    /// Posts a `FlagsChanged` event carrying the current Caps Lock state.
    ///
    /// `is_active` must already be reflected in `modifier_state`; callers
    /// update that bit first so the event flags match the tracked state.
    fn post_caps_lock_flags(&self, is_active: bool) -> Result<()> {
        let source = self.source.clone();
        let event = CGEvent::new_keyboard_event(source, MAC_KEY_CAPS_LOCK, is_active)
            .map_err(|_| anyhow!("Failed to create Caps Lock event"))?;
        let flags = modifier_state_to_flags(self.modifier_state.load(Ordering::Relaxed));
        event.set_flags(flags);
        ffi::set_event_type(&event, CGEventType::FlagsChanged);
        event.set_integer_value_field(
            EventField::EVENT_SOURCE_USER_DATA,
            local_source_marker() as i64,
        );
        if !self.dry_run {
            event.post(CGEventTapLocation::HID);
        }
        Ok(())
    }

    fn release_modifiers(&self, source: &CGEventSource) -> Result<()> {
        let modifier_state = self.modifier_state.fetch_and(MOD_CAPS_LOCK, Ordering::Relaxed);

        if (modifier_state & !MOD_CAPS_LOCK) != 0 {
            tracing::debug!(
                modifier_state = format_args!("{:04x}", modifier_state),
                "releasing modifier keys"
            );
        }

        for hid in 0xE0..=0xE7 {
            release_modifier(source, hid)?;
        }

        Ok(())
    }

    fn release_mouse_buttons(&self, source: &CGEventSource) {
        let mut clicks = self.clicks.lock().expect("click state lock poisoned");
        let pressed_buttons = self.get_pressed_buttons();
        if pressed_buttons == 0 {
            clicks.reset();
            return;
        }

        let current_location = CGEvent::new(source.clone())
            .map(|e| e.location())
            .unwrap_or(CGPoint { x: 0.0, y: 0.0 });

        for button in 0..8 {
            if pressed_buttons & (1u8 << button) != 0 {
                release_mouse_button(source, current_location, button, clicks.count(button));
            }
        }

        self.pressed_buttons.store(0, Ordering::Relaxed);
        clicks.reset();
    }

    fn inject_shortcut(&self, keycode: u16, extra_flags: CGEventFlags, pressed: bool) -> Result<()> {
        if self.dry_run {
            return Ok(());
        }
        let source = self.source.clone();
        let event = CGEvent::new_keyboard_event(source, keycode, pressed)
            .map_err(|_| anyhow!("Failed to create shortcut event"))?;
        let flags = modifier_state_to_flags(self.modifier_state.load(Ordering::Relaxed)) | extra_flags;
        event.set_flags(flags);
        event.set_integer_value_field(
            EventField::EVENT_SOURCE_USER_DATA,
            local_source_marker() as i64,
        );
        event.post(CGEventTapLocation::HID);
        Ok(())
    }
}

impl EventInjection for MacosEventInjection {
    fn inject_mouse_move_subpixel(&self, x: f64, y: f64) -> Result<()> {
        let point = CGPoint { x, y };
        let (dx, dy) = {
            let mut last_pos = self.last_position.lock().unwrap();
            let delta = match *last_pos {
                Some(prev) => (x - prev.x, y - prev.y),
                None => (0.0, 0.0),
            };
            *last_pos = Some(point);
            delta
        };

        let pressed = self.get_pressed_buttons();
        let source = self.source.clone();

        if pressed != 0 {
            let event_type = if pressed & 0x02 != 0 {
                CGEventType::RightMouseDragged
            } else {
                CGEventType::LeftMouseDragged
            };

            let button = if pressed & 0x02 != 0 {
                CGMouseButton::Right
            } else {
                CGMouseButton::Left
            };

            let event = CGEvent::new_mouse_event(source, event_type, point, button)
                .map_err(|_| anyhow!("Failed to create mouse drag event"))?;

            let flags = modifier_state_to_flags(self.modifier_state.load(Ordering::Relaxed));
            event.set_flags(flags);

            event.set_integer_value_field(
                EventField::EVENT_SOURCE_USER_DATA,
                local_source_marker() as i64,
            );
            event.set_integer_value_field(EventField::MOUSE_EVENT_DELTA_X, dx.round() as i64);
            event.set_integer_value_field(EventField::MOUSE_EVENT_DELTA_Y, dy.round() as i64);
            ffi::set_double_value_field(&event, EventField::MOUSE_EVENT_DELTA_X, dx);
            ffi::set_double_value_field(&event, EventField::MOUSE_EVENT_DELTA_Y, dy);

            event.post(CGEventTapLocation::HID);
        } else {
            let event = CGEvent::new_mouse_event(
                source,
                CGEventType::MouseMoved,
                point,
                CGMouseButton::Left,
            )
            .map_err(|_| anyhow!("Failed to create mouse moved event"))?;

            event.set_integer_value_field(
                EventField::EVENT_SOURCE_USER_DATA,
                local_source_marker() as i64,
            );
            event.set_integer_value_field(EventField::MOUSE_EVENT_DELTA_X, dx.round() as i64);
            event.set_integer_value_field(EventField::MOUSE_EVENT_DELTA_Y, dy.round() as i64);
            ffi::set_double_value_field(&event, EventField::MOUSE_EVENT_DELTA_X, dx);
            ffi::set_double_value_field(&event, EventField::MOUSE_EVENT_DELTA_Y, dy);

            event.post(CGEventTapLocation::HID);
        }
        Ok(())
    }

    fn inject_mouse_move(&self, x: i32, y: i32) -> Result<()> {
        self.inject_mouse_move_subpixel(f64::from(x), f64::from(y))
    }

    fn inject_mouse_move_relative(&self, dx: i32, dy: i32) -> Result<()> {
        let source = self.source.clone();
        let current_location = CGEvent::new(source.clone())
            .map(|e| e.location())
            .unwrap_or(CGPoint { x: 0.0, y: 0.0 });
        let point = CGPoint {
            x: current_location.x + f64::from(dx),
            y: current_location.y + f64::from(dy),
        };
        if let Ok(mut last_pos) = self.last_position.lock() {
            *last_pos = Some(point);
        }

        let pressed = self.get_pressed_buttons();
        let (event_type, button) = if pressed != 0 {
            if pressed & 0x02 != 0 {
                (CGEventType::RightMouseDragged, CGMouseButton::Right)
            } else {
                (CGEventType::LeftMouseDragged, CGMouseButton::Left)
            }
        } else {
            (CGEventType::MouseMoved, CGMouseButton::Left)
        };

        let event = CGEvent::new_mouse_event(source, event_type, point, button)
            .map_err(|_| anyhow!("Failed to create mouse event"))?;

        let flags = modifier_state_to_flags(self.modifier_state.load(Ordering::Relaxed));
        event.set_flags(flags);

        event.set_integer_value_field(
            EventField::EVENT_SOURCE_USER_DATA,
            local_source_marker() as i64,
        );
        event.set_integer_value_field(EventField::MOUSE_EVENT_DELTA_X, i64::from(dx));
        event.set_integer_value_field(EventField::MOUSE_EVENT_DELTA_Y, i64::from(dy));
        ffi::set_double_value_field(&event, EventField::MOUSE_EVENT_DELTA_X, f64::from(dx));
        ffi::set_double_value_field(&event, EventField::MOUSE_EVENT_DELTA_Y, f64::from(dy));

        event.post(CGEventTapLocation::HID);
        Ok(())
    }

    fn inject_mouse_button(&self, button: u8, pressed: bool) -> Result<()> {
        self.set_button_pressed(button, pressed);

        let source = self.source.clone();

        let event_type = match (button, pressed) {
            (0, true) => CGEventType::LeftMouseDown,
            (0, false) => CGEventType::LeftMouseUp,
            (1, true) => CGEventType::RightMouseDown,
            (1, false) => CGEventType::RightMouseUp,
            (_, true) => CGEventType::OtherMouseDown,
            (_, false) => CGEventType::OtherMouseUp,
        };

        let current_location = CGEvent::new(source.clone())
            .map(|e| e.location())
            .unwrap_or(CGPoint { x: 0.0, y: 0.0 });

        let event = CGEvent::new_mouse_event(
            source,
            event_type,
            current_location,
            mouse_button_from_index(button),
        )
        .map_err(|_| anyhow!("Failed to create mouse button event"))?;

        let mut clicks = self.clicks.lock().expect("click state lock poisoned");
        let click_count = if pressed {
            clicks.press(
                button,
                (current_location.x, current_location.y),
                Instant::now(),
                ffi::double_click_interval(),
            )
        } else {
            clicks.count(button)
        };
        event.set_integer_value_field(EventField::MOUSE_EVENT_CLICK_STATE, click_count);

        if button > 1 {
            event.set_integer_value_field(EventField::MOUSE_EVENT_BUTTON_NUMBER, i64::from(button));
        }

        let flags = modifier_state_to_flags(self.modifier_state.load(Ordering::Relaxed));
        event.set_flags(flags);

        event.set_integer_value_field(
            EventField::EVENT_SOURCE_USER_DATA,
            local_source_marker() as i64,
        );

        event.post(CGEventTapLocation::HID);
        Ok(())
    }

    fn inject_mouse_wheel(&self, delta_x: i32, delta_y: i32) -> Result<()> {
        if delta_x == 0 && delta_y == 0 {
            return Ok(());
        }
        let (lines_x, remainder_x) = split_scroll_delta(delta_x);
        let (lines_y, remainder_y) = split_scroll_delta(delta_y);
        if lines_x != 0 || lines_y != 0 {
            self.post_line_scroll(lines_x, lines_y)?;
        }
        let px_x = scale_delta_to_pixels(remainder_x);
        let px_y = scale_delta_to_pixels(remainder_y);
        if px_x != 0 || px_y != 0 {
            self.post_pixel_scroll(px_x, px_y)?;
        }
        Ok(())
    }

    fn inject_key_press(&self, keycode: u32) -> Result<()> {
        if keycode == HID_KEY_CAPS_LOCK {
            let is_active = if !self.dry_run {
                ffi::toggle_caps_lock()?
            } else {
                (self.modifier_state.load(Ordering::Relaxed) & MOD_CAPS_LOCK) == 0
            };
            if is_active {
                self.modifier_state.fetch_or(MOD_CAPS_LOCK, Ordering::Relaxed);
            } else {
                self.modifier_state.fetch_and(!MOD_CAPS_LOCK, Ordering::Relaxed);
            }

            return self.post_caps_lock_flags(is_active);
        }

        if let Some(media_key) = hid_to_system_media_key(keycode) {
            if self.dry_run {
                return Ok(());
            }
            return inject_system_media_key(media_key, true);
        }

        if let Some(app) = hid_to_app_launch(keycode) {
            if self.dry_run {
                return Ok(());
            }
            let _ = std::process::Command::new("open")
                .args(["-a", app])
                .spawn();
            return Ok(());
        }

        if let Some((shortcut_key, extra_flags)) = hid_to_mac_shortcut(keycode) {
            return self.inject_shortcut(shortcut_key, extra_flags, true);
        }

        let is_modifier = modifier_hid_to_bit(keycode);
        if let Some(bit) = is_modifier {
            self.modifier_state.fetch_or(bit, Ordering::Relaxed);
        }

        let source = self.source.clone();

        let mac_keycode = hid_to_mac_keycode(keycode)
            .ok_or_else(|| anyhow!("Unsupported HID keycode for macOS injection: {keycode}"))?;
        let event = CGEvent::new_keyboard_event(source, mac_keycode, true)
            .map_err(|_| anyhow!("Failed to create key press event"))?;

        let flags = modifier_state_to_flags(self.modifier_state.load(Ordering::Relaxed));
        event.set_flags(flags);

        if is_modifier.is_some() {
            ffi::set_event_type(&event, CGEventType::FlagsChanged);
        }

        event.set_integer_value_field(
            EventField::EVENT_SOURCE_USER_DATA,
            local_source_marker() as i64,
        );
        if !self.dry_run {
            event.post(CGEventTapLocation::HID);
        }

        Ok(())
    }

    fn inject_key_release(&self, keycode: u32) -> Result<()> {
        if keycode == HID_KEY_CAPS_LOCK {
            return Ok(());
        }

        if let Some(media_key) = hid_to_system_media_key(keycode) {
            if self.dry_run {
                return Ok(());
            }
            return inject_system_media_key(media_key, false);
        }

        if hid_to_app_launch(keycode).is_some() {
            return Ok(());
        }

        if let Some((shortcut_key, extra_flags)) = hid_to_mac_shortcut(keycode) {
            return self.inject_shortcut(shortcut_key, extra_flags, false);
        }

        let is_modifier = modifier_hid_to_bit(keycode);
        if let Some(bit) = is_modifier {
            self.modifier_state.fetch_and(!bit, Ordering::Relaxed);
        }

        let source = self.source.clone();

        let mac_keycode = hid_to_mac_keycode(keycode)
            .ok_or_else(|| anyhow!("Unsupported HID keycode for macOS injection: {keycode}"))?;
        let event = CGEvent::new_keyboard_event(source, mac_keycode, false)
            .map_err(|_| anyhow!("Failed to create key release event"))?;

        let flags = modifier_state_to_flags(self.modifier_state.load(Ordering::Relaxed));
        event.set_flags(flags);

        if is_modifier.is_some() {
            ffi::set_event_type(&event, CGEventType::FlagsChanged);
        }

        event.set_integer_value_field(
            EventField::EVENT_SOURCE_USER_DATA,
            local_source_marker() as i64,
        );
        if !self.dry_run {
            event.post(CGEventTapLocation::HID);
        }

        Ok(())
    }

    fn caps_lock_active(&self) -> Option<bool> {
        ffi::caps_lock_state().ok()
    }

    fn set_caps_lock(&self, active: bool) -> Result<()> {
        let current = if self.dry_run {
            self.modifier_state.load(Ordering::Relaxed) & MOD_CAPS_LOCK != 0
        } else {
            ffi::caps_lock_state()?
        };
        if current == active {
            return Ok(());
        }
        if !self.dry_run {
            ffi::set_caps_lock_state(active)?;
        }
        if active {
            self.modifier_state.fetch_or(MOD_CAPS_LOCK, Ordering::Relaxed);
        } else {
            self.modifier_state.fetch_and(!MOD_CAPS_LOCK, Ordering::Relaxed);
        }
        self.post_caps_lock_flags(active)
    }

    fn set_cursor_hidden(&self, hidden: bool) -> Result<()> {
        let previous = self.cursor_hidden.swap(hidden, Ordering::SeqCst);
        if previous == hidden || self.dry_run {
            return Ok(());
        }

        let result = if hidden {
            ffi::hide_cursor()
        } else {
            ffi::show_cursor()
        };
        if result.is_err() {
            self.cursor_hidden.store(previous, Ordering::SeqCst);
        }
        result
    }

    fn warp_cursor(&self, x: i32, y: i32) -> Result<()> {
        let display = MacosDisplayManager::new();
        let (clamped_x, clamped_y) = display.clamp_to_active_display(x, y)?;
        let point = CGPoint {
            x: clamped_x as f64,
            y: clamped_y as f64,
        };
        if let Ok(mut last_pos) = self.last_position.lock() {
            *last_pos = Some(point);
        }
        ffi::warp_mouse_cursor_position(point)?;
        // CGWarpMouseCursorPosition suppresses subsequent mouse delta events
        // on macOS. Toggling CGAssociateMouseAndMouseCursorPosition resets the
        // internal delta tracking so the cursor responds immediately.
        ffi::associate_mouse_and_cursor(false)?;
        ffi::associate_mouse_and_cursor(true)
    }

    fn lock_cursor_position(&self) -> Result<()> {
        ffi::associate_mouse_and_cursor(false)
    }

    fn unlock_cursor_position(&self) -> Result<()> {
        ffi::associate_mouse_and_cursor(true)
    }

    fn restore_cursor_position(&self, x: i32, y: i32) -> Result<()> {
        let display = MacosDisplayManager::new();
        let (clamped_x, clamped_y) = display.clamp_to_active_display(x, y)?;
        ffi::associate_mouse_and_cursor(false)?;
        let move_result = self.inject_mouse_move(clamped_x, clamped_y);
        let unlock_result = ffi::associate_mouse_and_cursor(true);
        move_result?;
        unlock_result
    }

    fn release_all_keys(&self) -> Result<()> {
        self.release_modifiers(&self.source)?;
        self.release_mouse_buttons(&self.source);

        Ok(())
    }

    fn declare_user_activity(&self) -> Result<()> {
        ffi::declare_user_activity()
    }
}

impl Drop for MacosEventInjection {
    fn drop(&mut self) {
        let _ = self.release_all_keys();
        let _ = ffi::associate_mouse_and_cursor(true);
        let _ = ffi::show_cursor();
        self.cursor_hidden.store(false, Ordering::SeqCst);
    }
}

fn release_modifier(source: &CGEventSource, hid: u32) -> Result<()> {
    let Some(mac_keycode) = hid_to_mac_keycode(hid) else {
        return Ok(());
    };

    let event = CGEvent::new_keyboard_event(source.clone(), mac_keycode, false)
        .map_err(|_| anyhow!("Failed to create key release event"))?;

    event.set_flags(core_graphics::event::CGEventFlags::empty());
    ffi::set_event_type(&event, CGEventType::FlagsChanged);

    event.set_integer_value_field(
        EventField::EVENT_SOURCE_USER_DATA,
        local_source_marker() as i64,
    );
    event.post(CGEventTapLocation::HID);
    Ok(())
}

fn release_mouse_button(source: &CGEventSource, location: CGPoint, button: u8, click_count: i64) {
    let event_type = match button {
        0 => CGEventType::LeftMouseUp,
        1 => CGEventType::RightMouseUp,
        _ => CGEventType::OtherMouseUp,
    };

    if let Ok(event) = CGEvent::new_mouse_event(
        source.clone(),
        event_type,
        location,
        mouse_button_from_index(button),
    ) {
        event.set_integer_value_field(EventField::MOUSE_EVENT_CLICK_STATE, click_count);
        if button > 1 {
            event.set_integer_value_field(EventField::MOUSE_EVENT_BUTTON_NUMBER, i64::from(button));
        }

        event.set_integer_value_field(
            EventField::EVENT_SOURCE_USER_DATA,
            local_source_marker() as i64,
        );

        event.post(CGEventTapLocation::HID);
    }
}

fn mouse_button_from_index(idx: u8) -> CGMouseButton {
    match idx {
        1 => CGMouseButton::Right,
        2 => CGMouseButton::Center,
        _ => CGMouseButton::Left,
    }
}

const MAC_KEY_CAPS_LOCK: u16 = 0x39;
const HID_KEY_CAPS_LOCK: u32 = 0x39;
const MOD_CAPS_LOCK: u16 = 1 << 8;

fn modifier_hid_to_bit(hid: u32) -> Option<u16> {
    match hid {
        0xE1 => Some(1 << 0), // Left Shift
        0xE5 => Some(1 << 1), // Right Shift
        0xE0 => Some(1 << 2), // Left Control
        0xE4 => Some(1 << 3), // Right Control
        0xE2 => Some(1 << 4), // Left Alt/Option
        0xE6 => Some(1 << 5), // Right Alt/Option
        0xE3 => Some(1 << 6), // Left GUI/Command
        0xE7 => Some(1 << 7), // Right GUI/Command
        _ => None,
    }
}

fn modifier_state_to_flags(modifier_state: u16) -> CGEventFlags {
    let mut flags = CGEventFlags::empty();
    if modifier_state & (1 << 0 | 1 << 1) != 0 {
        flags |= CGEventFlags::CGEventFlagShift;
    }
    if modifier_state & (1 << 2 | 1 << 3) != 0 {
        flags |= CGEventFlags::CGEventFlagControl;
    }
    if modifier_state & (1 << 4 | 1 << 5) != 0 {
        flags |= CGEventFlags::CGEventFlagAlternate;
    }
    if modifier_state & (1 << 6 | 1 << 7) != 0 {
        flags |= CGEventFlags::CGEventFlagCommand;
    }
    if modifier_state & MOD_CAPS_LOCK != 0 {
        flags |= CGEventFlags::CGEventFlagAlphaShift;
    }
    flags
}

fn hid_to_mac_keycode(hid: u32) -> Option<u16> {
    match hid {
        0x04 => Some(0x00), // A
        0x05 => Some(0x0B), // B
        0x06 => Some(0x08), // C
        0x07 => Some(0x02), // D
        0x08 => Some(0x0E), // E
        0x09 => Some(0x03), // F
        0x0A => Some(0x05), // G
        0x0B => Some(0x04), // H
        0x0C => Some(0x22), // I
        0x0D => Some(0x26), // J
        0x0E => Some(0x28), // K
        0x0F => Some(0x25), // L
        0x10 => Some(0x2E), // M
        0x11 => Some(0x2D), // N
        0x12 => Some(0x1F), // O
        0x13 => Some(0x23), // P
        0x14 => Some(0x0C), // Q
        0x15 => Some(0x0F), // R
        0x16 => Some(0x01), // S
        0x17 => Some(0x11), // T
        0x18 => Some(0x20), // U
        0x19 => Some(0x09), // V
        0x1A => Some(0x0D), // W
        0x1B => Some(0x07), // X
        0x1C => Some(0x10), // Y
        0x1D => Some(0x06), // Z
        0x1E => Some(0x12), // 1
        0x1F => Some(0x13), // 2
        0x20 => Some(0x14), // 3
        0x21 => Some(0x15), // 4
        0x22 => Some(0x17), // 5
        0x23 => Some(0x16), // 6
        0x24 => Some(0x1A), // 7
        0x25 => Some(0x1C), // 8
        0x26 => Some(0x19), // 9
        0x27 => Some(0x1D), // 0
        0x28 => Some(0x24), // Enter
        0x29 => Some(0x35), // Escape
        0x2A => Some(0x33), // Backspace
        0x2B => Some(0x30), // Tab
        0x2C => Some(0x31), // Space
        0x2D => Some(0x1B), // -
        0x2E => Some(0x18), // =
        0x2F => Some(0x21), // [
        0x30 => Some(0x1E), // ]
        0x31 => Some(0x2A), // \
        0x33 => Some(0x29), // ;
        0x34 => Some(0x27), // '
        0x35 => Some(0x32), // `
        0x36 => Some(0x2B), // ,
        0x37 => Some(0x2F), // .
        0x38 => Some(0x2C), // /
        0x39 => Some(0x39), // Caps Lock
        0x3A => Some(0x7A), // F1
        0x3B => Some(0x78), // F2
        0x3C => Some(0x63), // F3
        0x3D => Some(0x76), // F4
        0x3E => Some(0x60), // F5
        0x3F => Some(0x61), // F6
        0x40 => Some(0x62), // F7
        0x41 => Some(0x64), // F8
        0x42 => Some(0x65), // F9
        0x43 => Some(0x6D), // F10
        0x44 => Some(0x67), // F11
        0x45 => Some(0x6F), // F12
        0x46 => Some(0x69), // Print Screen -> F13
        0x47 => Some(0x6B), // Scroll Lock -> F14
        0x48 => Some(0x71), // Pause -> F15
        0x49 => Some(0x72), // Insert/Help
        0x4A => Some(0x73), // Home
        0x4B => Some(0x74), // Page Up
        0x4C => Some(0x75), // Delete Forward
        0x4D => Some(0x77), // End
        0x4E => Some(0x79), // Page Down
        0x4F => Some(0x7C), // Right Arrow
        0x50 => Some(0x7B), // Left Arrow
        0x51 => Some(0x7D), // Down Arrow
        0x52 => Some(0x7E), // Up Arrow
        0x53 => Some(0x47), // Num Lock/Clear
        0x54 => Some(0x4B), // Keypad /
        0x55 => Some(0x43), // Keypad *
        0x56 => Some(0x4E), // Keypad -
        0x57 => Some(0x45), // Keypad +
        0x58 => Some(0x4C), // Keypad Enter
        0x59 => Some(0x53), // Keypad 1
        0x5A => Some(0x54), // Keypad 2
        0x5B => Some(0x55), // Keypad 3
        0x5C => Some(0x56), // Keypad 4
        0x5D => Some(0x57), // Keypad 5
        0x5E => Some(0x58), // Keypad 6
        0x5F => Some(0x59), // Keypad 7
        0x60 => Some(0x5B), // Keypad 8
        0x61 => Some(0x5C), // Keypad 9
        0x62 => Some(0x52), // Keypad 0
        0x63 => Some(0x41), // Keypad .
        0x64 => Some(0x0A),
        0x65 => Some(0x6E), // Menu / Application -> kVK_ContextualMenu
        0x67 => Some(0x51), // Keypad =
        0x68 => Some(0x69), // F13
        0x69 => Some(0x6B), // F14
        0x6A => Some(0x71), // F15
        0x6B => Some(0x6A), // F16
        0x6C => Some(0x40), // F17
        0x6D => Some(0x4F), // F18
        0x6E => Some(0x50), // F19
        0x6F => Some(0x5A), // F20
        0x7F => Some(0x4A), // Mute
        0x80 => Some(0x48), // Volume Up
        0x81 => Some(0x49), // Volume Down
        0x85 => Some(0x5F), // Keypad ,
        0x87 => Some(0x5E),
        0x88 => Some(0x68),
        0x89 => Some(0x5D),
        0x8A => Some(0x68),
        0x8B => Some(0x66),
        0x90 => Some(0x68),
        0x91 => Some(0x66),
        0x94 => Some(0x68),
        0xE0 => Some(0x3B), // Left Control
        0xE1 => Some(0x38), // Left Shift
        0xE2 => Some(0x3A), // Left Option
        0xE3 => Some(0x37), // Left Command
        0xE4 => Some(0x3E), // Right Control
        0xE5 => Some(0x3C), // Right Shift
        0xE6 => Some(0x3D), // Right Option
        0xE7 => Some(0x36), // Right Command
        _ => None,
    }
}

const NX_KEYTYPE_SOUND_UP: u32 = 0;
const NX_KEYTYPE_SOUND_DOWN: u32 = 1;
const NX_KEYTYPE_BRIGHTNESS_UP: u32 = 2;
const NX_KEYTYPE_BRIGHTNESS_DOWN: u32 = 3;
const NX_KEYTYPE_POWER_KEY: u32 = 6;
const NX_KEYTYPE_MUTE: u32 = 7;
const NX_KEYTYPE_EJECT: u32 = 14;
const NX_KEYTYPE_PLAY: u32 = 16;
const NX_KEYTYPE_NEXT: u32 = 17;
const NX_KEYTYPE_PREVIOUS: u32 = 18;
const NX_KEYTYPE_FAST: u32 = 19;
const NX_KEYTYPE_REWIND: u32 = 20;
const NX_KEYTYPE_ILLUMINATION_UP: u32 = 21;
const NX_KEYTYPE_ILLUMINATION_DOWN: u32 = 22;
const NX_KEYTYPE_ILLUMINATION_TOGGLE: u32 = 23;

fn hid_to_system_media_key(hid: u32) -> Option<u32> {
    match hid {
        0x7F => Some(NX_KEYTYPE_MUTE),
        0x80 => Some(NX_KEYTYPE_SOUND_UP),
        0x81 => Some(NX_KEYTYPE_SOUND_DOWN),
        0x82 => Some(NX_KEYTYPE_POWER_KEY),
        0xCD => Some(NX_KEYTYPE_PLAY),
        0xB5 => Some(NX_KEYTYPE_NEXT),
        0xB6 => Some(NX_KEYTYPE_PREVIOUS),
        0xB3 => Some(NX_KEYTYPE_FAST),
        0xB4 => Some(NX_KEYTYPE_REWIND),
        0xB7 => Some(NX_KEYTYPE_PLAY),
        0xB8 => Some(NX_KEYTYPE_EJECT),
        0x000C_006F => Some(NX_KEYTYPE_BRIGHTNESS_UP),
        0x000C_0070 => Some(NX_KEYTYPE_BRIGHTNESS_DOWN),
        0x000C_0079 => Some(NX_KEYTYPE_ILLUMINATION_UP),
        0x000C_007A => Some(NX_KEYTYPE_ILLUMINATION_DOWN),
        0x000C_007C => Some(NX_KEYTYPE_ILLUMINATION_TOGGLE),
        _ => None,
    }
}

fn hid_to_app_launch(hid: u32) -> Option<&'static str> {
    match hid {
        0x18A => Some("Mail"),
        0x192 => Some("Calculator"),
        0x193 => Some("Music"),
        0x194 => Some("Finder"),
        0x196 => Some("Safari"),
        _ => None,
    }
}

fn hid_to_mac_shortcut(hid: u32) -> Option<(u16, CGEventFlags)> {
    match hid {
        0x182 => Some((0x0B, CGEventFlags::CGEventFlagCommand | CGEventFlags::CGEventFlagAlternate)),
        0x19E => Some((0x0C, CGEventFlags::CGEventFlagCommand | CGEventFlags::CGEventFlagControl)),
        0x221 => Some((0x31, CGEventFlags::CGEventFlagCommand)),
        0x223 => Some((0x04, CGEventFlags::CGEventFlagCommand | CGEventFlags::CGEventFlagShift)),
        0x224 => Some((0x21, CGEventFlags::CGEventFlagCommand)),
        0x225 => Some((0x1E, CGEventFlags::CGEventFlagCommand)),
        0x227 => Some((0x0F, CGEventFlags::CGEventFlagCommand)),
        _ => None,
    }
}

fn inject_system_media_key(key: u32, pressed: bool) -> Result<()> {
    let event = ffi::create_system_media_event(key, pressed)?;
    event.set_integer_value_field(
        EventField::EVENT_SOURCE_USER_DATA,
        local_source_marker() as i64,
    );
    event.post(CGEventTapLocation::HID);
    Ok(())
}

fn post_tagged_scroll_event(event: &CGEvent) {
    event.set_integer_value_field(
        EventField::EVENT_SOURCE_USER_DATA,
        local_source_marker() as i64,
    );
    event.post(CGEventTapLocation::HID);
}

pub(crate) fn split_scroll_delta(delta: i32) -> (i32, i32) {
    (delta / WHEEL_DELTA_UNIT, delta % WHEEL_DELTA_UNIT)
}

pub(crate) fn scale_delta_to_pixels(delta: i32) -> i32 {
    if delta == 0 {
        return 0;
    }
    let half_step = UNITS_PER_POINT / 2;
    let rounded = (delta + if delta > 0 { half_step } else { -half_step }) / UNITS_PER_POINT;
    if rounded == 0 {
        if delta > 0 { 1 } else { -1 }
    } else {
        rounded
    }
}

#[cfg(test)]
mod tests {
    use super::{
        hid_to_app_launch, hid_to_mac_keycode, hid_to_mac_shortcut, hid_to_system_media_key,
        scale_delta_to_pixels, split_scroll_delta,
    };

    #[test]
    fn split_scroll_delta_separates_whole_notches() {
        assert_eq!(split_scroll_delta(120), (1, 0));
        assert_eq!(split_scroll_delta(-120), (-1, 0));
        assert_eq!(split_scroll_delta(240), (2, 0));
        assert_eq!(split_scroll_delta(150), (1, 30));
        assert_eq!(split_scroll_delta(-150), (-1, -30));
        assert_eq!(split_scroll_delta(60), (0, 60));
        assert_eq!(split_scroll_delta(0), (0, 0));
    }

    #[test]
    fn scale_delta_to_pixels_scales_correctly() {
        assert_eq!(scale_delta_to_pixels(120), 10);
        assert_eq!(scale_delta_to_pixels(-120), -10);
        assert_eq!(scale_delta_to_pixels(24), 2);
        assert_eq!(scale_delta_to_pixels(-24), -2);
        assert_eq!(scale_delta_to_pixels(6), 1);
        assert_eq!(scale_delta_to_pixels(-6), -1);
        assert_eq!(scale_delta_to_pixels(0), 0);
    }

    #[test]
    fn hid_to_mac_keycode_maps_common_keys() {
        assert_eq!(hid_to_mac_keycode(0x04), Some(0x00));
        assert_eq!(hid_to_mac_keycode(0x28), Some(0x24));
        assert_eq!(hid_to_mac_keycode(0x2C), Some(0x31));
        assert_eq!(hid_to_mac_keycode(0x46), Some(0x69)); // Print Screen -> F13
        assert_eq!(hid_to_mac_keycode(0x47), Some(0x6B)); // Scroll Lock -> F14
        assert_eq!(hid_to_mac_keycode(0x48), Some(0x71)); // Pause -> F15
        assert_eq!(hid_to_mac_keycode(0x50), Some(0x7B));
        assert_eq!(hid_to_mac_keycode(0x64), Some(0x0A));
        assert_eq!(hid_to_mac_keycode(0x65), Some(0x6E)); // Contextual Menu
        assert_eq!(hid_to_mac_keycode(0x68), Some(0x69)); // F13
        assert_eq!(hid_to_mac_keycode(0x69), Some(0x6B)); // F14
        assert_eq!(hid_to_mac_keycode(0x6A), Some(0x71)); // F15
        assert_eq!(hid_to_mac_keycode(0x6B), Some(0x6A)); // F16
        assert_eq!(hid_to_mac_keycode(0x6F), Some(0x5A)); // F20
        assert_eq!(hid_to_mac_keycode(0x7F), Some(0x4A)); // Mute
        assert_eq!(hid_to_mac_keycode(0x80), Some(0x48)); // Volume Up
        assert_eq!(hid_to_mac_keycode(0x81), Some(0x49)); // Volume Down
        assert_eq!(hid_to_mac_keycode(0x85), Some(0x5F)); // Keypad ,
        assert_eq!(hid_to_mac_keycode(0x87), Some(0x5E));
        assert_eq!(hid_to_mac_keycode(0x88), Some(0x68));
        assert_eq!(hid_to_mac_keycode(0x89), Some(0x5D));
        assert_eq!(hid_to_mac_keycode(0x8A), Some(0x68));
        assert_eq!(hid_to_mac_keycode(0x8B), Some(0x66));
        assert_eq!(hid_to_mac_keycode(0x90), Some(0x68));
        assert_eq!(hid_to_mac_keycode(0x91), Some(0x66));
        assert_eq!(hid_to_mac_keycode(0x94), Some(0x68));
    }

    #[test]
    fn hid_to_app_launch_maps_apps() {
        assert_eq!(hid_to_app_launch(0x18A), Some("Mail"));
        assert_eq!(hid_to_app_launch(0x192), Some("Calculator"));
        assert_eq!(hid_to_app_launch(0x193), Some("Music"));
        assert_eq!(hid_to_app_launch(0x194), Some("Finder"));
        assert_eq!(hid_to_app_launch(0x196), Some("Safari"));
        assert_eq!(hid_to_app_launch(0xFFFF), None);
    }

    #[test]
    fn hid_to_mac_shortcut_maps_browser_keys() {
        use core_graphics::event::CGEventFlags;
        assert_eq!(
            hid_to_mac_shortcut(0x182),
            Some((0x0B, CGEventFlags::CGEventFlagCommand | CGEventFlags::CGEventFlagAlternate))
        );
        assert_eq!(
            hid_to_mac_shortcut(0x19E),
            Some((0x0C, CGEventFlags::CGEventFlagCommand | CGEventFlags::CGEventFlagControl))
        );
        assert_eq!(
            hid_to_mac_shortcut(0x221),
            Some((0x31, CGEventFlags::CGEventFlagCommand))
        );
        assert_eq!(
            hid_to_mac_shortcut(0x223),
            Some((0x04, CGEventFlags::CGEventFlagCommand | CGEventFlags::CGEventFlagShift))
        );
        assert_eq!(
            hid_to_mac_shortcut(0x224),
            Some((0x21, CGEventFlags::CGEventFlagCommand))
        );
        assert_eq!(
            hid_to_mac_shortcut(0x225),
            Some((0x1E, CGEventFlags::CGEventFlagCommand))
        );
        assert_eq!(
            hid_to_mac_shortcut(0x227),
            Some((0x0F, CGEventFlags::CGEventFlagCommand))
        );
        assert_eq!(hid_to_mac_shortcut(0xFFFF), None);
    }

    #[test]
    fn hid_to_system_media_key_maps_media_keys() {
        assert_eq!(hid_to_system_media_key(0x7F), Some(7));
        assert_eq!(hid_to_system_media_key(0x80), Some(0));
        assert_eq!(hid_to_system_media_key(0x81), Some(1));
        assert_eq!(hid_to_system_media_key(0x82), Some(6));
        assert_eq!(hid_to_system_media_key(0xCD), Some(16));
        assert_eq!(hid_to_system_media_key(0xB5), Some(17));
        assert_eq!(hid_to_system_media_key(0xB6), Some(18));
        assert_eq!(hid_to_system_media_key(0xB3), Some(19));
        assert_eq!(hid_to_system_media_key(0xB4), Some(20));
        assert_eq!(hid_to_system_media_key(0xB7), Some(16));
        assert_eq!(hid_to_system_media_key(0xB8), Some(14));
        assert_eq!(hid_to_system_media_key(0x000C_006F), Some(2));
        assert_eq!(hid_to_system_media_key(0x000C_0070), Some(3));
        assert_eq!(hid_to_system_media_key(0x000C_0079), Some(21));
        assert_eq!(hid_to_system_media_key(0x000C_007A), Some(22));
        assert_eq!(hid_to_system_media_key(0x000C_007C), Some(23));
        assert_eq!(hid_to_system_media_key(0x04), None);
    }

    #[test]
    fn hid_to_mac_keycode_maps_modifiers() {
        assert_eq!(hid_to_mac_keycode(0xE0), Some(0x3B));
        assert_eq!(hid_to_mac_keycode(0xE1), Some(0x38));
        assert_eq!(hid_to_mac_keycode(0xE7), Some(0x36));
    }

    #[test]
    fn hid_to_mac_keycode_returns_none_for_unmapped_key() {
        assert_eq!(hid_to_mac_keycode(0xFFFF), None);
    }

    #[test]
    fn release_all_keys_clears_mouse_states() {
        use crate::platform::EventInjection;
        let injection = super::MacosEventInjection::new();
        injection.set_button_pressed(0, true);
        injection.set_button_pressed(1, true);

        assert_eq!(injection.get_pressed_buttons(), 3);

        injection.release_all_keys().unwrap();

        assert_eq!(injection.get_pressed_buttons(), 0);
    }

    #[test]
    fn declare_user_activity_succeeds() {
        use crate::platform::EventInjection;
        let injection = super::MacosEventInjection::new();
        assert!(injection.declare_user_activity().is_ok());
    }

    #[test]
    fn modifier_state_to_flags_converts_all_modifiers() {
        use super::modifier_state_to_flags;
        use core_graphics::event::CGEventFlags;

        assert_eq!(modifier_state_to_flags(0), CGEventFlags::empty());
        assert_eq!(
            modifier_state_to_flags(1 << 0),
            CGEventFlags::CGEventFlagShift
        );
        assert_eq!(
            modifier_state_to_flags(1 << 2),
            CGEventFlags::CGEventFlagControl
        );
        assert_eq!(
            modifier_state_to_flags(1 << 4),
            CGEventFlags::CGEventFlagAlternate
        );
        assert_eq!(
            modifier_state_to_flags(1 << 6),
            CGEventFlags::CGEventFlagCommand
        );
        assert_eq!(
            modifier_state_to_flags((1 << 4) | (1 << 6)),
            CGEventFlags::CGEventFlagAlternate | CGEventFlags::CGEventFlagCommand
        );
        assert_eq!(
            modifier_state_to_flags(super::MOD_CAPS_LOCK),
            CGEventFlags::CGEventFlagAlphaShift
        );
    }

    #[test]
    fn inject_key_press_and_release_tracks_modifiers() {
        use crate::platform::EventInjection;
        let injection = super::MacosEventInjection::new_test();

        assert_eq!(injection.modifier_state.load(std::sync::atomic::Ordering::Relaxed), 0);

        injection.inject_key_press(0xE1).unwrap(); // Left Shift
        assert_eq!(
            injection.modifier_state.load(std::sync::atomic::Ordering::Relaxed),
            1 << 0
        );

        injection.inject_key_press(0xE3).unwrap(); // Left Command
        assert_eq!(
            injection.modifier_state.load(std::sync::atomic::Ordering::Relaxed),
            (1 << 0) | (1 << 6)
        );

        injection.inject_key_release(0xE1).unwrap();
        assert_eq!(
            injection.modifier_state.load(std::sync::atomic::Ordering::Relaxed),
            1 << 6
        );

        injection.inject_key_release(0xE3).unwrap();
        assert_eq!(injection.modifier_state.load(std::sync::atomic::Ordering::Relaxed), 0);
    }

    #[test]
    fn inject_regular_key_succeeds() {
        use crate::platform::EventInjection;
        let injection = super::MacosEventInjection::new_test();
        assert!(injection.inject_key_press(0x04).is_ok()); // 'A'
        assert!(injection.inject_key_release(0x04).is_ok());
    }

    #[test]
    fn inject_media_keys_dry_run_succeeds() {
        use crate::platform::EventInjection;
        let injection = super::MacosEventInjection::new_test();
        assert!(injection.inject_key_press(0x7F).is_ok());
        assert!(injection.inject_key_release(0x7F).is_ok());
        assert!(injection.inject_key_press(0x80).is_ok());
        assert!(injection.inject_key_release(0x80).is_ok());
        assert!(injection.inject_key_press(0x81).is_ok());
        assert!(injection.inject_key_release(0x81).is_ok());
    }

    #[test]
    fn inject_caps_lock_dry_run_succeeds() {
        use crate::platform::EventInjection;
        let injection = super::MacosEventInjection::new_test();
        assert_eq!(
            injection.modifier_state.load(std::sync::atomic::Ordering::Relaxed),
            0
        );
        assert!(injection.inject_key_press(0x39).is_ok());
        assert_eq!(
            injection.modifier_state.load(std::sync::atomic::Ordering::Relaxed),
            super::MOD_CAPS_LOCK
        );
        assert!(injection.inject_key_release(0x39).is_ok());
        assert_eq!(
            injection.modifier_state.load(std::sync::atomic::Ordering::Relaxed),
            super::MOD_CAPS_LOCK
        );
        assert!(injection.inject_key_press(0x39).is_ok());
        assert_eq!(
            injection.modifier_state.load(std::sync::atomic::Ordering::Relaxed),
            0
        );
    }

    #[test]
    fn set_caps_lock_dry_run_updates_tracked_state() {
        use crate::platform::EventInjection;
        let injection = super::MacosEventInjection::new_test();

        injection.set_caps_lock(true).unwrap();
        assert_eq!(
            injection.modifier_state.load(std::sync::atomic::Ordering::Relaxed),
            super::MOD_CAPS_LOCK
        );

        injection.set_caps_lock(false).unwrap();
        assert_eq!(
            injection.modifier_state.load(std::sync::atomic::Ordering::Relaxed),
            0
        );
    }

    #[test]
    fn release_all_keys_preserves_caps_lock() {
        use crate::platform::EventInjection;
        let injection = super::MacosEventInjection::new_test();
        injection.modifier_state.store(
            super::MOD_CAPS_LOCK | (1 << 0),
            std::sync::atomic::Ordering::Relaxed,
        );
        injection.release_all_keys().unwrap();
        assert_eq!(
            injection.modifier_state.load(std::sync::atomic::Ordering::Relaxed),
            super::MOD_CAPS_LOCK
        );
    }
}
