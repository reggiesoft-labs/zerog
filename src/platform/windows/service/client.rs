use std::sync::Arc;
use std::time::Duration;

use anyhow::{Result, anyhow, bail};
use serde::{Deserialize, Serialize};

use crate::platform::EventInjection;

use super::protocol::{Envelope, InputOp, PROTOCOL_VERSION};

/// HID usage for Caps Lock; Windows toggles the system lock on this key event.
const HID_KEY_CAPS_LOCK: u32 = 0x39;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WindowsInputStatus {
    pub supported: bool,
    pub elevated: bool,
    pub service_installed: bool,
    pub service_running: bool,
    pub secure_desktop_enabled: Option<bool>,
    pub service_reachable: bool,
}

pub struct ServiceBackedInjection {
    fallback: Arc<dyn EventInjection>,
    marker: u64,
    #[cfg(target_os = "windows")]
    secure_probe: std::sync::Mutex<SecureProbe>,
    #[cfg(target_os = "windows")]
    pipe: std::sync::Mutex<PipeLink>,
}

#[cfg(target_os = "windows")]
struct SecureProbe {
    checked_at: Option<std::time::Instant>,
    secure_active: bool,
}

impl ServiceBackedInjection {
    pub fn new(fallback: Arc<dyn EventInjection>, marker: u64) -> Self {
        Self {
            fallback,
            marker,
            #[cfg(target_os = "windows")]
            secure_probe: std::sync::Mutex::new(SecureProbe {
                checked_at: None,
                secure_active: false,
            }),
            #[cfg(target_os = "windows")]
            pipe: std::sync::Mutex::new(PipeLink::new()),
        }
    }

    pub fn service_reachable(&self) -> bool {
        #[cfg(target_os = "windows")]
        {
            probe_service()
        }
        #[cfg(not(target_os = "windows"))]
        {
            false
        }
    }

    fn route(&self, op: InputOp, fallback: impl FnOnce() -> Result<()>) -> Result<()> {
        #[cfg(target_os = "windows")]
        {
            if self.use_privileged_service() {
                let envelope = Envelope {
                    version: PROTOCOL_VERSION,
                    marker: self.marker,
                    op,
                };
                match self.send(&envelope) {
                    Ok(()) => Ok(()),
                    Err(_) => fallback(),
                }
            } else {
                match fallback() {
                    Ok(()) => Ok(()),
                    Err(_) => {
                        let envelope = Envelope {
                            version: PROTOCOL_VERSION,
                            marker: self.marker,
                            op,
                        };
                        self.send(&envelope)
                    }
                }
            }
        }
        #[cfg(not(target_os = "windows"))]
        {
            let _ = (self.marker, op);
            fallback()
        }
    }

    #[cfg(target_os = "windows")]
    fn use_privileged_service(&self) -> bool {
        let mut probe = match self.secure_probe.lock() {
            Ok(probe) => probe,
            Err(_) => return false,
        };
        let now = std::time::Instant::now();
        if let Some(checked_at) = probe.checked_at
            && now.duration_since(checked_at) < Duration::from_millis(500)
        {
            return probe.secure_active;
        }
        let secure_active = secure_desktop_active();
        probe.secure_active = secure_active;
        probe.checked_at = Some(now);
        secure_active
    }

    #[cfg(target_os = "windows")]
    fn send(&self, envelope: &Envelope) -> Result<()> {
        use super::protocol::encode_frame;

        let frame = encode_frame(envelope)?;
        let mut link = self.pipe.lock().map_err(|_| anyhow!("input pipe lock failed"))?;
        if link.in_cooldown() {
            bail!("input service cooling down");
        }
        if !link.connected() {
            link.connect()?;
        }
        if link.write(&frame).is_err() {
            link.reconnect()?;
            link.write(&frame)?;
        }
        Ok(())
    }
}

impl EventInjection for ServiceBackedInjection {
    fn inject_mouse_move(&self, x: i32, y: i32) -> Result<()> {
        self.route(InputOp::MouseMove { x, y }, || {
            self.fallback.inject_mouse_move(x, y)
        })
    }

