use std::mem::size_of;
use std::sync::atomic::{AtomicIsize, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use anyhow::{Result, anyhow, bail};
use windows_sys::Win32::Foundation::{
    CloseHandle, FALSE, HANDLE, INVALID_HANDLE_VALUE, LocalFree,
};
use windows_sys::Win32::Security::Authorization::{
    ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
};
use windows_sys::Win32::Security::SECURITY_ATTRIBUTES;
use windows_sys::Win32::Security::{
    GetLengthSid, GetTokenInformation, PSID, TOKEN_QUERY, TOKEN_USER, TokenUser,
};
use windows_sys::Win32::Storage::FileSystem::{PIPE_ACCESS_DUPLEX, ReadFile};
use windows_sys::Win32::System::Pipes::{
    ConnectNamedPipe, CreateNamedPipeW, DisconnectNamedPipe, GetNamedPipeClientProcessId,
    PIPE_READMODE_MESSAGE, PIPE_TYPE_MESSAGE, PIPE_WAIT,
};
use windows_sys::Win32::System::Services::{
    RegisterServiceCtrlHandlerExW, SERVICE_ACCEPT_STOP, SERVICE_CONTROL_STOP, SERVICE_RUNNING,
    SERVICE_STATUS, SERVICE_STATUS_HANDLE, SERVICE_STOP_PENDING, SERVICE_STOPPED,
    SERVICE_TABLE_ENTRYW, SERVICE_WIN32_OWN_PROCESS, SetServiceStatus, StartServiceCtrlDispatcherW,
};
use windows_sys::Win32::Security::RevertToSelf;
use windows_sys::Win32::System::StationsAndDesktops::{
    CloseDesktop, CloseWindowStation, OpenInputDesktop, SetProcessWindowStation, SetThreadDesktop,
    HDESK, HWINSTA,
};
use windows_sys::Win32::System::Threading::{
    OpenProcess, OpenProcessToken, PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows_sys::core::{PCWSTR, PWSTR};

use super::protocol::{
    Envelope, InputOp, MAX_PIPE_INSTANCES, SERVICE_NAME, decode_frame, pipe_name,
    pipe_security_descriptor,
};
use crate::core::platform::traits::EventInjection;
use crate::platform::windows::injection::WindowsEventInjection;

static SHUTDOWN_REQUESTED: AtomicIsize = AtomicIsize::new(0);
static STATUS_HANDLE: AtomicIsize = AtomicIsize::new(0);

/// Sampled console-user SID and the instant it was sampled.
type ConsoleSidCache = Mutex<Option<(std::time::Instant, Option<Vec<u8>>)>>;

pub fn run_service_main() -> Result<()> {
    let name = super::wide_null(SERVICE_NAME);
    let table = [
        SERVICE_TABLE_ENTRYW {
            lpServiceName: name.as_ptr() as PWSTR,
            lpServiceProc: Some(service_main),
        },
        SERVICE_TABLE_ENTRYW {
            lpServiceName: std::ptr::null_mut(),
            lpServiceProc: None,
        },
    ];
    let started = unsafe { StartServiceCtrlDispatcherW(table.as_ptr()) };
    if started == 0 {
        bail!("service dispatcher refused startup; run without --input-service in a user session");
    }
    Ok(())
}

pub fn run_service_standalone() -> Result<()> {
    report_state(SERVICE_RUNNING);
    if let Err(error) = ctrlc::set_handler(|| {
        request_stop();
    }) {
        return Err(anyhow!("standalone service shutdown handler failed: {error}"));
    }
    run_pipe_loop();
    Ok(())
}

unsafe extern "system" fn service_main(_argc: u32, _argv: *mut PWSTR) {
    let name = super::wide_null(SERVICE_NAME);
    let handle =
        unsafe { RegisterServiceCtrlHandlerExW(name.as_ptr(), Some(control_handler), std::ptr::null()) };
    if handle.is_null() {
        return;
    }
    STATUS_HANDLE.store(handle as isize, Ordering::SeqCst);
    report_state(SERVICE_RUNNING);
    run_pipe_loop();
    report_state(SERVICE_STOPPED);
}

unsafe extern "system" fn control_handler(
    control: u32,
    _event: u32,
    _data: *mut core::ffi::c_void,
    _context: *mut core::ffi::c_void,
) -> u32 {
    if control == SERVICE_CONTROL_STOP {
        report_state(SERVICE_STOP_PENDING);
        request_stop();
    }
    0
}

fn report_state(state: u32) {
    let handle = STATUS_HANDLE.load(Ordering::SeqCst) as SERVICE_STATUS_HANDLE;
    if handle.is_null() {
        return;
    }
    let status = SERVICE_STATUS {
        dwServiceType: SERVICE_WIN32_OWN_PROCESS,
        dwCurrentState: state,
        dwControlsAccepted: if state == SERVICE_RUNNING {
            SERVICE_ACCEPT_STOP
        } else {
            0
        },
        dwWin32ExitCode: 0,
        dwServiceSpecificExitCode: 0,
        dwCheckPoint: 0,
        dwWaitHint: 0,
    };
    unsafe {
        SetServiceStatus(handle, &status);
    }
}

fn request_stop() {
    SHUTDOWN_REQUESTED.store(1, Ordering::SeqCst);
    poke_pipe();
}

fn stopped() -> bool {
    SHUTDOWN_REQUESTED.load(Ordering::SeqCst) != 0
}

fn poke_pipe() {
    use windows_sys::Win32::Storage::FileSystem::{
        CreateFileW, FILE_ATTRIBUTE_NORMAL, OPEN_EXISTING,
    };
    use windows_sys::Win32::System::Pipes::WaitNamedPipeW;

    let pipe_name = pipe_name();
    let name = super::wide_null(&pipe_name);
    unsafe {
        if WaitNamedPipeW(name.as_ptr(), 500) == 0 {
            return;
        }
        let handle = CreateFileW(
            name.as_ptr(),
            0,
            0,
            std::ptr::null(),
            OPEN_EXISTING,
            FILE_ATTRIBUTE_NORMAL,
            std::ptr::null_mut(),
        );
        if handle != INVALID_HANDLE_VALUE {
            CloseHandle(handle);
        }
    }
}

fn run_pipe_loop() {
    while !stopped() {
        // Bind the pipe to whoever owns the physical console session right now.
        // Re-resolved every iteration so fast user switching picks up the new
        // owner instead of keeping the previous user's access.
        let session_sid = match active_console_user_sid() {
            Some(sid) => sid,
            None => {
                std::thread::sleep(Duration::from_millis(500));
                continue;
            }
        };
        let listener = match sid_to_string(&session_sid) {
            Some(sid_text) => create_listener(&sid_text),
            None => {
                std::thread::sleep(Duration::from_millis(500));
                continue;
            }
        };
        let handle = match listener {
            Ok(handle) => handle,
            Err(_) => {
                std::thread::sleep(Duration::from_millis(500));
                continue;
            }
        };
        let connected = unsafe { ConnectNamedPipe(handle, std::ptr::null_mut()) };
        if connected == 0
            && unsafe { windows_sys::Win32::Foundation::GetLastError() }
                != windows_sys::Win32::Foundation::ERROR_PIPE_CONNECTED
        {
            unsafe {
                CloseHandle(handle);
            }
            continue;
        }
        if stopped() {
            unsafe {
                DisconnectNamedPipe(handle);
                CloseHandle(handle);
            }
            break;
        }
        let connection = ConnectionHandle(handle);
        std::thread::spawn(move || {
            serve_connection(connection, &session_sid);
        });
    }
}

fn create_listener(session_sid: &str) -> Result<HANDLE> {
    create_listener_named(&pipe_name(), session_sid)
}

fn create_listener_named(name: &str, session_sid: &str) -> Result<HANDLE> {
    let pipe_sddl = super::wide_null(&pipe_security_descriptor(session_sid));
    let mut descriptor: *mut core::ffi::c_void = std::ptr::null_mut();
    let mut descriptor_size = 0u32;
    let converted = unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            pipe_sddl.as_ptr(),
            SDDL_REVISION_1,
            &mut descriptor,
            &mut descriptor_size,
        )
    };
    if converted == 0 {
        bail!("pipe security descriptor failed");
    }
    let attributes = SECURITY_ATTRIBUTES {
        nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: descriptor,
        bInheritHandle: FALSE,
    };
    let name = super::wide_null(name);
    let handle = unsafe {
        CreateNamedPipeW(
            name.as_ptr() as PCWSTR,
            PIPE_ACCESS_DUPLEX,
            PIPE_TYPE_MESSAGE | PIPE_READMODE_MESSAGE | PIPE_WAIT,
            MAX_PIPE_INSTANCES,
            65536,
            65536,
            0,
            &attributes,
        )
    };
    unsafe {
        LocalFree(descriptor);
    }
    if handle == INVALID_HANDLE_VALUE {
        bail!("named pipe creation failed");
    }
    Ok(handle)
}

