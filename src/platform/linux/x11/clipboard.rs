//! X11 clipboard manager using xclip subprocess.
//!
//! This implementation uses xclip for clipboard operations as it handles
//! the complex X11 INCR protocol automatically. For native Wayland,
//! clipboard is not available as there's no standardized protocol.

use crate::core::platform::traits::{ClipboardError, ClipboardManager};
use std::process::Command;
use std::sync::{Arc, Mutex};

pub struct X11ClipboardManager {
    operation_mutex: Mutex<()>,
}

unsafe impl Send for X11ClipboardManager {}
unsafe impl Sync for X11ClipboardManager {}

impl X11ClipboardManager {
    pub fn new() -> Result<Arc<Self>, ClipboardError> {
        Ok(Arc::new(Self {
            operation_mutex: Mutex::new(()),
        }))
    }

    pub(crate) fn check_xclip_available() -> bool {
        Command::new("xclip")
            .arg("-version")
            .output()
            .map(|output| output.status.success())
            .unwrap_or(false)
    }
}

const SENSITIVE_X11_TARGETS: &[&str] = &[
    "x-kde-passwordManagerHint",
    "application/x-keepassxc-selection",
];

fn has_sensitive_targets() -> bool {
    let output = Command::new("xclip")
        .args(["-selection", "clipboard", "-o", "-t", "TARGETS"])
        .output();
    if let Ok(out) = output {
        if out.status.success() {
            let targets = String::from_utf8_lossy(&out.stdout);
            for line in targets.lines() {
                if SENSITIVE_X11_TARGETS.contains(&line.trim()) {
                    return true;
                }
            }
        }
    }
    false
}

impl ClipboardManager for X11ClipboardManager {
    fn get_clipboard_text(&self) -> Result<Option<String>, ClipboardError> {
        let _guard = self.operation_mutex.lock().map_err(|_| {
            ClipboardError::PlatformError("Failed to acquire clipboard lock".into())
        })?;

        if has_sensitive_targets() {
            return Ok(None);
        }

        let output = Command::new("xclip")
            .args(["-selection", "clipboard", "-o", "-t", "text/plain"])
            .output()
            .map_err(|e| ClipboardError::PlatformError(format!("xclip failed: {}", e)))?;

        if output.status.success() {
            let text = String::from_utf8_lossy(&output.stdout).to_string();
            if text.is_empty() {
                Ok(None)
            } else {
                Ok(Some(text))
            }
        } else {
            Ok(None)
        }
    }

    fn set_clipboard_text(&self, content: &str) -> Result<(), ClipboardError> {
        let _guard = self.operation_mutex.lock().map_err(|_| {
            ClipboardError::PlatformError("Failed to acquire clipboard lock".into())
        })?;

        let mut child = Command::new("xclip")
            .args(["-selection", "clipboard", "-i"])
            .stdin(std::process::Stdio::piped())
            .spawn()
            .map_err(|e| ClipboardError::PlatformError(format!("xclip spawn failed: {}", e)))?;

        if let Some(mut stdin) = child.stdin.take() {
            use std::io::Write;
            stdin
                .write_all(content.as_bytes())
                .map_err(|e| ClipboardError::PlatformError(format!("xclip stdin failed: {}", e)))?;
        }

        let status = child
            .wait()
            .map_err(|e| ClipboardError::PlatformError(format!("xclip wait failed: {}", e)))?;

        if status.success() {
            Ok(())
        } else {
            Err(ClipboardError::PlatformError(format!(
                "xclip exited with code: {:?}",
                status.code()
            )))
        }
    }

    fn clear_clipboard(&self) -> Result<(), ClipboardError> {
        let _guard = self.operation_mutex.lock().map_err(|_| {
            ClipboardError::PlatformError("Failed to acquire clipboard lock".into())
        })?;

        let mut child = Command::new("xclip")
            .args(["-selection", "clipboard", "-i", "-rmlast"])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .map_err(|e| ClipboardError::PlatformError(format!("xclip clear failed: {}", e)))?;

        let started = std::time::Instant::now();
        loop {
            match child.try_wait() {
                Ok(Some(_)) => return Ok(()),
                Ok(None) => {
                    if started.elapsed() >= std::time::Duration::from_secs(2) {
                        let _ = child.kill();
                        return Ok(());
                    }
                    std::thread::sleep(std::time::Duration::from_millis(20));
                }
                Err(e) => {
                    return Err(ClipboardError::PlatformError(format!(
                        "xclip clear wait failed: {}",
                        e
                    )))
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::platform::linux::live_test;
    use serial_test::serial;

    const ROUND_TRIP_TEXT: &str = "mouse-thing-x11-clipboard-test";

    fn clipboard_available() -> bool {
        X11ClipboardManager::check_xclip_available() && live_test::x11_display_available()
    }

    #[test]
    #[serial]
    fn set_get_clear_round_trip() {
        if !live_test::require_live(
            "set_get_clear_round_trip",
            clipboard_available(),
            "no live X display or xclip binary",
        ) {
            return;
        }
        let clipboard = X11ClipboardManager::new().expect("manager should build");
        clipboard
            .set_clipboard_text(ROUND_TRIP_TEXT)
            .expect("set should succeed");
        assert_eq!(
            clipboard.get_clipboard_text().expect("get should succeed"),
            Some(ROUND_TRIP_TEXT.to_string())
        );
        let started = std::time::Instant::now();
        loop {
            clipboard.clear_clipboard().expect("clear should succeed");
            let current = clipboard
                .get_clipboard_text()
                .expect("get after clear should succeed");
            if current.is_none() {
                break;
            }
            assert!(
                started.elapsed() < std::time::Duration::from_secs(10),
                "clipboard did not clear, still holds {current:?}"
            );
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
    }
}
