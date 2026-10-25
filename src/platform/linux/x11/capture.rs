//! X11 capture implementation using XInput2 raw motion and polling for legacy events.

use anyhow::{Result, anyhow};
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;
use tokio::sync::mpsc;

use crate::platform::{CapturePolicy, CapturedInputEvent, EventCapture};
use x11rb::connection::Connection;
use x11rb::protocol::Event;
use x11rb::protocol::xinput::ConnectionExt as XinputConnectionExt;
use x11rb::protocol::xinput::{Device, EventMask, RawMotionEvent, XIEventMask};
use x11rb::protocol::xproto::ConnectionExt as XprotoConnectionExt;

use super::marker;
use crate::platform::shared::capture_queue;

const POLL_INTERVAL: Duration = Duration::from_millis(4);

const BUTTON1_MASK: u16 = 1 << 8;
const BUTTON2_MASK: u16 = 1 << 9;
const BUTTON3_MASK: u16 = 1 << 10;
const KEYMAP_BYTES: usize = 32;

pub(crate) struct X11EventCapture {
    state: Arc<CaptureState>,
}

struct CaptureState {
    installed: AtomicBool,
    shutdown: AtomicBool,
    event_rx: Mutex<Option<mpsc::Receiver<CapturedInputEvent>>>,
    worker: Mutex<Option<JoinHandle<()>>>,
    capture_policy: Arc<AtomicU8>,
}

impl X11EventCapture {
    pub(crate) fn new() -> Self {
        Self::with_capture_policy(Arc::new(AtomicU8::new(CapturePolicy::DeliverLocally as u8)))
    }