fn serve_connection(connection: ConnectionHandle, session_sid: &[u8]) {
    let handle = connection.0;
    let _guard = PipeGuard(handle);
    // Defense in depth: the DACL already limits who can open the pipe, but the
    // DACL alone would still trust anything that got a handle. Refuse any
    // client whose process token does not belong to the session user.
    if !caller_is_session_user(handle, session_sid) {
        return;
    }
    let mut cached: Option<(u64, WindowsEventInjection)> = None;
    let mut buffer = vec![0u8; 65536];
    loop {
        if stopped() {
            break;
        }
        let mut read = 0u32;
        let ok = unsafe {
            ReadFile(
                handle,
                buffer.as_mut_ptr(),
                buffer.len() as u32,
                &mut read,
                std::ptr::null_mut(),
            )
        };
        if ok == 0 || read == 0 {
            break;
        }
        // Fast user switching can make a listener that was created for the
        // previous console user reachable while someone else owns the session.
        // Stop before injecting into the new user's desktop.
        if !session_still_current(session_sid) {
            break;
        }
        let mut offset = 0usize;
        while offset < read as usize {
            let (envelope, consumed) = match decode_frame(&buffer[offset..read as usize]) {
                Ok(frame) => frame,
                Err(_) => break,
            };
            offset += consumed;
            dispatch(&mut cached, &envelope);
        }
    }
}

