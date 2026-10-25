use crate::core::api::PeerRefSnapshot;
use crate::core::handshake::{HandshakeClient, HandshakePeer};
use crate::core::peer_state::{ConnectionStatus, PeerRuntimeStateSnapshot, TransportReachability};
use crate::core::platform::EdgeAssignments;
use crate::core::platform::traits::{ClipboardError, ClipboardManager};
use anyhow::{Result, anyhow, bail};
use socket2::{Domain, Protocol, Socket, Type};
use std::net::UdpSocket;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use crate::platform::shared::capture_queue;

static TEST_CERT_PATH_COUNTER: AtomicU64 = AtomicU64::new(1);
static TEST_PEER_PAIR_COUNTER: AtomicU64 = AtomicU64::new(1);

pub fn unique_test_cert_path(prefix: &str) -> std::path::PathBuf {
    let nonce = TEST_CERT_PATH_COUNTER.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!("{prefix}-{}-{nonce}", std::process::id()))
}

fn install_crypto_provider() {
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    let _ = tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .with_writer(std::io::stderr)
        .try_init();
}

static CRYPTO_PROVIDER_INSTALLED: std::sync::Once = std::sync::Once::new();
static TEST_RUNTIME_GUARD: Mutex<()> = Mutex::new(());

pub fn ensure_crypto_provider() {
    CRYPTO_PROVIDER_INSTALLED.call_once(install_crypto_provider);
}

pub fn with_tokio_runtime<F, R>(f: F) -> R
where
    F: FnOnce() -> R + Send + 'static,
    R: Send + 'static,
{
    ensure_crypto_provider();
    let _test_guard = TEST_RUNTIME_GUARD
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("failed to create tokio runtime");

    let handle = rt.handle().clone();
    let result = rt.block_on(async move {
        let _guard = handle.enter();
        f()
    });
    drop(rt);
    thread::sleep(Duration::from_millis(100));
    result
}

struct EnvVarGuard {
    key: &'static str,
    original: Option<std::ffi::OsString>,
}

impl EnvVarGuard {
    fn set(key: &'static str, value: impl AsRef<std::ffi::OsStr>) -> Self {
        let original = std::env::var_os(key);
        unsafe {
            std::env::set_var(key, value);
        }
        Self { key, original }
    }
}

impl Drop for EnvVarGuard {
    fn drop(&mut self) {
        match &self.original {
            Some(value) => unsafe {
                std::env::set_var(self.key, value);
            },
            None => unsafe {
                std::env::remove_var(self.key);
            },
        }
    }
}

static TEST_ENV_GUARD: Mutex<()> = Mutex::new(());

pub fn with_temp_home<T>(f: impl FnOnce() -> T) -> T {
    let _guard = TEST_ENV_GUARD
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let temp_home = std::env::temp_dir().join(format!(
        "mouse-thing-test-home-{}-{nonce}",
        std::process::id()
    ));
    let appdata = temp_home.join("AppData").join("Roaming");
    let local_appdata = temp_home.join("AppData").join("Local");
    let config_dir = temp_home.join(".config");
    let cert_dir = temp_home.join("certs");
    let _ = std::fs::create_dir_all(&appdata);
    let _ = std::fs::create_dir_all(&local_appdata);
    let _ = std::fs::create_dir_all(&config_dir);
    let _ = std::fs::create_dir_all(&cert_dir);

    let test_pipe = format!(r"\\.\pipe\zerog-input-test-absent-{}", nonce);

    let _g_home = EnvVarGuard::set("HOME", &temp_home);
    let _g_userprofile = EnvVarGuard::set("USERPROFILE", &temp_home);
    let _g_appdata = EnvVarGuard::set("APPDATA", &appdata);
    let _g_localappdata = EnvVarGuard::set("LOCALAPPDATA", &local_appdata);
    let _g_xdg = EnvVarGuard::set("XDG_CONFIG_HOME", &config_dir);
    let _g_cert = EnvVarGuard::set("ZEROG_CERT_DIR", &cert_dir);
    let _g_mdns = EnvVarGuard::set("ZEROG_DISABLE_MDNS", "1");
    let _g_pipe = EnvVarGuard::set("ZEROG_INPUT_PIPE", &test_pipe);

    let result = f();
    let _ = std::fs::remove_dir_all(&temp_home);
    result
}

use crate::core::platform::engine::PlatformEngineTestExt;
use crate::core::platform::engine::RuntimeContactSnapshot;
use crate::events::{Event, LayoutUpdateEvent};
use crate::platform::{
    CapturePolicy, CapturedInputEvent, DisplayBounds, DisplayManager, EventCapture, EventInjection,
    PeerEdge, PermissionChecker, PlatformEngine, RuntimeEngine, SessionEngine, TransferMode,
};

use super::engine::{
    CaptureExt, EngineBindings, RuntimeConfig, RuntimeEngineImpl,
    create_engine_pair as create_pair_inner,
};
use super::quic::{CertificateStore, QuicEndpoint};

pub const DEFAULT_MAX_EVENTS_PER_PACKET: usize = 32;
pub const DEFAULT_WAIT_TIMEOUT: Duration = Duration::from_secs(2);
pub const DEFAULT_WAIT_POLL: Duration = Duration::from_millis(2);
pub const EDGE_BUFFER_TIMER_WAIT: Duration = Duration::from_millis(10);
pub const DISPLAY_REFRESH_WAIT: Duration = Duration::from_millis(260);
pub const MODE_STABILITY_WAIT: Duration = Duration::from_millis(25);
const EDGE_RETURN_DISTANCE: i32 = 8;

#[derive(Debug)]
pub struct TestCapture {
    installed: Arc<AtomicBool>,
    source_marker: u64,
    event_rx: Mutex<Option<tokio::sync::mpsc::Receiver<CapturedInputEvent>>>,
    event_tx: Arc<Mutex<tokio::sync::mpsc::Sender<CapturedInputEvent>>>,
}

#[derive(Clone, Debug)]
pub struct TestCaptureHandle {
    installed: Arc<AtomicBool>,
    event_tx: Arc<Mutex<tokio::sync::mpsc::Sender<CapturedInputEvent>>>,
    capture_policy: Arc<AtomicU8>,
}