    fn inject_mouse_move_relative(&self, dx: i32, dy: i32) -> Result<()> {
        self.route(InputOp::MouseMoveRelative { dx, dy }, || {
            self.fallback.inject_mouse_move_relative(dx, dy)
        })
    }

    fn inject_mouse_button(&self, button: u8, pressed: bool) -> Result<()> {
        self.route(InputOp::MouseButton { button, pressed }, || {
            self.fallback.inject_mouse_button(button, pressed)
        })
    }

    fn inject_mouse_wheel(&self, delta_x: i32, delta_y: i32) -> Result<()> {
        self.route(
            InputOp::MouseWheel { delta_x, delta_y },
            || self.fallback.inject_mouse_wheel(delta_x, delta_y),
        )
    }

    fn inject_key_press(&self, keycode: u32) -> Result<()> {
        self.route(InputOp::KeyPress { keycode }, || {
            self.fallback.inject_key_press(keycode)
        })
    }

    fn inject_key_release(&self, keycode: u32) -> Result<()> {
        self.route(InputOp::KeyRelease { keycode }, || {
            self.fallback.inject_key_release(keycode)
        })
    }

    fn caps_lock_active(&self) -> Option<bool> {
        // Lock state is read from the interactive session; it is never
        // injected, so the privileged service is not involved.
        self.fallback.caps_lock_active()
    }

    fn set_caps_lock(&self, active: bool) -> Result<()> {
        if self.caps_lock_active() == Some(active) {
            return Ok(());
        }
        self.inject_key_press(HID_KEY_CAPS_LOCK)?;
        self.inject_key_release(HID_KEY_CAPS_LOCK)
    }

    fn set_cursor_hidden(&self, hidden: bool) -> Result<()> {
        self.fallback.set_cursor_hidden(hidden)
    }

    fn warp_cursor(&self, x: i32, y: i32) -> Result<()> {
        self.fallback.warp_cursor(x, y)
    }

    fn lock_cursor_position(&self) -> Result<()> {
        self.fallback.lock_cursor_position()
    }

    fn unlock_cursor_position(&self) -> Result<()> {
        self.fallback.unlock_cursor_position()
    }

    fn release_all_keys(&self) -> Result<()> {
        let _ = self.route(InputOp::ReleaseAll, || Ok(()));
        self.fallback.release_all_keys()
    }

    fn declare_user_activity(&self) -> Result<()> {
        self.fallback.declare_user_activity()
    }
}

#[cfg(target_os = "windows")]
struct PipeHandle(windows_sys::Win32::Foundation::HANDLE);

#[cfg(target_os = "windows")]
unsafe impl Send for PipeHandle {}

#[cfg(target_os = "windows")]
struct PipeLink {
    handle: PipeHandle,
    fail_until: Option<std::time::Instant>,
}

#[cfg(target_os = "windows")]
impl PipeLink {
    fn new() -> Self {
        Self {
            handle: PipeHandle(windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE),
            fail_until: None,
        }
    }

    fn connected(&self) -> bool {
        self.handle.0 != windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE
    }

    fn in_cooldown(&mut self) -> bool {
        match self.fail_until {
            Some(until) if std::time::Instant::now() < until => true,
            _ => {
                self.fail_until = None;
                false
            }
        }
    }

    fn mark_down(&mut self) {
        self.close();
        self.fail_until = Some(std::time::Instant::now() + Duration::from_secs(2));
    }

    fn close(&mut self) {
        if self.connected() {
            unsafe { windows_sys::Win32::Foundation::CloseHandle(self.handle.0) };
            self.handle.0 = windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE;
        }
    }

    fn connect(&mut self) -> Result<()> {
        use windows_sys::Win32::Foundation::{GENERIC_READ, GENERIC_WRITE};
        use windows_sys::Win32::Storage::FileSystem::{
            CreateFileW, FILE_ATTRIBUTE_NORMAL, OPEN_EXISTING,
        };
        use windows_sys::Win32::System::Pipes::WaitNamedPipeW;

        let pipe_name = super::protocol::pipe_name();
        let name = super::wide_null(&pipe_name);
        let waited = unsafe { WaitNamedPipeW(name.as_ptr(), 150) };
        if waited == 0 {
            self.mark_down();
            bail!("input service pipe not available");
        }
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
        if handle == windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE {
            self.mark_down();
            bail!("input service pipe refused connection");
        }
        self.handle.0 = handle;
        Ok(())
    }

