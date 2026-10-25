//! Native libei injection implementation using reis and ashpd RemoteDesktop portal.
//!
//! This module implements mouse and keyboard injection via the libei protocol.
//! The EIS file descriptor is obtained from the xdg-desktop-portal RemoteDesktop
//! interface (Sender/emulation mode), then used to create a reis `ei::Context`
//! that synthesises input events into the compositor via an async tokio task.
//!
//! Commands are dispatched from the synchronous `EventInjection` trait methods
//! through a bounded `mpsc::SyncSender<InjectionCommand>` channel into the
//! async injection loop running on the Wayland tokio runtime.

use anyhow::{Context, Result};
use flume::Sender;
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use super::display::WaylandDisplayManager;
use super::portal::{self, PortalSession};
use super::runtime::wayland_runtime_handle;
use crate::platform::{DisplayManager, EventInjection};
use crate::platform::linux::keycodes;

/// Commands sent from the synchronous injection API into the async injection loop.
#[derive(Debug)]
pub(crate) enum InjectionCommand {
    /// Inject a relative mouse motion to the given absolute logical coordinates.
    /// The injection loop tracks the last position and sends the delta.
    MouseMove { x: i32, y: i32 },
    /// Inject a relative mouse motion delta.
    MouseMoveRelative { dx: i32, dy: i32 },
    /// Inject a mouse button press or release (wire button index 0–4).
    MouseButton { button: u8, pressed: bool },
    /// Inject a mouse wheel step (delta_x = horizontal, delta_y = vertical where positive = up).
    MouseWheel { delta_x: i32, delta_y: i32 },
    /// Inject a keyboard key press (HID usage code).
    KeyPress { keycode: u32 },
    /// Inject a keyboard key release (HID usage code).
    KeyRelease { keycode: u32 },
    /// Warp the cursor to the given absolute coordinates.
    WarpCursor { x: i32, y: i32 },
    /// Release all currently tracked pressed keys.
    ReleaseAllKeys,
    /// Shut down the injection loop cleanly.
    Shutdown,
}

/// Native libei-based input injection.
///
/// Sends injection commands through a bounded channel into an async task that
/// maintains a libei Sender context against the compositor's EIS socket.
pub(crate) struct LibeiInjection {
    /// Command sender to the async injection loop.
    cmd_tx: Sender<InjectionCommand>,
    /// Set of HID keycodes currently reported as pressed (for release_all_keys).
    pressed_keys: Mutex<HashSet<u32>>,
    /// Set of mouse button indices currently reported as pressed (for release_all_keys).
    pressed_buttons: Mutex<HashSet<u8>>,
    cursor_hidden: AtomicBool,
    /// Portal session providing the RemoteDesktop EIS fd.
    portal: Arc<PortalSession>,
}

impl LibeiInjection {
    /// Create a new LibeiInjection instance and start the injection loop.
    ///
    /// Obtains (or reuses) the global portal session, ensures a RemoteDesktop
    /// session is active, takes its EIS fd, and spawns the async injection loop
    /// on the Wayland tokio runtime.
    pub(crate) fn new() -> Result<Self> {
        let portal = portal::get_or_create_portal_session()?;

        // Ensure there is an active RemoteDesktop session.
        if !portal.rd_active() {
            portal
                .create_rd_session()
                .context("failed to create RemoteDesktop session for libei injection")?;
        }

        let eis_fd = portal
            .take_rd_eis_fd()
            .context("no EIS fd available from RemoteDesktop session")?;

        let (cmd_tx, cmd_rx) = flume::bounded(256);

        // Spawn the injection loop on the Wayland async runtime.
        wayland_runtime_handle().spawn(injection_loop(eis_fd, cmd_rx));

        Ok(Self {
            cmd_tx,
            pressed_keys: Mutex::new(HashSet::new()),
            pressed_buttons: Mutex::new(HashSet::new()),
            cursor_hidden: AtomicBool::new(false),
            portal,
        })
    }

    /// Send a command to the injection loop, logging on failure.
    fn send(&self, cmd: InjectionCommand) -> Result<()> {
        self.cmd_tx
            .try_send(cmd)
            .map_err(|e| anyhow::anyhow!("libei injection channel full or disconnected: {e}"))
    }
}

impl Drop for LibeiInjection {
    fn drop(&mut self) {
        let _ = self.release_all_keys();
        let _ = self.cmd_tx.try_send(InjectionCommand::Shutdown);
        self.portal.close_rd_session();
    }
}

