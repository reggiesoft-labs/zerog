//! Windows event-capture subsystem using Raw Input and low-level hooks.
//!
//! This module captures relative mouse movement with Raw Input and captures
//! buttons, wheel events, and keyboard input with global low-level hooks.
//!
//! # Architecture
//!
//! - [`WindowsEventCapture`] implements [`EventCapture`] by spawning a
//!   dedicated hook thread with a Win32 message pump.
//! - Hooks are installed using `SetWindowsHookExW` and processed in
//!   system callbacks (`mouse_hook_proc`, `keyboard_hook_proc`).
//! - Raw mouse packets are read from `WM_INPUT` on the same message-pump thread.
//! - Self-injected events are suppressed by checking `dwExtraInfo` against
//!   a local marker.
//!
//! # Threading
//!
//! Windows hooks must be processed by a thread with a message pump. The
//! capture thread runs `GetMessageW`/`DispatchMessageW` to receive hook
//! notifications from the OS.

use anyhow::{Result, anyhow};
use std::collections::VecDeque;
use std::mem::size_of;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU32, AtomicU64, Ordering};
use std::sync::mpsc::{SyncSender, sync_channel};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};
use tokio::sync::mpsc;

use super::marker::local_mouse_marker;
use crate::platform::shared::capture_queue;
use crate::platform::{CapturePolicy, CapturedInputEvent, EventCapture};

use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::System::Threading::GetCurrentThreadId;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYEVENTF_KEYUP, SendInput,
    VK_BACK, VK_CAPITAL, VK_CONTROL, VK_ESCAPE, VK_LMENU, VK_LWIN, VK_MENU, VK_NUMLOCK, VK_PAUSE,
    VK_RMENU, VK_RWIN, VK_SCROLL, VK_SHIFT,
};
use windows_sys::Win32::UI::Input::{
    GetRawInputData, MOUSE_MOVE_ABSOLUTE, RAWINPUT, RAWINPUTDEVICE, RAWINPUTHEADER, RAWKEYBOARD,
    RAWMOUSE, RID_INPUT, RIDEV_INPUTSINK, RIM_TYPEKEYBOARD, RIM_TYPEMOUSE, RegisterRawInputDevices,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetCursorPos,
    GetMessageW, HC_ACTION, HHOOK, HWND_MESSAGE, KBDLLHOOKSTRUCT, MSG, MSLLHOOKSTRUCT,
    PostThreadMessageW, RI_KEY_E0, RegisterClassW, SetWindowsHookExW, TranslateMessage,
    UnhookWindowsHookEx, WH_KEYBOARD_LL, WH_MOUSE_LL, WM_INPUT, WM_KEYDOWN, WM_KEYUP,
    WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MBUTTONDOWN, WM_MBUTTONUP, WM_MOUSEHWHEEL, WM_MOUSEMOVE,
    WM_MOUSEWHEEL, WM_QUIT, WM_RBUTTONDOWN, WM_RBUTTONUP, WM_SYSKEYDOWN, WM_SYSKEYUP,
    WM_XBUTTONDOWN, WM_XBUTTONUP, WNDCLASSW, WS_OVERLAPPED,
};

const XBUTTON1: u32 = 0x0001;

#[derive(Clone)]
struct CallbackState {
    event_tx: mpsc::Sender<CapturedInputEvent>,
    capture_policy: Arc<AtomicU8>,
}

static CALLBACK_STATE: OnceLock<Mutex<Option<CallbackState>>> = OnceLock::new();
static RECENT_KEYBOARD_EVENTS: OnceLock<Mutex<VecDeque<RecordedKeyboardEvent>>> = OnceLock::new();

const KEYBOARD_DEDUP_WINDOW: Duration = Duration::from_millis(25);

#[derive(Clone, Copy, PartialEq, Eq)]
struct KeyboardEventSignature {
    keycode: u32,
    pressed: bool,
}

struct RecordedKeyboardEvent {
    signature: KeyboardEventSignature,
    captured_at: Instant,
}

fn callback_state() -> &'static Mutex<Option<CallbackState>> {
    CALLBACK_STATE.get_or_init(|| Mutex::new(None))
}

fn recent_keyboard_events() -> &'static Mutex<VecDeque<RecordedKeyboardEvent>> {
    RECENT_KEYBOARD_EVENTS.get_or_init(|| Mutex::new(VecDeque::new()))
}

fn record_keyboard_event(signature: KeyboardEventSignature) -> bool {
    let now = Instant::now();
    let mut events = recent_keyboard_events()
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    events.retain(|event| now.duration_since(event.captured_at) <= KEYBOARD_DEDUP_WINDOW);
    if events.iter().any(|event| event.signature == signature) {
        return false;
    }
    events.push_back(RecordedKeyboardEvent {
        signature,
        captured_at: now,
    });
    true
}

fn keyboard_event_signature(event: &CapturedInputEvent) -> Option<KeyboardEventSignature> {
    match event {
        CapturedInputEvent::KeyPress { keycode } => Some(KeyboardEventSignature {
            keycode: *keycode,
            pressed: true,
        }),
        CapturedInputEvent::KeyRelease { keycode } => Some(KeyboardEventSignature {
            keycode: *keycode,
            pressed: false,
        }),
        _ => None,
    }
}

/// Windows event capture implementation using low-level hooks.
pub(crate) struct WindowsEventCapture {
    state: Arc<CaptureState>,
    capture_policy: Arc<AtomicU8>,
}

/// Shared state for the Windows hook thread and message pump.
struct CaptureState {
    installed: AtomicBool,
    hook_thread_id: AtomicU32,
    event_rx: Mutex<Option<mpsc::Receiver<CapturedInputEvent>>>,
    worker: Mutex<Option<JoinHandle<()>>>,
}

struct InstalledHooks {
    mouse: HHOOK,
    keyboard: HHOOK,
}

