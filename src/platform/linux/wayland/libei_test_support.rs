//! In-process EIS server harness for libei injection/capture tests.
//!
//! These helpers spin up a minimal EIS implementation inside the test process
//! and connect it to the real `injection_loop`/`capture_loop` code over a
//! private unix socket. That exercises the actual reis handshake, interface
//! negotiation, wire encoding, and keycode/button translation without needing a
//! compositor or the RemoteDesktop/InputCapture portals.
//!
//! The server is deliberately hand-rolled from the raw `reis::eis` API instead
//! of reis's `calloop` helpers so the tests need no extra feature or runtime.

use reis::PendingRequestResult;
use reis::eis::{self, device::DeviceType};
use reis::event::DeviceCapability;
use reis::handshake::{EisHandshakeResp, EisHandshaker};
use std::collections::HashMap;
use std::io;
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

/// How long the harness waits for the client before failing.
pub(crate) const WAIT_TIMEOUT: Duration = Duration::from_secs(10);

const POLL_INTERVAL: Duration = Duration::from_millis(1);

/// Interface versions advertised by the test server.
///
/// Mirrors the client map in `libei_injection.rs`/`libei_capture.rs` so the
/// handshake negotiates exactly the interfaces the production code expects.
fn server_interfaces() -> HashMap<&'static str, u32> {
    HashMap::from([
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
    ])
}

/// Seat capability bit for a device capability (`2 << enum value`).
fn capability_mask(capability: DeviceCapability) -> u64 {
    2 << capability as u64
}

/// A running test EIS server.
pub(crate) struct TestEisServer<R> {
    socket_path: PathBuf,
    _dir: tempfile::TempDir,
    join: thread::JoinHandle<R>,
}

impl<R> TestEisServer<R> {
    /// Connect a client to the server socket, retrying until the listener is up.
    pub(crate) fn connect(&self) -> UnixStream {
        let deadline = Instant::now() + WAIT_TIMEOUT;
        loop {
            match UnixStream::connect(&self.socket_path) {
                Ok(stream) => return stream,
                Err(err) => {
                    assert!(
                        Instant::now() < deadline,
                        "timed out connecting to test EIS socket: {err}"
                    );
                    thread::sleep(POLL_INTERVAL);
                }
            }
        }
    }

    /// Wait for the server thread to finish and surface any panic it raised.
    pub(crate) fn join(self) -> R {
        self.join.join().expect("test EIS server thread panicked")
    }
}

/// Spawn a test EIS server.
///
/// `run` is invoked once a client has connected and completed the handshake.
/// The returned handle owns the listener socket for the lifetime of the test.
pub(crate) fn spawn<R, F>(run: F) -> TestEisServer<R>
where
    R: Send + 'static,
    F: FnOnce(ServerSession) -> R + Send + 'static,
{
    let dir = tempfile::tempdir().expect("temp dir for EIS socket");
    let socket_path = dir.path().join("eis.sock");
    let bind_path = socket_path.clone();
    let (ready_tx, ready_rx) = mpsc::channel();

    let join = thread::spawn(move || {
        let listener = eis::Listener::bind(&bind_path).expect("bind test EIS listener");
        ready_tx.send(()).expect("signal listener ready");
        let context = accept(&listener);
        let resp = handshake(&context);
        run(ServerSession::new(context, resp))
    });

    ready_rx
        .recv_timeout(WAIT_TIMEOUT)
        .expect("test EIS listener failed to start");

    TestEisServer {
        socket_path,
        _dir: dir,
        join,
    }
}

/// Block until a client connects.
fn accept(listener: &eis::Listener) -> eis::Context {
    let deadline = Instant::now() + WAIT_TIMEOUT;
    loop {
        match listener.accept() {
            Ok(Some(context)) => return context,
            Ok(None) => {}
            Err(err) => panic!("accepting EIS client failed: {err}"),
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for an ei client"
        );
        thread::sleep(POLL_INTERVAL);
    }
}

/// Drive the EIS handshake to completion.
fn handshake(context: &eis::Context) -> EisHandshakeResp {
    let interfaces = server_interfaces();
    let mut handshaker = EisHandshaker::new(context, &interfaces, 1);
    let deadline = Instant::now() + WAIT_TIMEOUT;

    loop {
        context.read().expect("read during EIS handshake");
        while let Some(result) = context.pending_request() {
            let request = expect_request(result, "handshake");
            if let Some(resp) = handshaker
                .handle_request(request)
                .expect("EIS handshake request")
            {
                return resp;
            }
        }
        assert!(Instant::now() < deadline, "timed out during EIS handshake");
        thread::sleep(POLL_INTERVAL);
    }
}