fn dispatch(cached: &mut Option<(u64, WindowsEventInjection)>, envelope: &Envelope) {
    let needs_new = !matches!(cached, Some((marker, _)) if *marker == envelope.marker);
    if needs_new {
        *cached = Some((
            envelope.marker,
            WindowsEventInjection::with_marker(envelope.marker as usize),
        ));
    }
    let injection = match cached {
        Some((_, injection)) => injection,
        None => return,
    };
    let _service_order = service_inject_serial()
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let _user_desktop = attach_user_input_desktop();
    let _desktop = if _user_desktop.is_none() {
        attach_input_desktop()
    } else {
        None
    };
    let _ = match &envelope.op {
        InputOp::MouseMove { x, y } => injection.inject_mouse_move(*x, *y),
        InputOp::MouseMoveRelative { dx, dy } => injection.inject_mouse_move_relative(*dx, *dy),
        InputOp::MouseButton { button, pressed } => {
            injection.inject_mouse_button(*button, *pressed)
        }
        InputOp::MouseWheel { delta_x, delta_y } => {
            injection.inject_mouse_wheel(*delta_x, *delta_y)
        }
        InputOp::KeyPress { keycode } => injection.inject_key_press(*keycode),
        InputOp::KeyRelease { keycode } => injection.inject_key_release(*keycode),
        InputOp::ReleaseAll => injection.release_all_keys(),
        InputOp::Ping => Ok(()),
    };
}