impl TestCapture {
    fn new(
        source_marker: u64,
        capture_policy: Arc<AtomicU8>,
    ) -> (Self, TestCaptureHandle) {
        let installed = Arc::new(AtomicBool::new(false));
        let (event_tx, event_rx) = tokio::sync::mpsc::channel(capture_queue::EVENT_QUEUE_CAPACITY);
        let event_tx = Arc::new(Mutex::new(event_tx));
        (
            Self {
                installed: Arc::clone(&installed),
                source_marker,
                event_rx: Mutex::new(Some(event_rx)),
                event_tx: Arc::clone(&event_tx),
            },
            TestCaptureHandle {
                installed,
                event_tx,
                capture_policy,
            },
        )
    }
}

impl EventCapture for TestCapture {
    fn install_tap(&self) -> Result<()> {
        let (event_tx, event_rx) = tokio::sync::mpsc::channel(capture_queue::EVENT_QUEUE_CAPACITY);
        *self
            .event_tx
            .lock()
            .expect("test capture sender lock poisoned") = event_tx;
        *self
            .event_rx
            .lock()
            .expect("test capture receiver lock poisoned") = Some(event_rx);
        self.installed.store(true, Ordering::SeqCst);
        Ok(())
    }

    fn uninstall_tap(&self) -> Result<()> {
        self.installed.store(false, Ordering::SeqCst);
        Ok(())
    }

    fn is_installed(&self) -> bool {
        self.installed.load(Ordering::SeqCst)
    }
}

impl CaptureExt for TestCapture {
    fn take_event_receiver(&self) -> Option<tokio::sync::mpsc::Receiver<CapturedInputEvent>> {
        self.event_rx
            .lock()
            .expect("test capture receiver lock poisoned")
            .take()
    }
}

impl TestCaptureHandle {
    pub fn emit(&self, mut event: CapturedInputEvent) -> Result<()> {
        if let CapturedInputEvent::MouseMove {
            locally_delivered,
            ..
        } = &mut event
        {
            *locally_delivered = !CapturePolicy::from_u8(
                self.capture_policy.load(Ordering::Acquire),
            )
            .suppresses_local_delivery();
        }
        self.emit_captured(event)
    }

    pub fn emit_captured(&self, event: CapturedInputEvent) -> Result<()> {
        if !self.installed.load(Ordering::SeqCst) {
            bail!("cannot emit synthetic input while capture is not installed");
        }

        let sender = self
            .event_tx
            .lock()
            .expect("test capture sender lock poisoned")
            .clone();

        sender
            .try_send(event)
            .map_err(|_| anyhow!("failed to deliver synthetic input event"))
    }