impl WindowsEventCapture {
    pub(crate) fn new(capture_policy: Arc<AtomicU8>) -> Self {
        Self {
            state: Arc::new(CaptureState {
                installed: AtomicBool::new(false),
                hook_thread_id: AtomicU32::new(0),
                event_rx: Mutex::new(None),
                worker: Mutex::new(None),
            }),
            capture_policy,
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

impl EventCapture for WindowsEventCapture {
    fn install_tap(&self) -> Result<()> {
        if self.state.installed.swap(true, Ordering::SeqCst) {
            return Ok(());
        }

        recent_keyboard_events()
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clear();

        let (event_tx, event_rx) = mpsc::channel(capture_queue::EVENT_QUEUE_CAPACITY);
        {
            let mut guard = callback_state()
                .lock()
                .map_err(|_| anyhow!("capture callback state lock poisoned"))?;
            *guard = Some(CallbackState {
                event_tx,
                capture_policy: Arc::clone(&self.capture_policy),
            });
        }

        let (started_tx, started_rx) = sync_channel(1);
        let state = Arc::clone(&self.state);

        let worker = thread::Builder::new()
            .name("windows-mouse-hook".to_string())
            .spawn(move || run_hook_thread(state, started_tx))?;

        match started_rx
            .recv()
            .map_err(|_| anyhow!("hook startup channel closed unexpectedly"))?
        {
            Ok(thread_id) => {
                self.state.hook_thread_id.store(thread_id, Ordering::SeqCst);
                let mut guard = self
                    .state
                    .event_rx
                    .lock()
                    .map_err(|_| anyhow!("event receiver lock poisoned"))?;
                *guard = Some(event_rx);

                let mut worker_guard = self
                    .state
                    .worker
                    .lock()
                    .map_err(|_| anyhow!("capture worker lock poisoned"))?;
                *worker_guard = Some(worker);
                Ok(())
            }
            Err(err) => {
                self.state.installed.store(false, Ordering::SeqCst);
                if let Ok(mut guard) = callback_state().lock() {
                    guard.take();
                }
                let _ = worker.join();
                Err(anyhow!(err))
            }
        }
    }

    fn uninstall_tap(&self) -> Result<()> {
        if !self.state.installed.swap(false, Ordering::SeqCst) {
            return Ok(());
        }

        let thread_id = self.state.hook_thread_id.swap(0, Ordering::SeqCst);
        if thread_id != 0 {
            // SAFETY: `thread_id` is captured from the running hook thread at startup.
            unsafe {
                let _ = PostThreadMessageW(thread_id, WM_QUIT, 0, 0);
            }
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
                .map_err(|_| anyhow!("capture hook worker panicked"))?;
        }

        if let Ok(mut guard) = callback_state().lock() {
            guard.take();
        }
        if let Ok(mut guard) = self.state.event_rx.lock() {
            guard.take();
        }
        recent_keyboard_events()
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clear();

        Ok(())
    }

    fn is_installed(&self) -> bool {
        self.state.installed.load(Ordering::SeqCst)
    }
}

fn run_hook_thread(state: Arc<CaptureState>, started_tx: SyncSender<Result<u32, String>>) {
    crate::platform::shared::priority::set_realtime_thread_priority();
    // SAFETY: Reading current thread id has no additional preconditions.
    let thread_id = unsafe { GetCurrentThreadId() };

    // SAFETY: Null module name asks for the current process module handle.
    let module = unsafe { GetModuleHandleW(std::ptr::null()) };
    let raw_input_window = match setup_raw_input(module) {
        Ok(window) => window,
        Err(error) => {
            let _ = started_tx.send(Err(error));
            state.installed.store(false, Ordering::SeqCst);
            return;
        }
    };
    tracing::info!("Windows mouse capture using Raw Input relative motion");
    let hooks = match install_hooks(module) {
        Ok(hooks) => hooks,
        Err(error) => {
            let _ = started_tx.send(Err(error));
            cleanup_raw_input(raw_input_window);
            state.installed.store(false, Ordering::SeqCst);
            return;
        }
    };

    if started_tx.send(Ok(thread_id)).is_err() {
        cleanup_capture(raw_input_window, &hooks);
        state.installed.store(false, Ordering::SeqCst);
        return;
    }

    run_message_pump();
    cleanup_capture(raw_input_window, &hooks);

    state.installed.store(false, Ordering::SeqCst);
    state.hook_thread_id.store(0, Ordering::SeqCst);
}

fn install_hooks(
    module: windows_sys::Win32::Foundation::HINSTANCE,
) -> Result<InstalledHooks, String> {
    // SAFETY: We install a global low-level mouse hook with a valid callback.
    let hooks = InstalledHooks {
        mouse: unsafe { SetWindowsHookExW(WH_MOUSE_LL, Some(mouse_hook_proc), module, 0) },
        keyboard: unsafe { SetWindowsHookExW(WH_KEYBOARD_LL, Some(keyboard_hook_proc), module, 0) },
    };

    if hooks.mouse.is_null() || hooks.keyboard.is_null() {
        uninstall_hooks(&hooks);
        return Err(format!(
            "SetWindowsHookExW low-level hooks failed: {}",
            std::io::Error::last_os_error()
        ));
    }

    Ok(hooks)
}

fn run_message_pump() {
    // SAFETY: Message pump runs on the hook thread until WM_QUIT or error.
    unsafe {
        let mut msg: MSG = std::mem::zeroed();

        loop {
            let result = GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0);
            if result <= 0 {
                break;
            }
            if msg.message == WM_INPUT {
                handle_raw_input(msg.lParam);
            }
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}

fn cleanup_capture(raw_input_window: HWND, hooks: &InstalledHooks) {
    uninstall_hooks(hooks);
    cleanup_raw_input(raw_input_window);
}

fn uninstall_hooks(hooks: &InstalledHooks) {
    if !hooks.mouse.is_null() {
        // SAFETY: hook was successfully installed above.
        unsafe {
            let _ = UnhookWindowsHookEx(hooks.mouse);
        }
    }
    if !hooks.keyboard.is_null() {
        // SAFETY: hook was successfully installed above.
        unsafe {
            let _ = UnhookWindowsHookEx(hooks.keyboard);
        }
    }
}

fn cleanup_raw_input(raw_input_window: HWND) {
    unregister_raw_input();
    unsafe {
        DestroyWindow(raw_input_window);
    }
}

const RAW_INPUT_WINDOW_CLASS: &[u16] = &[
    b'm' as u16,
    b'o' as u16,
    b'u' as u16,
    b's' as u16,
    b'e' as u16,
    b'-' as u16,
    b't' as u16,
    b'h' as u16,
    b'i' as u16,
    b'n' as u16,
    b'g' as u16,
    b'-' as u16,
    b'r' as u16,
    b'a' as u16,
    b'w' as u16,
    0,
];

fn setup_raw_input(
    module: windows_sys::Win32::Foundation::HINSTANCE,
) -> Result<windows_sys::Win32::Foundation::HWND, String> {
    let class = WNDCLASSW {
        lpfnWndProc: Some(raw_input_window_proc),
        hInstance: module,
        lpszClassName: RAW_INPUT_WINDOW_CLASS.as_ptr(),
        ..unsafe { std::mem::zeroed() }
    };

    // SAFETY: The class structure and callback remain valid for the process lifetime.
    unsafe {
        let _ = RegisterClassW(&class);
    }

    // SAFETY: The class name is static and the message-only parent is valid.
    let window = unsafe {
        CreateWindowExW(
            0,
            RAW_INPUT_WINDOW_CLASS.as_ptr(),
            std::ptr::null(),
            WS_OVERLAPPED,
            0,
            0,
            0,
            0,
            HWND_MESSAGE,
            std::ptr::null_mut(),
            module,
            std::ptr::null(),
        )
    };
    if window.is_null() {
        return Err(format!(
            "CreateWindowExW for raw input failed: {}",
            std::io::Error::last_os_error()
        ));
    }

    let devices = [
        RAWINPUTDEVICE {
            usUsagePage: 0x01,
            usUsage: 0x02,
            dwFlags: RIDEV_INPUTSINK,
            hwndTarget: window,
        },
        RAWINPUTDEVICE {
            usUsagePage: 0x01,
            usUsage: 0x06,
            dwFlags: RIDEV_INPUTSINK,
            hwndTarget: window,
        },
    ];
    // SAFETY: `devices` are valid mouse and keyboard registration descriptors.
    let registered = unsafe {
        RegisterRawInputDevices(
            devices.as_ptr(),
            devices.len() as u32,
            size_of::<RAWINPUTDEVICE>() as u32,
        )
    };
    if registered == 0 {
        unsafe {
            DestroyWindow(window);
        }
        return Err(format!(
            "RegisterRawInputDevices failed: {}",
            std::io::Error::last_os_error()
        ));
    }

    Ok(window)
}

fn unregister_raw_input() {
    let devices = [
        RAWINPUTDEVICE {
            usUsagePage: 0x01,
            usUsage: 0x02,
            dwFlags: windows_sys::Win32::UI::Input::RIDEV_REMOVE,
            hwndTarget: std::ptr::null_mut(),
        },
        RAWINPUTDEVICE {
            usUsagePage: 0x01,
            usUsage: 0x06,
            dwFlags: windows_sys::Win32::UI::Input::RIDEV_REMOVE,
            hwndTarget: std::ptr::null_mut(),
        },
    ];
    // SAFETY: Removing mouse and keyboard registration uses the documented null target.
    unsafe {
        let _ = RegisterRawInputDevices(
            devices.as_ptr(),
            devices.len() as u32,
            size_of::<RAWINPUTDEVICE>() as u32,
        );
    }
}

unsafe extern "system" fn raw_input_window_proc(
    hwnd: windows_sys::Win32::Foundation::HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    // SAFETY: This callback forwards messages to the system window procedure.
    unsafe { DefWindowProcW(hwnd, message, wparam, lparam) }
}

fn handle_raw_input(lparam: LPARAM) {
    let mut size = 0u32;
    // SAFETY: The first call only asks Windows for the required buffer size.
    let result = unsafe {
        GetRawInputData(
            lparam as *mut std::ffi::c_void,
            RID_INPUT,
            std::ptr::null_mut(),
            &mut size,
            size_of::<RAWINPUTHEADER>() as u32,
        )
    };
    if result == u32::MAX || size == 0 {
        return;
    }

    let mut buffer = vec![0u8; size as usize];
    // SAFETY: The buffer has the size returned by the preceding Windows call.
    let copied = unsafe {
        GetRawInputData(
            lparam as *mut std::ffi::c_void,
            RID_INPUT,
            buffer.as_mut_ptr().cast(),
            &mut size,
            size_of::<RAWINPUTHEADER>() as u32,
        )
    };
    if copied == u32::MAX || copied as usize > buffer.len() {
        return;
    }

    // SAFETY: Windows filled the buffer with a RAWINPUT structure.
    let raw = unsafe { &*(buffer.as_ptr().cast::<RAWINPUT>()) };
    if raw.header.dwType == RIM_TYPEMOUSE {
        let mouse = unsafe { raw.data.mouse };
        handle_raw_mouse(mouse);
    } else if raw.header.dwType == RIM_TYPEKEYBOARD {
        let keyboard = unsafe { raw.data.keyboard };
        handle_raw_keyboard(keyboard);
    }
}

fn handle_raw_mouse(mouse: RAWMOUSE) {
    if mouse.ulExtraInformation == local_mouse_marker() as u32 {
        return;
    }
    let Some((dx, dy)) = raw_mouse_delta(&mouse) else {
        return;
    };

    let mut point = POINT { x: 0, y: 0 };
    // SAFETY: `point` is a valid writable POINT.
    if unsafe { GetCursorPos(&mut point) } == 0 {
        return;
    }

    let locally_delivered = callback_state()
        .lock()
        .ok()
        .and_then(|state| {
            state
                .as_ref()
                .map(|state| should_deliver_locally(&state.capture_policy))
        })
        .unwrap_or(true);

    send_captured_event(CapturedInputEvent::MouseMove {
        x: point.x,
        y: point.y,
        dx: f64::from(dx),
        dy: f64::from(dy),
        locally_delivered,
    });
}

fn handle_raw_keyboard(keyboard: RAWKEYBOARD) {
    if keyboard.ExtraInformation == local_mouse_marker() as u32 {
        return;
    }

    let Some(event) = raw_keyboard_event(&keyboard) else {
        return;
    };

    let should_send = keyboard_event_signature(&event).map_or(true, record_keyboard_event);
    if should_send {
        send_captured_event(event);
    }
}

fn raw_keyboard_event(keyboard: &RAWKEYBOARD) -> Option<CapturedInputEvent> {
    let extended = keyboard.Flags & RI_KEY_E0 as u16 != 0;
    let vk_code = normalized_raw_virtual_key(keyboard.VKey, keyboard.MakeCode, extended);
    let hook = KBDLLHOOKSTRUCT {
        vkCode: u32::from(vk_code),
        scanCode: u32::from(keyboard.MakeCode),
        flags: u32::from(extended),
        time: 0,
        dwExtraInfo: keyboard.ExtraInformation as usize,
    };
    keyboard_message_to_event(keyboard.Message, &hook)
}

fn normalized_raw_virtual_key(vk_code: u16, scan_code: u16, extended: bool) -> u16 {
    match vk_code {
        VK_SHIFT => {
            if scan_code == 0x36 {
                0xA1
            } else {
                0xA0
            }
        }
        VK_CONTROL => {
            if extended {
                0xA3
            } else {
                0xA2
            }
        }
        VK_MENU => {
            if extended {
                0xA5
            } else {
                0xA4
            }
        }
        _ => vk_code,
    }
}

fn raw_mouse_delta(mouse: &RAWMOUSE) -> Option<(i32, i32)> {
    if mouse.usFlags & MOUSE_MOVE_ABSOLUTE != 0 {
        return None;
    }
    if mouse.lLastX == 0 && mouse.lLastY == 0 {
        return None;
    }
    Some((mouse.lLastX, mouse.lLastY))
}

fn send_captured_event(event: CapturedInputEvent) {
    let Ok(guard) = callback_state().lock() else {
        return;
    };
    let Some(state) = guard.as_ref() else {
        return;
    };
    capture_queue::try_send(&state.event_tx, event, &DROPPED_EVENTS, "windows");
}

static DROPPED_EVENTS: AtomicU64 = AtomicU64::new(0);

/// Win32 callback for low-level mouse hooks.
///
/// # Safety
///
/// This is a system callback. `lparam` must be a valid `MSLLHOOKSTRUCT` pointer
/// as per Windows documentation for `WH_MOUSE_LL`.
unsafe extern "system" fn mouse_hook_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        if code == HC_ACTION as i32 {
            // SAFETY: For HC_ACTION, LPARAM points to a valid MSLLHOOKSTRUCT per Win32 contract.
            let hook = unsafe { &*(lparam as *const MSLLHOOKSTRUCT) };

            let marker_match = if let Ok(guard) = callback_state().lock() {
                if let Some(_state) = guard.as_ref() {
                    hook.dwExtraInfo == local_mouse_marker()
                } else {
                    false
                }
            } else {
                false
            };
            if !marker_match {
                let (event, deliver_locally) = if let Ok(guard) = callback_state().lock() {
                    if let Some(state) = guard.as_ref() {
                        let event = message_to_event(wparam as u32, hook);
                        let deliver = should_deliver_locally(&state.capture_policy);
                        (event, deliver)
                    } else {
                        (None, true)
                    }
                } else {
                    (None, true)
                };

                if let Some(event) = event {
                    send_captured_event(event);
                }

                if !deliver_locally {
                    return 1;
                }
            }
        }

        // SAFETY: Forwarding to next hook is required for low-level hook chains.
        unsafe {
            CallNextHookEx(
                std::ptr::null_mut::<core::ffi::c_void>() as HHOOK,
                code,
                wparam,
                lparam,
            )
        }
    }));