    pub(crate) fn with_capture_policy(capture_policy: Arc<AtomicU8>) -> Self {
        Self {
            state: Arc::new(CaptureState {
                installed: AtomicBool::new(false),
                shutdown: AtomicBool::new(false),
                event_rx: Mutex::new(None),
                worker: Mutex::new(None),
                capture_policy,
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
}

impl EventCapture for X11EventCapture {
    fn install_tap(&self) -> Result<()> {
        if self.state.installed.swap(true, Ordering::SeqCst) {
            return Ok(());
        }

        self.state.shutdown.store(false, Ordering::SeqCst);
        let (event_tx, event_rx) = mpsc::channel(capture_queue::EVENT_QUEUE_CAPACITY);
        let state = Arc::clone(&self.state);

        let worker = thread::Builder::new()
            .name("linux-x11-capture".to_string())
            .spawn(move || run_capture_loop(state, event_tx))?;

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

        self.state.shutdown.store(true, Ordering::SeqCst);

        if let Some(worker) = self
            .state
            .worker
            .lock()
            .map_err(|_| anyhow!("capture worker lock poisoned"))?
            .take()
        {
            worker
                .join()
                .map_err(|_| anyhow!("capture worker thread panicked"))?;
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

/// Whether a captured event was already delivered to the local cursor.
///
/// The cursor runtime sets the policy to `SuppressLocalDelivery` while a peer
/// owns the cursor, so the runtime can forward the event instead of dropping
/// it on the floor.
fn local_delivery(capture_policy: &Arc<AtomicU8>) -> bool {
    !CapturePolicy::from_u8(capture_policy.load(Ordering::Acquire)).suppresses_local_delivery()
}

fn run_capture_loop(state: Arc<CaptureState>, event_tx: mpsc::Sender<CapturedInputEvent>) {
    let Ok((conn, screen_num)) = x11rb::connect(None) else {
        state.installed.store(false, Ordering::SeqCst);
        return;
    };

    let Some(screen) = conn.setup().roots.get(screen_num) else {
        state.installed.store(false, Ordering::SeqCst);
        return;
    };
    let root = screen.root;

    let mut last_mask = 0u16;
    let mut last_keymap = [0u8; KEYMAP_BYTES];
    let mut modifiers = ModifierState::default();

    if let Err(error) = install_raw_motion(&conn, root) {
        tracing::warn!(error = %error, "XInput2 raw motion capture unavailable");
        state.installed.store(false, Ordering::SeqCst);
        return;
    }
    tracing::info!("X11 mouse capture using XInput2 raw motion");

    while !state.shutdown.load(Ordering::SeqCst) {
        while let Ok(Some(event)) = conn.poll_for_event() {
            if let Event::XinputRawMotion(raw) = event
                && let Some((dx, dy)) = raw_motion_delta(&raw)
                && let Ok(cookie) = conn.query_pointer(root)
                && let Ok(reply) = cookie.reply()
            {
                let x = i32::from(reply.root_x);
                let y = i32::from(reply.root_y);
                if !marker::should_suppress_move(x, y) {
                    try_send(
                        &event_tx,
                        CapturedInputEvent::MouseMove {
                            x,
                            y,
                            dx,
                            dy,
                            locally_delivered: local_delivery(&state.capture_policy),
                        },
                    );
                }
            }
        }

        let cookie = match conn.query_pointer(root) {
            Ok(cookie) => cookie,
            Err(_) => {
                thread::sleep(POLL_INTERVAL);
                continue;
            }
        };
        let reply = match cookie.reply() {
            Ok(reply) => reply,
            Err(_) => {
                thread::sleep(POLL_INTERVAL);
                continue;
            }
        };

        let _x = i32::from(reply.root_x);
        let _y = i32::from(reply.root_y);
        let mask = u16::from(reply.mask);

        emit_button_transition(
            &event_tx,
            last_mask,
            mask,
            BUTTON1_MASK,
            0,
            &marker::should_suppress_button,
        );

        if let Ok(cookie) = conn.query_keymap() {
            if let Ok(reply) = cookie.reply() {
                let keymap = reply.keys;
                emit_keyboard_transitions(&event_tx, &last_keymap, &keymap, &mut modifiers);
                last_keymap = keymap;
            }
        }
        emit_button_transition(
            &event_tx,
            last_mask,
            mask,
            BUTTON3_MASK,
            1,
            &marker::should_suppress_button,
        );
        emit_button_transition(
            &event_tx,
            last_mask,
            mask,
            BUTTON2_MASK,
            2,
            &marker::should_suppress_button,
        );

        last_mask = mask;
        thread::sleep(POLL_INTERVAL);
    }

    state.installed.store(false, Ordering::SeqCst);
}

fn install_raw_motion<C: Connection>(conn: &C, root: u32) -> Result<()> {
    let version = conn.xinput_xi_query_version(2, 0)?.reply()?;
    if version.major_version < 2 {
        return Err(anyhow!(
            "XInput2 version {} is not supported",
            version.major_version
        ));
    }

    let masks = [EventMask {
        deviceid: u16::from(Device::ALL_MASTER),
        mask: vec![XIEventMask::RAW_MOTION],
    }];
    conn.xinput_xi_select_events(root, &masks)?.check()?;
    conn.flush()?;
    Ok(())
}

fn raw_motion_delta(event: &RawMotionEvent) -> Option<(f64, f64)> {
    let dx = raw_axis_value(event, 0).unwrap_or(0.0);
    let dy = raw_axis_value(event, 1).unwrap_or(0.0);
    if dx == 0.0 && dy == 0.0 {
        None
    } else {
        Some((dx, dy))
    }
}

fn raw_axis_value(event: &RawMotionEvent, axis: usize) -> Option<f64> {
    let mut value_index = 0;
    for (word_index, mask) in event.valuator_mask.iter().enumerate() {
        for bit in 0..32 {
            if mask & (1 << bit) == 0 {
                continue;
            }
            if word_index * 32 + bit == axis {
                return event
                    .axisvalues_raw
                    .get(value_index)
                    .map(|value| f64::from(value.integral) + f64::from(value.frac) / 4_294_967_296.0);
            }
            value_index += 1;
        }
    }
    None
}

fn emit_keyboard_transitions(
    event_tx: &mpsc::Sender<CapturedInputEvent>,
    old_keymap: &[u8; KEYMAP_BYTES],
    new_keymap: &[u8; KEYMAP_BYTES],
    modifiers: &mut ModifierState,
) {
    for (byte_idx, (&old_byte, &new_byte)) in old_keymap.iter().zip(new_keymap.iter()).enumerate() {
        let changed = old_byte ^ new_byte;
        if changed == 0 {
            continue;
        }

        for bit in 0..8 {
            let bit_mask = 1u8 << bit;
            if changed & bit_mask == 0 {
                continue;
            }

            let keycode = (byte_idx * 8 + bit) as u8;
            if keycode < 8 {
                continue;
            }

            let pressed = (new_byte & bit_mask) != 0;
            let Some(hid_keycode) = x11_keycode_to_hid(keycode) else {
                continue;
            };
            if marker::should_suppress_key(hid_keycode, pressed) {
                continue;
            }

            let event = if pressed {
                CapturedInputEvent::KeyPress {
                    keycode: hid_keycode,
                }
            } else {
                CapturedInputEvent::KeyRelease {
                    keycode: hid_keycode,
                }
            };
            is_emergency_key_event(hid_keycode, pressed, modifiers);
            try_send(event_tx, event);
        }
    }
}

#[derive(Default)]
struct ModifierState {
    ctrl_down: bool,
    alt_down: bool,
    shift_down: bool,
}

fn is_emergency_key_event(keycode: u32, pressed: bool, modifiers: &mut ModifierState) -> bool {
    match keycode {
        0xE0 | 0xE4 => modifiers.ctrl_down = pressed,
        0xE1 | 0xE5 => modifiers.shift_down = pressed,
        0xE2 | 0xE6 => modifiers.alt_down = pressed,
        0x29 => {
            return pressed && modifiers.ctrl_down && modifiers.alt_down && modifiers.shift_down;
        }
        0x48 => return pressed && modifiers.ctrl_down && modifiers.alt_down,
        0x2A => {
            return pressed && modifiers.ctrl_down && modifiers.alt_down && modifiers.shift_down;
        }
        _ => {}
    }
    false
}

fn x11_keycode_to_hid(keycode: u8) -> Option<u32> {
    let evdev = super::super::keycodes::x11_to_evdev(keycode as u32);
    super::super::keycodes::evdev_to_hid(evdev)
}

fn emit_button_transition(
    event_tx: &mpsc::Sender<CapturedInputEvent>,
    old_mask: u16,
    new_mask: u16,
    bit: u16,
    logical_button: u8,
    suppress: &dyn Fn(u8, bool) -> bool,
) {
    let old_pressed = old_mask & bit != 0;
    let new_pressed = new_mask & bit != 0;
    if old_pressed == new_pressed {
        return;
    }
    if suppress(logical_button, new_pressed) {
        return;
    }

    try_send(
        event_tx,
        CapturedInputEvent::MouseButton {
            button: logical_button,
            pressed: new_pressed,
        },
    );
}

fn try_send(event_tx: &mpsc::Sender<CapturedInputEvent>, event: CapturedInputEvent) {
    capture_queue::try_send(event_tx, event, &DROPPED_EVENTS, "x11");
}

static DROPPED_EVENTS: AtomicU64 = AtomicU64::new(0);

#[cfg(test)]
mod live_tests {
    use super::X11EventCapture;
    use crate::platform::linux::live_test;
    use crate::platform::{CapturedInputEvent, EventCapture};
    use serial_test::serial;
    use std::time::{Duration, Instant};
    use tokio::sync::mpsc;
    use x11rb::connection::Connection;
    use x11rb::protocol::xproto::{
        BUTTON_PRESS_EVENT, BUTTON_RELEASE_EVENT, KEY_PRESS_EVENT, KEY_RELEASE_EVENT,
    };
    use x11rb::protocol::xtest::ConnectionExt as _;

    const CAPTURE_SETTLE: Duration = Duration::from_millis(150);
    const OBSERVE_TIMEOUT: Duration = Duration::from_secs(2);
    const OBSERVE_POLL: Duration = Duration::from_millis(5);

    /// X11 keycode for HID usage 0x04 ("a"): evdev 30 plus the X11 offset of 8.
    const X11_KEYCODE_A: u8 = 38;
    const HID_KEYCODE_A: u32 = 0x04;

    fn x11_or_skip(test: &str) -> bool {
        live_test::require_live(test, live_test::x11_display_available(), "no live X display")
    }

    /// Injects an event straight through XTest, bypassing the local-injection
    /// markers so the capture loop treats it as external input.
    fn inject_externally(event_type: u8, detail: u8) {
        let (conn, screen_num) = x11rb::connect(None).expect("X11 display should be reachable");
        let root = conn.setup().roots[screen_num].root;
        conn.xtest_fake_input(event_type, detail, 0, root, 0, 0, 0)
            .expect("XTest fake input should be accepted");
        conn.flush().expect("XTest fake input should flush");
    }

    /// Drains captured events until one satisfies `matches` or the timeout hits.
    fn wait_for_capture(
        rx: &mut mpsc::Receiver<CapturedInputEvent>,
        matches: impl Fn(&CapturedInputEvent) -> bool,
    ) -> Option<CapturedInputEvent> {
        let deadline = Instant::now() + OBSERVE_TIMEOUT;
        loop {
            match rx.try_recv() {
                Ok(event) => {
                    if matches(&event) {
                        return Some(event);
                    }
                }
                Err(mpsc::error::TryRecvError::Disconnected) => return None,
                Err(mpsc::error::TryRecvError::Empty) => {
                    if Instant::now() >= deadline {
                        return None;
                    }
                    std::thread::sleep(OBSERVE_POLL);
                }
            }
        }
    }

    fn install_with_receiver(
        capture: &X11EventCapture,
    ) -> mpsc::Receiver<CapturedInputEvent> {
        capture.install_tap().expect("install should succeed");
        let rx = capture
            .take_event_receiver()
            .expect("receiver should be available after install");
        std::thread::sleep(CAPTURE_SETTLE);
        rx
    }

    #[test]
    #[serial]
    fn install_uninstall_lifecycle() {
        if !x11_or_skip("install_uninstall_lifecycle") {
            return;
        }
        let capture = X11EventCapture::new();
        capture.install_tap().expect("install should succeed");
        assert!(capture.take_event_receiver().is_some());
        capture.install_tap().expect("double install should succeed");
        capture.uninstall_tap().expect("uninstall should succeed");
        capture.uninstall_tap().expect("double uninstall should succeed");
        capture.install_tap().expect("reinstall should succeed");
        capture.uninstall_tap().expect("final uninstall should succeed");
    }

    #[test]
    #[serial]
    fn captures_button_press_from_another_source() {
        if !x11_or_skip("captures_button_press_from_another_source") {
            return;
        }

        let capture = X11EventCapture::new();
        let mut rx = install_with_receiver(&capture);

        inject_externally(BUTTON_PRESS_EVENT, 1);
        inject_externally(BUTTON_RELEASE_EVENT, 1);
        let observed = wait_for_capture(&mut rx, |event| {
            matches!(
                event,
                CapturedInputEvent::MouseButton {
                    button: 0,
                    pressed: true
                }
            )
        });
        capture.uninstall_tap().expect("uninstall should succeed");

        assert_eq!(
            observed,
            Some(CapturedInputEvent::MouseButton {
                button: 0,
                pressed: true
            }),
            "capture should report an externally injected left-button press"
        );
    }

    #[test]
    #[serial]
    fn captures_key_press_from_another_source() {
        if !x11_or_skip("captures_key_press_from_another_source") {
            return;
        }

        let capture = X11EventCapture::new();
        let mut rx = install_with_receiver(&capture);

        inject_externally(KEY_PRESS_EVENT, X11_KEYCODE_A);
        inject_externally(KEY_RELEASE_EVENT, X11_KEYCODE_A);
        let observed = wait_for_capture(&mut rx, |event| {
            matches!(
                event,
                CapturedInputEvent::KeyPress { keycode } if *keycode == HID_KEYCODE_A
            )
        });
        capture.uninstall_tap().expect("uninstall should succeed");

        assert_eq!(
            observed,
            Some(CapturedInputEvent::KeyPress {
                keycode: HID_KEYCODE_A
            }),
            "capture should translate an externally injected X11 keycode to HID"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::super::super::keycodes::evdev_to_hid;
    use super::{raw_axis_value, raw_motion_delta, x11_keycode_to_hid};
    use x11rb::protocol::xinput::{Fp3232, RawMotionEvent};

    #[test]
    fn x11_keycode_to_hid_maps_common_keys() {
        assert_eq!(x11_keycode_to_hid(38), Some(0x04));
        assert_eq!(x11_keycode_to_hid(36), Some(0x28));
        assert_eq!(x11_keycode_to_hid(65), Some(0x2C));
        assert_eq!(x11_keycode_to_hid(113), Some(0x50));
    }

    #[test]
    fn evdev_to_hid_maps_modifier_keys() {
        assert_eq!(evdev_to_hid(29), Some(0xE0));
        assert_eq!(evdev_to_hid(42), Some(0xE1));
        assert_eq!(evdev_to_hid(125), Some(0xE3));
    }

    #[test]
    fn evdev_to_hid_returns_none_for_unmapped_key() {
        assert_eq!(evdev_to_hid(255), None);
    }

    #[test]
    fn raw_motion_decodes_relative_axes() {
        let event = RawMotionEvent {
            valuator_mask: vec![0b11],
            axisvalues_raw: vec![
                Fp3232 {
                    integral: 6,
                    frac: 0,
                },
                Fp3232 {
                    integral: -3,
                    frac: 0,
                },
            ],
            ..Default::default()
        };
        assert_eq!(raw_motion_delta(&event), Some((6.0, -3.0)));
        assert_eq!(raw_axis_value(&event, 2), None);

        let horizontal = RawMotionEvent {
            valuator_mask: vec![1],
            axisvalues_raw: vec![Fp3232 {
                integral: 4,
                frac: 0,
            }],
            ..Default::default()
        };
        assert_eq!(raw_motion_delta(&horizontal), Some((4.0, 0.0)));
    }
}
