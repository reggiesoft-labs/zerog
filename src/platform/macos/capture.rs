//! macOS event-capture subsystem using CGEventTap.
//!
//! This module implements low-level input event capture on macOS using the
//! Core Graphics event tap API. It captures mouse movements, button clicks,
//! scroll wheel events, and keyboard input from the session-level event stream.
//!
//! # Architecture
//!
//! - [`MacosEventCapture`] implements [`EventCapture`] by installing a
//!   `CGEventTap` on a dedicated thread with its own CFRunLoop
//! - Events are filtered through a source marker to suppress self-injected events
//! - The tap runs in a separate thread to avoid blocking the main thread
//!
//! # Thread Safety
//!
//! The capture state uses atomics for `installed` and `capture_policy` flags,
//! and mutexes for the event receiver and worker handle. The CFRunLoop must
//! run on the same thread that installed the tap.
//!
//! # Self-Injection Suppression
//!
//! Each process generates a unique source marker stored in `EVENT_SOURCE_USER_DATA`.
//! When capturing events, any event with our marker is ignored to prevent
//! feedback loops when injecting events.

use anyhow::{Context, Result, anyhow};
use core_foundation::base::TCFType;
use core_foundation::mach_port::{CFMachPort, CFMachPortInvalidate, CFMachPortRef};
use core_foundation::runloop::CFRunLoop;
use core_graphics::event::{CGEvent, CGEventFlags, CallbackResult, EventField};
use foreign_types::ForeignType;
use std::process;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU16, AtomicU64, Ordering};

use super::ffi;
use std::sync::mpsc::SyncSender;
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::sync::mpsc;
use tracing::warn;

use crate::platform::shared::capture_queue;
use crate::platform::{CapturePolicy, CapturedInputEvent, EventCapture};

/// macOS event capture implementation using CGEventTap.
///
/// Creates a session-level event tap that intercepts input events before
/// they reach applications. Events are queued and made available via a
/// channel receiver.
pub(crate) struct MacosEventCapture {
    state: Arc<CaptureState>,
}

/// Shared state between the capture handle and the event tap thread.
struct CaptureState {
    installed: AtomicBool,
    source_marker: u64,
    modifier_state: AtomicU16,
    capture_policy: Arc<AtomicU8>,
    event_rx: Mutex<Option<mpsc::Receiver<CapturedInputEvent>>>,
    run_loop: Mutex<Option<CFRunLoop>>,
    worker: Mutex<Option<JoinHandle<()>>>,
}

impl MacosEventCapture {
    pub(crate) fn new(capture_policy: Arc<AtomicU8>) -> Self {
        Self {
            state: Arc::new(CaptureState {
                installed: AtomicBool::new(false),
                source_marker: local_source_marker(),
                modifier_state: AtomicU16::new(0),
                capture_policy,
                event_rx: Mutex::new(None),
                run_loop: Mutex::new(None),
                worker: Mutex::new(None),
            }),
        }
    }

    pub(crate) fn take_event_receiver(&self) -> Option<mpsc::Receiver<CapturedInputEvent>> {
        self.state
            .event_rx
            .lock()
            .expect("event receiver lock poisoned")
            .take()
    }

    pub(crate) fn source_marker(&self) -> u64 {
        self.state.source_marker
    }
}

impl EventCapture for MacosEventCapture {
    fn install_tap(&self) -> Result<()> {
        if self.state.installed.swap(true, Ordering::SeqCst) {
            return Ok(());
        }

        let (event_tx, event_rx) = mpsc::channel(capture_queue::EVENT_QUEUE_CAPACITY);
        let (started_tx, started_rx) = std::sync::mpsc::sync_channel(1);
        let state = Arc::clone(&self.state);

        let worker = thread::Builder::new()
            .name("mouse-capture-tap".to_string())
            .spawn(move || run_event_tap_thread(state, event_tx, started_tx))
            .context("failed to spawn macOS event capture thread")?;

        let run_loop = match started_rx
            .recv()
            .map_err(|_| anyhow!("event tap setup channel closed unexpectedly"))?
        {
            Ok(loop_ref) => loop_ref,
            Err(err) => {
                self.state.installed.store(false, Ordering::SeqCst);
                let _ = worker.join();
                return Err(anyhow!(err));
            }
        };

        {
            let mut guard = self
                .state
                .event_rx
                .lock()
                .map_err(|_| anyhow!("event receiver lock poisoned"))?;
            *guard = Some(event_rx);
        }

        {
            let mut guard = self
                .state
                .run_loop
                .lock()
                .map_err(|_| anyhow!("run loop lock poisoned"))?;
            *guard = Some(run_loop);
        }

        {
            let mut guard = self
                .state
                .worker
                .lock()
                .map_err(|_| anyhow!("capture worker lock poisoned"))?;
            *guard = Some(worker);
        }

        Ok(())
    }

    fn uninstall_tap(&self) -> Result<()> {
        if !self.state.installed.swap(false, Ordering::SeqCst) {
            return Ok(());
        }

        if let Some(run_loop) = self
            .state
            .run_loop
            .lock()
            .map_err(|_| anyhow!("run loop lock poisoned"))?
            .take()
        {
            run_loop.stop();
        }

        if let Some(worker) = self
            .state
            .worker
            .lock()
            .map_err(|_| anyhow!("capture worker lock poisoned"))?
            .take()
        {
            worker
                .join()
                .map_err(|_| anyhow!("macOS capture worker thread panicked"))?;
        }

        if let Ok(mut guard) = self.state.event_rx.lock() {
            guard.take();
        }

        Ok(())
    }

    fn is_installed(&self) -> bool {
        self.state.installed.load(Ordering::SeqCst)
    }
}

struct TapCallbackData {
    state: Arc<CaptureState>,
    event_tx: mpsc::Sender<CapturedInputEvent>,
    source_marker: u64,
}

