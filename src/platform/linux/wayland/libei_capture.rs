//! Native libei capture implementation using reis and ashpd InputCapture portal.
//!
//! This module implements mouse and keyboard capture via the libei protocol.
//! The EIS file descriptor is obtained from the xdg-desktop-portal InputCapture
//! interface, then used to create a reis `ei::Context` that receives input
//! events as an async tokio stream.

use anyhow::{Context, Result};
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::mpsc;

use super::portal::{self, PortalSession};
use super::runtime::wayland_runtime_handle;
use crate::platform::linux::keycodes;
use crate::platform::shared::capture_queue;
use crate::platform::{CapturePolicy, CapturedInputEvent, PeerEdge};

/// Native libei-based input capture.
///
/// Captures mouse and keyboard events via the libei protocol, using an EIS
/// file descriptor obtained from the xdg-desktop-portal InputCapture interface.
pub(crate) struct LibeiCapture {
    /// Event sender, used by the capture loop to forward events.
    tx: mpsc::Sender<CapturedInputEvent>,
    /// Event receiver, taken once by `take_event_receiver()`.
    rx: Mutex<Option<mpsc::Receiver<CapturedInputEvent>>>,
    /// Portal session providing the EIS fd.
    portal: Arc<PortalSession>,
    /// Whether capture is currently installed (tap running).
    installed: AtomicBool,
    /// Shutdown signal for the capture task.
    shutdown: Arc<AtomicBool>,
    peer_edge: PeerEdge,
    /// Capture policy published by the cursor runtime.
    capture_policy: Arc<AtomicU8>,
}

static DROPPED_EVENTS: AtomicU64 = AtomicU64::new(0);

/// How long a barrier activation may stay pending before we hand the pointer
/// back to the compositor.
///
/// When a barrier fires, the compositor captures the pointer and expects the
/// client to either take remote control or call `Release`. If no peer handoff
/// starts (e.g. no connected peer, or movement parallel to the edge), nothing
/// would ever release the pointer and the user would lose the mouse. The
/// runtime flips the capture policy to `SuppressLocalDelivery` as soon as it
/// commits to a handoff, so the absence of that flip within this window means
/// we must release.
const ACTIVATION_RELEASE_GRACE: Duration = Duration::from_millis(200);

impl LibeiCapture {
    /// Create a new LibeiCapture instance.
    ///
    /// This obtains the global portal session but does not start capturing
    /// until `install_tap()` is called. The portal IC session should already
    /// be created (or will be created lazily).
    #[allow(dead_code)]
    pub(crate) fn new() -> Result<Self> {
        Self::with_peer_edge(
            PeerEdge::Right,
            Arc::new(AtomicU8::new(CapturePolicy::DeliverLocally as u8)),
        )
    }

    #[allow(dead_code)]
    pub(crate) fn with_capture_policy(capture_policy: Arc<AtomicU8>) -> Result<Self> {
        Self::with_peer_edge(PeerEdge::Right, capture_policy)
    }

    pub(crate) fn with_peer_edge(
        peer_edge: PeerEdge,
        capture_policy: Arc<AtomicU8>,
    ) -> Result<Self> {
        let portal = portal::get_or_create_portal_session()?;
        let (tx, rx) = mpsc::channel(capture_queue::EVENT_QUEUE_CAPACITY);

        Ok(Self {
            tx,
            rx: Mutex::new(Some(rx)),
            portal,
            installed: AtomicBool::new(false),
            shutdown: Arc::new(AtomicBool::new(false)),
            peer_edge,
            capture_policy,
        })
    }

    /// Take the event receiver. Can only be called once.
    pub(crate) fn take_event_receiver(&self) -> Option<mpsc::Receiver<CapturedInputEvent>> {
        self.rx.lock().ok()?.take()
    }