    pub fn emit_many<I>(&self, events: I) -> Result<()>
    where
        I: IntoIterator<Item = CapturedInputEvent>,
    {
        for event in events {
            self.emit(event)?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InjectedCall {
    MouseMove { x: i32, y: i32 },
    MouseMoveRelative { dx: i32, dy: i32 },
    MouseButton { button: u8, pressed: bool },
    MouseWheel { delta_x: i32, delta_y: i32 },
    KeyPress { keycode: u32 },
    KeyRelease { keycode: u32 },
    SetCapsLock { active: bool },
    CursorHidden { hidden: bool },
    CursorWarped { x: i32, y: i32 },
    CursorLocked,
    CursorUnlocked,
    AllKeysReleased,
    UserActivityDeclared,
}

#[derive(Debug, Default)]
pub struct TestInjectionState {
    calls: Mutex<Vec<InjectedCall>>,
    cursor_hidden: AtomicBool,
    cursor_locked: AtomicBool,
    cursor_pos: Mutex<(i32, i32)>,
    fail_mouse_move: AtomicBool,
    caps_lock: AtomicBool,
}

impl TestInjectionState {
    pub fn take_calls(&self) -> Vec<InjectedCall> {
        let mut guard = self
            .calls
            .lock()
            .expect("test injection calls lock poisoned");
        std::mem::take(&mut *guard)
    }

    pub fn clear_calls(&self) {
        self.calls
            .lock()
            .expect("test injection calls lock poisoned")
            .clear();
    }

    pub fn cursor_pos(&self) -> (i32, i32) {
        *self
            .cursor_pos
            .lock()
            .expect("test injection cursor position lock poisoned")
    }

    pub fn is_cursor_hidden(&self) -> bool {
        self.cursor_hidden.load(Ordering::SeqCst)
    }

    pub fn is_cursor_locked(&self) -> bool {
        self.cursor_locked.load(Ordering::SeqCst)
    }

    pub fn snapshot_calls(&self) -> Vec<InjectedCall> {
        self.calls
            .lock()
            .expect("test injection calls lock poisoned")
            .clone()
    }

    pub fn set_mouse_move_failure(&self, failed: bool) {
        self.fail_mouse_move.store(failed, Ordering::SeqCst);
    }

    pub fn caps_lock_active(&self) -> bool {
        self.caps_lock.load(Ordering::SeqCst)
    }

    pub fn set_caps_lock_state(&self, active: bool) {
        self.caps_lock.store(active, Ordering::SeqCst);
    }

    fn push_call(&self, call: InjectedCall) {
        self.calls
            .lock()
            .expect("test injection calls lock poisoned")
            .push(call);
    }

    fn set_cursor_pos(&self, x: i32, y: i32) {
        *self
            .cursor_pos
            .lock()
            .expect("test injection cursor position lock poisoned") = (x, y);
    }
}

#[derive(Clone, Debug)]
pub struct TestInjection {
    state: Arc<TestInjectionState>,
    display: Arc<TestDisplayState>,
}

impl EventInjection for TestInjection {
    fn inject_mouse_move(&self, x: i32, y: i32) -> Result<()> {
        if self.state.fail_mouse_move.load(Ordering::SeqCst) {
            bail!("injected mouse move failure");
        }
        let (clamped_x, clamped_y) = self.display.clamp_point(x, y);
        self.state.set_cursor_pos(clamped_x, clamped_y);
        self.state.push_call(InjectedCall::MouseMove {
            x: clamped_x,
            y: clamped_y,
        });
        Ok(())
    }

    fn inject_mouse_move_relative(&self, dx: i32, dy: i32) -> Result<()> {
        if self.state.fail_mouse_move.load(Ordering::SeqCst) {
            bail!("injected mouse move failure");
        }
        let (cx, cy) = self.state.cursor_pos();
        let (clamped_x, clamped_y) =
            self.display.clamp_point(cx.saturating_add(dx), cy.saturating_add(dy));
        self.state.set_cursor_pos(clamped_x, clamped_y);
        self.state.push_call(InjectedCall::MouseMoveRelative { dx, dy });
        Ok(())
    }

    fn inject_mouse_button(&self, button: u8, pressed: bool) -> Result<()> {
        self.state
            .push_call(InjectedCall::MouseButton { button, pressed });
        Ok(())
    }

    fn inject_mouse_wheel(&self, delta_x: i32, delta_y: i32) -> Result<()> {
        self.state
            .push_call(InjectedCall::MouseWheel { delta_x, delta_y });
        Ok(())
    }

    fn inject_key_press(&self, keycode: u32) -> Result<()> {
        self.state.push_call(InjectedCall::KeyPress { keycode });
        Ok(())
    }

    fn inject_key_release(&self, keycode: u32) -> Result<()> {
        self.state.push_call(InjectedCall::KeyRelease { keycode });
        Ok(())
    }

    fn caps_lock_active(&self) -> Option<bool> {
        Some(self.state.caps_lock_active())
    }

    fn set_caps_lock(&self, active: bool) -> Result<()> {
        self.state.set_caps_lock_state(active);
        self.state.push_call(InjectedCall::SetCapsLock { active });
        Ok(())
    }

    fn set_cursor_hidden(&self, hidden: bool) -> Result<()> {
        self.state.cursor_hidden.store(hidden, Ordering::SeqCst);
        self.state.push_call(InjectedCall::CursorHidden { hidden });
        Ok(())
    }

    fn warp_cursor(&self, x: i32, y: i32) -> Result<()> {
        let (clamped_x, clamped_y) = self.display.clamp_point(x, y);
        self.state.set_cursor_pos(clamped_x, clamped_y);
        self.state.push_call(InjectedCall::CursorWarped {
            x: clamped_x,
            y: clamped_y,
        });
        Ok(())
    }

    fn lock_cursor_position(&self) -> Result<()> {
        self.state.cursor_locked.store(true, Ordering::SeqCst);
        self.state.push_call(InjectedCall::CursorLocked);
        Ok(())
    }

    fn unlock_cursor_position(&self) -> Result<()> {
        self.state.cursor_locked.store(false, Ordering::SeqCst);
        self.state.push_call(InjectedCall::CursorUnlocked);
        Ok(())
    }

    fn release_all_keys(&self) -> Result<()> {
        self.state.push_call(InjectedCall::AllKeysReleased);
        Ok(())
    }

    fn declare_user_activity(&self) -> Result<()> {
        self.state.push_call(InjectedCall::UserActivityDeclared);
        Ok(())
    }
}

#[derive(Debug)]
pub struct TestDisplayState {
    bounds: Mutex<DisplayBounds>,
    signature: AtomicU64,
}

impl TestDisplayState {
    fn new() -> Self {
        Self {
            bounds: Mutex::new(DisplayBounds {
                x: 0,
                y: 0,
                width: 1920,
                height: 1080,
            }),
            signature: AtomicU64::new(1),
        }
    }

    pub fn bounds(&self) -> DisplayBounds {
        *self
            .bounds
            .lock()
            .expect("test display bounds lock poisoned")
    }

    pub fn set_bounds(&self, bounds: DisplayBounds) {
        *self
            .bounds
            .lock()
            .expect("test display bounds lock poisoned") = bounds;
        self.signature.fetch_add(1, Ordering::SeqCst);
    }

    fn clamp_point(&self, x: i32, y: i32) -> (i32, i32) {
        let bounds = self.bounds();
        if bounds.width == 0 || bounds.height == 0 {
            return (bounds.x, bounds.y);
        }
        let max_x = bounds.x + bounds.width as i32 - 1;
        let max_y = bounds.y + bounds.height as i32 - 1;
        (x.clamp(bounds.x, max_x), y.clamp(bounds.y, max_y))
    }
}

#[derive(Clone, Debug)]
pub struct TestDisplay {
    state: Arc<TestDisplayState>,
}

impl DisplayManager for TestDisplay {
    fn discover_displays(&self) -> Result<Vec<DisplayBounds>> {
        Ok(vec![self.state.bounds()])
    }

    fn get_virtual_bounds(&self) -> Result<DisplayBounds> {
        Ok(self.state.bounds())
    }

    fn contains_point(&self, x: i32, y: i32) -> Result<bool> {
        let bounds = self.state.bounds();
        let max_x = bounds.x + bounds.width as i32 - 1;
        let max_y = bounds.y + bounds.height as i32 - 1;
        Ok(x >= bounds.x && x <= max_x && y >= bounds.y && y <= max_y)
    }

    fn clamp_to_active_display(&self, x: i32, y: i32) -> Result<(i32, i32)> {
        Ok(self.state.clamp_point(x, y))
    }

    fn get_signature(&self) -> Result<u64> {
        Ok(self.state.signature.load(Ordering::SeqCst))
    }

    fn cursor_position(&self) -> Result<(i32, i32)> {
        Ok(self.state.clamp_point(0, 0))
    }
}

#[derive(Debug)]
pub struct TestPermissionState {
    granted: AtomicBool,
}

impl TestPermissionState {
    fn new() -> Self {
        Self {
            granted: AtomicBool::new(true),
        }
    }

    pub fn set_granted(&self, granted: bool) {
        self.granted.store(granted, Ordering::SeqCst);
    }
}

#[derive(Clone, Debug)]
pub struct TestPermissionChecker {
    state: Arc<TestPermissionState>,
}

impl PermissionChecker for TestPermissionChecker {
    fn check_accessibility_permission(&self) -> bool {
        self.state.granted.load(Ordering::SeqCst)
    }

    fn prompt_accessibility_permission(&self) -> Result<()> {
        Ok(())
    }
}

#[derive(Debug)]
pub struct TestClipboardState {
    pub text: Mutex<Option<String>>,
    pub set_calls: Mutex<Vec<String>>,
    pub fail_set: AtomicBool,
}

#[derive(Clone, Debug)]
pub struct TestClipboardManager {
    pub state: Arc<TestClipboardState>,
}

impl ClipboardManager for TestClipboardManager {
    fn get_clipboard_text(&self) -> std::result::Result<Option<String>, ClipboardError> {
        Ok(self.state.text.lock().unwrap().clone())
    }
    fn set_clipboard_text(&self, content: &str) -> std::result::Result<(), ClipboardError> {
        if self.state.fail_set.load(Ordering::SeqCst) {
            return Err(ClipboardError::PlatformError(
                "injected clipboard set failure".to_string(),
            ));
        }
        let mut set_calls = self.state.set_calls.lock().unwrap();
        set_calls.push(content.to_string());
        *self.state.text.lock().unwrap() = Some(content.to_string());
        Ok(())
    }
    fn clear_clipboard(&self) -> std::result::Result<(), ClipboardError> {
        *self.state.text.lock().unwrap() = None;
        Ok(())
    }
}

pub struct TestHooks {
    capture: Mutex<Option<TestCapture>>,
    capture_policy: Arc<AtomicU8>,
    injection: Arc<TestInjectionState>,
    display: Arc<TestDisplayState>,
    permission: Arc<TestPermissionState>,
    clipboard: Arc<TestClipboardState>,
}

impl TestHooks {
    pub fn new(
        source_marker: u64,
    ) -> (
        Self,
        TestCaptureHandle,
        Arc<TestInjectionState>,
        Arc<TestDisplayState>,
        Arc<TestPermissionState>,
        Arc<TestClipboardState>,
    ) {
        let capture_policy = Arc::new(AtomicU8::new(CapturePolicy::DeliverLocally as u8));
        let (capture, capture_handle) =
            TestCapture::new(source_marker, Arc::clone(&capture_policy));
        let injection = Arc::new(TestInjectionState::default());
        let display = Arc::new(TestDisplayState::new());
        let permission = Arc::new(TestPermissionState::new());
        let clipboard = Arc::new(TestClipboardState {
            text: Mutex::new(None),
            set_calls: Mutex::new(Vec::new()),
            fail_set: AtomicBool::new(false),
        });

        (
            Self {
                capture: Mutex::new(Some(capture)),
                capture_policy,
                injection: Arc::clone(&injection),
                display: Arc::clone(&display),
                permission: Arc::clone(&permission),
                clipboard: Arc::clone(&clipboard),
            },
            capture_handle,
            injection,
            display,
            permission,
            clipboard,
        )
    }

    fn into_bindings(self) -> EngineBindings {
        let capture = self
            .capture
            .lock()
            .expect("test hooks capture lock poisoned")
            .take()
            .expect("test hooks capture was already created");
        let source_marker = capture.source_marker;

        EngineBindings {
            capture: Box::new(capture),
            injection: Arc::new(TestInjection {
                state: Arc::clone(&self.injection),
                display: Arc::clone(&self.display),
            }),
            display: Box::new(TestDisplay {
                state: Arc::clone(&self.display),
            }),
            permission: Box::new(TestPermissionChecker {
                state: Arc::clone(&self.permission),
            }),
            capture_policy: Arc::clone(&self.capture_policy),
            source_marker,
            platform_name: "test",
            create_thread_display: {
                let display = Arc::clone(&self.display);
                Arc::new(move || {
                    Box::new(TestDisplay {
                        state: Arc::clone(&display),
                    })
                })
            },
            on_capture_installed: Arc::new(|| {}),
            on_capture_uninstalling: Arc::new(|| {}),
            clipboard: Some(Arc::new(TestClipboardManager {
                state: Arc::clone(&self.clipboard),
            })),
        }
    }
}

pub fn reserve_local_port() -> Result<u16> {
    let socket = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))?;
    socket.set_reuse_address(true)?;
    socket.bind(
        &"127.0.0.1:0"
            .parse::<std::net::SocketAddr>()
            .unwrap()
            .into(),
    )?;
    let socket: UdpSocket = socket.into();
    let port = socket.local_addr()?.port();
    Ok(port)
}

fn default_runtime_config(listen_port: u16, peer_id: &str) -> RuntimeConfig {
    let cert_path = unique_test_cert_path("mouse-thing-test-certs");
    let cert_store = CertificateStore::load_or_create(&cert_path)
        .expect("failed to create certificate store for test");
    let endpoint = QuicEndpoint::new(listen_port, peer_id, cert_store)
        .expect("failed to create QUIC endpoint for test");
    RuntimeConfig {
        endpoint: Arc::new(endpoint),
        batch_window_us: 0,
        max_events_per_packet: DEFAULT_MAX_EVENTS_PER_PACKET,
        movement_cadence_us: 0,
        clipboard_poll_interval_ms: 50,
        clipboard_ack_timeout_ms: 200,
    }
}

pub fn test_peer_ref(peer_id: &str, peer_name: &str) -> PeerRefSnapshot {
    PeerRefSnapshot {
        peer_id: peer_id.to_string(),
        name: peer_name.to_string(),
        platform: None,
    }
}

type TestEngineResult = Result<(
    TestEngine,
    TestCaptureHandle,
    Arc<TestInjectionState>,
    Arc<TestDisplayState>,
    Arc<TestPermissionState>,
    Arc<TestClipboardState>,
    Arc<QuicEndpoint>,
)>;

type TestEngine = RuntimeEngineImpl;

fn build_test_engine(
    peer_id: &str,
    peer_name: &str,
    peer_edge: PeerEdge,
    listen_port: u16,
    source_marker: u64,
) -> TestEngineResult {
    ensure_crypto_provider();
    let (hooks, capture, injection, display, permission, clipboard) = TestHooks::new(source_marker);
    let engine = RuntimeEngineImpl::from_bindings(peer_edge, hooks.into_bindings());
    engine.set_local_identity(test_peer_ref(peer_id, peer_name))?;
    let runtime = default_runtime_config(listen_port, peer_id);
    let endpoint = Arc::clone(&runtime.endpoint);
    engine.configure_runtime(
        runtime.endpoint,
        runtime.batch_window_us,
        runtime.max_events_per_packet,
        runtime.movement_cadence_us,
        peer_id.to_string(),
        runtime.clipboard_poll_interval_ms,
        runtime.clipboard_ack_timeout_ms,
    )?;
    Ok((
        engine, capture, injection, display, permission, clipboard, endpoint,
    ))
}

pub fn create_test_engine(
    peer_id: &str,
    peer_name: &str,
    peer_edge: PeerEdge,
    listen_port: u16,
    source_marker: u64,
) -> Result<Box<dyn RuntimeEngine>> {
    let (engine, _capture, _injection, _display, _permission, _clipboard, _endpoint) =
        build_test_engine(peer_id, peer_name, peer_edge, listen_port, source_marker)?;
    Ok(Box::new(engine))
}

pub fn create_test_engine_pair(
    peer_id: &str,
    peer_name: &str,
    peer_edge: PeerEdge,
    listen_port: u16,
    source_marker: u64,
) -> Result<(Arc<dyn PlatformEngine>, Arc<dyn SessionEngine>)> {
    ensure_crypto_provider();
    let (hooks, _capture, _injection, _display, _permission, _clipboard) =
        TestHooks::new(source_marker);
    let (platform, session) = create_pair_inner(peer_edge, hooks.into_bindings());
    let runtime = default_runtime_config(listen_port, peer_id);
    session.set_local_identity(test_peer_ref(peer_id, peer_name))?;
    session.configure_runtime(
        Arc::clone(&runtime.endpoint),
        runtime.batch_window_us,
        runtime.max_events_per_packet,
        runtime.movement_cadence_us,
        peer_id.to_string(),
        runtime.clipboard_poll_interval_ms,
        runtime.clipboard_ack_timeout_ms,
    )?;
    Ok((platform, session))
}

pub fn wait_until(
    label: &str,
    timeout: Duration,
    poll: Duration,
    mut condition: impl FnMut() -> bool,
    on_timeout: impl FnOnce() -> String,
) -> Result<()> {
    let start = Instant::now();
    while start.elapsed() <= timeout {
        if condition() {
            return Ok(());
        }
        thread::sleep(poll);
    }

    bail!("timed out waiting for {label}: {}", on_timeout())
}

pub fn assert_mode_stays(
    peer: &TestPeer,
    expected: TransferMode,
    duration: Duration,
) -> Result<()> {
    let start = Instant::now();
    while start.elapsed() <= duration {
        if peer.transfer_mode() != expected {
            bail!(
                "expected {} to remain in {expected:?}: {}",
                peer.peer_name,
                peer.debug_snapshot()
            );
        }
        thread::sleep(DEFAULT_WAIT_POLL);
    }

    Ok(())
}

pub struct TestPeer {
    engine: TestEngine,
    pub endpoint: Arc<QuicEndpoint>,
    pub peer_id: String,
    pub peer_name: String,
    pub listen_port: u16,
    pub sender_id: u64,
    capture: TestCaptureHandle,
    injection: Arc<TestInjectionState>,
    display: Arc<TestDisplayState>,
    permission: Arc<TestPermissionState>,
    pub clipboard: Arc<TestClipboardState>,
}

impl TestPeer {
    pub fn new(
        peer_id: &str,
        peer_name: &str,
        peer_edge: PeerEdge,
        listen_port: u16,
        source_marker: u64,
    ) -> Result<Self> {
        let (engine, capture, injection, display, permission, clipboard, endpoint) =
            build_test_engine(peer_id, peer_name, peer_edge, listen_port, source_marker)?;
        Ok(Self {
            engine,
            endpoint,
            peer_id: peer_id.to_string(),
            peer_name: peer_name.to_string(),
            listen_port,
            sender_id: source_marker,
            capture,
            injection,
            display,
            permission,
            clipboard,
        })
    }