unsafe extern "C" fn event_tap_callback(
    proxy: core_graphics::event::CGEventTapProxy,
    event_type: u32,
    event: *mut core_graphics::sys::CGEvent,
    user_info: *mut std::ffi::c_void,
) -> *mut core_graphics::sys::CGEvent {
    let data = unsafe { &*(user_info as *const TapCallbackData) };

    if event_type == CG_EVENT_TAP_DISABLED_BY_TIMEOUT
        || event_type == CG_EVENT_TAP_DISABLED_BY_USER_INPUT
    {
        warn!(event_type, "Event tap disabled by system; re-enabling");
        re_enable_event_tap(proxy);
        reset_modifier_state(&data.state, &data.event_tx);
        return event;
    }

    if event.is_null() {
        return event;
    }

    let cg_event = std::mem::ManuallyDrop::new(unsafe { CGEvent::from_ptr(event) });
    let result = handle_event_tap_callback(
        event_type,
        &cg_event,
        &data.state,
        data.source_marker,
        &data.event_tx,
    );
    match result {
        CallbackResult::Keep => event,
        CallbackResult::Drop => std::ptr::null_mut(),
        CallbackResult::Replace(new_event) => std::mem::ManuallyDrop::new(new_event).as_ptr(),
    }
}

fn reset_modifier_state(state: &CaptureState, event_tx: &mpsc::Sender<CapturedInputEvent>) {
    let previous = state.modifier_state.swap(0, Ordering::Relaxed);
    for &(hid, bit) in &[
        (0xE1, 1 << 0), // Left Shift
        (0xE5, 1 << 1), // Right Shift
        (0xE0, 1 << 2), // Left Ctrl
        (0xE4, 1 << 3), // Right Ctrl
        (0xE2, 1 << 4), // Left Alt
        (0xE6, 1 << 5), // Right Alt
        (0xE3, 1 << 6), // Left Cmd
        (0xE7, 1 << 7), // Right Cmd
    ] {
        if previous & bit != 0 {
            capture_queue::try_send(
                event_tx,
                CapturedInputEvent::KeyRelease { keycode: hid },
                &DROPPED_EVENTS,
                "macos",
            );
        }
    }
}

fn run_event_tap_thread(
    state: Arc<CaptureState>,
    event_tx: mpsc::Sender<CapturedInputEvent>,
    started_tx: SyncSender<Result<CFRunLoop, String>>,
) {
    crate::platform::shared::priority::set_realtime_thread_priority();
    let source_marker = state.source_marker;
    let callback_data = Box::new(TapCallbackData {
        state: Arc::clone(&state),
        event_tx,
        source_marker,
    });
    let raw_data = Box::into_raw(callback_data);

    let event_mask = capture_event_mask();
    let tap_ref = unsafe {
        CGEventTapCreate(
            1, // kCGSessionEventTap
            0, // kCGHeadInsertEventTap
            0, // kCGEventTapOptionDefault
            event_mask,
            event_tap_callback,
            raw_data as *mut std::ffi::c_void,
        )
    };

    if tap_ref.is_null() {
        let _ = unsafe { Box::from_raw(raw_data) };
        let _ = started_tx.send(Err("failed to install CGEventTap".to_string()));
        state.installed.store(false, Ordering::SeqCst);
        return;
    }

    let mach_port = unsafe { CFMachPort::wrap_under_create_rule(tap_ref) };
    let loop_source = match mach_port.create_runloop_source(0) {
        Ok(source) => source,
        Err(_) => {
            unsafe {
                CFMachPortInvalidate(mach_port.as_concrete_TypeRef());
                let _ = Box::from_raw(raw_data);
            }
            let _ = started_tx.send(Err("failed to create run loop source for CGEventTap".to_string()));
            state.installed.store(false, Ordering::SeqCst);
            return;
        }
    };

    let run_loop = CFRunLoop::get_current();
    run_loop.add_source(&loop_source, unsafe { core_foundation::runloop::kCFRunLoopCommonModes });
    unsafe { CGEventTapEnable(mach_port.as_concrete_TypeRef(), true) };

    let _ = started_tx.send(Ok(run_loop.clone()));
    CFRunLoop::run_current();

    unsafe {
        CFMachPortInvalidate(mach_port.as_concrete_TypeRef());
        let _ = Box::from_raw(raw_data);
    }

    state.installed.store(false, Ordering::SeqCst);
    if let Ok(mut guard) = state.run_loop.lock() {
        guard.take();
    }
}

fn handle_event_tap_callback(
    event_type: u32,
    event: &CGEvent,
    state: &CaptureState,
    source_marker: u64,
    event_tx: &mpsc::Sender<CapturedInputEvent>,
) -> CallbackResult {
    let incoming_marker = event.get_integer_value_field(EventField::EVENT_SOURCE_USER_DATA);
    if incoming_marker == source_marker as i64 {
        return CallbackResult::Keep;
    }

    if event_type == CG_EVENT_KEY_DOWN || event_type == CG_EVENT_KEY_UP {
        reconcile_modifiers(event, state, event_tx);
    }

    let is_emergency = is_emergency_key_event(event_type, event)
        && !should_deliver_locally(&state.capture_policy);
    let deliver_locally = is_emergency || should_deliver_locally(&state.capture_policy);

    if event_type == CG_EVENT_FLAGS_CHANGED {
        let mac_keycode = event.get_integer_value_field(EventField::KEYBOARD_EVENT_KEYCODE) as u16;
        if mac_keycode == MAC_KEY_CAPS_LOCK {
            capture_queue::try_send(
                event_tx,
                CapturedInputEvent::KeyPress {
                    keycode: HID_KEY_CAPS_LOCK,
                },
                &DROPPED_EVENTS,
                "macos",
            );
            capture_queue::try_send(
                event_tx,
                CapturedInputEvent::KeyRelease {
                    keycode: HID_KEY_CAPS_LOCK,
                },
                &DROPPED_EVENTS,
                "macos",
            );
            // Keep the event so the local OS toggles its own lock state and
            // any attached keyboard LED follows, even while controlling a
            // remote peer. The press is still forwarded to the peer above.
            return CallbackResult::Keep;
        }
    }

    let is_media_event = event_type == NX_SYSDEFINED;
    let message = event_to_message(event_type, event, state, deliver_locally);
    let is_recognized_media = is_media_event && message.is_some();

    if let Some(msg) = message {
        capture_queue::try_send(event_tx, msg, &DROPPED_EVENTS, "macos");
    }

    if deliver_locally || (is_media_event && !is_recognized_media) {
        CallbackResult::Keep
    } else {
        CallbackResult::Drop
    }
}