    match result {
        Ok(lresult) => lresult,
        Err(err) => {
            eprintln!(
                "[windows-capture] caught panic in mouse_hook_proc: {:?}",
                err
            );
            unsafe {
                CallNextHookEx(
                    std::ptr::null_mut::<core::ffi::c_void>() as HHOOK,
                    code,
                    wparam,
                    lparam,
                )
            }
        }
    }
}

/// Win32 callback for low-level keyboard hooks.
///
/// # Safety
///
/// This is a system callback. `lparam` must be a valid `KBDLLHOOKSTRUCT` pointer
/// as per Windows documentation for `WH_KEYBOARD_LL`.
unsafe extern "system" fn keyboard_hook_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        if code == HC_ACTION as i32 {
            // SAFETY: For HC_ACTION, LPARAM points to a valid KBDLLHOOKSTRUCT.
            let hook = unsafe { &*(lparam as *const KBDLLHOOKSTRUCT) };

            let marker_match = hook.dwExtraInfo == local_mouse_marker();
            if !marker_match {
                let is_emergency = is_emergency_escape_combo(wparam as u32, hook);
                let (event, deliver_locally) = if let Ok(guard) = callback_state().lock() {
                    if let Some(state) = guard.as_ref() {
                        let is_emergency =
                            is_emergency && !should_deliver_locally(&state.capture_policy);
                        let event = keyboard_message_to_event(wparam as u32, hook);
                        let deliver = is_emergency || should_deliver_locally(&state.capture_policy);
                        (event, deliver)
                    } else {
                        (None, true)
                    }
                } else {
                    (None, true)
                };

                let should_send = event
                    .as_ref()
                    .and_then(keyboard_event_signature)
                    .map_or(event.is_some(), record_keyboard_event);

                if should_send {
                    if let Some(event) = event {
                        send_captured_event(event);
                    }
                }

                if !deliver_locally {
                    let vk = hook.vkCode as u16;
                    let is_release = matches!(wparam as u32, WM_KEYUP | WM_SYSKEYUP);
                    if is_release
                        && (vk == VK_MENU
                            || vk == VK_LMENU
                            || vk == VK_RMENU
                            || vk == VK_LWIN
                            || vk == VK_RWIN)
                    {
                        unsafe {
                            suppress_windows_menu_activation();
                        }
                    }
                    // Lock keys keep toggling the local OS so the physical
                    // keyboard LEDs follow the machine the user is typing on.
                    // The key is still forwarded to the remote peer above.
                    if !is_lock_key(vk) {
                        return 1;
                    }
                }
            }
        }

        // SAFETY: Forwarding to next hook is required for low-level hook chains.
        unsafe {
            CallNextHookEx(
                std::ptr::null_mut::<core::ffi::c_void>() as HHOOK,
                code,
                wparam,
                lparam,
            )
        }
    }));

    match result {
        Ok(lresult) => lresult,
        Err(err) => {
            eprintln!(
                "[windows-capture] caught panic in keyboard_hook_proc: {:?}",
                err
            );
            unsafe {
                CallNextHookEx(
                    std::ptr::null_mut::<core::ffi::c_void>() as HHOOK,
                    code,
                    wparam,
                    lparam,
                )
            }
        }
    }
}