    fn reconnect(&mut self) -> Result<()> {
        self.close();
        self.connect()
    }

    fn write(&self, frame: &[u8]) -> Result<()> {
        use windows_sys::Win32::Storage::FileSystem::WriteFile;

        let mut written = 0u32;
        let ok = unsafe {
            WriteFile(
                self.handle.0,
                frame.as_ptr(),
                frame.len() as u32,
                &mut written,
                std::ptr::null_mut(),
            )
        };
        if ok == 0 || written as usize != frame.len() {
            bail!("input service pipe write failed");
        }
        Ok(())
    }
}

#[cfg(target_os = "windows")]
impl Drop for PipeLink {
    fn drop(&mut self) {
        self.close();
    }
}

#[cfg(target_os = "windows")]
pub fn query_status() -> WindowsInputStatus {
    WindowsInputStatus {
        supported: true,
        elevated: is_elevated(),
        service_installed: service_state().0,
        service_running: service_state().1,
        secure_desktop_enabled: secure_desktop_enabled().ok(),
        service_reachable: probe_service(),
    }
}

#[cfg(not(target_os = "windows"))]
pub fn query_status() -> WindowsInputStatus {
    WindowsInputStatus {
        supported: false,
        elevated: false,
        service_installed: false,
        service_running: false,
        secure_desktop_enabled: None,
        service_reachable: false,
    }
}

#[cfg(target_os = "windows")]
pub fn is_elevated() -> bool {
    use std::mem::size_of;
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::Security::{GetTokenInformation, TOKEN_ELEVATION, TOKEN_QUERY, TokenElevation};
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

    unsafe {
        let mut token: windows_sys::Win32::Foundation::HANDLE = std::ptr::null_mut();
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) == 0 {
            return false;
        }
        let mut elevation = TOKEN_ELEVATION { TokenIsElevated: 0 };
        let mut returned = 0u32;
        let ok = GetTokenInformation(
            token,
            TokenElevation,
            &mut elevation as *mut TOKEN_ELEVATION as *mut core::ffi::c_void,
            size_of::<TOKEN_ELEVATION>() as u32,
            &mut returned,
        );
        CloseHandle(token);
        ok != 0 && elevation.TokenIsElevated != 0
    }
}

#[cfg(not(target_os = "windows"))]
pub fn is_elevated() -> bool {
    false
}

#[cfg(target_os = "windows")]
pub fn restart_elevated() -> Result<()> {
    use windows_sys::Win32::UI::Shell::ShellExecuteW;
    use windows_sys::Win32::UI::WindowsAndMessaging::SW_NORMAL;

    let exe = current_exe_path()?;
    let operation = super::wide_null("runas");
    let file = super::wide_null(&exe);
    let result = unsafe {
        ShellExecuteW(
            std::ptr::null_mut(),
            operation.as_ptr(),
            file.as_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            SW_NORMAL,
        )
    };
    if (result as usize) <= 32 {
        bail!("system declined the administrator relaunch");
    }
    std::process::exit(0);
}

#[cfg(not(target_os = "windows"))]
pub fn restart_elevated() -> Result<()> {
    bail!("administrator relaunch is only available on Windows")
}