/// Reads the user SID out of an access token.
fn token_user_sid(token: HANDLE) -> Option<Vec<u8>> {
    let mut needed = 0u32;
    unsafe {
        GetTokenInformation(token, TokenUser, std::ptr::null_mut(), 0, &mut needed);
    }
    if needed == 0 {
        return None;
    }
    // TOKEN_USER holds pointers, so the buffer must be pointer aligned.
    let words = (needed as usize).div_ceil(size_of::<usize>());
    let mut buffer = vec![0usize; words];
    let ok = unsafe {
        GetTokenInformation(
            token,
            TokenUser,
            buffer.as_mut_ptr() as *mut core::ffi::c_void,
            needed,
            &mut needed,
        )
    };
    if ok == 0 {
        return None;
    }
    let user = unsafe { &*(buffer.as_ptr() as *const TOKEN_USER) };
    let sid = user.User.Sid;
    if sid.is_null() {
        return None;
    }
    let length = unsafe { GetLengthSid(sid) } as usize;
    if length == 0 {
        return None;
    }
    Some(unsafe { std::slice::from_raw_parts(sid as *const u8, length) }.to_vec())
}

/// SID of the user logged into the physical console session, if any.
fn active_console_user_sid() -> Option<Vec<u8>> {
    use windows_sys::Win32::System::RemoteDesktop::{
        WTSGetActiveConsoleSessionId, WTSQueryUserToken,
    };

    unsafe {
        let session = WTSGetActiveConsoleSessionId();
        if session == 0xFFFF_FFFF {
            return None;
        }
        let mut token: HANDLE = std::ptr::null_mut();
        if WTSQueryUserToken(session, &mut token) == 0 || token.is_null() {
            return None;
        }
        let sid = token_user_sid(token);
        CloseHandle(token);
        sid
    }
}

/// SID of the account that owns a process.
fn process_user_sid(process_id: u32) -> Option<Vec<u8>> {
    unsafe {
        let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, FALSE, process_id);
        if process.is_null() {
            return None;
        }
        let mut token: HANDLE = std::ptr::null_mut();
        if OpenProcessToken(process, TOKEN_QUERY, &mut token) == 0 || token.is_null() {
            CloseHandle(process);
            return None;
        }
        let sid = token_user_sid(token);
        CloseHandle(token);
        CloseHandle(process);
        sid
    }
}

fn sid_to_string(sid: &[u8]) -> Option<String> {
    let mut text: PWSTR = std::ptr::null_mut();
    let ok = unsafe { ConvertSidToStringSidW(sid.as_ptr() as PSID, &mut text) };
    if ok == 0 || text.is_null() {
        return None;
    }
    let value = unsafe {
        let mut length = 0usize;
        while *text.add(length) != 0 {
            length += 1;
        }
        String::from_utf16_lossy(std::slice::from_raw_parts(text, length))
    };
    unsafe {
        LocalFree(text as *mut core::ffi::c_void);
    }
    Some(value)
}

/// True when the process on the far end of the pipe runs as the session user.
fn caller_is_session_user(handle: HANDLE, session_sid: &[u8]) -> bool {
    let mut process_id = 0u32;
    if unsafe { GetNamedPipeClientProcessId(handle, &mut process_id) } == 0 || process_id == 0 {
        return false;
    }
    match process_user_sid(process_id) {
        Some(caller_sid) => sids_equal(&caller_sid, session_sid),
        None => false,
    }
}

/// True while the console session is still owned by the user this connection
/// was accepted for. The console user is cached briefly so the check is cheap
/// on the input hot path.
fn session_still_current(expected_sid: &[u8]) -> bool {
    const CACHE_TTL: Duration = Duration::from_millis(500);

    static CACHE: OnceLock<ConsoleSidCache> = OnceLock::new();
    let cache = CACHE.get_or_init(|| Mutex::new(None));
    let mut guard = cache.lock().unwrap_or_else(|error| error.into_inner());
    let fresh = match guard.as_ref() {
        Some((checked_at, sid)) if checked_at.elapsed() < CACHE_TTL => Some(sid.clone()),
        _ => {
            let sid = active_console_user_sid();
            *guard = Some((std::time::Instant::now(), sid.clone()));
            Some(sid)
        }
    };
    match fresh {
        Some(Some(current_sid)) => sids_equal(&current_sid, expected_sid),
        _ => false,
    }
}