/// True for the lock keys whose physical LED is driven by the local OS.
///
/// These keys are forwarded to the remote peer but are also allowed through
/// the local input stack while remote control suppresses other keys, so the
/// Caps Lock / Num Lock / Scroll Lock LEDs stay coherent on the shared
/// keyboard.
fn is_lock_key(vk: u16) -> bool {
    matches!(vk, VK_CAPITAL | VK_NUMLOCK | VK_SCROLL)
}

unsafe fn suppress_windows_menu_activation() {
    let mut inputs = [
        INPUT {
            r#type: INPUT_KEYBOARD,
            Anonymous: INPUT_0 {
                ki: KEYBDINPUT {
                    wVk: 0xFF,
                    wScan: 0,
                    dwFlags: 0,
                    time: 0,
                    dwExtraInfo: local_mouse_marker(),
                },
            },
        },
        INPUT {
            r#type: INPUT_KEYBOARD,
            Anonymous: INPUT_0 {
                ki: KEYBDINPUT {
                    wVk: 0xFF,
                    wScan: 0,
                    dwFlags: KEYEVENTF_KEYUP,
                    time: 0,
                    dwExtraInfo: local_mouse_marker(),
                },
            },
        },
    ];
    unsafe {
        SendInput(2, inputs.as_mut_ptr(), size_of::<INPUT>() as i32);
    }
}