fn is_emergency_key_event(event_type: u32, event: &CGEvent) -> bool {
    if event_type != CG_EVENT_KEY_DOWN {
        return false;
    }

    let keycode = event.get_integer_value_field(EventField::KEYBOARD_EVENT_KEYCODE) as u16;
    let flags = event.get_flags();
    let ctrl = flags.contains(CGEventFlags::CGEventFlagControl);
    let alt = flags.contains(CGEventFlags::CGEventFlagAlternate);
    let shift = flags.contains(CGEventFlags::CGEventFlagShift);

    match keycode {
        0x35 | 0x33 => ctrl && alt && shift,
        0x71 => ctrl && alt,
        _ => false,
    }
}

fn should_deliver_locally(capture_policy: &Arc<AtomicU8>) -> bool {
    !CapturePolicy::from_u8(capture_policy.load(Ordering::Acquire)).suppresses_local_delivery()
}

static DROPPED_EVENTS: AtomicU64 = AtomicU64::new(0);

/// Re-enables an event tap that was disabled by timeout or user input.
///
/// # Safety
///
/// `proxy` must be a valid `CGEventTapProxy` from an event tap callback.
/// `CGEventTapEnable` requires a valid `CFMachPortRef` derived from the proxy.
fn re_enable_event_tap(proxy: core_graphics::event::CGEventTapProxy) {
    unsafe {
        CGEventTapEnable(proxy as CFMachPortRef, true);
    }
}

fn event_to_message(
    event_type: u32,
    event: &CGEvent,
    state: &CaptureState,
    locally_delivered: bool,
) -> Option<CapturedInputEvent> {
    match event_type {
        CG_EVENT_MOUSE_MOVED
        | CG_EVENT_LEFT_MOUSE_DRAGGED
        | CG_EVENT_RIGHT_MOUSE_DRAGGED
        | CG_EVENT_OTHER_MOUSE_DRAGGED => Some(mouse_move_message(event, locally_delivered)),
        CG_EVENT_LEFT_MOUSE_DOWN
        | CG_EVENT_LEFT_MOUSE_UP
        | CG_EVENT_RIGHT_MOUSE_DOWN
        | CG_EVENT_RIGHT_MOUSE_UP
        | CG_EVENT_OTHER_MOUSE_DOWN
        | CG_EVENT_OTHER_MOUSE_UP => mouse_button_message(event_type, event),
        CG_EVENT_SCROLL_WHEEL => scroll_message(event),
        CG_EVENT_KEY_DOWN => key_message(event, true),
        CG_EVENT_KEY_UP => key_message(event, false),
        CG_EVENT_FLAGS_CHANGED => modifier_flags_changed_to_event(event, state),
        NX_SYSDEFINED => system_media_message(event),
        _ => None,
    }
}

fn system_media_message(event: &CGEvent) -> Option<CapturedInputEvent> {
    let (key_type, pressed) = ffi::decode_system_media_event(event)?;
    let keycode = system_media_key_to_hid(key_type)?;
    if pressed {
        Some(CapturedInputEvent::KeyPress { keycode })
    } else {
        Some(CapturedInputEvent::KeyRelease { keycode })
    }
}

pub(crate) fn system_media_key_to_hid(key_type: u32) -> Option<u32> {
    match key_type {
        0 => Some(0x80),         // NX_KEYTYPE_SOUND_UP -> Volume Up
        1 => Some(0x81),         // NX_KEYTYPE_SOUND_DOWN -> Volume Down
        6 => Some(0x82),         // NX_POWER_KEY -> Power
        7 => Some(0x7F),         // NX_KEYTYPE_MUTE -> Mute
        16 => Some(0xCD),        // NX_KEYTYPE_PLAY -> Play/Pause
        17 => Some(0xB5),        // NX_KEYTYPE_NEXT -> Next Track
        18 => Some(0xB6),        // NX_KEYTYPE_PREVIOUS -> Previous Track
        19 => Some(0xB3),        // NX_KEYTYPE_FAST -> Fast Forward
        20 => Some(0xB4),        // NX_KEYTYPE_REWIND -> Rewind
        14 => Some(0xB8),        // NX_KEYTYPE_EJECT -> Eject
        2 => Some(0x000C_006F),  // NX_KEYTYPE_BRIGHTNESS_UP
        3 => Some(0x000C_0070),  // NX_KEYTYPE_BRIGHTNESS_DOWN
        21 => Some(0x000C_0079), // NX_KEYTYPE_ILLUMINATION_UP
        22 => Some(0x000C_007A), // NX_KEYTYPE_ILLUMINATION_DOWN
        23 => Some(0x000C_007C), // NX_KEYTYPE_ILLUMINATION_TOGGLE
        _ => None,
    }
}