/// SIDs are fixed-layout byte strings, so identical bytes mean the same account.
fn sids_equal(left: &[u8], right: &[u8]) -> bool {
    !left.is_empty() && left == right
}

fn attach_input_desktop() -> Option<DesktopGuard> {
    use windows_sys::Win32::Foundation::GENERIC_ALL;

    let desktop = unsafe { OpenInputDesktop(0, FALSE, GENERIC_ALL) };
    if desktop.is_null() {
        return None;
    }
    if unsafe { SetThreadDesktop(desktop) } == 0 {
        unsafe {
            CloseDesktop(desktop);
        }
        return None;
    }
    Some(DesktopGuard(desktop))
}

static SERVICE_INJECT_SERIAL: OnceLock<Mutex<()>> = OnceLock::new();

fn service_inject_serial() -> &'static Mutex<()> {
    SERVICE_INJECT_SERIAL.get_or_init(|| Mutex::new(()))
}

struct UserSessionDesktop {
    input: HDESK,
    station: HWINSTA,
    previous_station: HWINSTA,
    token: HANDLE,
}

impl Drop for UserSessionDesktop {
    fn drop(&mut self) {
        unsafe {
            if !self.previous_station.is_null() {
                SetProcessWindowStation(self.previous_station);
            }
            CloseDesktop(self.input);
            CloseWindowStation(self.station);
            RevertToSelf();
            CloseHandle(self.token);
        }
    }
}

fn attach_user_input_desktop() -> Option<UserSessionDesktop> {
    use windows_sys::Win32::Foundation::GENERIC_ALL;
    use windows_sys::Win32::Security::{ImpersonateLoggedOnUser, RevertToSelf};
    use windows_sys::Win32::System::RemoteDesktop::{
        WTSGetActiveConsoleSessionId, WTSQueryUserToken,
    };
    use windows_sys::Win32::System::StationsAndDesktops::{
        CloseWindowStation, GetProcessWindowStation, OpenWindowStationW, SetProcessWindowStation,
    };

    unsafe {
        let session = WTSGetActiveConsoleSessionId();
        if session == 0xFFFF_FFFF {
            return None;
        }
        let mut token: HANDLE = std::ptr::null_mut();
        if WTSQueryUserToken(session, &mut token) == 0 || token.is_null() {
            return None;
        }
        if ImpersonateLoggedOnUser(token) == 0 {
            CloseHandle(token);
            return None;
        }
        let station_name = super::wide_null("WinSta0");
        let station = OpenWindowStationW(station_name.as_ptr(), FALSE, GENERIC_ALL);
        if station.is_null() {
            RevertToSelf();
            CloseHandle(token);
            return None;
        }
        let previous_station = GetProcessWindowStation();
        if SetProcessWindowStation(station) == 0 {
            CloseWindowStation(station);
            RevertToSelf();
            CloseHandle(token);
            return None;
        }
        let input = OpenInputDesktop(0, FALSE, GENERIC_ALL);
        if input.is_null() || SetThreadDesktop(input) == 0 {
            if !input.is_null() {
                CloseDesktop(input);
            }
            SetProcessWindowStation(previous_station);
            CloseWindowStation(station);
            RevertToSelf();
            CloseHandle(token);
            return None;
        }
        Some(UserSessionDesktop {
            input,
            station,
            previous_station,
            token,
        })
    }
}

struct ConnectionHandle(HANDLE);

unsafe impl Send for ConnectionHandle {}

struct DesktopGuard(HDESK);

impl Drop for DesktopGuard {
    fn drop(&mut self) {
        unsafe {
            CloseDesktop(self.0);
        }
    }
}

struct PipeGuard(HANDLE);

