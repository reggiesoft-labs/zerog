//! High-resolution multimedia timer guard for Windows.

#[derive(Debug)]
pub(crate) struct HighResolutionTimerGuard {
    period_ms: u32,
}

impl HighResolutionTimerGuard {
    pub(crate) fn new(period_ms: u32) -> Self {
        // SAFETY: timeBeginPeriod sets the minimum timer resolution for the process.
        unsafe {
            windows_sys::Win32::Media::timeBeginPeriod(period_ms);
        }
        Self { period_ms }
    }
}

impl Drop for HighResolutionTimerGuard {
    fn drop(&mut self) {
        // SAFETY: timeEndPeriod restores the previous timer resolution.
        unsafe {
            windows_sys::Win32::Media::timeEndPeriod(self.period_ms);
        }
    }
}
