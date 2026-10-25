//! Thin unsafe FFI wrappers for macOS ApplicationServices APIs.
//!
//! This module provides safe wrappers around unsafe CoreGraphics and
//! Accessibility APIs. Each wrapper handles error codes and converts
//! them to Rust `Result` types.

use anyhow::{Result, bail};
use core_foundation::base::TCFType;
use core_foundation::boolean::kCFBooleanTrue;
use core_foundation::string::CFString;
use core_graphics::event::{CGEvent, CGEventType};
use core_graphics::event_source::CGEventSource;
use core_graphics::geometry::CGPoint;
use foreign_types::ForeignType;
use std::process::Command;

pub(super) const SCROLL_EVENT_UNIT_PIXEL: u32 = 0;
pub(super) const SCROLL_EVENT_UNIT_LINE: u32 = 1;

pub(super) fn double_click_interval() -> std::time::Duration {
    use objc::{class, msg_send, sel, sel_impl};

    let seconds: f64 = unsafe { msg_send![class!(NSEvent), doubleClickInterval] };
    std::time::Duration::from_secs_f64(seconds)
}

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    fn CFRetain(cf: *const std::ffi::c_void) -> *const std::ffi::c_void;
}

#[link(name = "ApplicationServices", kind = "framework")]
unsafe extern "C" {
    fn AXIsProcessTrusted() -> bool;
    fn CGMainDisplayID() -> u32;
    fn CGDisplayHideCursor(display: u32) -> i32;
    fn CGDisplayShowCursor(display: u32) -> i32;
    fn CGWarpMouseCursorPosition(new_cursor_position: CGPoint) -> i32;
    fn CGAssociateMouseAndMouseCursorPosition(connected: bool) -> i32;
}

#[link(name = "CoreGraphics", kind = "framework")]
unsafe extern "C" {
    fn _CGSDefaultConnection() -> isize;
    fn CGSSetConnectionProperty(
        conn: isize,
        target: isize,
        key: *const std::ffi::c_void,
        value: *const std::ffi::c_void,
    );
    fn CGEventSetType(event: *mut std::ffi::c_void, event_type: u32);
    fn CGEventGetDoubleValueField(event: *const std::ffi::c_void, field: u32) -> f64;
    fn CGEventSetDoubleValueField(event: *mut std::ffi::c_void, field: u32, value: f64);
    fn CGEventCreateScrollWheelEvent2(
        source: *const std::ffi::c_void,
        units: u32,
        wheel_count: u32,
        wheel1: i32,
        wheel2: i32,
        wheel3: i32,
    ) -> *mut std::ffi::c_void;
}

const K_IOHID_SYSTEM_CLASS: &[u8] = b"IOHIDSystem\0";
const K_IOHID_PARAM_CONNECT_TYPE: u32 = 1;
const K_IOHID_CAPS_LOCK_STATE: i32 = 1;

type IoConnect = u32;
type IoService = u32;
type KernReturn = i32;

#[link(name = "IOKit", kind = "framework")]
unsafe extern "C" {
    fn IOPMAssertionDeclareUserActivity(
        assertion_name: core_foundation::string::CFStringRef,
        user_type: u32,
        assertion_id: *mut u32,
    ) -> i32;
    fn mach_task_self() -> u32;
    fn IOServiceMatching(name: *const u8) -> *const std::ffi::c_void;
    fn IOServiceGetMatchingService(main_port: u32, matching: *const std::ffi::c_void) -> IoService;
    fn IOServiceOpen(
        service: IoService,
        owning_task: u32,
        connect_type: u32,
        connect: *mut IoConnect,
    ) -> KernReturn;
    fn IOServiceClose(connect: IoConnect) -> KernReturn;
    fn IOObjectRelease(object: IoService) -> KernReturn;
    fn IOHIDGetModifierLockState(handle: IoConnect, selector: i32, state: *mut bool) -> KernReturn;
    fn IOHIDSetModifierLockState(handle: IoConnect, selector: i32, state: bool) -> KernReturn;
}