impl EventInjection for LibeiInjection {
    fn inject_mouse_move(&self, x: i32, y: i32) -> Result<()> {
        if self.cursor_hidden.load(Ordering::SeqCst) {
            return Ok(());
        }
        self.send(InjectionCommand::MouseMove { x, y })
    }

    fn inject_mouse_move_relative(&self, dx: i32, dy: i32) -> Result<()> {
        if self.cursor_hidden.load(Ordering::SeqCst) {
            return Ok(());
        }
        self.send(InjectionCommand::MouseMoveRelative { dx, dy })
    }

    fn inject_mouse_button(&self, button: u8, pressed: bool) -> Result<()> {
        if let Ok(mut buttons) = self.pressed_buttons.lock() {
            if pressed {
                buttons.insert(button);
            } else {
                buttons.remove(&button);
            }
        }
        self.send(InjectionCommand::MouseButton { button, pressed })
    }

    fn inject_mouse_wheel(&self, delta_x: i32, delta_y: i32) -> Result<()> {
        self.send(InjectionCommand::MouseWheel { delta_x, delta_y })
    }

    fn inject_key_press(&self, keycode: u32) -> Result<()> {
        if let Ok(mut keys) = self.pressed_keys.lock() {
            keys.insert(keycode);
        }
        self.send(InjectionCommand::KeyPress { keycode })
    }

    fn inject_key_release(&self, keycode: u32) -> Result<()> {
        if let Ok(mut keys) = self.pressed_keys.lock() {
            keys.remove(&keycode);
        }
        self.send(InjectionCommand::KeyRelease { keycode })
    }

    fn set_cursor_hidden(&self, hidden: bool) -> Result<()> {
        let previous = self.cursor_hidden.swap(hidden, Ordering::SeqCst);
        if previous == hidden {
            return Ok(());
        }
        if hidden {
            let _ = self.lock_cursor_position();
        }
        Ok(())
    }

    fn warp_cursor(&self, x: i32, y: i32) -> Result<()> {
        let manager = WaylandDisplayManager::new();
        let (clamped_x, clamped_y) = manager.clamp_to_active_display(x, y).unwrap_or((x, y));
        self.send(InjectionCommand::WarpCursor {
            x: clamped_x,
            y: clamped_y,
        })
    }

    fn release_all_keys(&self) -> Result<()> {
        let snapshot: Vec<u32> = self
            .pressed_keys
            .lock()
            .map(|keys| keys.iter().copied().collect())
            .unwrap_or_default();

        for keycode in snapshot {
            let _ = self.send(InjectionCommand::KeyRelease { keycode });
        }

        if let Ok(mut keys) = self.pressed_keys.lock() {
            keys.clear();
        }

        let snapshot_buttons: Vec<u8> = self
            .pressed_buttons
            .lock()
            .map(|buttons| buttons.iter().copied().collect())
            .unwrap_or_default();

        for button in snapshot_buttons {
            let _ = self.send(InjectionCommand::MouseButton {
                button,
                pressed: false,
            });
        }

        if let Ok(mut buttons) = self.pressed_buttons.lock() {
            buttons.clear();
        }

        self.send(InjectionCommand::ReleaseAllKeys)
    }

    fn lock_cursor_position(&self) -> Result<()> {
        let manager = WaylandDisplayManager::new();
        let (park_x, park_y) = match manager.get_virtual_bounds() {
            Ok(bounds) => (
                bounds.x.saturating_add(bounds.width as i32),
                bounds.y.saturating_add(bounds.height as i32),
            ),
            Err(_) => (32767, 32767),
        };
        self.warp_cursor(park_x, park_y)
    }

    fn unlock_cursor_position(&self) -> Result<()> {
        self.portal.release_ic(None);
        Ok(())
    }

    fn restore_cursor_position(&self, x: i32, y: i32) -> Result<()> {
        self.portal.release_ic(Some((f64::from(x), f64::from(y))));
        self.warp_cursor(x, y)
    }
}

/// Convert a wire-format mouse button index to a Linux evdev button code.
///
/// Wire format (from EventInjection trait): 0=Left, 1=Right, 2=Middle, 3=X1, 4=X2.
/// Linux BTN_ codes: BTN_LEFT=0x110, BTN_RIGHT=0x111, BTN_MIDDLE=0x112, BTN_SIDE=0x113,
/// BTN_EXTRA=0x114.
fn wire_to_linux_button(button: u8) -> u32 {
    match button {
        0 => 0x110, // BTN_LEFT
        1 => 0x111, // BTN_RIGHT
        2 => 0x112, // BTN_MIDDLE
        3 => 0x113, // BTN_SIDE  / X1
        4 => 0x114, // BTN_EXTRA / X2
        other => 0x110u32.saturating_add(other as u32),
    }
}

