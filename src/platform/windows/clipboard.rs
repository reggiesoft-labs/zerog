//! Windows clipboard manager using Win32 API.
//!
//! Win32 clipboard API is NOT thread-safe, so we serialize all access
//! through a Mutex and use try_lock to avoid blocking.

use crate::core::platform::traits::{ClipboardError, ClipboardManager};
use std::sync::{Arc, Mutex};
use windows_sys::Win32::Foundation::GlobalFree;
use windows_sys::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, GetClipboardData, IsClipboardFormatAvailable, OpenClipboard,
    RegisterClipboardFormatA, SetClipboardData,
};
use windows_sys::Win32::System::Memory::{
    GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalUnlock,
};

const CF_UNICODETEXT: u32 = 13;

const SENSITIVE_WINDOWS_FORMATS: &[&[u8]] = &[
    b"Clipboard Viewer Ignore\0",
    b"ExcludeClipboardContentFromMonitorProcessing\0",
];

unsafe fn has_sensitive_format() -> bool {
    for &format_name in SENSITIVE_WINDOWS_FORMATS {
        let atom = unsafe { RegisterClipboardFormatA(format_name.as_ptr()) };
        if atom != 0 && unsafe { IsClipboardFormatAvailable(atom) } != 0 {
            return true;
        }
    }
    false
}

pub struct WindowsClipboardManager {
    clipboard_mutex: Mutex<()>,
}

unsafe impl Send for WindowsClipboardManager {}
unsafe impl Sync for WindowsClipboardManager {}

impl WindowsClipboardManager {
    pub fn new() -> Result<Arc<Self>, ClipboardError> {
        Ok(Arc::new(Self {
            clipboard_mutex: Mutex::new(()),
        }))
    }

    fn with_clipboard_lock<F, T>(&self, f: F) -> Result<T, ClipboardError>
    where
        F: FnOnce() -> Result<T, ClipboardError>,
    {
        let _guard = self
            .clipboard_mutex
            .lock()
            .map_err(|_| ClipboardError::PlatformError("Mutex poisoned".into()))?;

        let mut opened = false;
        for _ in 0..5 {
            unsafe {
                if OpenClipboard(std::ptr::null_mut()) != 0 {
                    opened = true;
                    break;
                }
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }

        if !opened {
            return Err(ClipboardError::Busy);
        }

        let result = f();

        unsafe {
            CloseClipboard();
        }

        result
    }
}

impl ClipboardManager for WindowsClipboardManager {
    fn get_clipboard_text(&self) -> Result<Option<String>, ClipboardError> {
        self.with_clipboard_lock(|| unsafe {
            if has_sensitive_format() {
                return Ok(None);
            }

            let handle = GetClipboardData(CF_UNICODETEXT);
            if handle.is_null() {
                return Ok(None);
            }

            let ptr = GlobalLock(handle);
            if ptr.is_null() {
                return Ok(None);
            }

            let wide_ptr: *const u16 = ptr as *const u16;
            let len = (0..).take_while(|&i| *wide_ptr.add(i) != 0).count();
            let slice = std::slice::from_raw_parts(wide_ptr, len);
            let result = Ok(Some(String::from_utf16_lossy(slice)));

            GlobalUnlock(handle);

            result
        })
    }

    fn set_clipboard_text(&self, content: &str) -> Result<(), ClipboardError> {
        self.with_clipboard_lock(|| unsafe {
            if EmptyClipboard() == 0 {
                return Err(ClipboardError::PlatformError(
                    "EmptyClipboard failed".into(),
                ));
            }

            let wide: Vec<u16> = content.encode_utf16().chain(std::iter::once(0)).collect();
            let size_bytes = wide.len() * std::mem::size_of::<u16>();

            let alloc = GlobalAlloc(GMEM_MOVEABLE, size_bytes);
            if alloc.is_null() {
                return Err(ClipboardError::PlatformError("Out of memory".into()));
            }

            let ptr = GlobalLock(alloc);
            if ptr.is_null() {
                GlobalFree(alloc);
                return Err(ClipboardError::PlatformError("GlobalLock failed".into()));
            }

            std::ptr::copy_nonoverlapping(wide.as_ptr(), ptr as *mut u16, wide.len());
            GlobalUnlock(alloc);

            if SetClipboardData(CF_UNICODETEXT, alloc).is_null() {
                GlobalFree(alloc);
                return Err(ClipboardError::PlatformError(
                    "SetClipboardData failed".into(),
                ));
            }

            Ok(())
        })
    }

    fn clear_clipboard(&self) -> Result<(), ClipboardError> {
        self.with_clipboard_lock(|| unsafe {
            if EmptyClipboard() == 0 {
                return Err(ClipboardError::PlatformError(
                    "EmptyClipboard failed".into(),
                ));
            }
            Ok(())
        })
    }
}