/// Checks if the current process has accessibility permissions.
///
/// # Safety
///
/// Calls AXIsProcessTrusted() which is thread-safe and always returns a valid bool.
pub(super) fn is_accessibility_trusted() -> bool {
    unsafe { AXIsProcessTrusted() }
}

/// Opens the macOS Accessibility system preferences pane.
pub(super) fn open_accessibility_settings() {
    let _ = Command::new("open")
        .arg("x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility")
        .status();
}

fn enable_cursor_in_background() {
    // _CGSDefaultConnection and CGSSetConnectionProperty are private/undocumented
    // Apple APIs. On macOS 16 (Tahoe) these APIs changed behaviour and calling
    // CGSSetConnectionProperty with an invalid connection causes SIGABRT.
    // The app already requires Accessibility permission, so CGDisplayHideCursor /
    // CGDisplayShowCursor work in the background without this workaround.
    // Guard against a zero/negative connection handle to avoid the crash.
    let key = CFString::new("SetsCursorInBackground");
    unsafe {
        let conn = _CGSDefaultConnection();
        if conn <= 0 {
            return;
        }
        CGSSetConnectionProperty(
            conn,
            conn,
            key.as_CFTypeRef(),
            kCFBooleanTrue as *const std::ffi::c_void,
        );
    }
}

/// Hides the cursor on the main display.
///
/// # Safety
///
/// Calls CGDisplayHideCursor with the main display ID. The display ID is
/// always valid for the main display. Returns an error if the call fails.
pub(super) fn hide_cursor() -> Result<()> {
    enable_cursor_in_background();
    let display = unsafe { CGMainDisplayID() };
    let result = unsafe { CGDisplayHideCursor(display) };
    if result == 0 {
        Ok(())
    } else {
        bail!("CGDisplayHideCursor failed with error code {result}")
    }
}

/// Shows the cursor on the main display.
///
/// # Safety
///
/// Calls CGDisplayShowCursor with the main display ID. The display ID is
/// always valid for the main display. Returns an error if the call fails.
pub(super) fn show_cursor() -> Result<()> {
    enable_cursor_in_background();
    let display = unsafe { CGMainDisplayID() };
    let result = unsafe { CGDisplayShowCursor(display) };
    if result == 0 {
        Ok(())
    } else {
        bail!("CGDisplayShowCursor failed with error code {result}")
    }
}

/// Associates or disassociates mouse movement with cursor position.
///
/// When `connected` is false, physical mouse movement does not move the cursor.
/// This is used to lock the cursor in place while sending input to a remote peer.
///
/// # Safety
///
/// Calls CGAssociateMouseAndMouseCursorPosition with a boolean parameter.
/// This is safe to call from any thread. Returns an error if the call fails.
pub(super) fn associate_mouse_and_cursor(connected: bool) -> Result<()> {
    let result = unsafe { CGAssociateMouseAndMouseCursorPosition(connected) };
    if result == 0 {
        Ok(())
    } else {
        bail!("CGAssociateMouseAndMouseCursorPosition failed with error code {result}")
    }
}

/// Warps the cursor to an absolute position.
///
/// # Safety
///
/// Calls CGWarpMouseCursorPosition with a valid CGPoint. The point coordinates
/// are clamped to display bounds by the caller. Returns an error if the call fails.
pub(super) fn warp_mouse_cursor_position(point: CGPoint) -> Result<()> {
    let result = unsafe { CGWarpMouseCursorPosition(point) };
    if result == 0 {
        Ok(())
    } else {
        bail!("CGWarpMouseCursorPosition failed with error code {result}")
    }
}