/// Per-device libei emulation bookkeeping.
#[derive(Default)]
struct DeviceEmulation {
    /// Serial from the most recent `resumed` event for this device.
    ///
    /// Every `frame` must carry this serial. The protocol explicitly allows a
    /// compositor to silently discard events whose serial is stale.
    last_serial: u32,
    /// True between `start_emulating` and `paused`.
    emulating: bool,
}

/// Returns the serial to use for the next frame on `device`.
///
/// `None` means the device is not currently emulating. Events for a device that
/// is not resumed are a client bug and may be discarded, so callers skip them.
fn frame_serial(
    emulation: &HashMap<reis::event::Device, DeviceEmulation>,
    device: &reis::event::Device,
) -> Option<u32> {
    emulation
        .get(device)
        .filter(|state| state.emulating)
        .map(|state| state.last_serial)
}

/// Applies `resumed`/`paused` bookkeeping, requesting emulation on resume.
///
/// A sender context must call `ei_device.start_emulating` before it may frame
/// events, and must track the serial of the latest `resumed` event. Returns
/// `true` when the event was handled.
fn apply_emulation_event(
    event: &reis::event::EiEvent,
    emulation: &mut HashMap<reis::event::Device, DeviceEmulation>,
    sequence: &mut u32,
    context: &reis::ei::Context,
) -> bool {
    match event {
        reis::event::EiEvent::DeviceResumed(dev_event) => {
            let state = emulation.entry(dev_event.device.clone()).or_default();
            state.last_serial = dev_event.serial;
            if !state.emulating {
                // The sequence must increase on every start_emulating request.
                *sequence = sequence.wrapping_add(1);
                dev_event
                    .device
                    .device()
                    .start_emulating(dev_event.serial, *sequence);
                state.emulating = true;
                if let Err(e) = context.flush() {
                    tracing::warn!("libei injection: flush after start_emulating failed: {e}");
                }
            }
            true
        }
        reis::event::EiEvent::DevicePaused(dev_event) => {
            if let Some(state) = emulation.get_mut(&dev_event.device) {
                state.emulating = false;
            }
            true
        }
        _ => false,
    }
}