    pub fn peer_ref(&self) -> PeerRefSnapshot {
        test_peer_ref(&self.peer_id, &self.peer_name)
    }

    pub fn addr(&self) -> Result<std::net::SocketAddr> {
        Ok(format!("127.0.0.1:{}", self.listen_port).parse()?)
    }

    pub fn start(&self) -> Result<()> {
        let event_rx = PlatformEngine::start(&self.engine)?;
        SessionEngine::start(&self.engine, event_rx)
    }

    pub fn stop(&self) -> Result<()> {
        self.engine.stop()
    }

    pub fn configure_runtime(
        &self,
        endpoint: Arc<QuicEndpoint>,
        batch_window_us: u64,
        max_events_per_packet: usize,
        movement_cadence_us: u64,
        peer_id: String,
        clipboard_poll_interval_ms: u64,
        clipboard_ack_timeout_ms: u64,
    ) -> Result<()> {
        self.engine.configure_runtime(
            endpoint,
            batch_window_us,
            max_events_per_packet,
            movement_cadence_us,
            peer_id,
            clipboard_poll_interval_ms,
            clipboard_ack_timeout_ms,
        )
    }

    pub fn add_peer(
        &self,
        peer: PeerRefSnapshot,
        addr: std::net::SocketAddr,
        edge: PeerEdge,
    ) -> Result<()> {
        self.engine.add_peer(peer, addr, edge, None, false)
    }