fn mouse_move_message(event: &CGEvent, locally_delivered: bool) -> CapturedInputEvent {
    let position = event.location();
    let mut dx = ffi::get_double_value_field(event, EventField::MOUSE_EVENT_DELTA_X);
    let mut dy = ffi::get_double_value_field(event, EventField::MOUSE_EVENT_DELTA_Y);
    if dx == 0.0 {
        dx = event.get_integer_value_field(EventField::MOUSE_EVENT_DELTA_X) as f64;
    }
    if dy == 0.0 {
        dy = event.get_integer_value_field(EventField::MOUSE_EVENT_DELTA_Y) as f64;
    }
    CapturedInputEvent::MouseMove {
        x: position.x.round() as i32,
        y: position.y.round() as i32,
        dx,
        dy,
        locally_delivered,
    }
}

fn mouse_button_message(event_type: u32, event: &CGEvent) -> Option<CapturedInputEvent> {
    let (button, pressed) = match event_type {
        CG_EVENT_LEFT_MOUSE_DOWN => (0, true),
        CG_EVENT_LEFT_MOUSE_UP => (0, false),
        CG_EVENT_RIGHT_MOUSE_DOWN => (1, true),
        CG_EVENT_RIGHT_MOUSE_UP => (1, false),
        CG_EVENT_OTHER_MOUSE_DOWN => (
            event.get_integer_value_field(EventField::MOUSE_EVENT_BUTTON_NUMBER) as u8,
            true,
        ),
        CG_EVENT_OTHER_MOUSE_UP => (
            event.get_integer_value_field(EventField::MOUSE_EVENT_BUTTON_NUMBER) as u8,
            false,
        ),
        _ => return None,
    };
    Some(CapturedInputEvent::MouseButton { button, pressed })
}

fn scroll_message(event: &CGEvent) -> Option<CapturedInputEvent> {
    let line_y = event.get_integer_value_field(EventField::SCROLL_WHEEL_EVENT_DELTA_AXIS_1) as i32;
    let line_x = event.get_integer_value_field(EventField::SCROLL_WHEEL_EVENT_DELTA_AXIS_2) as i32;
    let point_y =
        event.get_integer_value_field(EventField::SCROLL_WHEEL_EVENT_POINT_DELTA_AXIS_1) as i32;
    let point_x =
        event.get_integer_value_field(EventField::SCROLL_WHEEL_EVENT_POINT_DELTA_AXIS_2) as i32;
    let (delta_x, delta_y) = normalize_scroll_deltas(line_x, line_y, point_x, point_y);
    if delta_x == 0 && delta_y == 0 {
        return None;
    }
    Some(CapturedInputEvent::MouseWheel { delta_x, delta_y })
}

fn key_message(event: &CGEvent, pressed: bool) -> Option<CapturedInputEvent> {
    let mac_keycode = event.get_integer_value_field(EventField::KEYBOARD_EVENT_KEYCODE) as u16;
    let keycode = captured_keycode_to_hid(mac_keycode)?;
    if pressed {
        Some(CapturedInputEvent::KeyPress { keycode })
    } else {
        Some(CapturedInputEvent::KeyRelease { keycode })
    }
}

fn captured_keycode_to_hid(keycode: u16) -> Option<u32> {
    if keycode == 0x71 {
        return Some(0x48);
    }
    mac_keycode_to_hid(keycode)
}

const MAC_KEY_CAPS_LOCK: u16 = 0x39;
const HID_KEY_CAPS_LOCK: u32 = 0x39;

const MOD_LEFT_SHIFT: u16 = 1 << 0;
const MOD_RIGHT_SHIFT: u16 = 1 << 1;
const MOD_LEFT_CTRL: u16 = 1 << 2;
const MOD_RIGHT_CTRL: u16 = 1 << 3;
const MOD_LEFT_ALT: u16 = 1 << 4;
const MOD_RIGHT_ALT: u16 = 1 << 5;
const MOD_LEFT_CMD: u16 = 1 << 6;
const MOD_RIGHT_CMD: u16 = 1 << 7;

fn reconcile_modifiers(
    event: &CGEvent,
    state: &CaptureState,
    event_tx: &mpsc::Sender<CapturedInputEvent>,
) {
    let flags = event.get_flags();
    let previous = state.modifier_state.load(Ordering::Relaxed);
    if previous == 0 {
        return;
    }

    let mut to_release = Vec::new();
    let mut cleared_mask = 0u16;

    if !flags.contains(CGEventFlags::CGEventFlagShift) {
        if previous & MOD_LEFT_SHIFT != 0 {
            to_release.push(0xE1);
            cleared_mask |= MOD_LEFT_SHIFT;
        }
        if previous & MOD_RIGHT_SHIFT != 0 {
            to_release.push(0xE5);
            cleared_mask |= MOD_RIGHT_SHIFT;
        }
    }
    if !flags.contains(CGEventFlags::CGEventFlagControl) {
        if previous & MOD_LEFT_CTRL != 0 {
            to_release.push(0xE0);
            cleared_mask |= MOD_LEFT_CTRL;
        }
        if previous & MOD_RIGHT_CTRL != 0 {
            to_release.push(0xE4);
            cleared_mask |= MOD_RIGHT_CTRL;
        }
    }
    if !flags.contains(CGEventFlags::CGEventFlagAlternate) {
        if previous & MOD_LEFT_ALT != 0 {
            to_release.push(0xE2);
            cleared_mask |= MOD_LEFT_ALT;
        }
        if previous & MOD_RIGHT_ALT != 0 {
            to_release.push(0xE6);
            cleared_mask |= MOD_RIGHT_ALT;
        }
    }
    if !flags.contains(CGEventFlags::CGEventFlagCommand) {
        if previous & MOD_LEFT_CMD != 0 {
            to_release.push(0xE3);
            cleared_mask |= MOD_LEFT_CMD;
        }
        if previous & MOD_RIGHT_CMD != 0 {
            to_release.push(0xE7);
            cleared_mask |= MOD_RIGHT_CMD;
        }
    }

    if cleared_mask != 0 {
        state.modifier_state.store(previous & !cleared_mask, Ordering::Relaxed);
        for keycode in to_release {
            capture_queue::try_send(
                event_tx,
                CapturedInputEvent::KeyRelease { keycode },
                &DROPPED_EVENTS,
                "macos",
            );
        }
    }
}