#[cfg(target_os = "windows")]
fn run_elevated(arg: &str) -> Result<()> {
    use std::mem::size_of;
    use windows_sys::Win32::Foundation::{CloseHandle, GetLastError, ERROR_CANCELLED};
    use windows_sys::Win32::System::Threading::{GetExitCodeProcess, WaitForSingleObject};
    use windows_sys::Win32::UI::Shell::{ShellExecuteExW, SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW};
    use windows_sys::Win32::UI::WindowsAndMessaging::SW_HIDE;

    let exe = current_exe_path()?;
    let operation = super::wide_null("runas");
    let file = super::wide_null(&exe);
    let parameters = super::wide_null(arg);

    let mut info: SHELLEXECUTEINFOW = unsafe { std::mem::zeroed() };
    info.cbSize = size_of::<SHELLEXECUTEINFOW>() as u32;
    info.fMask = SEE_MASK_NOCLOSEPROCESS;
    info.lpVerb = operation.as_ptr();
    info.lpFile = file.as_ptr();
    info.lpParameters = parameters.as_ptr();
    info.nShow = SW_HIDE;

    let success = unsafe { ShellExecuteExW(&mut info) };
    if success == 0 {
        let err = unsafe { GetLastError() };
        if err == ERROR_CANCELLED {
            bail!("administrator permission was declined");
        }
        bail!("could not request administrator elevation (error {err})");
    }

    if info.hProcess.is_null() {
        return Ok(());
    }

    unsafe {
        WaitForSingleObject(info.hProcess, 30_000);
        let mut exit_code = 0u32;
        let got_exit = GetExitCodeProcess(info.hProcess, &mut exit_code);
        CloseHandle(info.hProcess);
        if got_exit == 0 || exit_code != 0 {
            bail!("operation failed in elevated process (exit code {exit_code})");
        }
    }

    Ok(())
}

#[cfg(target_os = "windows")]
fn staged_service_path() -> Result<std::path::PathBuf> {
    let program_files =
        std::env::var("PROGRAMFILES").map_err(|_| anyhow!("could not locate Program Files"))?;
    Ok(std::path::PathBuf::from(program_files)
        .join("ZeroG")
        .join("zerog-input.exe"))
}

#[cfg(target_os = "windows")]
fn stop_service_by_name(service_name: &str) {
    use windows_sys::Win32::System::Services::{
        CloseServiceHandle, ControlService, OpenSCManagerW, OpenServiceW, QueryServiceStatus,
        SC_MANAGER_CONNECT, SERVICE_CONTROL_STOP, SERVICE_QUERY_STATUS, SERVICE_STATUS,
        SERVICE_STOP, SERVICE_STOPPED,
    };

    unsafe {
        let manager = OpenSCManagerW(std::ptr::null(), std::ptr::null(), SC_MANAGER_CONNECT);
        if manager.is_null() {
            return;
        }
        let name = super::wide_null(service_name);
        let service = OpenServiceW(
            manager,
            name.as_ptr(),
            SERVICE_STOP | SERVICE_QUERY_STATUS,
        );
        CloseServiceHandle(manager);
        if service.is_null() {
            return;
        }
        let mut status: SERVICE_STATUS = std::mem::zeroed();
        if QueryServiceStatus(service, &mut status) != 0
            && status.dwCurrentState != SERVICE_STOPPED
        {
            ControlService(service, SERVICE_CONTROL_STOP, &mut status);
            for _ in 0..50 {
                std::thread::sleep(Duration::from_millis(100));
                if QueryServiceStatus(service, &mut status) == 0
                    || status.dwCurrentState == SERVICE_STOPPED
                {
                    break;
                }
            }
        }
        CloseServiceHandle(service);
    }
}

#[cfg(target_os = "windows")]
fn stop_existing_service() {
    stop_service_by_name(super::protocol::SERVICE_NAME);
    stop_service_by_name("mouse-thing-input");
}


#[cfg(target_os = "windows")]
fn is_in_secure_directory(path: &std::path::Path) -> bool {
    let canonical = match path.canonicalize() {
        Ok(p) => p,
        Err(_) => return false,
    };

    let mut secure_roots = Vec::new();
    for var in &["PROGRAMFILES", "PROGRAMFILES(X86)", "PROGRAMW6432", "SystemRoot"] {
        if let Ok(val) = std::env::var(var) {
            let p = std::path::PathBuf::from(val);
            if let Ok(canon_root) = p.canonicalize() {
                secure_roots.push(canon_root);
            }
        }
    }

    secure_roots
        .iter()
        .any(|root| crate::core::secure_fs::is_subpath_case_insensitive(&canonical, root))
}