fn expect_request(result: PendingRequestResult<eis::Request>, phase: &str) -> eis::Request {
    match result {
        PendingRequestResult::Request(request) => request,
        PendingRequestResult::ParseError(err) => panic!("{phase}: parse error: {err:?}"),
        PendingRequestResult::InvalidObject(id) => panic!("{phase}: invalid object {id}"),
    }
}

/// The server half of an established EIS connection.
pub(crate) struct ServerSession {
    context: eis::Context,
    connection: eis::Connection,
}

impl ServerSession {
    fn new(context: eis::Context, resp: EisHandshakeResp) -> Self {
        Self {
            context,
            connection: resp.connection,
        }
    }

    /// Advertise a seat with every capability the production clients bind to.
    pub(crate) fn advertise_seat(&self) -> eis::Seat {
        let seat = self.connection.seat(1);
        seat.name("mouse-thing-test-seat");
        for (capability, interface) in [
            (DeviceCapability::Pointer, "ei_pointer"),
            (DeviceCapability::PointerAbsolute, "ei_pointer_absolute"),
            (DeviceCapability::Scroll, "ei_scroll"),
            (DeviceCapability::Button, "ei_button"),
            (DeviceCapability::Keyboard, "ei_keyboard"),
        ] {
            seat.capability(capability_mask(capability), interface);
        }
        seat.done();
        self.context.flush().expect("flush advertised seat");
        seat
    }

    /// Block until the client binds the advertised seat, returning the mask.
    ///
    /// Requests read alongside the bind are left in the reis buffer for the
    /// next `poll_requests` call.
    pub(crate) fn wait_for_bind(&self) -> u64 {
        let deadline = Instant::now() + WAIT_TIMEOUT;
        loop {
            if self.context.read().is_err() {
                panic!("client closed the EIS socket before binding");
            }
            while let Some(result) = self.context.pending_request() {
                if let eis::Request::Seat(_, eis::seat::Request::Bind { capabilities }) =
                    expect_request(result, "wait_for_bind")
                {
                    return capabilities;
                }
            }
            assert!(
                Instant::now() < deadline,
                "timed out waiting for the client to bind the seat"
            );
            thread::sleep(POLL_INTERVAL);
        }
    }

    /// Create a device with the requested capabilities.
    ///
    /// The `ei_device.done` event is sent before returning, so the client is
    /// guaranteed to have a usable device handle.
    pub(crate) fn create_device(
        &self,
        seat: &eis::Seat,
        name: &str,
        capabilities: &[DeviceCapability],
    ) -> TestDevice {
        let device = seat.device(2);
        device.name(name);
        device.device_type(DeviceType::Virtual);

        let mut handle = TestDevice {
            context: self.context.clone(),
            device: device.clone(),
            pointer: None,
            pointer_absolute: None,
            scroll: None,
            button: None,
            keyboard: None,
        };

        for capability in capabilities {
            match capability {
                DeviceCapability::Pointer => {
                    handle.pointer = Some(device.interface::<eis::Pointer>(1));
                }
                DeviceCapability::PointerAbsolute => {
                    handle.pointer_absolute = Some(device.interface::<eis::PointerAbsolute>(1));
                }
                DeviceCapability::Scroll => {
                    handle.scroll = Some(device.interface::<eis::Scroll>(1));
                }
                DeviceCapability::Button => {
                    handle.button = Some(device.interface::<eis::Button>(1));
                }
                DeviceCapability::Keyboard => {
                    handle.keyboard = Some(device.interface::<eis::Keyboard>(1));
                }
                DeviceCapability::Touch => {}
            }
        }

        device.done();
        self.context.flush().expect("flush created device");
        handle
    }

    /// Read any requests the client has sent.
    ///
    /// Returns `Err` once the client closes the connection.
    pub(crate) fn poll_requests(&self) -> io::Result<Vec<eis::Request>> {
        self.context.read()?;
        let mut requests = Vec::new();
        while let Some(result) = self.context.pending_request() {
            requests.push(expect_request(result, "poll_requests"));
        }
        Ok(requests)
    }
}