    pub fn reconcile_peer_edges(&self, assignments: &EdgeAssignments) -> Result<()> {
        self.engine.reconcile_peer_edges(assignments)
    }

    pub fn reconcile_layout(&self, config: &crate::core::api::ConfigSnapshot) -> Result<()> {
        self.engine.reconcile_layout(config)
    }

    pub fn remove_peer(&self, peer_id: &str) -> Result<()> {
        self.engine.remove_peer(peer_id)
    }

    pub fn mark_peer_connected(&self, peer_id: &str) {
        use crate::core::platform::PlatformEngineTestExt;
        self.engine.mark_peer_connected(peer_id);
    }

    pub fn connect_to(&self, addr: std::net::SocketAddr) -> Result<()> {
        let local_peer = HandshakePeer::local(
            self.peer_id.clone(),
            self.peer_name.clone(),
            Some(self.display_bounds()),
            Vec::new(),
        );
        let client = HandshakeClient::new(Arc::clone(&self.endpoint), local_peer);
        let mut last_error = None;
        for _ in 0..10 {
            match tokio::task::block_in_place(|| {
                tokio::runtime::Handle::current()
                    .block_on(client.connect(addr, DEFAULT_WAIT_TIMEOUT, false))
            }) {
                Ok(remote_peer) => {
                    use crate::core::platform::PlatformEngineTestExt;
                    self.engine.mark_peer_connected(&remote_peer.peer_id);
                    return Ok(());
                }
                Err(error) => {
                    last_error = Some(error);
                    thread::sleep(Duration::from_millis(100));
                }
            }
        }

        Err(anyhow!(
            "connection handshake failed after retries: {}",
            last_error
                .map(|error| error.to_string())
                .unwrap_or_else(|| "unknown error".to_string())
        ))
    }