#[cfg(target_os = "windows")]
fn stage_service_binary() -> Result<String> {
    let dest = staged_service_path()?;
    if dest.is_file() {
        if is_in_secure_directory(&dest) {
            return Ok(dest.to_string_lossy().into_owned());
        }
        bail!(
            "staged service path is not in a secure directory: {}",
            dest.display()
        );
    }

    let source = std::path::PathBuf::from(current_exe_path()?);
    if !is_in_secure_directory(&source) {
        bail!(
            "input service can only be installed when ZeroG is installed in Program Files; refusing to stage from an unverified directory: {}",
            source.display()
        );
    }

    if !source.to_string_lossy().eq_ignore_ascii_case(&dest.to_string_lossy()) {
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|error| anyhow!("could not create service directory: {error}"))?;
        }
        stop_existing_service();
        std::fs::copy(&source, &dest)
            .map_err(|error| anyhow!("could not stage service binary: {error}"))?;
    }
    Ok(dest.to_string_lossy().into_owned())
}

#[cfg(target_os = "windows")]
fn install_service_direct() -> Result<()> {
    use windows_sys::Win32::System::Services::{
        CloseServiceHandle, CreateServiceW, OpenSCManagerW, OpenServiceW, SC_MANAGER_CREATE_SERVICE,
        SERVICE_ALL_ACCESS, SERVICE_AUTO_START, SERVICE_ERROR_NORMAL,
        SERVICE_WIN32_OWN_PROCESS,
    };

    let staged = stage_service_binary()?;
    let binary = format!("\"{staged}\" --input-service");
    let name = super::wide_null(super::protocol::SERVICE_NAME);
    let display = super::wide_null(super::protocol::SERVICE_DISPLAY_NAME);
    let path = super::wide_null(&binary);
    unsafe {
        let manager = OpenSCManagerW(
            std::ptr::null(),
            std::ptr::null(),
            SC_MANAGER_CREATE_SERVICE,
        );
        if manager.is_null() {
            bail!("could not open the service manager (administrator rights required)");
        }
        let created = CreateServiceW(
            manager,
            name.as_ptr(),
            display.as_ptr(),
            SERVICE_ALL_ACCESS,
            SERVICE_WIN32_OWN_PROCESS,
            SERVICE_AUTO_START,
            SERVICE_ERROR_NORMAL,
            path.as_ptr(),
            std::ptr::null(),
            std::ptr::null_mut(),
            std::ptr::null(),
            std::ptr::null(),
            std::ptr::null(),
        );
        if created.is_null() {
            let existing = OpenServiceW(manager, name.as_ptr(), SERVICE_ALL_ACCESS);
            CloseServiceHandle(manager);
            if existing.is_null() {
                bail!("could not register the input service");
            }
            let reconfigured =
                windows_sys::Win32::System::Services::ChangeServiceConfigW(
                    existing,
                    windows_sys::Win32::System::Services::SERVICE_NO_CHANGE,
                    windows_sys::Win32::System::Services::SERVICE_NO_CHANGE,
                    windows_sys::Win32::System::Services::SERVICE_NO_CHANGE,
                    path.as_ptr(),
                    std::ptr::null(),
                    std::ptr::null_mut(),
                    std::ptr::null(),
                    std::ptr::null(),
                    std::ptr::null(),
                    std::ptr::null(),
                );
            if reconfigured == 0 {
                CloseServiceHandle(existing);
                bail!("input service is registered but could not be updated");
            }
            let started =
                windows_sys::Win32::System::Services::StartServiceW(existing, 0, std::ptr::null());
            CloseServiceHandle(existing);
            if started == 0 {
                let err = windows_sys::Win32::Foundation::GetLastError();
                if err != 1056 {
                    bail!("input service is registered but could not be started");
                }
            }
            return Ok(());
        }
        let started =
            windows_sys::Win32::System::Services::StartServiceW(created, 0, std::ptr::null());
        CloseServiceHandle(created);
        CloseServiceHandle(manager);
        if started == 0 {
            let err = windows_sys::Win32::Foundation::GetLastError();
            if err != 1056 {
                bail!("input service was registered but could not be started");
            }
        }
        Ok(())
    }
}

#[cfg(target_os = "windows")]
pub fn install_service() -> Result<()> {
    if !is_elevated() {
        if std::env::args().any(|arg| arg == "--install-service") {
            bail!("administrator rights are required to install the service");
        }
        return run_elevated("--install-service");
    }
    install_service_direct()
}

#[cfg(not(target_os = "windows"))]
pub fn install_service() -> Result<()> {
    bail!("input service installation is only available on Windows")
}