fn create_scroll_event(
    source: &CGEventSource,
    units: u32,
    delta_x: i32,
    delta_y: i32,
) -> Result<CGEvent> {
    unsafe {
        let raw = CGEventCreateScrollWheelEvent2(
            source.as_ptr() as *const std::ffi::c_void,
            units,
            2,
            delta_y,
            delta_x,
            0,
        );
        if raw.is_null() {
            bail!("CGEventCreateScrollWheelEvent2 failed");
        }
        Ok(CGEvent::from_ptr(raw as *mut core_graphics::sys::CGEvent))
    }
}

pub(super) fn create_scroll_wheel_event(
    source: &CGEventSource,
    delta_x_px: i32,
    delta_y_px: i32,
) -> Result<CGEvent> {
    create_scroll_event(source, SCROLL_EVENT_UNIT_PIXEL, delta_x_px, delta_y_px)
}

pub(super) fn create_line_scroll_wheel_event(
    source: &CGEventSource,
    delta_x_lines: i32,
    delta_y_lines: i32,
) -> Result<CGEvent> {
    create_scroll_event(source, SCROLL_EVENT_UNIT_LINE, delta_x_lines, delta_y_lines)
}

pub(super) fn set_event_type(event: &CGEvent, event_type: CGEventType) {
    unsafe {
        CGEventSetType(event.as_ptr() as *mut std::ffi::c_void, event_type as u32);
    }
}

pub(super) fn get_double_value_field(event: &CGEvent, field: u32) -> f64 {
    unsafe { CGEventGetDoubleValueField(event.as_ptr() as *const std::ffi::c_void, field) }
}

pub(super) fn set_double_value_field(event: &CGEvent, field: u32, value: f64) {
    unsafe {
        CGEventSetDoubleValueField(event.as_ptr() as *mut std::ffi::c_void, field, value);
    }
}

pub(super) fn declare_user_activity() -> Result<()> {
    let name = CFString::new("mouse-thing keep alive");
    let mut assertion_id: u32 = 0;
    let result = unsafe {
        IOPMAssertionDeclareUserActivity(name.as_concrete_TypeRef(), 0, &mut assertion_id)
    };
    if result == 0 {
        Ok(())
    } else {
        bail!("IOPMAssertionDeclareUserActivity failed with error code {result}")
    }
}

pub(super) fn create_system_media_event(key: u32, pressed: bool) -> Result<CGEvent> {
    use objc::runtime::Object;
    use objc::{class, msg_send, sel, sel_impl};

    let flags = if pressed { 0xa00usize } else { 0xb00usize };
    let data1 = ((key as isize) << 16) | ((if pressed { 0xa } else { 0xb }) << 8);
    let event: *mut Object = unsafe {
        msg_send![class!(NSEvent),
            otherEventWithType: 14usize
            location: CGPoint { x: 0.0, y: 0.0 }
            modifierFlags: flags
            timestamp: 0.0f64
            windowNumber: 0isize
            context: std::ptr::null_mut::<Object>()
            subtype: 8i16
            data1: data1
            data2: -1isize
        ]
    };
    if event.is_null() {
        bail!("Failed to create system-defined media event");
    }
    let raw: *mut core_graphics::sys::CGEvent = unsafe { msg_send![event, CGEvent] };
    if raw.is_null() {
        bail!("Failed to get CGEvent from NSEvent");
    }
    unsafe {
        CFRetain(raw as *const std::ffi::c_void);
    }
    Ok(unsafe { CGEvent::from_ptr(raw) })
}