/// A device created by [`ServerSession::create_device`].
#[allow(dead_code)]
pub(crate) struct TestDevice {
    context: eis::Context,
    device: eis::Device,
    pointer: Option<eis::Pointer>,
    pointer_absolute: Option<eis::PointerAbsolute>,
    scroll: Option<eis::Scroll>,
    button: Option<eis::Button>,
    keyboard: Option<eis::Keyboard>,
}

impl TestDevice {
    /// Mark the device as resumed so the client knows it may exchange events.
    pub(crate) fn resume(&self, serial: u32) {
        self.device.resumed(serial);
        self.context.flush().expect("flush resumed device");
    }

    /// Notify a receiver client that the device is about to start emitting.
    pub(crate) fn start_emulating(&self, serial: u32, sequence: u32) {
        self.device.start_emulating(serial, sequence);
        self.context.flush().expect("flush start_emulating");
    }

    /// Emit relative pointer motion.
    pub(crate) fn motion_relative(&self, x: f32, y: f32) {
        let pointer = self.pointer.as_ref().expect("device has no pointer");
        pointer.motion_relative(x, y);
    }

    /// Emit a button press/release.
    pub(crate) fn button(&self, button: u32, pressed: bool) {
        let handle = self.button.as_ref().expect("device has no button");
        let state = if pressed {
            eis::button::ButtonState::Press
        } else {
            eis::button::ButtonState::Released
        };
        handle.button(button, state);
    }

    /// Emit a keyboard key press/release.
    pub(crate) fn key(&self, key: u32, pressed: bool) {
        let handle = self.keyboard.as_ref().expect("device has no keyboard");
        let state = if pressed {
            eis::keyboard::KeyState::Press
        } else {
            eis::keyboard::KeyState::Released
        };
        handle.key(key, state);
    }

    /// Emit a smooth scroll delta.
    pub(crate) fn scroll(&self, x: f32, y: f32) {
        let handle = self.scroll.as_ref().expect("device has no scroll");
        handle.scroll(x, y);
    }

    /// Close the current event frame.
    pub(crate) fn frame(&self, serial: u32, timestamp: u64) {
        self.device.frame(serial, timestamp);
        self.context.flush().expect("flush frame");
    }
}

/// A decoded client request, used to assert on the injection path.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum RecordedRequest {
    PointerMotionRelative { x: f32, y: f32 },
    PointerMotionAbsolute { x: f32, y: f32 },
    Button { button: u32, pressed: bool },
    Key { key: u32, pressed: bool },
    Scroll { x: f32, y: f32 },
    StartEmulating { last_serial: u32, sequence: u32 },
    StopEmulating { last_serial: u32 },
    Frame { last_serial: u32 },
}

impl RecordedRequest {
    /// Decode a client request, ignoring anything outside the input interfaces.
    pub(crate) fn from_eis(request: &eis::Request) -> Option<Self> {
        match request {
            eis::Request::Pointer(_, eis::pointer::Request::MotionRelative { x, y }) => {
                Some(Self::PointerMotionRelative { x: *x, y: *y })
            }
            eis::Request::PointerAbsolute(
                _,
                eis::pointer_absolute::Request::MotionAbsolute { x, y },
            ) => Some(Self::PointerMotionAbsolute { x: *x, y: *y }),
            eis::Request::Button(_, eis::button::Request::Button { button, state }) => {
                Some(Self::Button {
                    button: *button,
                    pressed: *state == eis::button::ButtonState::Press,
                })
            }
            eis::Request::Keyboard(_, eis::keyboard::Request::Key { key, state }) => {
                Some(Self::Key {
                    key: *key,
                    pressed: *state == eis::keyboard::KeyState::Press,
                })
            }
            eis::Request::Scroll(_, eis::scroll::Request::Scroll { x, y }) => {
                Some(Self::Scroll { x: *x, y: *y })
            }
            eis::Request::Device(
                _,
                eis::device::Request::StartEmulating {
                    last_serial,
                    sequence,
                },
            ) => Some(Self::StartEmulating {
                last_serial: *last_serial,
                sequence: *sequence,
            }),
            eis::Request::Device(
                _,
                eis::device::Request::StopEmulating { last_serial },
            ) => Some(Self::StopEmulating {
                last_serial: *last_serial,
            }),
            eis::Request::Device(_, eis::device::Request::Frame { last_serial, .. }) => {
                Some(Self::Frame {
                    last_serial: *last_serial,
                })
            }
            _ => None,
        }
    }
}