fn is_emergency_escape_combo(message: u32, hook: &KBDLLHOOKSTRUCT) -> bool {
    if message != WM_KEYDOWN && message != WM_SYSKEYDOWN {
        return false;
    }
    let vk = hook.vkCode as u16;
    let is_esc = vk == VK_ESCAPE;
    let is_pause = vk == VK_PAUSE;
    let is_back = vk == VK_BACK;

    if !is_esc && !is_pause && !is_back {
        return false;
    }

    // SAFETY: GetAsyncKeyState has no memory safety preconditions.
    unsafe {
        let ctrl = (GetAsyncKeyState(VK_CONTROL as i32) as u16 & 0x8000) != 0;
        let alt = (GetAsyncKeyState(VK_MENU as i32) as u16 & 0x8000) != 0;
        let shift = (GetAsyncKeyState(VK_SHIFT as i32) as u16 & 0x8000) != 0;

        (is_esc && ctrl && alt && shift)
            || (is_pause && ctrl && alt)
            || (is_back && ctrl && alt && shift)
    }
}

fn should_deliver_locally(capture_policy: &Arc<AtomicU8>) -> bool {
    !CapturePolicy::from_u8(capture_policy.load(Ordering::Acquire)).suppresses_local_delivery()
}

fn message_to_event(message: u32, hook: &MSLLHOOKSTRUCT) -> Option<CapturedInputEvent> {
    match message {
        WM_MOUSEMOVE => None,
        WM_LBUTTONDOWN => Some(CapturedInputEvent::MouseButton {
            button: 0,
            pressed: true,
        }),
        WM_LBUTTONUP => Some(CapturedInputEvent::MouseButton {
            button: 0,
            pressed: false,
        }),
        WM_RBUTTONDOWN => Some(CapturedInputEvent::MouseButton {
            button: 1,
            pressed: true,
        }),
        WM_RBUTTONUP => Some(CapturedInputEvent::MouseButton {
            button: 1,
            pressed: false,
        }),
        WM_MBUTTONDOWN => Some(CapturedInputEvent::MouseButton {
            button: 2,
            pressed: true,
        }),
        WM_MBUTTONUP => Some(CapturedInputEvent::MouseButton {
            button: 2,
            pressed: false,
        }),
        WM_XBUTTONDOWN => Some(CapturedInputEvent::MouseButton {
            button: xbutton_index(hook.mouseData),
            pressed: true,
        }),
        WM_XBUTTONUP => Some(CapturedInputEvent::MouseButton {
            button: xbutton_index(hook.mouseData),
            pressed: false,
        }),
        WM_MOUSEWHEEL => Some(CapturedInputEvent::MouseWheel {
            delta_x: 0,
            delta_y: wheel_delta(hook.mouseData),
        }),
        WM_MOUSEHWHEEL => Some(CapturedInputEvent::MouseWheel {
            delta_x: wheel_delta(hook.mouseData),
            delta_y: 0,
        }),
        _ => None,
    }
}

fn keyboard_message_to_event(message: u32, hook: &KBDLLHOOKSTRUCT) -> Option<CapturedInputEvent> {
    let extended = (hook.flags & 0x01) != 0;
    let hid = if hook.vkCode == VK_PAUSE as u32 {
        0x48
    } else if let Some(modifier_hid) = vk_to_modifier_hid(hook.vkCode, extended) {
        modifier_hid
    } else if let Some(media_hid) = vk_to_media_hid(hook.vkCode) {
        media_hid
    } else {
        let scancode = if hook.scanCode != 0 {
            hook.scanCode
        } else {
            // SAFETY: MapVirtualKeyW has no memory safety preconditions.
            unsafe {
                windows_sys::Win32::UI::Input::KeyboardAndMouse::MapVirtualKeyW(hook.vkCode, 0)
            }
        };
        windows_scancode_to_hid(scancode, extended)?
    };

    match message {
        WM_KEYDOWN | WM_SYSKEYDOWN => Some(CapturedInputEvent::KeyPress { keycode: hid }),
        WM_KEYUP | WM_SYSKEYUP => Some(CapturedInputEvent::KeyRelease { keycode: hid }),
        _ => None,
    }
}

fn vk_to_media_hid(vk: u32) -> Option<u32> {
    match vk as u16 {
        0x15 => Some(0x90),
        0x19 => Some(0x91),
        0x1C => Some(0x8A),
        0x1D => Some(0x8B),
        0x5F => Some(0x82),
        0xA6 => Some(0x224),
        0xA7 => Some(0x225),
        0xA8 => Some(0x227),
        0xAA => Some(0x221),
        0xAB => Some(0x182),
        0xAC => Some(0x223),
        0xAD => Some(0x7F), // VK_VOLUME_MUTE
        0xAE => Some(0x81), // VK_VOLUME_DOWN
        0xAF => Some(0x80), // VK_VOLUME_UP
        0xB0 => Some(0xB5), // VK_MEDIA_NEXT_TRACK
        0xB1 => Some(0xB6), // VK_MEDIA_PREV_TRACK
        0xB2 => Some(0xB7), // VK_MEDIA_STOP
        0xB3 => Some(0xCD), // VK_MEDIA_PLAY_PAUSE
        0xB4 => Some(0x18A),
        0xB5 => Some(0x193),
        0xB6 => Some(0x194),
        0xB7 => Some(0x192),
        0xF3 | 0xF4 => Some(0x94),
        _ => None,
    }
}

fn vk_to_modifier_hid(vk: u32, extended: bool) -> Option<u32> {
    match vk as u16 {
        0xA0 => Some(0xE1), // VK_LSHIFT
        0xA1 => Some(0xE5), // VK_RSHIFT
        0xA2 => Some(0xE0), // VK_LCONTROL
        0xA3 => Some(0xE4), // VK_RCONTROL
        0xA4 => Some(0xE2), // VK_LMENU (Left Alt)
        0xA5 => Some(0xE6), // VK_RMENU (Right Alt)
        0x5B => Some(0xE3), // VK_LWIN
        0x5C => Some(0xE7), // VK_RWIN
        VK_SHIFT => {
            if extended {
                Some(0xE5)
            } else {
                Some(0xE1)
            }
        }
        VK_CONTROL => {
            if extended {
                Some(0xE4)
            } else {
                Some(0xE0)
            }
        }
        VK_MENU => {
            if extended {
                Some(0xE6)
            } else {
                Some(0xE2)
            }
        }
        _ => None,
    }
}