#[cfg(target_os = "windows")]
fn uninstall_service_direct() -> Result<()> {
    use windows_sys::Win32::System::Services::{
        CloseServiceHandle, ControlService, DeleteService, OpenSCManagerW, OpenServiceW,
        QueryServiceStatus, SC_MANAGER_CONNECT, SERVICE_ALL_ACCESS, SERVICE_CONTROL_STOP,
        SERVICE_STATUS, SERVICE_STOPPED,
    };

    let name = super::wide_null(super::protocol::SERVICE_NAME);
    unsafe {
        let manager = OpenSCManagerW(std::ptr::null(), std::ptr::null(), SC_MANAGER_CONNECT);
        if manager.is_null() {
            bail!("could not open the service manager (administrator rights required)");
        }
        let service = OpenServiceW(manager, name.as_ptr(), SERVICE_ALL_ACCESS);
        CloseServiceHandle(manager);
        if service.is_null() {
            return Ok(());
        }
        let mut status: SERVICE_STATUS = std::mem::zeroed();
        if QueryServiceStatus(service, &mut status) != 0
            && status.dwCurrentState != SERVICE_STOPPED
        {
            ControlService(service, SERVICE_CONTROL_STOP, &mut status);
            for _ in 0..50 {
                std::thread::sleep(Duration::from_millis(100));
                if QueryServiceStatus(service, &mut status) == 0
                    || status.dwCurrentState == SERVICE_STOPPED
                {
                    break;
                }
            }
        }
        let deleted = DeleteService(service);
        CloseServiceHandle(service);
        if deleted == 0 {
            bail!("could not remove the input service");
        }
        if let Ok(staged) = staged_service_path() {
            let _ = std::fs::remove_file(&staged);
            if let Some(parent) = staged.parent() {
                let _ = std::fs::remove_dir(parent);
            }
        }
        Ok(())
    }
}

#[cfg(target_os = "windows")]
pub fn uninstall_service() -> Result<()> {
    if !is_elevated() {
        if std::env::args().any(|arg| arg == "--uninstall-service") {
            bail!("administrator rights are required to remove the service");
        }
        return run_elevated("--uninstall-service");
    }
    uninstall_service_direct()
}

#[cfg(not(target_os = "windows"))]
pub fn uninstall_service() -> Result<()> {
    bail!("input service removal is only available on Windows")
}

#[cfg(target_os = "windows")]
pub fn secure_desktop_enabled() -> Result<bool> {
    use windows_sys::Win32::Foundation::ERROR_SUCCESS;
    use windows_sys::Win32::System::Registry::{
        RegCloseKey, RegOpenKeyExW, RegQueryValueExW, HKEY, HKEY_LOCAL_MACHINE, KEY_READ,
    };

    const SUBKEY: &str = r"SOFTWARE\Microsoft\Windows\CurrentVersion\Policies\System";
    const VALUE: &str = "PromptOnSecureDesktop";

    let subkey = super::wide_null(SUBKEY);
    let value = super::wide_null(VALUE);
    unsafe {
        let mut key: HKEY = std::ptr::null_mut();
        if RegOpenKeyExW(HKEY_LOCAL_MACHINE, subkey.as_ptr(), 0, KEY_READ, &mut key)
            != ERROR_SUCCESS
        {
            return Ok(true);
        }
        let mut kind = 0u32;
        let mut data = 0u32;
        let mut size = std::mem::size_of::<u32>() as u32;
        let queried = RegQueryValueExW(
            key,
            value.as_ptr(),
            std::ptr::null(),
            &mut kind,
            &mut data as *mut u32 as *mut u8,
            &mut size,
        );
        RegCloseKey(key);
        if queried != ERROR_SUCCESS {
            return Ok(true);
        }
        Ok(data != 0)
    }
}

#[cfg(not(target_os = "windows"))]
pub fn secure_desktop_enabled() -> Result<bool> {
    bail!("secure desktop state is only available on Windows")
}