    pub fn emit(&self, event: CapturedInputEvent) -> Result<()> {
        self.capture.emit(event)
    }

    pub fn emit_captured(&self, event: CapturedInputEvent) -> Result<()> {
        self.capture.emit_captured(event)
    }

    pub fn emit_many<I>(&self, events: I) -> Result<()>
    where
        I: IntoIterator<Item = CapturedInputEvent>,
    {
        self.capture.emit_many(events)
    }

    pub fn transfer_mode(&self) -> TransferMode {
        self.engine.get_transfer_mode()
    }

    pub fn wait_for_mode(&self, expected: TransferMode, timeout: Duration) -> Result<()> {
        wait_until(
            &format!("{} mode {expected:?}", self.peer_name),
            timeout,
            DEFAULT_WAIT_POLL,
            || self.transfer_mode() == expected,
            || self.debug_snapshot(),
        )
    }

    pub fn wait_for_calls(
        &self,
        label: &str,
        timeout: Duration,
        predicate: impl Fn(&[InjectedCall]) -> bool,
    ) -> Result<()> {
        wait_until(
            label,
            timeout,
            DEFAULT_WAIT_POLL,
            || {
                let snapshot = self.injection.snapshot_calls();
                predicate(&snapshot)
            },
            || self.debug_snapshot(),
        )
    }

    pub fn take_injected(&self) -> Vec<InjectedCall> {
        self.injection.take_calls()
    }

    pub fn clear_injected(&self) {
        self.injection.clear_calls();
    }

    pub fn set_mouse_move_failure(&self, failed: bool) {
        self.injection.set_mouse_move_failure(failed);
    }

    pub fn set_caps_lock_state(&self, active: bool) {
        self.injection.set_caps_lock_state(active);
    }

    pub fn caps_lock_active(&self) -> bool {
        self.injection.caps_lock_active()
    }

    pub fn cursor_pos(&self) -> (i32, i32) {
        self.injection.cursor_pos()
    }

    pub fn display_bounds(&self) -> DisplayBounds {
        self.display.bounds()
    }

    pub fn set_display_bounds(&self, bounds: DisplayBounds) {
        self.display.set_bounds(bounds);
    }

    pub fn refresh_display_topology(&self) -> Result<()> {
        use crate::core::platform::PlatformEngineTestExt;
        self.engine.refresh_display_topology()
    }

    pub fn set_permission_granted(&self, granted: bool) {
        self.permission.set_granted(granted);
    }

    pub fn clipboard(&self) -> TestClipboardManager {
        TestClipboardManager {
            state: Arc::clone(&self.clipboard),
        }
    }

    pub fn set_clipboard_set_failure(&self, fail: bool) {
        self.clipboard.fail_set.store(fail, Ordering::SeqCst);
    }

    pub fn runtime_state(&self) -> Vec<PeerRuntimeStateSnapshot> {
        self.engine.get_peer_runtime_state()
    }

    pub fn throughput_metrics(&self) -> crate::core::platform::ThroughputMetrics {
        self.engine.get_throughput_metrics()
    }

    pub fn send_layout_update(&self, update: LayoutUpdateEvent) -> Result<()> {
        self.engine.send_layout_update(update)
    }

    pub fn send_events(&self, events: Vec<Event>) -> Result<()> {
        self.engine.send_events(events)
    }

    pub fn runtime_contacts(&self) -> Vec<RuntimeContactSnapshot> {
        self.engine.get_runtime_contacts()
    }

    pub fn debug_snapshot(&self) -> String {
        format!(
            "peer={} mode={:?} runtime={:?} injected={:?} cursor={:?} hidden={} locked={}",
            self.peer_name,
            self.transfer_mode(),
            self.runtime_state(),
            self.injection.snapshot_calls(),
            self.injection.cursor_pos(),
            self.injection.is_cursor_hidden(),
            self.injection.is_cursor_locked(),
        )
    }

    pub fn peer_state(&self, peer_id: &str) -> Option<PeerRuntimeStateSnapshot> {
        self.runtime_state()
            .iter()
            .find(|s| s.peer_id == peer_id)
            .cloned()
    }

    pub fn peer_reachability(&self, peer_id: &str) -> Option<TransportReachability> {
        self.peer_state(peer_id).map(|s| s.transport_reachability)
    }

    pub fn peer_session_state(&self, peer_id: &str) -> Option<ConnectionStatus> {
        self.peer_state(peer_id).map(|s| s.connection_status)
    }

    /// Closes the QUIC connection to a specific peer (simulates network partition).
    pub fn close_connection_to(&self, peer_id: &str) {
        use crate::core::platform::PlatformEngineTestExt;
        self.engine.close_connection_to(peer_id);
    }

    /// Returns the number of active (non-closed) QUIC connections to a peer.
    #[allow(dead_code)]
    pub fn get_connection_count(&self, peer_id: &str) -> usize {
        use crate::core::platform::PlatformEngineTestExt;
        self.engine.get_connection_count(peer_id)
    }