/// The async injection loop that writes events to the EIS socket.
///
/// Runs on the Wayland tokio runtime. Creates a reis `ei::Context` in
/// Sender/emulation mode, performs the handshake, then processes
/// `InjectionCommand` values from the channel, translating each into the
/// appropriate libei sequence.
async fn injection_loop(eis_fd: std::os::fd::OwnedFd, cmd_rx: flume::Receiver<InjectionCommand>) {
    use reis::ei;
    use std::os::unix::net::UnixStream;

    // Wrap the OwnedFd in a UnixStream for reis.
    let stream = UnixStream::from(eis_fd);
    if let Err(e) = stream.set_nonblocking(true) {
        tracing::error!("libei injection: failed to set EIS socket non-blocking: {e}");
        return;
    }

    // Create the ei::Context (client side, Sender context type).
    let context = match ei::Context::new(stream) {
        Ok(ctx) => ctx,
        Err(e) => {
            tracing::error!("libei injection: failed to create ei::Context: {e}");
            return;
        }
    };

    if let Err(e) = context.flush() {
        tracing::error!("libei injection: failed to flush initial ei context: {e}");
        return;
    }

    // Create the async event stream for the handshake phase.
    let mut event_stream = match reis::tokio::EiEventStream::new(context.clone()) {
        Ok(s) => s,
        Err(e) => {
            tracing::error!("libei injection: failed to create EiEventStream: {e}");
            return;
        }
    };

    // Interface map for capability negotiation.
    let interfaces = std::collections::HashMap::from([
        ("ei_connection", 1),
        ("ei_callback", 1),
        ("ei_pingpong", 1),
        ("ei_seat", 1),
        ("ei_device", 2),
        ("ei_pointer", 1),
        ("ei_pointer_absolute", 1),
        ("ei_scroll", 1),
        ("ei_button", 1),
        ("ei_keyboard", 1),
    ]);

    // Perform the handshake as a Sender (emulation / injection) context.
    let handshake_resp = match reis::tokio::ei_handshake(
        &mut event_stream,
        "mouse-thing",
        ei::handshake::ContextType::Sender,
        &interfaces,
    )
    .await
    {
        Ok(resp) => resp,
        Err(e) => {
            tracing::error!("libei injection: handshake failed: {e}");
            return;
        }
    };

    tracing::debug!(
        "libei injection: handshake complete, serial={}, interfaces={:?}",
        handshake_resp.serial,
        handshake_resp
            .negotiated_interfaces
            .keys()
            .collect::<Vec<_>>()
    );

    // Build the high-level EIS convert-stream so we can process seat/device events.
    use futures::stream::StreamExt;
    use reis::event::{DeviceCapability, EiEvent};

    let mut convert_stream =
        reis::tokio::EiConvertEventStream::new(event_stream, handshake_resp.serial);

    // Tracks the first seat (and optionally its pointer/keyboard devices) once
    // the compositor offers them. We need a seat to bind capabilities and a
    // device to send events through.
    let mut seat: Option<reis::event::Seat> = None;
    let mut pointer_device: Option<reis::event::Device> = None;
    let mut keyboard_device: Option<reis::event::Device> = None;
    let start_time = std::time::Instant::now();

    // Per-device emulation state. A sender context must request emulation on
    // resume, and every frame must carry that device's latest resumed serial.
    let mut emulation: HashMap<reis::event::Device, DeviceEmulation> = HashMap::new();
    let mut sequence: u32 = 0;

    // Cursor position tracking for mouse-move (we emit absolute motion).
    let mut cursor_x: i32 = 0;
    let mut cursor_y: i32 = 0;

    // Perform an initial drain of the event stream so we receive the seat/device
    // offers before we start processing commands.  We use a short timeout so we
    // don't block indefinitely if the compositor is slow.
    let drain_deadline = tokio::time::Instant::now() + std::time::Duration::from_millis(500);
    loop {
        let remaining = drain_deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            break;
        }

        let maybe_event = tokio::select! {
            ev = convert_stream.next() => ev,
            _ = tokio::time::sleep(remaining) => break,
        };

        let Some(ev_result) = maybe_event else {
            tracing::warn!("libei injection: EIS stream ended during initial drain");
            return;
        };

        let ei_event = match ev_result {
            Ok(ev) => ev,
            Err(e) => {
                tracing::warn!("libei injection: event error during drain: {e:?}");
                break;
            }
        };

        if apply_emulation_event(&ei_event, &mut emulation, &mut sequence, &context) {
            continue;
        }

        match ei_event {
            EiEvent::SeatAdded(seat_event) => {
                tracing::debug!("libei injection: seat added: {:?}", seat_event.seat.name());
                // Bind the capabilities we need for injection.
                seat_event.seat.bind_capabilities(&[
                    DeviceCapability::Pointer,
                    DeviceCapability::PointerAbsolute,
                    DeviceCapability::Keyboard,
                    DeviceCapability::Scroll,
                    DeviceCapability::Button,
                ]);
                if let Err(e) = context.flush() {
                    tracing::warn!("libei injection: flush after bind_capabilities failed: {e}");
                }
                seat = Some(seat_event.seat);
            }
            EiEvent::DeviceAdded(device_event) => {
                let dev = &device_event.device;
                let has_pointer = dev.interface::<ei::Pointer>().is_some()
                    || dev.interface::<ei::PointerAbsolute>().is_some();
                let has_keyboard = dev.interface::<ei::Keyboard>().is_some();
                tracing::debug!(
                    "libei injection: device added: {:?}, pointer={}, kbd={}",
                    dev.name(),
                    has_pointer,
                    has_keyboard
                );
                // Capture the first device that supports the capability we need.
                if has_pointer && pointer_device.is_none() {
                    pointer_device = Some(dev.clone());
                }
                if has_keyboard && keyboard_device.is_none() {
                    keyboard_device = Some(dev.clone());
                }
            }
            EiEvent::Disconnected(disconnected) => {
                tracing::warn!(
                    "libei injection: disconnected during drain (reason={:?})",
                    disconnected.reason
                );
                return;
            }
            _ => {}
        }
    }

    if seat.is_none() {
        tracing::warn!("libei injection: no seat offered by compositor; injection unavailable");
    }

    tracing::debug!("libei injection: entering command loop");

    // Command processing loop.
    loop {
        // Receive the next injection command (blocking).
        // Use a thread-spawned receive so we can also poll the event stream
        // for ping/disconnect events from the compositor.
        let cmd = match cmd_rx.recv_async().await {
            Ok(cmd) => cmd,
            Err(_) => {
                tracing::debug!("libei injection: command channel closed, exiting");
                break;
            }
        };

        match cmd {
            InjectionCommand::Shutdown => {
                tracing::debug!("libei injection: received Shutdown command");
                break;
            }

            InjectionCommand::MouseMove { x, y } => {
                let dx = (x - cursor_x) as f64;
                let dy = (y - cursor_y) as f64;
                cursor_x = x;
                cursor_y = y;

                if let Some(dev) = &pointer_device
                    && let Some(serial) = frame_serial(&emulation, dev)
                {
                    if let Some(pa) = dev.interface::<ei::PointerAbsolute>() {
                        pa.motion_absolute(x as f32, y as f32);
                    } else if let Some(p) = dev.interface::<ei::Pointer>() {
                        p.motion_relative(dx as f32, dy as f32);
                    }
                    dev.device().frame(serial, start_time.elapsed().as_micros() as u64);
                    if let Err(e) = context.flush() {
                        tracing::warn!("libei injection: flush (mouse_move) failed: {e}");
                    }
                }
            }

            InjectionCommand::MouseMoveRelative { dx, dy } => {
                cursor_x = cursor_x.saturating_add(dx);
                cursor_y = cursor_y.saturating_add(dy);

                if let Some(dev) = &pointer_device
                    && let Some(serial) = frame_serial(&emulation, dev)
                {
                    if let Some(p) = dev.interface::<ei::Pointer>() {
                        p.motion_relative(dx as f32, dy as f32);
                    }
                    dev.device().frame(serial, start_time.elapsed().as_micros() as u64);
                    if let Err(e) = context.flush() {
                        tracing::warn!("libei injection: flush (mouse_move_relative) failed: {e}");
                    }
                }
            }

            InjectionCommand::WarpCursor { x, y } => {
                let dx = (x - cursor_x) as f64;
                let dy = (y - cursor_y) as f64;
                cursor_x = x;
                cursor_y = y;

                if let Some(dev) = &pointer_device
                    && let Some(serial) = frame_serial(&emulation, dev)
                {
                    if let Some(pa) = dev.interface::<ei::PointerAbsolute>() {
                        pa.motion_absolute(x as f32, y as f32);
                    } else if let Some(p) = dev.interface::<ei::Pointer>() {
                        p.motion_relative(dx as f32, dy as f32);
                    }
                    dev.device().frame(serial, start_time.elapsed().as_micros() as u64);
                    if let Err(e) = context.flush() {
                        tracing::warn!("libei injection: flush (warp) failed: {e}");
                    }
                }
            }

            InjectionCommand::MouseButton { button, pressed } => {
                let linux_button = wire_to_linux_button(button);

                if let Some(dev) = &pointer_device
                    && let Some(serial) = frame_serial(&emulation, dev)
                {
                    if let Some(btn) = dev.interface::<ei::Button>() {
                        let state = if pressed {
                            ei::button::ButtonState::Press
                        } else {
                            ei::button::ButtonState::Released
                        };
                        btn.button(linux_button, state);
                        dev.device().frame(serial, start_time.elapsed().as_micros() as u64);
                        if let Err(e) = context.flush() {
                            tracing::warn!("libei injection: flush (button) failed: {e}");
                        }
                    }
                }
            }

            InjectionCommand::MouseWheel { delta_x, delta_y } => {
                if delta_x == 0 && delta_y == 0 {
                    continue;
                }

                if let Some(dev) = &pointer_device
                    && let Some(serial) = frame_serial(&emulation, dev)
                {
                    if let Some(scroll) = dev.interface::<ei::Scroll>() {
                        let px_x = (delta_x as f32) / 8.0;
                        let px_y = (delta_y as f32) / 8.0;
                        scroll.scroll(px_x, -px_y);
                        dev.device().frame(serial, start_time.elapsed().as_micros() as u64);
                        if let Err(e) = context.flush() {
                            tracing::warn!("libei injection: flush (wheel) failed: {e}");
                        }
                    }
                }
            }

            InjectionCommand::KeyPress { keycode } => {
                if let Some(evdev) = keycodes::hid_to_evdev(keycode) {
                    if let Some(dev) = &keyboard_device
                        && let Some(serial) = frame_serial(&emulation, dev)
                    {
                        if let Some(kbd) = dev.interface::<ei::Keyboard>() {
                            kbd.key(evdev, ei::keyboard::KeyState::Press);
                            dev.device()
                                .frame(serial, start_time.elapsed().as_micros() as u64);
                            if let Err(e) = context.flush() {
                                tracing::warn!("libei injection: flush (key_press) failed: {e}");
                            }
                        }
                    }
                } else {
                    tracing::trace!(
                        "libei injection: no evdev mapping for HID keycode {keycode:#x}"
                    );
                }
            }

            InjectionCommand::KeyRelease { keycode } => {
                if let Some(evdev) = keycodes::hid_to_evdev(keycode) {
                    if let Some(dev) = &keyboard_device
                        && let Some(serial) = frame_serial(&emulation, dev)
                    {
                        if let Some(kbd) = dev.interface::<ei::Keyboard>() {
                            kbd.key(evdev, ei::keyboard::KeyState::Released);
                            dev.device()
                                .frame(serial, start_time.elapsed().as_micros() as u64);
                            if let Err(e) = context.flush() {
                                tracing::warn!("libei injection: flush (key_release) failed: {e}");
                            }
                        }
                    }
                } else {
                    tracing::trace!(
                        "libei injection: no evdev mapping for HID keycode {keycode:#x}"
                    );
                }
            }

            InjectionCommand::ReleaseAllKeys => {
                if let Some(dev) = &keyboard_device
                    && let Some(serial) = frame_serial(&emulation, dev)
                {
                    if let Some(kbd) = dev.interface::<ei::Keyboard>() {
                        for hid in 0xE0u32..=0xE7u32 {
                            if let Some(evdev) = keycodes::hid_to_evdev(hid) {
                                kbd.key(evdev, ei::keyboard::KeyState::Released);
                            }
                        }
                        dev.device().frame(serial, start_time.elapsed().as_micros() as u64);
                        if let Err(e) = context.flush() {
                            tracing::warn!("libei injection: flush (release_all_keys) failed: {e}");
                        }
                    }
                }

                if let Some(dev) = &pointer_device
                    && let Some(serial) = frame_serial(&emulation, dev)
                {
                    if let Some(btn) = dev.interface::<ei::Button>() {
                        for button in 0u8..=4u8 {
                            let linux_button = wire_to_linux_button(button);
                            btn.button(linux_button, ei::button::ButtonState::Released);
                        }
                        dev.device().frame(serial, start_time.elapsed().as_micros() as u64);
                        if let Err(e) = context.flush() {
                            tracing::warn!("libei injection: flush (release_all_buttons) failed: {e}");
                        }
                    }
                }
            }
        }

        // After processing a command, drain any pending compositor events
        // (pings, disconnects, device changes) without blocking.
        drain_pending_events(
            &mut convert_stream,
            &context,
            &mut pointer_device,
            &mut keyboard_device,
            &mut emulation,
            &mut sequence,
        )
        .await;
    }

    tracing::debug!("libei injection loop exited");
}