fn modifier_bit_to_flag(bit: u16) -> CGEventFlags {
    match bit {
        MOD_LEFT_SHIFT | MOD_RIGHT_SHIFT => CGEventFlags::CGEventFlagShift,
        MOD_LEFT_CTRL | MOD_RIGHT_CTRL => CGEventFlags::CGEventFlagControl,
        MOD_LEFT_ALT | MOD_RIGHT_ALT => CGEventFlags::CGEventFlagAlternate,
        MOD_LEFT_CMD | MOD_RIGHT_CMD => CGEventFlags::CGEventFlagCommand,
        _ => CGEventFlags::empty(),
    }
}

fn modifier_partner_bit(bit: u16) -> u16 {
    match bit {
        MOD_LEFT_SHIFT => MOD_RIGHT_SHIFT,
        MOD_RIGHT_SHIFT => MOD_LEFT_SHIFT,
        MOD_LEFT_CTRL => MOD_RIGHT_CTRL,
        MOD_RIGHT_CTRL => MOD_LEFT_CTRL,
        MOD_LEFT_ALT => MOD_RIGHT_ALT,
        MOD_RIGHT_ALT => MOD_LEFT_ALT,
        MOD_LEFT_CMD => MOD_RIGHT_CMD,
        MOD_RIGHT_CMD => MOD_LEFT_CMD,
        _ => 0,
    }
}

fn modifier_flags_changed_to_event(
    event: &CGEvent,
    state: &CaptureState,
) -> Option<CapturedInputEvent> {
    let keycode = event.get_integer_value_field(EventField::KEYBOARD_EVENT_KEYCODE) as u16;
    let (hid, bit) = modifier_keycode_to_hid_and_bit(keycode)?;
    let flag = modifier_bit_to_flag(bit);
    let flags = event.get_flags();
    let group_active = flags.contains(flag);

    let previous = state.modifier_state.load(Ordering::Relaxed);
    let was_pressed = (previous & bit) != 0;
    let partner_bit = modifier_partner_bit(bit);
    let partner_pressed = (previous & partner_bit) != 0;

    let is_pressed = if !group_active {
        false
    } else if !was_pressed {
        true
    } else {
        !partner_pressed
    };

    let next = if is_pressed {
        previous | bit
    } else if !group_active {
        let pair_mask = bit | partner_bit;
        previous & !pair_mask
    } else {
        previous & !bit
    };
    state.modifier_state.store(next, Ordering::Relaxed);

    if was_pressed && !is_pressed {
        Some(CapturedInputEvent::KeyRelease { keycode: hid })
    } else if !was_pressed && is_pressed {
        Some(CapturedInputEvent::KeyPress { keycode: hid })
    } else {
        None
    }
}

fn modifier_keycode_to_hid_and_bit(keycode: u16) -> Option<(u32, u16)> {
    match keycode {
        0x38 => Some((0xE1, MOD_LEFT_SHIFT)),
        0x3C => Some((0xE5, MOD_RIGHT_SHIFT)),
        0x3B => Some((0xE0, MOD_LEFT_CTRL)),
        0x3E => Some((0xE4, MOD_RIGHT_CTRL)),
        0x3A => Some((0xE2, MOD_LEFT_ALT)),
        0x3D => Some((0xE6, MOD_RIGHT_ALT)),
        0x37 => Some((0xE3, MOD_LEFT_CMD)),
        0x36 => Some((0xE7, MOD_RIGHT_CMD)),
        _ => None,
    }
}