    /// Sets a peer's last_recv timestamp to (now - elapsed_us) so the next
    /// timeout check will see the peer as stale.
    pub fn advance_peer_last_recv(&self, peer_id: &str, elapsed_us: u64) -> Result<()> {
        use crate::core::platform::PlatformEngineTestExt;
        let state = self.runtime_state();
        let peer_state = state
            .iter()
            .find(|s| s.peer_id == peer_id)
            .ok_or_else(|| anyhow::anyhow!("peer {} not found in runtime state", peer_id))?;
        let peer_addr: std::net::SocketAddr = peer_state.addr.parse()?;
        self.engine.advance_peer_last_recv(peer_addr, elapsed_us);
        Ok(())
    }
}

impl Drop for TestPeer {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}

pub struct TestPeerPair {
    pub a: TestPeer,
    pub b: TestPeer,
}

impl TestPeerPair {
    pub fn new(edge_a_to_b: PeerEdge, edge_b_to_a: PeerEdge) -> Result<Self> {
        Self::new_with_runtime_config(edge_a_to_b, edge_b_to_a, 0, DEFAULT_MAX_EVENTS_PER_PACKET)
    }

    pub fn new_with_runtime_config(
        edge_a_to_b: PeerEdge,
        edge_b_to_a: PeerEdge,
        batch_window_us: u64,
        max_events_per_packet: usize,
    ) -> Result<Self> {
        ensure_crypto_provider();
        let pair_nonce = TEST_PEER_PAIR_COUNTER.fetch_add(1, Ordering::Relaxed);
        let port_a = reserve_local_port()?;
        let port_b = reserve_local_port()?;
        let peer_a_id = format!("peer-a-{pair_nonce}");
        let peer_b_id = format!("peer-b-{pair_nonce}");
        let source_marker_a = 1_000 + pair_nonce * 2;
        let source_marker_b = source_marker_a + 1;

        let a = TestPeer::new(&peer_a_id, "Peer A", edge_a_to_b, port_a, source_marker_a)?;
        let b = TestPeer::new(&peer_b_id, "Peer B", edge_b_to_a, port_b, source_marker_b)?;

        a.engine.configure_runtime(
            Arc::clone(&a.endpoint),
            batch_window_us,
            max_events_per_packet,
            0,
            peer_a_id.clone(),
            50,
            200,
        )?;
        b.engine.configure_runtime(
            Arc::clone(&b.endpoint),
            batch_window_us,
            max_events_per_packet,
            0,
            peer_b_id.clone(),
            50,
            200,
        )?;

        a.start()?;
        b.start()?;

        let b_addr = b.addr()?;
        let a_addr = a.addr()?;
        a.add_peer(b.peer_ref(), b_addr, edge_a_to_b)?;
        b.add_peer(a.peer_ref(), a_addr, edge_b_to_a)?;
        thread::sleep(Duration::from_millis(50));
        a.connect_to(b_addr)?;

        let pair = Self { a, b };
        pair.wait_until_connected(DEFAULT_WAIT_TIMEOUT)?;
        pair.a.clear_injected();
        pair.b.clear_injected();
        Ok(pair)
    }