fn windows_scancode_to_hid(scancode: u32, extended: bool) -> Option<u32> {
    match (scancode, extended) {
        (0x01, false) => Some(0x29), // Esc
        (0x02, false) => Some(0x1E), // 1
        (0x03, false) => Some(0x1F), // 2
        (0x04, false) => Some(0x20), // 3
        (0x05, false) => Some(0x21), // 4
        (0x06, false) => Some(0x22), // 5
        (0x07, false) => Some(0x23), // 6
        (0x08, false) => Some(0x24), // 7
        (0x09, false) => Some(0x25), // 8
        (0x0A, false) => Some(0x26), // 9
        (0x0B, false) => Some(0x27), // 0
        (0x0C, false) => Some(0x2D), // -
        (0x0D, false) => Some(0x2E), // =
        (0x0E, false) => Some(0x2A), // Backspace
        (0x0F, false) => Some(0x2B), // Tab
        (0x10, false) => Some(0x14), // Q
        (0x10, true) => Some(0xB6),  // Media Prev Track
        (0x11, false) => Some(0x1A), // W
        (0x12, false) => Some(0x08), // E
        (0x13, false) => Some(0x15), // R
        (0x14, false) => Some(0x17), // T
        (0x15, false) => Some(0x1C), // Y
        (0x16, false) => Some(0x18), // U
        (0x17, false) => Some(0x0C), // I
        (0x18, false) => Some(0x12), // O
        (0x19, false) => Some(0x13), // P
        (0x19, true) => Some(0xB5),  // Media Next Track
        (0x1A, false) => Some(0x2F), // [
        (0x1B, false) => Some(0x30), // ]
        (0x1C, false) => Some(0x28), // Enter
        (0x1C, true) => Some(0x58),  // Keypad Enter
        (0x1D, false) => Some(0xE0), // Left Ctrl
        (0x1D, true) => Some(0xE4),  // Right Ctrl
        (0x1E, false) => Some(0x04), // A
        (0x1F, false) => Some(0x16), // S
        (0x20, false) => Some(0x07), // D
        (0x20, true) => Some(0x7F),  // Mute
        (0x21, false) => Some(0x09), // F
        (0x21, true) => Some(0x192),
        (0x22, false) => Some(0x0A), // G
        (0x22, true) => Some(0xCD),  // Media Play/Pause
        (0x23, false) => Some(0x0B), // H
        (0x24, false) => Some(0x0D), // J
        (0x24, true) => Some(0xB7),  // Media Stop
        (0x25, false) => Some(0x0E), // K
        (0x26, false) => Some(0x0F), // L
        (0x27, false) => Some(0x33), // ;
        (0x28, false) => Some(0x34), // '
        (0x29, false) => Some(0x35), // `
        (0x2A, false) => Some(0xE1), // Left Shift
        (0x2B, false) => Some(0x31), // Backslash
        (0x2C, false) => Some(0x1D), // Z
        (0x2D, false) => Some(0x1B), // X
        (0x2E, false) => Some(0x06), // C
        (0x2E, true) => Some(0x81),  // Volume Down
        (0x2F, false) => Some(0x19), // V
        (0x30, false) => Some(0x05), // B
        (0x30, true) => Some(0x80),  // Volume Up
        (0x31, false) => Some(0x11), // N
        (0x32, false) => Some(0x10), // M
        (0x32, true) => Some(0x223),
        (0x33, false) => Some(0x36), // ,
        (0x34, false) => Some(0x37), // .
        (0x35, false) => Some(0x38), // /
        (0x35, true) => Some(0x54),  // Keypad /
        (0x36, false) => Some(0xE5), // Right Shift
        (0x37, false) => Some(0x55), // Keypad *
        (0x37, true) => Some(0x46),  // PrintScreen
        (0x38, false) => Some(0xE2), // Left Alt
        (0x38, true) => Some(0xE6),  // Right Alt
        (0x39, false) => Some(0x2C), // Space
        (0x3A, false) => Some(0x39), // Caps Lock
        (0x3B, false) => Some(0x3A), // F1
        (0x3C, false) => Some(0x3B), // F2
        (0x3D, false) => Some(0x3C), // F3
        (0x3E, false) => Some(0x3D), // F4
        (0x3F, false) => Some(0x3E), // F5
        (0x40, false) => Some(0x3F), // F6
        (0x41, false) => Some(0x40), // F7
        (0x42, false) => Some(0x41), // F8
        (0x43, false) => Some(0x42), // F9
        (0x44, false) => Some(0x43), // F10
        (0x45, false) => Some(0x53), // Num Lock
        (0x46, false) => Some(0x47), // Scroll Lock
        (0x47, false) => Some(0x5F), // Keypad 7
        (0x47, true) => Some(0x4A),  // Home
        (0x48, false) => Some(0x60), // Keypad 8
        (0x48, true) => Some(0x52),  // Up
        (0x49, false) => Some(0x61), // Keypad 9
        (0x49, true) => Some(0x4B),  // Page Up
        (0x4A, false) => Some(0x56), // Keypad -
        (0x4B, false) => Some(0x5C), // Keypad 4
        (0x4B, true) => Some(0x50),  // Left
        (0x4C, false) => Some(0x5D), // Keypad 5
        (0x4D, false) => Some(0x5E), // Keypad 6
        (0x4D, true) => Some(0x4F),  // Right
        (0x4E, false) => Some(0x57), // Keypad +
        (0x4F, false) => Some(0x59), // Keypad 1
        (0x4F, true) => Some(0x4D),  // End
        (0x50, false) => Some(0x5A), // Keypad 2
        (0x50, true) => Some(0x51),  // Down
        (0x51, false) => Some(0x5B), // Keypad 3
        (0x51, true) => Some(0x4E),  // Page Down
        (0x52, false) => Some(0x62), // Keypad 0
        (0x52, true) => Some(0x49),  // Insert
        (0x53, false) => Some(0x63), // Keypad .
        (0x53, true) => Some(0x4C),  // Delete
        (0x56, false) => Some(0x64), // Non-US \ / ISO
        (0x57, false) => Some(0x44), // F11
        (0x58, false) => Some(0x45), // F12
        (0x59, false) => Some(0x67), // Keypad =
        (0x5B, true) => Some(0xE3),  // Left GUI
        (0x5C, true) => Some(0xE7),  // Right GUI
        (0x5D, true) => Some(0x65),  // Menu
        (0x5F, true) => Some(0x82),
        (0x64, false) => Some(0x68), // F13
        (0x65, false) => Some(0x69), // F14
        (0x65, true) => Some(0x221),
        (0x66, false) => Some(0x6A), // F15
        (0x66, true) => Some(0x182),
        (0x67, false) => Some(0x6B), // F16
        (0x67, true) => Some(0x227),
        (0x68, false) => Some(0x6C), // F17
        (0x69, false) => Some(0x6D), // F18
        (0x69, true) => Some(0x225),
        (0x6A, false) => Some(0x6E), // F19
        (0x6A, true) => Some(0x224),
        (0x6B, false) => Some(0x6F), // F20
        (0x6B, true) => Some(0x194),
        (0x6C, false) => Some(0x70), // F21
        (0x6C, true) => Some(0x18A),
        (0x6D, false) => Some(0x71), // F22
        (0x6D, true) => Some(0x193),
        (0x6E, false) => Some(0x72), // F23
        (0x70, false) => Some(0x88),
        (0x73, false) => Some(0x87),
        (0x76, false) => Some(0x73), // F24
        (0x79, false) => Some(0x8A),
        (0x7B, false) => Some(0x8B),
        (0x7D, false) => Some(0x89),
        (0x7E, false) => Some(0x85), // Keypad ,
        (0xF1, false) => Some(0x91),
        (0xF2, false) => Some(0x90),
        _ => None,
    }
}