fn mac_keycode_to_hid(keycode: u16) -> Option<u32> {
    match keycode {
        0x00 => Some(0x04), // A
        0x01 => Some(0x16), // S
        0x02 => Some(0x07), // D
        0x03 => Some(0x09), // F
        0x04 => Some(0x0B), // H
        0x05 => Some(0x0A), // G
        0x06 => Some(0x1D), // Z
        0x07 => Some(0x1B), // X
        0x08 => Some(0x06), // C
        0x09 => Some(0x19), // V
        0x0A => Some(0x64),
        0x0B => Some(0x05), // B
        0x0C => Some(0x14), // Q
        0x0D => Some(0x1A), // W
        0x0E => Some(0x08), // E
        0x0F => Some(0x15), // R
        0x10 => Some(0x1C), // Y
        0x11 => Some(0x17), // T
        0x12 => Some(0x1E), // 1
        0x13 => Some(0x1F), // 2
        0x14 => Some(0x20), // 3
        0x15 => Some(0x21), // 4
        0x16 => Some(0x23), // 6
        0x17 => Some(0x22), // 5
        0x18 => Some(0x2E), // =
        0x19 => Some(0x26), // 9
        0x1A => Some(0x24), // 7
        0x1B => Some(0x2D), // -
        0x1C => Some(0x25), // 8
        0x1D => Some(0x27), // 0
        0x1E => Some(0x30), // ]
        0x1F => Some(0x12), // O
        0x20 => Some(0x18), // U
        0x21 => Some(0x2F), // [
        0x22 => Some(0x0C), // I
        0x23 => Some(0x13), // P
        0x24 => Some(0x28), // Enter
        0x25 => Some(0x0F), // L
        0x26 => Some(0x0D), // J
        0x27 => Some(0x34), // '
        0x28 => Some(0x0E), // K
        0x29 => Some(0x33), // ;
        0x2A => Some(0x31), // Backslash
        0x2B => Some(0x36), // ,
        0x2C => Some(0x38), // /
        0x2D => Some(0x11), // N
        0x2E => Some(0x10), // M
        0x2F => Some(0x37), // .
        0x30 => Some(0x2B), // Tab
        0x31 => Some(0x2C), // Space
        0x32 => Some(0x35), // `
        0x33 => Some(0x2A), // Backspace
        0x35 => Some(0x29), // Escape
        0x36 => Some(0xE7), // Right Command
        0x37 => Some(0xE3), // Left Command
        0x38 => Some(0xE1), // Left Shift
        0x39 => Some(0x39), // Caps Lock
        0x3A => Some(0xE2), // Left Option
        0x3B => Some(0xE0), // Left Control
        0x3C => Some(0xE5), // Right Shift
        0x3D => Some(0xE6), // Right Option
        0x3E => Some(0xE4), // Right Control
        0x3F => Some(0x63), // Fn
        0x40 => Some(0x6C), // F17
        0x41 => Some(0x63), // Keypad .
        0x43 => Some(0x55), // Keypad *
        0x45 => Some(0x57), // Keypad +
        0x47 => Some(0x53), // Num Lock/Clear
        0x48 => Some(0x80), // Volume Up
        0x49 => Some(0x81), // Volume Down
        0x4A => Some(0x7F), // Mute
        0x4B => Some(0x54), // Keypad /
        0x4C => Some(0x58), // Keypad Enter
        0x4E => Some(0x56), // Keypad -
        0x4F => Some(0x6D), // F18
        0x50 => Some(0x6E), // F19
        0x51 => Some(0x67), // Keypad =
        0x52 => Some(0x62), // Keypad 0
        0x53 => Some(0x59), // Keypad 1
        0x54 => Some(0x5A), // Keypad 2
        0x55 => Some(0x5B), // Keypad 3
        0x56 => Some(0x5C), // Keypad 4
        0x57 => Some(0x5D), // Keypad 5
        0x58 => Some(0x5E), // Keypad 6
        0x59 => Some(0x5F), // Keypad 7
        0x5A => Some(0x6F), // F20
        0x5B => Some(0x60), // Keypad 8
        0x5C => Some(0x61), // Keypad 9
        0x5D => Some(0x89),
        0x5E => Some(0x87),
        0x5F => Some(0x85), // Keypad ,
        0x60 => Some(0x3E), // F5
        0x61 => Some(0x3F), // F6
        0x62 => Some(0x40), // F7
        0x63 => Some(0x3C), // F3
        0x64 => Some(0x41), // F8
        0x65 => Some(0x42), // F9
        0x66 => Some(0x8B),
        0x67 => Some(0x44), // F11
        0x68 => Some(0x88), // JIS Kana
        0x69 => Some(0x68), // F13
        0x6A => Some(0x6B), // F16
        0x6B => Some(0x69), // F14
        0x6D => Some(0x43), // F10
        0x6E => Some(0x65), // Contextual Menu
        0x6F => Some(0x45), // F12
        0x71 => Some(0x6A), // F15
        0x72 => Some(0x49), // Insert
        0x73 => Some(0x4A), // Home
        0x74 => Some(0x4B), // Page Up
        0x75 => Some(0x4C), // Delete Forward
        0x76 => Some(0x3D), // F4
        0x77 => Some(0x4D), // End
        0x78 => Some(0x3B), // F2
        0x79 => Some(0x4E), // Page Down
        0x7A => Some(0x3A), // F1
        0x7B => Some(0x50), // Left Arrow
        0x7C => Some(0x4F), // Right Arrow
        0x7D => Some(0x51), // Down Arrow
        0x7E => Some(0x52), // Up Arrow
        _ => None,
    }
}

fn make_source_marker() -> u64 {
    let now_nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    let pid = process::id() as u64;
    now_nanos ^ (pid << 32)
}

/// Returns the process-local source marker for self-injection suppression.
///
/// The marker is generated once per process by combining the current timestamp
/// with the process ID. It's stored in `EVENT_SOURCE_USER_DATA` on injected
/// events and checked during capture to filter out our own injections.
pub(super) fn local_source_marker() -> u64 {
    static LOCAL_SOURCE_MARKER: OnceLock<u64> = OnceLock::new();
    *LOCAL_SOURCE_MARKER.get_or_init(make_source_marker)
}

// FFI binding to re-enable a disabled event tap.
//
// # Safety
//
// - `tap` must be a valid `CFMachPortRef` from an installed event tap
// - This function is thread-safe with respect to the event tap state
#[link(name = "ApplicationServices", kind = "framework")]
unsafe extern "C" {
    fn CGEventTapCreate(
        tap: u32,
        place: u32,
        options: u32,
        events_of_interest: u64,
        callback: unsafe extern "C" fn(
            proxy: core_graphics::event::CGEventTapProxy,
            etype: u32,
            event: *mut core_graphics::sys::CGEvent,
            user_info: *mut std::ffi::c_void,
        ) -> *mut core_graphics::sys::CGEvent,
        user_info: *mut std::ffi::c_void,
    ) -> CFMachPortRef;
    fn CGEventTapEnable(tap: CFMachPortRef, enable: bool);
}

pub(super) const CG_EVENT_LEFT_MOUSE_DOWN: u32 = 1;
pub(super) const CG_EVENT_LEFT_MOUSE_UP: u32 = 2;
pub(super) const CG_EVENT_RIGHT_MOUSE_DOWN: u32 = 3;
pub(super) const CG_EVENT_RIGHT_MOUSE_UP: u32 = 4;
pub(super) const CG_EVENT_MOUSE_MOVED: u32 = 5;
pub(super) const CG_EVENT_LEFT_MOUSE_DRAGGED: u32 = 6;
pub(super) const CG_EVENT_RIGHT_MOUSE_DRAGGED: u32 = 7;
pub(super) const CG_EVENT_KEY_DOWN: u32 = 10;
pub(super) const CG_EVENT_KEY_UP: u32 = 11;
pub(super) const CG_EVENT_FLAGS_CHANGED: u32 = 12;
pub(super) const NX_SYSDEFINED: u32 = 14;
pub(super) const CG_EVENT_SCROLL_WHEEL: u32 = 22;
pub(super) const CG_EVENT_OTHER_MOUSE_DOWN: u32 = 25;
pub(super) const CG_EVENT_OTHER_MOUSE_UP: u32 = 26;
pub(super) const CG_EVENT_OTHER_MOUSE_DRAGGED: u32 = 27;
pub(super) const CG_EVENT_TAP_DISABLED_BY_TIMEOUT: u32 = 0xFFFFFFFE;
pub(super) const CG_EVENT_TAP_DISABLED_BY_USER_INPUT: u32 = 0xFFFFFFFF;

