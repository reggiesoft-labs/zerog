//! Thread and process scheduling priority elevation for real-time mouse latency.

/// Sets the current thread to real-time / user-interactive scheduling priority.
pub(crate) fn set_realtime_thread_priority() {
    #[cfg(target_os = "macos")]
    unsafe {
        unsafe extern "C" {
            fn pthread_set_qos_class_self_np(qos_class: u32, relative_priority: i32) -> i32;
        }
        // QOS_CLASS_USER_INTERACTIVE = 0x21 (33)
        let _ = pthread_set_qos_class_self_np(0x21, 0);
    }

    #[cfg(target_os = "windows")]
    unsafe {
        use windows_sys::Win32::System::Threading::{
            GetCurrentThread, SetThreadPriority, THREAD_PRIORITY_HIGHEST,
        };
        let _ = SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_HIGHEST);
    }

    #[cfg(target_os = "linux")]
    unsafe {
        unsafe extern "C" {
            fn nice(inc: i32) -> i32;
        }
        let _ = nice(-10);
    }
}