impl Drop for PipeGuard {
    fn drop(&mut self) {
        unsafe {
            DisconnectNamedPipe(self.0);
            CloseHandle(self.0);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering as AtomicOrdering};
    use windows_sys::Win32::Foundation::{GENERIC_READ, GENERIC_WRITE};
    use windows_sys::Win32::Storage::FileSystem::{
        CreateFileW, FILE_ATTRIBUTE_NORMAL, OPEN_EXISTING,
    };
    use windows_sys::Win32::System::Threading::GetCurrentProcessId;

    fn sid(identifier: u32) -> Vec<u8> {
        let mut bytes = vec![1u8, 2, 0, 0, 0, 0, 0, 5];
        bytes.extend_from_slice(&identifier.to_le_bytes());
        bytes
    }

    fn test_pipe_name(tag: &str) -> String {
        static NEXT: AtomicU32 = AtomicU32::new(0);
        let sequence = NEXT.fetch_add(1, AtomicOrdering::SeqCst);
        let process = unsafe { GetCurrentProcessId() };
        format!(r"\\.\pipe\zerog-input-test-{tag}-{process}-{sequence}")
    }

    fn connect_client(name: &str) -> Option<HANDLE> {
        let name = super::super::wide_null(name);
        let handle = unsafe {
            CreateFileW(
                name.as_ptr(),
                GENERIC_READ | GENERIC_WRITE,
                0,
                std::ptr::null(),
                OPEN_EXISTING,
                FILE_ATTRIBUTE_NORMAL,
                std::ptr::null_mut(),
            )
        };
        (handle != INVALID_HANDLE_VALUE).then_some(handle)
    }

    fn current_user_sid() -> Vec<u8> {
        process_user_sid(unsafe { GetCurrentProcessId() }).expect("current process token sid")
    }

    #[test]
    fn sid_comparison_matches_exact_accounts_only() {
        assert!(sids_equal(&sid(1001), &sid(1001)));
        assert!(!sids_equal(&sid(1001), &sid(1002)));
        assert!(!sids_equal(&sid(1001), &[]));
        assert!(!sids_equal(&[], &[]));
    }

    #[test]
    fn caller_check_accepts_session_user_and_rejects_others() {
        let session_sid = current_user_sid();
        let session_text = sid_to_string(&session_sid).expect("session sid text");
        let name = test_pipe_name("caller");
        let listener = create_listener_named(&name, &session_text).expect("listener");
        let client = connect_client(&name).expect("session user may connect");
        // The client side completed the connection; let the server observe it.
        unsafe { ConnectNamedPipe(listener, std::ptr::null_mut()) };

        assert!(caller_is_session_user(listener, &session_sid));
        assert!(!caller_is_session_user(listener, &sid(4242)));

        unsafe {
            CloseHandle(client);
            DisconnectNamedPipe(listener);
            CloseHandle(listener);
        }
    }

    #[test]
    fn pipe_descriptor_denies_processes_outside_the_acl() {
        // Only the built-in Guests group may use this pipe. The test process
        // runs as a different account, so the open must be refused.
        let name = test_pipe_name("deny");
        let listener = create_listener_named(&name, "S-1-5-32-546").expect("listener");
        assert!(
            connect_client(&name).is_none(),
            "a user outside the pipe DACL must not connect"
        );
        unsafe { CloseHandle(listener) };
    }

    #[test]
    fn session_guard_follows_the_live_console_user() {
        let Some(session_sid) = active_console_user_sid() else {
            return;
        };
        assert!(session_still_current(&session_sid));
        assert!(!session_still_current(&sid(4242)));
    }

    #[test]
    fn console_session_sid_produces_a_usable_pipe_descriptor() {
        // Headless hosts and CI have no interactive session; nothing to check.
        let Some(session_sid) = active_console_user_sid() else {
            return;
        };
        let sid_text = sid_to_string(&session_sid).expect("console sid text");
        assert!(
            sid_text.starts_with("S-1-"),
            "unexpected console sid text: {sid_text}"
        );
        let name = test_pipe_name("console");
        let listener = create_listener_named(&name, &sid_text).expect("listener for console user");
        if let Some(client) = connect_client(&name) {
            unsafe { CloseHandle(client) };
        }
        unsafe { CloseHandle(listener) };
    }
}