    pub fn wait_until_connected(&self, timeout: Duration) -> Result<()> {
        wait_until(
            "peer pair connection",
            timeout,
            DEFAULT_WAIT_POLL,
            || {
                is_connected_to(&self.a, &self.b.peer_id)
                    && is_connected_to(&self.b, &self.a.peer_id)
            },
            || {
                format!(
                    "A: {}; B: {}",
                    self.a.debug_snapshot(),
                    self.b.debug_snapshot()
                )
            },
        )
    }
}

fn is_connected_to(peer: &TestPeer, expected_peer_id: &str) -> bool {
    peer.runtime_state().iter().any(|state| {
        state.peer_id == expected_peer_id
            && matches!(
                state.connection_status,
                ConnectionStatus::Connected | ConnectionStatus::Incoming
            )
            && state.transport_reachability == TransportReachability::Reachable
    })
}

pub fn mouse_move(x: i32, y: i32, dx: i32, dy: i32) -> CapturedInputEvent {
    CapturedInputEvent::MouseMove {
        x,
        y,
        dx: f64::from(dx),
        dy: f64::from(dy),
        locally_delivered: true,
    }
}

pub fn suppressed_mouse_move(x: i32, y: i32, dx: i32, dy: i32) -> CapturedInputEvent {
    CapturedInputEvent::MouseMove {
        x,
        y,
        dx: f64::from(dx),
        dy: f64::from(dy),
        locally_delivered: false,
    }
}

pub fn expected_receiver_entry(
    bounds: DisplayBounds,
    sender_edge: PeerEdge,
    along_axis: i32,
    sender_bounds: Option<DisplayBounds>,
) -> (i32, i32) {
    let min_x = bounds.x;
    let min_y = bounds.y;
    let max_x = bounds.x + bounds.width as i32 - 1;
    let max_y = bounds.y + bounds.height as i32 - 1;
    let offset = super::topology::ENTRY_OFFSET_PX.round() as i32;

    let adjusted_along_axis = if let Some(sender_b) = sender_bounds {
        match sender_edge {
            PeerEdge::Left | PeerEdge::Right => along_axis - sender_b.y + bounds.y,
            PeerEdge::Top | PeerEdge::Bottom => along_axis - sender_b.x + bounds.x,
        }
    } else {
        along_axis
    };

    match sender_edge {
        PeerEdge::Left => (max_x - offset, adjusted_along_axis.clamp(min_y, max_y)),
        PeerEdge::Right => (min_x + offset, adjusted_along_axis.clamp(min_y, max_y)),
        PeerEdge::Top => (adjusted_along_axis.clamp(min_x, max_x), max_y - offset),
        PeerEdge::Bottom => (adjusted_along_axis.clamp(min_x, max_x), min_y + offset),
    }
}

pub fn drive_to_edge(peer: &TestPeer, edge: PeerEdge, along_axis: i32) -> Result<()> {
    let bounds = peer.display_bounds();
    let min_x = bounds.x;
    let min_y = bounds.y;
    let max_x = bounds.x + bounds.width as i32 - 1;
    let max_y = bounds.y + bounds.height as i32 - 1;

    let inside_offset = 20;
    let (inside_x, inside_y, edge_x, edge_y) = match edge {
        PeerEdge::Left => (
            min_x + inside_offset,
            along_axis.clamp(min_y, max_y),
            min_x,
            along_axis.clamp(min_y, max_y),
        ),
        PeerEdge::Right => (
            max_x - inside_offset,
            along_axis.clamp(min_y, max_y),
            max_x,
            along_axis.clamp(min_y, max_y),
        ),
        PeerEdge::Top => (
            along_axis.clamp(min_x, max_x),
            min_y + inside_offset,
            along_axis.clamp(min_x, max_x),
            min_y,
        ),
        PeerEdge::Bottom => (
            along_axis.clamp(min_x, max_x),
            max_y - inside_offset,
            along_axis.clamp(min_x, max_x),
            max_y,
        ),
    };
    let (edge_dx, edge_dy) = match edge {
        PeerEdge::Left => (-4, 0),
        PeerEdge::Right => (4, 0),
        PeerEdge::Top => (0, -4),
        PeerEdge::Bottom => (0, 4),
    };

    peer.emit_many([
        mouse_move(inside_x, inside_y, 0, 0),
        mouse_move(edge_x, edge_y, edge_dx, edge_dy),
    ])?;
    thread::sleep(EDGE_BUFFER_TIMER_WAIT);
    peer.emit(mouse_move(edge_x, edge_y, 0, 0))?;
    thread::sleep(EDGE_BUFFER_TIMER_WAIT);
    Ok(())
}

pub fn drive_back_from_remote(peer: &TestPeer, edge: PeerEdge, amount: i32) -> Result<()> {
    let (return_dx, return_dy) = match edge {
        PeerEdge::Right => (-(EDGE_RETURN_DISTANCE + amount), 0),
        PeerEdge::Left => (EDGE_RETURN_DISTANCE + amount, 0),
        PeerEdge::Top => (0, EDGE_RETURN_DISTANCE + amount),
        PeerEdge::Bottom => (0, -(EDGE_RETURN_DISTANCE + amount)),
    };

    peer.emit(mouse_move(0, 0, return_dx, return_dy))?;
    thread::sleep(EDGE_BUFFER_TIMER_WAIT);
    peer.emit(mouse_move(0, 0, 0, 0))
}

pub fn wait_for_refresh_window() {
    thread::sleep(DISPLAY_REFRESH_WAIT);
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use super::{
        DEFAULT_MAX_EVENTS_PER_PACKET, InjectedCall, TestDisplayState, TestInjection,
        TestInjectionState, TestPeer, TestPeerPair, build_test_engine, reserve_local_port,
        with_tokio_runtime,
    };
    use crate::core::peer_state::{ConnectionStatus, TransportReachability};
    use crate::core::platform::engine::PlatformEngineTestExt;
    use crate::core::platform::{PlatformEngine, SessionEngine};
    use crate::platform::{CapturedInputEvent, EventInjection, PeerEdge, TransferMode};

    #[test]
    fn one_peer_smoke_starts_and_stops() {
        with_tokio_runtime(|| {
            let port = reserve_local_port().expect("port reservation should succeed");
            let (engine, capture, injection, _display, _permission, _clipboard, _endpoint) =
                build_test_engine("peer-a", "Peer A", PeerEdge::Right, port, 11)
                    .expect("test engine should build");

            let event_rx = PlatformEngine::start(&engine).expect("engine platform should start");
            SessionEngine::start(&engine, event_rx).expect("engine session should start");
            assert!(engine.get_capture().is_installed());
            assert_eq!(engine.get_transfer_mode(), TransferMode::LocalControl);
            assert_eq!(
                injection.take_calls(),
                vec![InjectedCall::CursorHidden { hidden: false }]
            );

            capture
                .emit(CapturedInputEvent::MouseMove {
                    x: 400,
                    y: 300,
                    dx: 5.0,
                    dy: -2.0,
                    locally_delivered: true,
                })
                .expect("synthetic input should be accepted while running");

            engine.stop().expect("engine should stop");
            assert!(!engine.get_capture().is_installed());
            assert!(
                capture
                    .emit_many([CapturedInputEvent::MouseMove {
                        x: 1,
                        y: 1,
                        dx: 0.0,
                        dy: 0.0,
                        locally_delivered: true,
                    }])
                    .is_err()
            );

            let calls = injection.take_calls();
            assert_eq!(
                calls,
                vec![
                    InjectedCall::AllKeysReleased,
                    InjectedCall::CursorUnlocked,
                    InjectedCall::CursorHidden { hidden: false }
                ]
            );
            assert_eq!(DEFAULT_MAX_EVENTS_PER_PACKET, 32);
        })
    }

    #[test]
    fn pair_connects() {
        with_tokio_runtime(|| {
            let pair =
                TestPeerPair::new(PeerEdge::Right, PeerEdge::Left).expect("peer pair should start");

            let a_states = pair.a.runtime_state();
            let b_states = pair.b.runtime_state();
            assert_eq!(a_states.len(), 1);
            assert_eq!(b_states.len(), 1);
            assert_eq!(a_states[0].connection_status, ConnectionStatus::Connected);
            assert_eq!(b_states[0].connection_status, ConnectionStatus::Connected);
            assert_eq!(
                a_states[0].transport_reachability,
                TransportReachability::Reachable
            );
            assert_eq!(
                b_states[0].transport_reachability,
                TransportReachability::Reachable
            );
        })
    }

    #[test]
    fn permission_denied_prevents_start() {
        with_tokio_runtime(|| {
            let port = reserve_local_port().expect("port reservation should succeed");
            let peer = TestPeer::new("peer-a", "Peer A", PeerEdge::Right, port, 77)
                .expect("test peer should build");
            peer.set_permission_granted(false);

            let error = peer
                .start()
                .expect_err("start should fail without permission");
            assert!(
                error
                    .to_string()
                    .contains("accessibility permission is required")
            );
        })
    }

    #[test]
    fn relative_mouse_move_injection_updates_cursor_pos() {
        let injection_state = Arc::new(TestInjectionState::default());
        let display_state = Arc::new(TestDisplayState::new());
        let injection = TestInjection {
            state: Arc::clone(&injection_state),
            display: display_state,
        };
        injection.warp_cursor(100, 200).expect("warp should succeed");
        injection
            .inject_mouse_move_relative(15, -20)
            .expect("relative move should succeed");
        assert_eq!(
            injection_state.take_calls(),
            vec![
                InjectedCall::CursorWarped { x: 100, y: 200 },
                InjectedCall::MouseMoveRelative { dx: 15, dy: -20 },
            ]
        );
        assert_eq!(injection_state.cursor_pos(), (115, 180));
    }
}