pub(super) fn decode_system_media_event(event: &CGEvent) -> Option<(u32, bool)> {
    objc::rc::autoreleasepool(|| {
        use objc::runtime::Object;
        use objc::{class, msg_send, sel, sel_impl};

        let raw: *mut core_graphics::sys::CGEvent = event.as_ptr();
        let ns_event: *mut Object = unsafe { msg_send![class!(NSEvent), eventWithCGEvent: raw] };
        if ns_event.is_null() {
            return None;
        }
        let subtype: i16 = unsafe { msg_send![ns_event, subtype] };
        if subtype != 8 {
            return None;
        }
        let data1: isize = unsafe { msg_send![ns_event, data1] };
        let key_type = (((data1 as usize) & 0xFFFF_0000) >> 16) as u32;
        let key_flags = ((data1 as usize) & 0x0000_FFFF) as u32;
        let key_state = (key_flags & 0xFF00) >> 8;
        let pressed = match key_state {
            0xA => true,
            0xB => false,
            _ => return None,
        };
        Some((key_type, pressed))
    })
}

/// Runs `f` with a connection to the IOHIDSystem service open.
fn with_iohid_system<T>(f: impl FnOnce(IoConnect) -> Result<T>) -> Result<T> {
    unsafe {
        let matching = IOServiceMatching(K_IOHID_SYSTEM_CLASS.as_ptr());
        if matching.is_null() {
            bail!("IOServiceMatching failed");
        }
        let service = IOServiceGetMatchingService(0, matching);
        if service == 0 {
            bail!("IOServiceGetMatchingService failed");
        }
        let mut connect = 0;
        let kr = IOServiceOpen(
            service,
            mach_task_self(),
            K_IOHID_PARAM_CONNECT_TYPE,
            &mut connect,
        );
        if kr != 0 {
            IOObjectRelease(service);
            bail!("IOServiceOpen failed with error code {kr}");
        }
        let result = f(connect);
        IOServiceClose(connect);
        IOObjectRelease(service);
        result
    }
}

/// Reads the system-wide Caps Lock state.
pub(super) fn caps_lock_state() -> Result<bool> {
    with_iohid_system(|connect| unsafe {
        let mut state = false;
        let kr = IOHIDGetModifierLockState(connect, K_IOHID_CAPS_LOCK_STATE, &mut state);
        if kr != 0 {
            bail!("IOHIDGetModifierLockState failed with error code {kr}");
        }
        Ok(state)
    })
}

/// Sets the system-wide Caps Lock state to exactly `active`.
pub(super) fn set_caps_lock_state(active: bool) -> Result<()> {
    with_iohid_system(|connect| unsafe {
        let kr = IOHIDSetModifierLockState(connect, K_IOHID_CAPS_LOCK_STATE, active);
        if kr != 0 {
            bail!("IOHIDSetModifierLockState failed with error code {kr}");
        }
        Ok(())
    })
}

pub(super) fn toggle_caps_lock() -> Result<bool> {
    with_iohid_system(|connect| unsafe {
        let mut state = false;
        let get_kr = IOHIDGetModifierLockState(connect, K_IOHID_CAPS_LOCK_STATE, &mut state);
        if get_kr != 0 {
            bail!("IOHIDGetModifierLockState failed with error code {get_kr}");
        }
        let new_state = !state;
        let _ = IOHIDSetModifierLockState(connect, K_IOHID_CAPS_LOCK_STATE, new_state);
        Ok(new_state)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn system_media_event_roundtrip() {
        let event_up_press = create_system_media_event(0, true).unwrap();
        assert_eq!(decode_system_media_event(&event_up_press), Some((0, true)));

        let event_up_release = create_system_media_event(0, false).unwrap();
        assert_eq!(decode_system_media_event(&event_up_release), Some((0, false)));

        let event_down_press = create_system_media_event(1, true).unwrap();
        assert_eq!(decode_system_media_event(&event_down_press), Some((1, true)));

        let event_down_release = create_system_media_event(1, false).unwrap();
        assert_eq!(decode_system_media_event(&event_down_release), Some((1, false)));

        let event_mute_press = create_system_media_event(7, true).unwrap();
        assert_eq!(decode_system_media_event(&event_mute_press), Some((7, true)));

        let event_mute_release = create_system_media_event(7, false).unwrap();
        assert_eq!(decode_system_media_event(&event_mute_release), Some((7, false)));
    }
}