pub(super) fn capture_event_mask() -> u64 {
    let mut mask = 0u64;
    for etype in [
        CG_EVENT_LEFT_MOUSE_DOWN,
        CG_EVENT_LEFT_MOUSE_UP,
        CG_EVENT_RIGHT_MOUSE_DOWN,
        CG_EVENT_RIGHT_MOUSE_UP,
        CG_EVENT_MOUSE_MOVED,
        CG_EVENT_LEFT_MOUSE_DRAGGED,
        CG_EVENT_RIGHT_MOUSE_DRAGGED,
        CG_EVENT_KEY_DOWN,
        CG_EVENT_KEY_UP,
        CG_EVENT_FLAGS_CHANGED,
        NX_SYSDEFINED,
        CG_EVENT_SCROLL_WHEEL,
        CG_EVENT_OTHER_MOUSE_DOWN,
        CG_EVENT_OTHER_MOUSE_UP,
        CG_EVENT_OTHER_MOUSE_DRAGGED,
    ] {
        mask |= 1 << etype;
    }
    mask
}

pub(crate) const WHEEL_DELTA_UNIT: i32 = 120;
pub(crate) const POINTS_PER_WHEEL_NOTCH: i32 = 10;
pub(crate) const UNITS_PER_POINT: i32 = WHEEL_DELTA_UNIT / POINTS_PER_WHEEL_NOTCH;

pub(crate) fn normalize_scroll_deltas(
    line_x: i32,
    line_y: i32,
    point_x: i32,
    point_y: i32,
) -> (i32, i32) {
    let delta_y = if point_y != 0 {
        point_y * UNITS_PER_POINT
    } else {
        line_y * WHEEL_DELTA_UNIT
    };
    let delta_x = if point_x != 0 {
        point_x * UNITS_PER_POINT
    } else {
        line_x * WHEEL_DELTA_UNIT
    };
    (delta_x, delta_y)
}