    /// Start the capture loop.
    ///
    /// Creates an IC session (if not already active), takes the EIS fd,
    /// performs the libei handshake, and spawns an async task that reads
    /// events from the EIS socket and converts them to `Event` values.
    pub(crate) fn install_tap(&self) -> Result<()> {
        if self.installed.load(Ordering::Relaxed) {
            return Ok(());
        }

        // Ensure we have an active IC session with a valid EIS fd.
        if !self.portal.ic_active() {
            self.portal
                .create_ic_session_with_barriers(self.peer_edge)
                .context("failed to create InputCapture session for libei capture")?;
        }

        let eis_fd = self
            .portal
            .take_ic_eis_fd()
            .context("no EIS fd available from InputCapture session")?;

        let activation_rx = self.portal.take_ic_activation_rx();
        let tx = self.tx.clone();
        let shutdown = Arc::clone(&self.shutdown);
        let capture_policy = Arc::clone(&self.capture_policy);
        let portal = Arc::clone(&self.portal);
        self.shutdown.store(false, Ordering::SeqCst);
        self.installed.store(true, Ordering::SeqCst);

        // Spawn the capture loop on the wayland async runtime.
        wayland_runtime_handle().spawn(capture_loop(
            eis_fd,
            tx,
            shutdown,
            activation_rx,
            capture_policy,
            portal,
        ));

        Ok(())
    }

    /// Stop the capture loop.
    pub(crate) fn uninstall_tap(&self) -> Result<()> {
        if !self.installed.load(Ordering::Relaxed) {
            return Ok(());
        }
        self.shutdown.store(true, Ordering::SeqCst);
        self.installed.store(false, Ordering::SeqCst);
        self.portal.release_ic(None);
        self.portal.close_ic_session();
        Ok(())
    }

    /// Check whether capture is currently installed.
    pub(crate) fn is_installed(&self) -> bool {
        self.installed.load(Ordering::Relaxed)
    }
}

/// Convert a Linux evdev button code to our wire button number.
///
/// Wire format: 0=left, 1=right, 2=middle, 3=X1/side, 4=X2/extra.
/// Linux BTN_LEFT=0x110, BTN_RIGHT=0x111, BTN_MIDDLE=0x112, BTN_SIDE=0x113, BTN_EXTRA=0x114.
fn linux_button_to_wire(button: u32) -> u8 {
    match button {
        0x110 => 0, // BTN_LEFT
        0x111 => 1, // BTN_RIGHT
        0x112 => 2, // BTN_MIDDLE
        0x113 => 3, // BTN_SIDE / forward
        0x114 => 4, // BTN_EXTRA / back
        other => {
            if other >= 0x110 {
                (other - 0x110).min(255) as u8
            } else {
                other.min(255) as u8
            }
        }
    }
}