#[cfg(target_os = "windows")]
pub(crate) fn secure_desktop_active() -> bool {
    use windows_sys::Win32::Foundation::FALSE;
    use windows_sys::Win32::System::StationsAndDesktops::{
        CloseDesktop, GetUserObjectInformationW, OpenInputDesktop, DESKTOP_READOBJECTS, UOI_NAME,
    };

    unsafe {
        let desktop = OpenInputDesktop(0, FALSE, DESKTOP_READOBJECTS);
        if desktop.is_null() {
            return true;
        }
        let mut name = [0u16; 64];
        let mut needed = 0u32;
        let queried = GetUserObjectInformationW(
            desktop,
            UOI_NAME,
            name.as_mut_ptr() as *mut core::ffi::c_void,
            (name.len() * std::mem::size_of::<u16>()) as u32,
            &mut needed,
        );
        CloseDesktop(desktop);
        if queried == 0 {
            return false;
        }
        let chars = (needed as usize / std::mem::size_of::<u16>()).min(name.len());
        let desktop_name = String::from_utf16_lossy(&name[..chars.saturating_sub(1)]);
        !desktop_name.eq_ignore_ascii_case("default")
    }
}

#[cfg(not(target_os = "windows"))]
pub(crate) fn secure_desktop_active() -> bool {
    false
}

#[cfg(target_os = "windows")]
fn set_secure_desktop_enabled_direct(enabled: bool) -> Result<()> {
    use windows_sys::Win32::Foundation::ERROR_SUCCESS;
    use windows_sys::Win32::System::Registry::{
        RegCloseKey, RegCreateKeyExW, RegSetValueExW, HKEY, HKEY_LOCAL_MACHINE, KEY_SET_VALUE,
        REG_DWORD, REG_OPTION_NON_VOLATILE,
    };

    const SUBKEY: &str = r"SOFTWARE\Microsoft\Windows\CurrentVersion\Policies\System";
    const VALUE: &str = "PromptOnSecureDesktop";

    let subkey = super::wide_null(SUBKEY);
    let value = super::wide_null(VALUE);
    let data: u32 = if enabled { 1 } else { 0 };
    unsafe {
        let mut key: HKEY = std::ptr::null_mut();
        if RegCreateKeyExW(
            HKEY_LOCAL_MACHINE,
            subkey.as_ptr(),
            0,
            std::ptr::null(),
            REG_OPTION_NON_VOLATILE,
            KEY_SET_VALUE,
            std::ptr::null(),
            &mut key,
            std::ptr::null_mut(),
        ) != ERROR_SUCCESS
        {
            bail!("could not open the UAC policy key");
        }
        let written = RegSetValueExW(
            key,
            value.as_ptr(),
            0,
            REG_DWORD,
            &data as *const u32 as *const u8,
            std::mem::size_of::<u32>() as u32,
        );
        RegCloseKey(key);
        if written != ERROR_SUCCESS {
            bail!("could not update the secure desktop setting");
        }
        Ok(())
    }
}

#[cfg(target_os = "windows")]
pub fn set_secure_desktop_enabled(enabled: bool) -> Result<()> {
    if !is_elevated() {
        let flag = if enabled {
            "--enable-secure-desktop"
        } else {
            "--disable-secure-desktop"
        };
        if std::env::args().any(|arg| arg == flag) {
            bail!("administrator rights are required to change the secure desktop setting");
        }
        return run_elevated(flag);
    }
    set_secure_desktop_enabled_direct(enabled)
}

#[cfg(not(target_os = "windows"))]
pub fn set_secure_desktop_enabled(_enabled: bool) -> Result<()> {
    bail!("secure desktop setting is only available on Windows")
}

#[cfg(target_os = "windows")]
fn service_state() -> (bool, bool) {
    use windows_sys::Win32::System::Services::{
        CloseServiceHandle, OpenSCManagerW, OpenServiceW, QueryServiceStatus, SC_MANAGER_CONNECT,
        SERVICE_QUERY_STATUS, SERVICE_RUNNING, SERVICE_STATUS,
    };

    let name = super::wide_null(super::protocol::SERVICE_NAME);
    unsafe {
        let manager = OpenSCManagerW(std::ptr::null(), std::ptr::null(), SC_MANAGER_CONNECT);
        if manager.is_null() {
            return (false, false);
        }
        let service = OpenServiceW(manager, name.as_ptr(), SERVICE_QUERY_STATUS);
        CloseServiceHandle(manager);
        if service.is_null() {
            return (false, false);
        }
        let mut status: SERVICE_STATUS = std::mem::zeroed();
        let running = QueryServiceStatus(service, &mut status) != 0
            && status.dwCurrentState == SERVICE_RUNNING;
        CloseServiceHandle(service);
        (true, running)
    }
}