/// Drain any immediately available events from the EIS stream without blocking.
///
/// This handles compositor pings and disconnection notices that arrive between
/// injection commands.
async fn drain_pending_events(
    stream: &mut reis::tokio::EiConvertEventStream,
    context: &reis::ei::Context,
    pointer_device: &mut Option<reis::event::Device>,
    keyboard_device: &mut Option<reis::event::Device>,
    emulation: &mut HashMap<reis::event::Device, DeviceEmulation>,
    sequence: &mut u32,
) {
    use futures::stream::StreamExt;
    use reis::event::{DeviceCapability, EiEvent};

    loop {
        let maybe_event = tokio::select! {
            ev = stream.next() => ev,
            _ = tokio::time::sleep(std::time::Duration::ZERO) => break,
        };

        let Some(ev_result) = maybe_event else {
            break;
        };

        let ei_event = match ev_result {
            Ok(ev) => ev,
            Err(e) => {
                tracing::warn!("libei injection: event stream error: {e:?}");
                break;
            }
        };

        if apply_emulation_event(&ei_event, emulation, sequence, context) {
            continue;
        }

        match ei_event {
            EiEvent::DeviceAdded(device_event) => {
                let dev = &device_event.device;
                let has_pointer = dev.interface::<reis::ei::Pointer>().is_some()
                    || dev.interface::<reis::ei::PointerAbsolute>().is_some();
                let has_keyboard = dev.interface::<reis::ei::Keyboard>().is_some();
                if has_pointer && pointer_device.is_none() {
                    *pointer_device = Some(dev.clone());
                }
                if has_keyboard && keyboard_device.is_none() {
                    *keyboard_device = Some(dev.clone());
                }
            }
            EiEvent::DeviceRemoved(dev_event) => {
                tracing::debug!(
                    "libei injection: device removed: {:?}",
                    dev_event.device.name()
                );
                emulation.remove(&dev_event.device);
                // Clear the cached device handles if they refer to the removed device.
                if pointer_device
                    .as_ref()
                    .map(|d| d == &dev_event.device)
                    .unwrap_or(false)
                {
                    *pointer_device = None;
                }
                if keyboard_device
                    .as_ref()
                    .map(|d| d == &dev_event.device)
                    .unwrap_or(false)
                {
                    *keyboard_device = None;
                }
            }
            EiEvent::Disconnected(disconnected) => {
                tracing::warn!(
                    "libei injection: compositor disconnected (reason={:?}, explanation={})",
                    disconnected.reason,
                    disconnected.explanation
                );
                break;
            }
            EiEvent::SeatAdded(seat_event) => {
                tracing::debug!(
                    "libei injection: seat added (late): {:?}",
                    seat_event.seat.name()
                );
                seat_event.seat.bind_capabilities(&[
                    DeviceCapability::Pointer,
                    DeviceCapability::PointerAbsolute,
                    DeviceCapability::Keyboard,
                    DeviceCapability::Scroll,
                    DeviceCapability::Button,
                ]);
                if let Err(e) = context.flush() {
                    tracing::warn!("libei injection: flush after late bind_capabilities: {e}");
                }
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::platform::linux::wayland::libei_test_support::{
        RecordedRequest, WAIT_TIMEOUT, spawn,
    };
    use reis::event::DeviceCapability;
    use std::os::fd::OwnedFd;
    use std::sync::mpsc;
    use std::time::{Duration, Instant};

    /// Collect recorded requests until `min_frames` frames have arrived.
    fn collect_requests(
        rx: &mpsc::Receiver<RecordedRequest>,
        min_frames: usize,
    ) -> Vec<RecordedRequest> {
        let deadline = Instant::now() + WAIT_TIMEOUT;
        let mut collected = Vec::new();
        let mut frames = 0;
        while frames < min_frames {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                break;
            }
            match rx.recv_timeout(remaining) {
                Ok(request) => {
                    if matches!(request, RecordedRequest::Frame { .. }) {
                        frames += 1;
                    }
                    collected.push(request);
                }
                Err(_) => break,
            }
        }
        collected
    }

    #[test]
    fn dropping_injection_invalidates_consumed_portal_session() {
        let portal = Arc::new(PortalSession::with_consumed_sessions());
        let (cmd_tx, cmd_rx) = flume::bounded(256);
        let injection = LibeiInjection {
            cmd_tx,
            pressed_keys: Mutex::new(HashSet::new()),
            pressed_buttons: Mutex::new(HashSet::new()),
            cursor_hidden: AtomicBool::new(false),
            portal: Arc::clone(&portal),
        };
        assert!(portal.rd_active());
        assert!(portal.take_rd_eis_fd().is_none());

        drop(injection);

        assert!(!portal.rd_active());
        assert!(portal.ic_active());
        assert!(matches!(cmd_rx.recv().unwrap(), InjectionCommand::ReleaseAllKeys));
        assert!(matches!(cmd_rx.recv().unwrap(), InjectionCommand::Shutdown));
    }

    #[test]
    fn idle_injection_leaves_runtime_responsive() {
        let (ready_tx, ready_rx) = mpsc::channel();
        let server = spawn(move |session| {
            let _seat = session.advertise_seat();
            session.wait_for_bind();
            ready_tx.send(()).expect("signal seat bound");
            while session.poll_requests().is_ok() {
                std::thread::sleep(Duration::from_millis(1));
            }
        });
        let fd: OwnedFd = server.connect().into();
        let (cmd_tx, cmd_rx) = flume::bounded(256);
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("test tokio runtime");
        let handle = runtime.handle().clone();
        let loop_thread = std::thread::spawn(move || {
            runtime.block_on(injection_loop(fd, cmd_rx));
        });
        ready_rx.recv_timeout(WAIT_TIMEOUT).expect("seat never bound");
        std::thread::sleep(Duration::from_millis(750));

        let (progress_tx, progress_rx) = mpsc::channel();
        handle.spawn(async move {
            progress_tx.send(()).expect("signal runtime progress");
        });
        let progress = progress_rx.recv_timeout(Duration::from_secs(2));
        drop(cmd_tx);
        loop_thread.join().expect("injection loop thread panicked");
        server.join();
        assert!(progress.is_ok(), "idle injection blocked the Wayland runtime");
    }

    #[test]
    fn libei_injection_translates_commands_into_ei_requests() {
        let (requests_tx, requests_rx) = mpsc::channel();
        let (ready_tx, ready_rx) = mpsc::channel();

        // Minimal EIS implementation: offer a pointer and a keyboard, then
        // record every input request the injection loop sends.
        let server = spawn(move |session| {
            let seat = session.advertise_seat();
            session.wait_for_bind();

            let pointer = session.create_device(
                &seat,
                "pointer",
                &[
                    DeviceCapability::Pointer,
                    DeviceCapability::PointerAbsolute,
                    DeviceCapability::Button,
                    DeviceCapability::Scroll,
                ],
            );
            pointer.resume(2);

            let keyboard = session.create_device(&seat, "keyboard", &[DeviceCapability::Keyboard]);
            keyboard.resume(2);

            ready_tx.send(()).expect("signal device readiness");

            while let Ok(requests) = session.poll_requests() {
                for request in requests {
                    if let Some(recorded) = RecordedRequest::from_eis(&request) {
                        let _ = requests_tx.send(recorded);
                    }
                }
                std::thread::sleep(Duration::from_millis(1));
            }
        });

        let client = server.connect();
        let fd: OwnedFd = client.into();
        let (cmd_tx, cmd_rx) = flume::bounded(256);

        let loop_thread = std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("test tokio runtime");
            runtime.block_on(injection_loop(fd, cmd_rx));
        });

        ready_rx
            .recv_timeout(WAIT_TIMEOUT)
            .expect("injection loop never saw the offered devices");

        cmd_tx
            .send(InjectionCommand::MouseMove { x: 100, y: 100 })
            .expect("send MouseMove");
        cmd_tx
            .send(InjectionCommand::MouseButton {
                button: 0,
                pressed: true,
            })
            .expect("send button press");
        cmd_tx
            .send(InjectionCommand::MouseButton {
                button: 0,
                pressed: false,
            })
            .expect("send button release");
        cmd_tx
            .send(InjectionCommand::KeyPress { keycode: 0x04 })
            .expect("send key press");
        cmd_tx
            .send(InjectionCommand::KeyRelease { keycode: 0x04 })
            .expect("send key release");
        cmd_tx
            .send(InjectionCommand::MouseWheel {
                delta_x: 8,
                delta_y: 8,
            })
            .expect("send wheel");

        let recorded = collect_requests(&requests_rx, 6);

        // Pointer motion: the loop prefers the absolute interface.
        assert!(
            recorded.contains(&RecordedRequest::PointerMotionAbsolute { x: 100.0, y: 100.0 }),
            "absolute motion not translated: {recorded:?}"
        );
        // Wire button 0 (left) maps to BTN_LEFT (0x110).
        assert!(
            recorded.contains(&RecordedRequest::Button {
                button: 0x110,
                pressed: true,
            }),
            "button press not translated: {recorded:?}"
        );
        assert!(
            recorded.contains(&RecordedRequest::Button {
                button: 0x110,
                pressed: false,
            }),
            "button release not translated: {recorded:?}"
        );
        // HID 0x04 (A) maps to evdev 30 (KEY_A).
        assert!(
            recorded.contains(&RecordedRequest::Key {
                key: 30,
                pressed: true,
            }),
            "key press not translated: {recorded:?}"
        );
        assert!(
            recorded.contains(&RecordedRequest::Key {
                key: 30,
                pressed: false,
            }),
            "key release not translated: {recorded:?}"
        );
        // Wheel delta 8 -> 1.0 px, and the loop negates the vertical axis.
        assert!(
            recorded.contains(&RecordedRequest::Scroll { x: 1.0, y: -1.0 }),
            "wheel not translated: {recorded:?}"
        );

        // The client must announce emulation once per resumed device, using the
        // device's own resumed serial (2), not the handshake serial (1).
        let mut sequences: Vec<u32> = recorded
            .iter()
            .filter_map(|request| match request {
                RecordedRequest::StartEmulating {
                    last_serial: 2,
                    sequence,
                } => Some(*sequence),
                _ => None,
            })
            .collect();
        sequences.sort_unstable();
        assert_eq!(
            sequences,
            vec![1, 2],
            "expected one start_emulating per resumed device: {recorded:?}"
        );

        // Frames must carry the resumed serial. The old code reused the
        // handshake serial, which a strict compositor may discard.
        let frame_serials: Vec<u32> = recorded
            .iter()
            .filter_map(|request| match request {
                RecordedRequest::Frame { last_serial } => Some(*last_serial),
                _ => None,
            })
            .collect();
        assert!(!frame_serials.is_empty(), "no frames recorded: {recorded:?}");
        assert!(
            frame_serials.iter().all(|serial| *serial == 2),
            "frames used a stale serial: {frame_serials:?}"
        );

        cmd_tx
            .send(InjectionCommand::WarpCursor { x: 500, y: 600 })
            .expect("send warp cursor");
        let warp_requests = collect_requests(&requests_rx, 1);
        assert!(
            warp_requests.iter().any(|r| matches!(r, RecordedRequest::PointerMotionAbsolute { x, y } if *x == 500.0 && *y == 600.0)),
            "expected warp cursor to translate to absolute motion: {warp_requests:?}"
        );

        drop(cmd_tx);
        loop_thread.join().expect("injection loop thread panicked");
        server.join();
    }
}