fn xbutton_index(mouse_data: u32) -> u8 {
    let hi = hiword(mouse_data);
    if hi as u32 == XBUTTON1 { 3 } else { 4 }
}

fn wheel_delta(mouse_data: u32) -> i32 {
    i16::from_ne_bytes(hiword(mouse_data).to_ne_bytes()) as i32
}

fn hiword(value: u32) -> u16 {
    (value >> (size_of::<u16>() * 8)) as u16
}

#[cfg(test)]
mod tests {
    use super::{
        KeyboardEventSignature, is_lock_key, keyboard_message_to_event, raw_keyboard_event,
        raw_mouse_delta, recent_keyboard_events, record_keyboard_event, should_deliver_locally,
        vk_to_media_hid, vk_to_modifier_hid, windows_scancode_to_hid,
    };
    use crate::platform::{CapturePolicy, CapturedInputEvent};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU8, Ordering};
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
        VK_CAPITAL, VK_CONTROL, VK_MENU, VK_NUMLOCK, VK_SCROLL, VK_SHIFT,
    };
    use windows_sys::Win32::UI::Input::{MOUSE_MOVE_RELATIVE, RAWKEYBOARD, RAWMOUSE, RAWMOUSE_0};
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        KBDLLHOOKSTRUCT, RI_KEY_E0, WM_KEYDOWN, WM_KEYUP,
    };

    #[test]
    fn raw_keyboard_converts_keys_and_modifiers() {
        let key_press = RAWKEYBOARD {
            MakeCode: 0x1E,
            Flags: 0,
            Reserved: 0,
            VKey: 0x41,
            Message: WM_KEYDOWN,
            ExtraInformation: 0,
        };
        assert!(matches!(
            raw_keyboard_event(&key_press),
            Some(CapturedInputEvent::KeyPress { keycode: 0x04 })
        ));

        let right_control_release = RAWKEYBOARD {
            MakeCode: 0x1D,
            Flags: RI_KEY_E0 as u16,
            Reserved: 0,
            VKey: VK_CONTROL,
            Message: WM_KEYUP,
            ExtraInformation: 0,
        };
        assert!(matches!(
            raw_keyboard_event(&right_control_release),
            Some(CapturedInputEvent::KeyRelease { keycode: 0xE4 })
        ));
    }

    #[test]
    fn keyboard_event_dedup_drops_subsequent_within_window() {
        recent_keyboard_events()
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clear();
        let signature = KeyboardEventSignature {
            keycode: 0x04,
            pressed: true,
        };

        assert!(record_keyboard_event(signature));
        assert!(!record_keyboard_event(signature));
    }

    #[test]
    fn lock_keys_are_recognized_for_local_passthrough() {
        assert!(is_lock_key(VK_CAPITAL));
        assert!(is_lock_key(VK_NUMLOCK));
        assert!(is_lock_key(VK_SCROLL));
        assert!(!is_lock_key(VK_SHIFT));
    }

    #[test]
    fn keyboard_message_to_event_converts_keys_and_modifiers() {
        let key_press = KBDLLHOOKSTRUCT {
            vkCode: 0x41,
            scanCode: 0x1E,
            flags: 0,
            time: 0,
            dwExtraInfo: 0,
        };
        assert!(matches!(
            keyboard_message_to_event(WM_KEYDOWN, &key_press),
            Some(CapturedInputEvent::KeyPress { keycode: 0x04 })
        ));

        let right_control_release = KBDLLHOOKSTRUCT {
            vkCode: VK_CONTROL as u32,
            scanCode: 0x1D,
            flags: 0x01,
            time: 0,
            dwExtraInfo: 0,
        };
        assert!(matches!(
            keyboard_message_to_event(WM_KEYUP, &right_control_release),
            Some(CapturedInputEvent::KeyRelease { keycode: 0xE4 })
        ));
    }

    #[test]
    fn vk_to_modifier_hid_maps_all_modifiers() {
        assert_eq!(vk_to_modifier_hid(0xA0, false), Some(0xE1)); // VK_LSHIFT
        assert_eq!(vk_to_modifier_hid(0xA1, false), Some(0xE5)); // VK_RSHIFT
        assert_eq!(vk_to_modifier_hid(0xA2, false), Some(0xE0)); // VK_LCONTROL
        assert_eq!(vk_to_modifier_hid(0xA3, false), Some(0xE4)); // VK_RCONTROL
        assert_eq!(vk_to_modifier_hid(0xA4, false), Some(0xE2)); // VK_LMENU
        assert_eq!(vk_to_modifier_hid(0xA5, false), Some(0xE6)); // VK_RMENU
        assert_eq!(vk_to_modifier_hid(0x5B, false), Some(0xE3)); // VK_LWIN
        assert_eq!(vk_to_modifier_hid(0x5C, false), Some(0xE7)); // VK_RWIN
        assert_eq!(vk_to_modifier_hid(VK_SHIFT as u32, false), Some(0xE1));
        assert_eq!(vk_to_modifier_hid(VK_SHIFT as u32, true), Some(0xE5));
        assert_eq!(vk_to_modifier_hid(VK_CONTROL as u32, false), Some(0xE0));
        assert_eq!(vk_to_modifier_hid(VK_CONTROL as u32, true), Some(0xE4));
        assert_eq!(vk_to_modifier_hid(VK_MENU as u32, false), Some(0xE2));
        assert_eq!(vk_to_modifier_hid(VK_MENU as u32, true), Some(0xE6));
        assert_eq!(vk_to_modifier_hid(0x20, false), None); // VK_SPACE
    }

    #[test]
    fn vk_to_media_hid_maps_media_keys() {
        assert_eq!(vk_to_media_hid(0xAD), Some(0x7F)); // Mute
        assert_eq!(vk_to_media_hid(0xAE), Some(0x81)); // Vol Down
        assert_eq!(vk_to_media_hid(0xAF), Some(0x80)); // Vol Up
        assert_eq!(vk_to_media_hid(0xB0), Some(0xB5)); // Next Track
        assert_eq!(vk_to_media_hid(0xB1), Some(0xB6)); // Prev Track
        assert_eq!(vk_to_media_hid(0xB2), Some(0xB7)); // Stop
        assert_eq!(vk_to_media_hid(0xB3), Some(0xCD)); // Play/Pause
        assert_eq!(vk_to_media_hid(0x15), Some(0x90));
        assert_eq!(vk_to_media_hid(0x19), Some(0x91));
        assert_eq!(vk_to_media_hid(0x1C), Some(0x8A));
        assert_eq!(vk_to_media_hid(0x1D), Some(0x8B));
        assert_eq!(vk_to_media_hid(0x5F), Some(0x82));
        assert_eq!(vk_to_media_hid(0xA6), Some(0x224));
        assert_eq!(vk_to_media_hid(0xA7), Some(0x225));
        assert_eq!(vk_to_media_hid(0xA8), Some(0x227));
        assert_eq!(vk_to_media_hid(0xAA), Some(0x221));
        assert_eq!(vk_to_media_hid(0xAB), Some(0x182));
        assert_eq!(vk_to_media_hid(0xAC), Some(0x223));
        assert_eq!(vk_to_media_hid(0xB4), Some(0x18A));
        assert_eq!(vk_to_media_hid(0xB5), Some(0x193));
        assert_eq!(vk_to_media_hid(0xB6), Some(0x194));
        assert_eq!(vk_to_media_hid(0xB7), Some(0x192));
        assert_eq!(vk_to_media_hid(0xF3), Some(0x94));
        assert_eq!(vk_to_media_hid(0xF4), Some(0x94));
        assert_eq!(vk_to_media_hid(0x41), None);
    }

    #[test]
    fn windows_scancode_to_hid_maps_standard_keys() {
        assert_eq!(windows_scancode_to_hid(0x1E, false), Some(0x04));
        assert_eq!(windows_scancode_to_hid(0x1C, false), Some(0x28));
        assert_eq!(windows_scancode_to_hid(0x39, false), Some(0x2C));
        assert_eq!(windows_scancode_to_hid(0x56, false), Some(0x64));
        assert_eq!(windows_scancode_to_hid(0x59, false), Some(0x67)); // Keypad =
        assert_eq!(windows_scancode_to_hid(0x64, false), Some(0x68)); // F13
        assert_eq!(windows_scancode_to_hid(0x70, false), Some(0x88));
        assert_eq!(windows_scancode_to_hid(0x73, false), Some(0x87));
        assert_eq!(windows_scancode_to_hid(0x79, false), Some(0x8A));
        assert_eq!(windows_scancode_to_hid(0x7B, false), Some(0x8B));
        assert_eq!(windows_scancode_to_hid(0x7D, false), Some(0x89));
        assert_eq!(windows_scancode_to_hid(0x7E, false), Some(0x85)); // Keypad ,
        assert_eq!(windows_scancode_to_hid(0xF1, false), Some(0x91));
        assert_eq!(windows_scancode_to_hid(0xF2, false), Some(0x90));
    }

    #[test]
    fn windows_scancode_to_hid_maps_extended_keys() {
        assert_eq!(windows_scancode_to_hid(0x1D, true), Some(0xE4));
        assert_eq!(windows_scancode_to_hid(0x38, true), Some(0xE6));
        assert_eq!(windows_scancode_to_hid(0x4B, true), Some(0x50));
        assert_eq!(windows_scancode_to_hid(0x1C, true), Some(0x58));
        assert_eq!(windows_scancode_to_hid(0x37, true), Some(0x46));
        assert_eq!(windows_scancode_to_hid(0x20, true), Some(0x7F)); // Mute
        assert_eq!(windows_scancode_to_hid(0x30, true), Some(0x80)); // Volume Up
        assert_eq!(windows_scancode_to_hid(0x2E, true), Some(0x81)); // Volume Down
        assert_eq!(windows_scancode_to_hid(0x22, true), Some(0xCD)); // Play/Pause
        assert_eq!(windows_scancode_to_hid(0x21, true), Some(0x192));
        assert_eq!(windows_scancode_to_hid(0x5F, true), Some(0x82));
        assert_eq!(windows_scancode_to_hid(0x65, true), Some(0x221));
        assert_eq!(windows_scancode_to_hid(0x66, true), Some(0x182));
        assert_eq!(windows_scancode_to_hid(0x6A, true), Some(0x224));
        assert_eq!(windows_scancode_to_hid(0x69, true), Some(0x225));
        assert_eq!(windows_scancode_to_hid(0x6B, true), Some(0x194));
        assert_eq!(windows_scancode_to_hid(0x6D, true), Some(0x193));
    }

    #[test]
    fn windows_scancode_to_hid_returns_none_for_unmapped_key() {
        assert_eq!(windows_scancode_to_hid(0x00, false), None);
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
    fn raw_mouse_reports_relative_deltas() {
        let mouse = RAWMOUSE {
            usFlags: MOUSE_MOVE_RELATIVE,
            Anonymous: RAWMOUSE_0 { ulButtons: 0 },
            ulRawButtons: 0,
            lLastX: 6,
            lLastY: -3,
            ulExtraInformation: 0,
        };

        assert_eq!(raw_mouse_delta(&mouse), Some((6, -3)));
    }
}