#[cfg(target_os = "windows")]
fn probe_service() -> bool {
    use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::Foundation::{GENERIC_READ, GENERIC_WRITE};
    use windows_sys::Win32::Storage::FileSystem::{
        CreateFileW, FILE_ATTRIBUTE_NORMAL, OPEN_EXISTING,
    };
    use windows_sys::Win32::System::Pipes::WaitNamedPipeW;

    let pipe_name = super::protocol::pipe_name();
    let name = super::wide_null(&pipe_name);
    unsafe {
        if WaitNamedPipeW(name.as_ptr(), 150) == 0 {
            return false;
        }
        let handle = CreateFileW(
            name.as_ptr(),
            GENERIC_READ | GENERIC_WRITE,
            0,
            std::ptr::null(),
            OPEN_EXISTING,
            FILE_ATTRIBUTE_NORMAL,
            std::ptr::null_mut(),
        );
        if handle == INVALID_HANDLE_VALUE {
            return false;
        }
        CloseHandle(handle);
        true
    }
}

#[cfg(target_os = "windows")]
fn current_exe_path() -> Result<String> {
    use windows_sys::Win32::System::LibraryLoader::GetModuleFileNameW;

    unsafe {
        let mut buffer = vec![0u16; 32768];
        let length = GetModuleFileNameW(
            std::ptr::null_mut(),
            buffer.as_mut_ptr(),
            buffer.len() as u32,
        ) as usize;
        if length == 0 || length >= buffer.len() {
            bail!("could not determine the application path");
        }
        Ok(String::from_utf16_lossy(&buffer[..length]))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct FakeInjection {
        moves: AtomicUsize,
    }

    impl EventInjection for FakeInjection {
        fn inject_mouse_move(&self, _x: i32, _y: i32) -> Result<()> {
            self.moves.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }

        fn inject_mouse_button(&self, _button: u8, _pressed: bool) -> Result<()> {
            Ok(())
        }

        fn inject_mouse_wheel(&self, _dx: i32, _dy: i32) -> Result<()> {
            Ok(())
        }

        fn set_cursor_hidden(&self, _hidden: bool) -> Result<()> {
            Ok(())
        }

        fn warp_cursor(&self, _x: i32, _y: i32) -> Result<()> {
            Ok(())
        }
    }

    #[test]
    fn falls_back_to_direct_injection_when_service_absent() {
        crate::platform::shared::test_support::with_temp_home(|| {
            let fake = Arc::new(FakeInjection {
                moves: AtomicUsize::new(0),
            });
            let injection = ServiceBackedInjection::new(fake.clone(), 42);
            assert!(!injection.service_reachable());
            injection.inject_mouse_move(10, 20).unwrap();
            assert_eq!(fake.moves.load(Ordering::SeqCst), 1);
        });
    }

    #[test]
    fn unsupported_platform_reports_no_support() {
        let status = query_status();
        assert_eq!(status.supported, cfg!(target_os = "windows"));
        assert!(!is_elevated() || cfg!(target_os = "windows"));
    }

    #[test]
    fn secure_desktop_probe_reports_stable_result() {
        let first = secure_desktop_active();
        let second = secure_desktop_active();
        assert_eq!(first, second);
        #[cfg(not(target_os = "windows"))]
        assert!(!first);
    }

    #[test]
    fn direct_injection_preferred_when_secure_desktop_inactive() {
        crate::platform::shared::test_support::with_temp_home(|| {
            if secure_desktop_active() {
                return;
            }
            let fake = Arc::new(FakeInjection {
                moves: AtomicUsize::new(0),
            });
            let injection = ServiceBackedInjection::new(fake.clone(), 42);
            injection.inject_mouse_move(10, 20).unwrap();
            assert_eq!(fake.moves.load(Ordering::SeqCst), 1);
        });
    }
}