#[cfg(test)]
mod tests {
    use super::{
        captured_keycode_to_hid, mac_keycode_to_hid, modifier_keycode_to_hid_and_bit,
        normalize_scroll_deltas, should_deliver_locally,
    };
    use crate::platform::CapturePolicy;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU8, Ordering};

    #[test]
    fn normalize_scroll_deltas_scales_line_and_points() {
        assert_eq!(normalize_scroll_deltas(0, 1, 0, 10), (0, 120));
        assert_eq!(normalize_scroll_deltas(0, -1, 0, -10), (0, -120));
        assert_eq!(normalize_scroll_deltas(1, 0, 10, 0), (120, 0));
        assert_eq!(normalize_scroll_deltas(0, 1, 0, 0), (0, 120));
        assert_eq!(normalize_scroll_deltas(0, 0, 0, 2), (0, 24));
        assert_eq!(normalize_scroll_deltas(0, 0, -3, 0), (-36, 0));
        assert_eq!(normalize_scroll_deltas(0, 0, 0, 0), (0, 0));
    }

    #[test]
    fn mac_keycode_to_hid_maps_common_keys() {
        assert_eq!(mac_keycode_to_hid(0x00), Some(0x04));
        assert_eq!(mac_keycode_to_hid(0x24), Some(0x28));
        assert_eq!(mac_keycode_to_hid(0x31), Some(0x2C));
        assert_eq!(mac_keycode_to_hid(0x7B), Some(0x50));
        assert_eq!(mac_keycode_to_hid(0x7A), Some(0x3A)); // F1
        assert_eq!(mac_keycode_to_hid(0x67), Some(0x44)); // F11
        assert_eq!(mac_keycode_to_hid(0x68), Some(0x88)); // JIS Kana
        assert_eq!(mac_keycode_to_hid(0x69), Some(0x68)); // F13
        assert_eq!(mac_keycode_to_hid(0x6A), Some(0x6B)); // F16
        assert_eq!(mac_keycode_to_hid(0x6B), Some(0x69)); // F14
        assert_eq!(mac_keycode_to_hid(0x6E), Some(0x65)); // Contextual Menu
        assert_eq!(mac_keycode_to_hid(0x71), Some(0x6A)); // F15
        assert_eq!(mac_keycode_to_hid(0x48), Some(0x80)); // Volume Up
        assert_eq!(mac_keycode_to_hid(0x49), Some(0x81)); // Volume Down
        assert_eq!(mac_keycode_to_hid(0x4A), Some(0x7F)); // Mute
        assert_eq!(mac_keycode_to_hid(0x5A), Some(0x6F)); // F20
        assert_eq!(mac_keycode_to_hid(0x5F), Some(0x85)); // Keypad ,
        assert_eq!(mac_keycode_to_hid(0x0A), Some(0x64));
        assert_eq!(mac_keycode_to_hid(0x5D), Some(0x89));
        assert_eq!(mac_keycode_to_hid(0x5E), Some(0x87));
        assert_eq!(mac_keycode_to_hid(0x66), Some(0x8B));
    }

    #[test]
    fn mac_keycode_to_hid_returns_none_for_unmapped_key() {
        assert_eq!(mac_keycode_to_hid(0xFF), None);
    }

    #[test]
    fn captured_keycode_maps_pause_to_hid_pause() {
        assert_eq!(captured_keycode_to_hid(0x71), Some(0x48));
    }

    #[test]
    fn modifier_keycode_mapping_is_stable() {
        assert_eq!(modifier_keycode_to_hid_and_bit(0x3B), Some((0xE0, 1 << 2)));
        assert_eq!(modifier_keycode_to_hid_and_bit(0x38), Some((0xE1, 1 << 0)));
        assert_eq!(modifier_keycode_to_hid_and_bit(0x36), Some((0xE7, 1 << 7)));
        assert_eq!(modifier_keycode_to_hid_and_bit(0x10), None);
    }

    #[test]
    fn should_deliver_locally_blocks_only_sending_mode() {
        let capture_policy = Arc::new(AtomicU8::new(CapturePolicy::DeliverLocally as u8));
        assert!(should_deliver_locally(&capture_policy));

        capture_policy.store(
            CapturePolicy::SuppressLocalDelivery as u8,
            Ordering::Relaxed,
        );
        assert!(!should_deliver_locally(&capture_policy));

        capture_policy.store(CapturePolicy::DeliverLocally as u8, Ordering::Relaxed);
        assert!(should_deliver_locally(&capture_policy));
    }

    #[test]
    fn decode_capture_policy_handles_unknown_values() {
        assert_eq!(CapturePolicy::from_u8(0), CapturePolicy::DeliverLocally);
        assert_eq!(
            CapturePolicy::from_u8(1),
            CapturePolicy::SuppressLocalDelivery
        );
        assert_eq!(CapturePolicy::from_u8(2), CapturePolicy::DeliverLocally);
        assert_eq!(CapturePolicy::from_u8(255), CapturePolicy::DeliverLocally);
    }

    #[test]
    fn modifier_partner_bit_matches_pairs() {
        assert_eq!(super::modifier_partner_bit(1 << 0), 1 << 1);
        assert_eq!(super::modifier_partner_bit(1 << 1), 1 << 0);
        assert_eq!(super::modifier_partner_bit(1 << 2), 1 << 3);
        assert_eq!(super::modifier_partner_bit(1 << 3), 1 << 2);
        assert_eq!(super::modifier_partner_bit(1 << 4), 1 << 5);
        assert_eq!(super::modifier_partner_bit(1 << 5), 1 << 4);
        assert_eq!(super::modifier_partner_bit(1 << 6), 1 << 7);
        assert_eq!(super::modifier_partner_bit(1 << 7), 1 << 6);
    }

    #[test]
    fn modifier_bit_to_flag_maps_correctly() {
        use core_graphics::event::CGEventFlags;
        assert_eq!(super::modifier_bit_to_flag(1 << 0), CGEventFlags::CGEventFlagShift);
        assert_eq!(super::modifier_bit_to_flag(1 << 1), CGEventFlags::CGEventFlagShift);
        assert_eq!(super::modifier_bit_to_flag(1 << 2), CGEventFlags::CGEventFlagControl);
        assert_eq!(super::modifier_bit_to_flag(1 << 3), CGEventFlags::CGEventFlagControl);
        assert_eq!(super::modifier_bit_to_flag(1 << 4), CGEventFlags::CGEventFlagAlternate);
        assert_eq!(super::modifier_bit_to_flag(1 << 5), CGEventFlags::CGEventFlagAlternate);
        assert_eq!(super::modifier_bit_to_flag(1 << 6), CGEventFlags::CGEventFlagCommand);
        assert_eq!(super::modifier_bit_to_flag(1 << 7), CGEventFlags::CGEventFlagCommand);
    }

    #[test]
    fn mac_keycode_to_hid_maps_caps_lock() {
        assert_eq!(
            super::mac_keycode_to_hid(super::MAC_KEY_CAPS_LOCK),
            Some(super::HID_KEY_CAPS_LOCK)
        );
    }

    #[test]
    fn system_media_key_to_hid_maps_volume_and_media_keys() {
        assert_eq!(super::system_media_key_to_hid(0), Some(0x80)); // Volume Up
        assert_eq!(super::system_media_key_to_hid(1), Some(0x81)); // Volume Down
        assert_eq!(super::system_media_key_to_hid(6), Some(0x82)); // Power
        assert_eq!(super::system_media_key_to_hid(7), Some(0x7F)); // Mute
        assert_eq!(super::system_media_key_to_hid(16), Some(0xCD)); // Play/Pause
        assert_eq!(super::system_media_key_to_hid(17), Some(0xB5)); // Next Track
        assert_eq!(super::system_media_key_to_hid(18), Some(0xB6)); // Previous Track
        assert_eq!(super::system_media_key_to_hid(14), Some(0xB8)); // Eject
        assert_eq!(super::system_media_key_to_hid(2), Some(0x000C_006F)); // Brightness Up
        assert_eq!(super::system_media_key_to_hid(3), Some(0x000C_0070)); // Brightness Down
        assert_eq!(super::system_media_key_to_hid(999), None);
    }

    #[test]
    fn capture_event_mask_includes_media_and_standard_events() {
        let mask = super::capture_event_mask();
        assert_ne!(mask & (1 << super::NX_SYSDEFINED), 0);
        assert_ne!(mask & (1 << super::CG_EVENT_KEY_DOWN), 0);
        assert_ne!(mask & (1 << super::CG_EVENT_KEY_UP), 0);
        assert_ne!(mask & (1 << super::CG_EVENT_FLAGS_CHANGED), 0);
        assert_ne!(mask & (1 << super::CG_EVENT_MOUSE_MOVED), 0);
        assert_ne!(mask & (1 << super::CG_EVENT_LEFT_MOUSE_DOWN), 0);
        assert_ne!(mask & (1 << super::CG_EVENT_SCROLL_WHEEL), 0);
    }
}