/// The async capture loop that reads events from the EIS socket.
///
/// This function runs on the wayland tokio runtime and converts libei
/// events into our `Event` enum, sending them through the channel.
async fn capture_loop(
    eis_fd: std::os::fd::OwnedFd,
    tx: mpsc::Sender<CapturedInputEvent>,
    shutdown: Arc<AtomicBool>,
    mut activation_rx: Option<tokio::sync::mpsc::UnboundedReceiver<portal::ActivatedCoords>>,
    capture_policy: Arc<AtomicU8>,
    portal: Arc<PortalSession>,
) {
    use futures::stream::StreamExt;
    use reis::ei;
    use reis::event::{DeviceCapability, EiEvent};
    use std::collections::HashMap;
    use std::os::unix::net::UnixStream;

    // Create the ei::Context from the portal-provided fd.
    let stream = UnixStream::from(eis_fd);
    if let Err(e) = stream.set_nonblocking(true) {
        tracing::error!("Failed to set EIS socket non-blocking: {e}");
        return;
    }

    let context = match ei::Context::new(stream) {
        Ok(ctx) => ctx,
        Err(e) => {
            tracing::error!("Failed to create ei::Context: {e}");
            return;
        }
    };

    // Flush initial state.
    if let Err(e) = context.flush() {
        tracing::error!("Failed to flush ei context: {e}");
        return;
    }

    // Create the async event stream for the handshake.
    let mut event_stream = match reis::tokio::EiEventStream::new(context.clone()) {
        Ok(s) => s,
        Err(e) => {
            tracing::error!("Failed to create EiEventStream: {e}");
            return;
        }
    };

    // Build the interface map for handshake negotiation.
    let interfaces: HashMap<&str, u32> = HashMap::from([
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

    // Perform the handshake (we are a Receiver context for capture).
    let handshake_resp = match reis::tokio::ei_handshake(
        &mut event_stream,
        "mouse-thing",
        ei::handshake::ContextType::Receiver,
        &interfaces,
    )
    .await
    {
        Ok(resp) => resp,
        Err(e) => {
            tracing::error!("libei handshake failed: {e}");
            return;
        }
    };

    tracing::debug!(
        "libei handshake complete, serial={}, interfaces={:?}",
        handshake_resp.serial,
        handshake_resp
            .negotiated_interfaces
            .keys()
            .collect::<Vec<_>>()
    );

    // Create the high-level event converter stream.
    let mut convert_stream =
        reis::tokio::EiConvertEventStream::new(event_stream, handshake_resp.serial);

    // Track absolute cursor position (libei gives relative motion).
    let mut cursor_x: i32 = 0;
    let mut cursor_y: i32 = 0;
    let mut modifiers = ModifierState::default();
    let mut synced = activation_rx.is_none();
    let mut pending_dx = 0.0;
    let mut pending_dy = 0.0;
    let mut last_policy = CapturePolicy::from_u8(capture_policy.load(Ordering::Acquire));

    // Set when a barrier fires; cleared once the runtime commits to a handoff.
    let mut pending_release: Option<tokio::time::Instant> = None;

    // Event processing loop.
    loop {
        if shutdown.load(Ordering::Relaxed) {
            tracing::debug!("libei capture loop: shutdown requested");
            break;
        }

        let current_policy = CapturePolicy::from_u8(capture_policy.load(Ordering::Acquire));
        if last_policy.suppresses_local_delivery() && !current_policy.suppresses_local_delivery() {
            synced = activation_rx.is_none();
            pending_dx = 0.0;
            pending_dy = 0.0;
        }
        last_policy = current_policy;

        // Hand the pointer back to the compositor if a barrier activation never
        // turned into a handoff. See `ACTIVATION_RELEASE_GRACE`.
        //
        // A committed handoff suppresses local delivery, so the compositor is
        // already forwarding the pointer to us; the release no longer applies
        // and the capture synchronization state must stay untouched.
        if let Some(deadline) = pending_release
            && tokio::time::Instant::now() >= deadline
        {
            pending_release = None;
            if !current_policy.suppresses_local_delivery() {
                synced = activation_rx.is_none();
                pending_dx = 0.0;
                pending_dy = 0.0;
                tracing::debug!(
                    "barrier activated without a handoff; releasing input capture"
                );
                portal.release_ic(Some((f64::from(cursor_x), f64::from(cursor_y))));
            }
        }

        let event = tokio::select! {
            biased;
            maybe_coords = async {
                match activation_rx.as_mut() {
                    Some(rx) => rx.recv().await,
                    None => std::future::pending().await,
                }
            } => {
                if let Some(coords) = maybe_coords {
                    if let Some((x, y)) = coords.position {
                        cursor_x = (x + pending_dx).round() as i32;
                        cursor_y = (y + pending_dy).round() as i32;
                    } else {
                        cursor_x = cursor_x.saturating_add(pending_dx.round() as i32);
                        cursor_y = cursor_y.saturating_add(pending_dy.round() as i32);
                    }
                    synced = true;
                    if pending_dx != 0.0 || pending_dy != 0.0 {
                        send_capture_event(
                            &tx,
                            CapturedInputEvent::MouseMove {
                                x: cursor_x,
                                y: cursor_y,
                                dx: pending_dx,
                                dy: pending_dy,
                                locally_delivered: local_delivery(&capture_policy),
                            },
                        );
                        pending_dx = 0.0;
                        pending_dy = 0.0;
                    }
                    pending_release =
                        Some(tokio::time::Instant::now() + ACTIVATION_RELEASE_GRACE);
                } else {
                    activation_rx = None;
                    synced = true;
                }
                continue;
            }
            event = convert_stream.next() => event,
            _ = tokio::time::sleep(std::time::Duration::from_millis(100)) => {
                // Periodic check for shutdown signal.
                continue;
            }
        };

        let Some(event_result) = event else {
            tracing::info!("libei capture loop: EIS stream ended (EOF)");
            break;
        };

        let ei_event = match event_result {
            Ok(ev) => ev,
            Err(e) => {
                tracing::warn!("libei capture event error: {e:?}");
                break;
            }
        };

        match ei_event {
            EiEvent::SeatAdded(seat_event) => {
                tracing::debug!("libei: seat added: {:?}", seat_event.seat.name());
                // Bind to all capabilities we care about.
                seat_event.seat.bind_capabilities(&[
                    DeviceCapability::Pointer,
                    DeviceCapability::PointerAbsolute,
                    DeviceCapability::Keyboard,
                    DeviceCapability::Scroll,
                    DeviceCapability::Button,
                ]);
                if let Err(e) = context.flush() {
                    tracing::warn!("Failed to flush after bind_capabilities: {e}");
                }
            }
            EiEvent::DeviceAdded(device_event) => {
                tracing::debug!("libei: device added: {:?}", device_event.device.name());
            }
            EiEvent::DeviceResumed(_) => {
                tracing::debug!("libei: device resumed");
            }
            EiEvent::PointerMotion(motion) => {
                let dx = f64::from(motion.dx);
                let dy = f64::from(motion.dy);
                if !synced {
                    pending_dx += dx;
                    pending_dy += dy;
                    continue;
                }
                cursor_x = cursor_x.saturating_add(dx.round() as i32);
                cursor_y = cursor_y.saturating_add(dy.round() as i32);
                send_capture_event(
                    &tx,
                    CapturedInputEvent::MouseMove {
                        x: cursor_x,
                        y: cursor_y,
                        dx,
                        dy,
                        locally_delivered: local_delivery(&capture_policy),
                    },
                );
            }
            EiEvent::PointerMotionAbsolute(abs) => {
                let new_x = abs.dx_absolute as i32;
                let new_y = abs.dy_absolute as i32;
                let dx = f64::from(new_x - cursor_x);
                let dy = f64::from(new_y - cursor_y);
                cursor_x = new_x;
                cursor_y = new_y;
                synced = true;
                pending_dx = 0.0;
                pending_dy = 0.0;
                send_capture_event(
                    &tx,
                    CapturedInputEvent::MouseMove {
                        x: cursor_x,
                        y: cursor_y,
                        dx,
                        dy,
                        locally_delivered: local_delivery(&capture_policy),
                    },
                );
            }
            EiEvent::Button(button_event) => {
                let pressed = button_event.state == ei::button::ButtonState::Press;
                let wire_button = linux_button_to_wire(button_event.button);
                send_capture_event(
                    &tx,
                    CapturedInputEvent::MouseButton {
                        button: wire_button,
                        pressed,
                    },
                );
            }
            EiEvent::KeyboardKey(key_event) => {
                // libei keys are evdev keycodes; convert to HID for wire format.
                if let Some(hid) = keycodes::evdev_to_hid(key_event.key) {
                    match key_event.state {
                        ei::keyboard::KeyState::Press => {
                            is_emergency_key_event(hid, true, &mut modifiers);
                            send_capture_event(&tx, CapturedInputEvent::KeyPress { keycode: hid });
                        }
                        ei::keyboard::KeyState::Released => {
                            is_emergency_key_event(hid, false, &mut modifiers);
                            send_capture_event(
                                &tx,
                                CapturedInputEvent::KeyRelease { keycode: hid },
                            );
                        }
                    }
                }
            }
            EiEvent::ScrollDelta(scroll) => {
                // In libei, positive dy is down and positive dx is right.
                // Protocol Event::MouseWheel defines positive delta_y as UP and positive delta_x as RIGHT.
                let delta_y = (-scroll.dy * 8.0) as i32;
                let delta_x = (scroll.dx * 8.0) as i32;
                if delta_y != 0 || delta_x != 0 {
                    send_capture_event(
                        &tx,
                        CapturedInputEvent::MouseWheel { delta_x, delta_y },
                    );
                }
            }
            EiEvent::ScrollDiscrete(scroll) => {
                // In libei, positive discrete_dy is down and positive discrete_dx is right.
                let delta_y = if scroll.discrete_dy.abs() > 10 {
                    -scroll.discrete_dy
                } else {
                    -scroll.discrete_dy * 120
                };
                let delta_x = if scroll.discrete_dx.abs() > 10 {
                    scroll.discrete_dx
                } else {
                    scroll.discrete_dx * 120
                };
                if delta_y != 0 || delta_x != 0 {
                    send_capture_event(
                        &tx,
                        CapturedInputEvent::MouseWheel {
                            delta_x,
                            delta_y,
                        },
                    );
                }
            }
            EiEvent::Disconnected(disconnected) => {
                tracing::info!(
                    "libei: disconnected (reason={:?}, explanation={})",
                    disconnected.reason,
                    disconnected.explanation
                );
                break;
            }
            EiEvent::DeviceRemoved(_) => {
                tracing::debug!("libei: device removed");
            }
            EiEvent::DevicePaused(_) => {
                tracing::debug!("libei: device paused");
                synced = activation_rx.is_none();
                pending_dx = 0.0;
                pending_dy = 0.0;
            }
            // Frame, SeatRemoved, KeyboardModifiers, Touch events, etc.
            _ => {}
        }
    }

    tracing::debug!("libei capture loop exited");
}

fn send_capture_event(sender: &mpsc::Sender<CapturedInputEvent>, event: CapturedInputEvent) {
    capture_queue::try_send(sender, event, &DROPPED_EVENTS, "wayland-libei");
}

/// Whether a captured event was already delivered to the local cursor.
///
/// The cursor runtime sets the policy to `SuppressLocalDelivery` while a peer
/// owns the cursor, so the runtime can forward the event instead of dropping
/// it on the floor. In-flight events must re-read this every time.
fn local_delivery(capture_policy: &Arc<AtomicU8>) -> bool {
    !CapturePolicy::from_u8(capture_policy.load(Ordering::Acquire)).suppresses_local_delivery()
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

#[cfg(test)]
mod tests {
    use super::{capture_loop, linux_button_to_wire};
    use crate::platform::CapturedInputEvent;
    use crate::platform::linux::wayland::libei_test_support::{WAIT_TIMEOUT, spawn};
    use reis::event::DeviceCapability;
    use std::os::fd::OwnedFd;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
    use std::time::Duration;

    #[test]
    fn uninstalling_capture_invalidates_consumed_portal_session() {
        let portal = Arc::new(super::PortalSession::with_consumed_sessions());
        let (tx, rx) = super::mpsc::channel(1);
        let capture = super::LibeiCapture {
            tx,
            rx: super::Mutex::new(Some(rx)),
            portal: Arc::clone(&portal),
            installed: AtomicBool::new(true),
            shutdown: Arc::new(AtomicBool::new(false)),
            peer_edge: crate::platform::PeerEdge::Right,
            capture_policy: Arc::new(super::AtomicU8::new(
                crate::platform::CapturePolicy::DeliverLocally as u8,
            )),
        };
        assert!(portal.ic_active());
        assert!(portal.take_ic_eis_fd().is_none());

        capture.uninstall_tap().unwrap();

        assert!(!capture.is_installed());
        assert!(capture.shutdown.load(Ordering::SeqCst));
        assert!(!portal.ic_active());
        assert!(portal.rd_active());
        capture.uninstall_tap().unwrap();
        assert!(!portal.ic_active());
    }

    #[test]
    fn linux_button_to_wire_maps_standard_buttons() {
        assert_eq!(linux_button_to_wire(0x110), 0); // BTN_LEFT -> 0
        assert_eq!(linux_button_to_wire(0x111), 1); // BTN_RIGHT -> 1
        assert_eq!(linux_button_to_wire(0x112), 2); // BTN_MIDDLE -> 2
        assert_eq!(linux_button_to_wire(0x113), 3); // BTN_SIDE -> 3
        assert_eq!(linux_button_to_wire(0x114), 4); // BTN_EXTRA -> 4
    }

    #[test]
    fn libei_capture_translates_ei_events_into_captured_events() {
        // Minimal EIS implementation: offer a combined input device and emit
        // one event of each kind, grouped into a single frame.
        let server = spawn(|session| {
            let seat = session.advertise_seat();
            session.wait_for_bind();

            let device = session.create_device(
                &seat,
                "combined",
                &[
                    DeviceCapability::Pointer,
                    DeviceCapability::PointerAbsolute,
                    DeviceCapability::Button,
                    DeviceCapability::Keyboard,
                    DeviceCapability::Scroll,
                ],
            );

            let serial = 2;
            device.resume(serial);
            device.start_emulating(serial, 1);
            device.motion_relative(10.0, 20.0);
            device.button(0x110, true); // BTN_LEFT -> wire button 0
            device.key(30, true); // KEY_A -> HID 0x04
            device.scroll(1.0, -1.0); // -> delta_x 8, delta_y 8
            device.frame(serial, 42);

            // Stay alive until the client closes the socket.
            while session.poll_requests().is_ok() {
                std::thread::sleep(Duration::from_millis(1));
            }
        });

        let client = server.connect();
        let fd: OwnedFd = client.into();
        let (tx, mut rx) = tokio::sync::mpsc::channel(64);
        let shutdown = Arc::new(AtomicBool::new(false));

        let loop_thread = std::thread::spawn({
            let shutdown = Arc::clone(&shutdown);
            move || {
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .expect("test tokio runtime");
                runtime.block_on(capture_loop(
                    fd,
                    tx,
                    shutdown,
                    None,
                    Arc::new(super::AtomicU8::new(
                        crate::platform::CapturePolicy::DeliverLocally as u8,
                    )),
                    Arc::new(super::PortalSession::with_consumed_sessions()),
                ));
            }
        });

        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("test tokio runtime");
        let events = runtime.block_on(async {
            let deadline = tokio::time::Instant::now() + WAIT_TIMEOUT;
            let mut collected = Vec::new();
            while collected.len() < 4 {
                match tokio::time::timeout_at(deadline, rx.recv()).await {
                    Ok(Some(event)) => collected.push(event),
                    Ok(None) => break,
                    Err(_) => panic!("timed out waiting for captured events: {collected:?}"),
                }
            }
            collected
        });

        assert!(
            events.contains(&CapturedInputEvent::MouseMove {
                x: 10,
                y: 20,
                dx: 10.0,
                dy: 20.0,
                locally_delivered: true,
            }),
            "relative motion not translated: {events:?}"
        );
        assert!(
            events.contains(&CapturedInputEvent::MouseButton {
                button: 0,
                pressed: true,
            }),
            "button not translated: {events:?}"
        );
        assert!(
            events.contains(&CapturedInputEvent::KeyPress { keycode: 0x04 }),
            "key not translated: {events:?}"
        );
        assert!(
            events.contains(&CapturedInputEvent::MouseWheel {
                delta_x: 8,
                delta_y: 8,
            }),
            "scroll not translated: {events:?}"
        );

        shutdown.store(true, Ordering::SeqCst);
        loop_thread.join().expect("capture loop thread panicked");
        server.join();
    }

    #[test]
    fn barrier_activation_coordinates_synchronize_before_relative_motion() {
        let server = spawn(|session| {
            let seat = session.advertise_seat();
            session.wait_for_bind();

            let device = session.create_device(
                &seat,
                "pointer",
                &[DeviceCapability::Pointer],
            );

            let serial = 2;
            device.resume(serial);
            device.start_emulating(serial, 1);
            device.motion_relative(5.0, 10.0);
            device.frame(serial, 100);

            while session.poll_requests().is_ok() {
                std::thread::sleep(Duration::from_millis(1));
            }
        });

        let client = server.connect();
        let fd: OwnedFd = client.into();
        let (tx, mut rx) = tokio::sync::mpsc::channel(64);
        let (act_tx, act_rx) = tokio::sync::mpsc::unbounded_channel();
        let shutdown = Arc::new(AtomicBool::new(false));

        let loop_thread = std::thread::spawn({
            let shutdown = Arc::clone(&shutdown);
            move || {
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .expect("test tokio runtime");
                runtime.block_on(capture_loop(
                    fd,
                    tx,
                    shutdown,
                    Some(act_rx),
                    Arc::new(super::AtomicU8::new(
                        crate::platform::CapturePolicy::DeliverLocally as u8,
                    )),
                    Arc::new(super::PortalSession::with_consumed_sessions()),
                ));
            }
        });

        act_tx
            .send(super::portal::ActivatedCoords {
                position: Some((1920.0, 300.0)),
            })
            .expect("send activation coords");

        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("test tokio runtime");
        let event = runtime.block_on(async {
            let deadline = tokio::time::Instant::now() + WAIT_TIMEOUT;
            tokio::time::timeout_at(deadline, rx.recv())
                .await
                .expect("timed out waiting for motion event")
                .expect("channel closed")
        });

        assert_eq!(
            event,
            CapturedInputEvent::MouseMove {
                x: 1925,
                y: 310,
                dx: 5.0,
                dy: 10.0,
                locally_delivered: true,
            }
        );

        shutdown.store(true, Ordering::SeqCst);
        loop_thread.join().expect("capture loop thread panicked");
        server.join();
    }

    #[test]
    fn barrier_activation_coordinates_arrive_after_relative_motion() {
        let server = spawn(|session| {
            let seat = session.advertise_seat();
            session.wait_for_bind();

            let device = session.create_device(
                &seat,
                "pointer",
                &[DeviceCapability::Pointer],
            );

            let serial = 2;
            device.resume(serial);
            device.start_emulating(serial, 1);
            device.motion_relative(5.0, 10.0);
            device.frame(serial, 100);

            while session.poll_requests().is_ok() {
                std::thread::sleep(Duration::from_millis(1));
            }
        });

        let client = server.connect();
        let fd: OwnedFd = client.into();
        let (tx, mut rx) = tokio::sync::mpsc::channel(64);
        let (act_tx, act_rx) = tokio::sync::mpsc::unbounded_channel();
        let shutdown = Arc::new(AtomicBool::new(false));

        let loop_thread = std::thread::spawn({
            let shutdown = Arc::clone(&shutdown);
            move || {
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .expect("test tokio runtime");
                runtime.block_on(capture_loop(
                    fd,
                    tx,
                    shutdown,
                    Some(act_rx),
                    Arc::new(super::AtomicU8::new(
                        crate::platform::CapturePolicy::DeliverLocally as u8,
                    )),
                    Arc::new(super::PortalSession::with_consumed_sessions()),
                ));
            }
        });

        // Delay activation coordinates so motion arrives first.
        std::thread::sleep(Duration::from_millis(10));
        act_tx
            .send(super::portal::ActivatedCoords {
                position: Some((1920.0, 300.0)),
            })
            .expect("send activation coords");

        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("test tokio runtime");
        let event = runtime.block_on(async {
            let deadline = tokio::time::Instant::now() + WAIT_TIMEOUT;
            tokio::time::timeout_at(deadline, rx.recv())
                .await
                .expect("timed out waiting for motion event")
                .expect("channel closed")
        });

        assert_eq!(
            event,
            CapturedInputEvent::MouseMove {
                x: 1925,
                y: 310,
                dx: 5.0,
                dy: 10.0,
                locally_delivered: true,
            }
        );

        shutdown.store(true, Ordering::SeqCst);
        loop_thread.join().expect("capture loop thread panicked");
        server.join();
    }

    #[test]
    fn remote_motion_is_forwarded_after_activation_release_grace() {
        enum Command {
            Motion(f32, f32),
            Stop,
        }

        let (cmd_tx, cmd_rx) = std::sync::mpsc::channel::<Command>();
        let server = spawn(move |session| {
            let seat = session.advertise_seat();
            session.wait_for_bind();

            let device = session.create_device(&seat, "pointer", &[DeviceCapability::Pointer]);

            let serial = 2;
            device.resume(serial);
            device.start_emulating(serial, 1);
            device.motion_relative(5.0, 10.0);
            device.frame(serial, 100);

            while let Ok(command) = cmd_rx.recv() {
                match command {
                    Command::Motion(dx, dy) => {
                        device.motion_relative(dx, dy);
                        device.frame(serial, 200);
                    }
                    Command::Stop => break,
                }
            }

            while session.poll_requests().is_ok() {
                std::thread::sleep(Duration::from_millis(1));
            }
        });

        let client = server.connect();
        let fd: OwnedFd = client.into();
        let (tx, mut rx) = tokio::sync::mpsc::channel(64);
        let (act_tx, act_rx) = tokio::sync::mpsc::unbounded_channel();
        let shutdown = Arc::new(AtomicBool::new(false));
        let policy = Arc::new(AtomicU8::new(
            crate::platform::CapturePolicy::DeliverLocally as u8,
        ));

        let loop_thread = std::thread::spawn({
            let shutdown = Arc::clone(&shutdown);
            let policy = Arc::clone(&policy);
            move || {
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .expect("test tokio runtime");
                runtime.block_on(capture_loop(
                    fd,
                    tx,
                    shutdown,
                    Some(act_rx),
                    policy,
                    Arc::new(super::PortalSession::with_consumed_sessions()),
                ));
            }
        });

        act_tx
            .send(super::portal::ActivatedCoords {
                position: Some((1920.0, 300.0)),
            })
            .expect("send activation coords");

        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("test tokio runtime");
        let first = runtime.block_on(async {
            let deadline = tokio::time::Instant::now() + WAIT_TIMEOUT;
            tokio::time::timeout_at(deadline, rx.recv())
                .await
                .expect("timed out waiting for first motion event")
                .expect("channel closed")
        });
        assert_eq!(
            first,
            CapturedInputEvent::MouseMove {
                x: 1925,
                y: 310,
                dx: 5.0,
                dy: 10.0,
                locally_delivered: true,
            }
        );

        // Handoff commits immediately; the activation release grace expires
        // while the remote session is still streaming motion.
        policy.store(crate::platform::CapturePolicy::SuppressLocalDelivery as u8, Ordering::SeqCst);
        std::thread::sleep(Duration::from_millis(300));
        cmd_tx
            .send(Command::Motion(0.0, 25.0))
            .expect("send remote motion");

        let remote = runtime.block_on(async {
            let deadline = tokio::time::Instant::now() + WAIT_TIMEOUT;
            tokio::time::timeout_at(deadline, rx.recv())
                .await
                .expect("timed out waiting for remote motion event")
                .expect("channel closed")
        });
        assert_eq!(
            remote,
            CapturedInputEvent::MouseMove {
                x: 1925,
                y: 335,
                dx: 0.0,
                dy: 25.0,
                locally_delivered: false,
            }
        );

        cmd_tx.send(Command::Stop).ok();
        shutdown.store(true, Ordering::SeqCst);
        loop_thread.join().expect("capture loop thread panicked");
        server.join();
    }

    #[test]
    fn reactivation_after_remote_session_uses_fresh_activation_coordinates() {
        enum Command {
            Motion(f32, f32),
            Stop,
        }

        let (cmd_tx, cmd_rx) = std::sync::mpsc::channel::<Command>();
        let server = spawn(move |session| {
            let seat = session.advertise_seat();
            session.wait_for_bind();

            let device = session.create_device(&seat, "pointer", &[DeviceCapability::Pointer]);

            let serial = 2;
            device.resume(serial);
            device.start_emulating(serial, 1);
            device.motion_relative(5.0, 10.0);
            device.frame(serial, 100);

            while let Ok(command) = cmd_rx.recv() {
                match command {
                    Command::Motion(dx, dy) => {
                        device.motion_relative(dx, dy);
                        device.frame(serial, 200);
                    }
                    Command::Stop => break,
                }
            }

            while session.poll_requests().is_ok() {
                std::thread::sleep(Duration::from_millis(1));
            }
        });

        let client = server.connect();
        let fd: OwnedFd = client.into();
        let (tx, mut rx) = tokio::sync::mpsc::channel(64);
        let (act_tx, act_rx) = tokio::sync::mpsc::unbounded_channel();
        let shutdown = Arc::new(AtomicBool::new(false));
        let policy = Arc::new(AtomicU8::new(
            crate::platform::CapturePolicy::DeliverLocally as u8,
        ));

        let loop_thread = std::thread::spawn({
            let shutdown = Arc::clone(&shutdown);
            let policy = Arc::clone(&policy);
            move || {
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .expect("test tokio runtime");
                runtime.block_on(capture_loop(
                    fd,
                    tx,
                    shutdown,
                    Some(act_rx),
                    policy,
                    Arc::new(super::PortalSession::with_consumed_sessions()),
                ));
            }
        });

        act_tx
            .send(super::portal::ActivatedCoords {
                position: Some((1920.0, 300.0)),
            })
            .expect("send activation coords");

        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("test tokio runtime");
        let first = runtime.block_on(async {
            let deadline = tokio::time::Instant::now() + WAIT_TIMEOUT;
            tokio::time::timeout_at(deadline, rx.recv())
                .await
                .expect("timed out waiting for first motion event")
                .expect("channel closed")
        });
        assert_eq!(
            first,
            CapturedInputEvent::MouseMove {
                x: 1925,
                y: 310,
                dx: 5.0,
                dy: 10.0,
                locally_delivered: true,
            }
        );

        policy.store(crate::platform::CapturePolicy::SuppressLocalDelivery as u8, Ordering::SeqCst);
        std::thread::sleep(Duration::from_millis(250));
        policy.store(crate::platform::CapturePolicy::DeliverLocally as u8, Ordering::SeqCst);
        std::thread::sleep(Duration::from_millis(250));

        cmd_tx
            .send(Command::Motion(0.0, 40.0))
            .expect("send second motion");
        std::thread::sleep(Duration::from_millis(30));
        act_tx
            .send(super::portal::ActivatedCoords {
                position: Some((1920.0, 50.0)),
            })
            .expect("send second activation coords");

        let second = runtime.block_on(async {
            let deadline = tokio::time::Instant::now() + WAIT_TIMEOUT;
            tokio::time::timeout_at(deadline, rx.recv())
                .await
                .expect("timed out waiting for second motion event")
                .expect("channel closed")
        });
        assert_eq!(
            second,
            CapturedInputEvent::MouseMove {
                x: 1920,
                y: 90,
                dx: 0.0,
                dy: 40.0,
                locally_delivered: true,
            }
        );

        cmd_tx.send(Command::Stop).ok();
        shutdown.store(true, Ordering::SeqCst);
        loop_thread.join().expect("capture loop thread panicked");
        server.join();
    }
}

