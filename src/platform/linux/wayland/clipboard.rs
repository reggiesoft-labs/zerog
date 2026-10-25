//! Wayland clipboard manager stub.
//!
//! Native Wayland has no standardized clipboard protocol at the compositor level.
//! Clipboard is handled by individual applications (Qt, GTK, etc.) which use
//! their own clipboard APIs (e.g., Qt::QClipboard, GTK::Clipboard).
//!
//! When XWayland is available, the X11 clipboard manager can be used instead.
//! For native Wayland, clipboard sync is not available.

use crate::core::platform::traits::{ClipboardError, ClipboardManager};
use crate::platform::linux::x11::clipboard::X11ClipboardManager;
use std::io::Write;
use std::process::Command;
use std::sync::{Arc, Mutex};

pub enum WaylandClipboardManager {
    WlClipboard { operation_mutex: Mutex<()> },
    X11(Arc<X11ClipboardManager>),
}

unsafe impl Send for WaylandClipboardManager {}
unsafe impl Sync for WaylandClipboardManager {}

const SENSITIVE_WAYLAND_TARGETS: &[&str] = &[
    "x-kde-passwordManagerHint",
    "application/x-keepassxc-selection",
];

const TEXT_TARGETS: &[&str] = &[
    "text/plain;charset=utf-8",
    "UTF8_STRING",
    "text/plain",
    "TEXT",
    "STRING",
];

fn text_target(targets: &str) -> Option<&'static str> {
    if targets
        .lines()
        .any(|target| SENSITIVE_WAYLAND_TARGETS.contains(&target.trim()))
    {
        return None;
    }

    TEXT_TARGETS
        .iter()
        .copied()
        .find(|target| targets.lines().any(|line| line.trim() == *target))
}

impl WaylandClipboardManager {
    pub fn new() -> Result<Arc<Self>, ClipboardError> {
        if Self::check_wl_clipboard_available() {
            return Ok(Arc::new(Self::WlClipboard {
                operation_mutex: Mutex::new(()),
            }));
        }

        if X11ClipboardManager::check_xclip_available() {
            let x11 = X11ClipboardManager::new()?;
            return Ok(Arc::new(Self::X11(x11)));
        }

        Err(ClipboardError::PlatformError(
            "Neither wl-clipboard (wl-copy/wl-paste) nor xclip is available in PATH".into(),
        ))
    }

    pub fn check_wl_clipboard_available() -> bool {
        Command::new("wl-copy")
            .arg("--version")
            .output()
            .map(|output| output.status.success())
            .unwrap_or(false)
            && Command::new("wl-paste")
                .arg("--version")
                .output()
                .map(|output| output.status.success())
                .unwrap_or(false)
    }

    fn paste(args: &[&str]) -> Result<Option<String>, ClipboardError> {
        let output = Command::new("wl-paste")
            .args(args)
            .output()
            .map_err(|e| ClipboardError::PlatformError(format!("wl-paste failed: {e}")))?;
        if !output.status.success() {
            let error = String::from_utf8_lossy(&output.stderr);
            if error.trim() == "Nothing is copied" {
                return Ok(None);
            }
            return Err(ClipboardError::PlatformError(format!(
                "wl-paste exited with {}: {}",
                output.status,
                error.trim()
            )));
        }

        let text = String::from_utf8(output.stdout)
            .map_err(|e| ClipboardError::PlatformError(format!("Invalid clipboard UTF-8: {e}")))?;
        Ok((!text.is_empty()).then_some(text))
    }
}

impl ClipboardManager for WaylandClipboardManager {
    fn get_clipboard_text(&self) -> Result<Option<String>, ClipboardError> {
        match self {
            Self::WlClipboard { operation_mutex } => {
                let _guard = operation_mutex.lock().map_err(|_| {
                    ClipboardError::PlatformError("Failed to acquire clipboard lock".into())
                })?;

                let Some(targets) = Self::paste(&["--list-types"])? else {
                    return Ok(None);
                };
                let Some(target) = text_target(&targets) else {
                    return Ok(None);
                };
                Self::paste(&["--no-newline", "--type", target])
            }
            Self::X11(manager) => manager.get_clipboard_text(),
        }
    }

    fn set_clipboard_text(&self, content: &str) -> Result<(), ClipboardError> {
        match self {
            Self::WlClipboard { operation_mutex } => {
                let _guard = operation_mutex.lock().map_err(|_| {
                    ClipboardError::PlatformError("Failed to acquire clipboard lock".into())
                })?;

                let mut child = Command::new("wl-copy")
                    .arg("--type")
                    .arg("text/plain;charset=utf-8")
                    .stdin(std::process::Stdio::piped())
                    .spawn()
                    .map_err(|e| {
                        ClipboardError::PlatformError(format!("wl-copy spawn failed: {}", e))
                    })?;

                if let Some(mut stdin) = child.stdin.take() {
                    stdin.write_all(content.as_bytes()).map_err(|e| {
                        ClipboardError::PlatformError(format!("wl-copy stdin failed: {}", e))
                    })?;
                }

                let status = child.wait().map_err(|e| {
                    ClipboardError::PlatformError(format!("wl-copy wait failed: {}", e))
                })?;

                if status.success() {
                    Ok(())
                } else {
                    Err(ClipboardError::PlatformError(format!(
                        "wl-copy exited with code: {:?}",
                        status.code()
                    )))
                }
            }
            Self::X11(manager) => manager.set_clipboard_text(content),
        }
    }

    fn clear_clipboard(&self) -> Result<(), ClipboardError> {
        match self {
            Self::WlClipboard { operation_mutex } => {
                let _guard = operation_mutex.lock().map_err(|_| {
                    ClipboardError::PlatformError("Failed to acquire clipboard lock".into())
                })?;

                let status = Command::new("wl-copy")
                    .arg("--clear")
                    .status()
                    .map_err(|e| {
                        ClipboardError::PlatformError(format!("wl-copy clear wait failed: {}", e))
                    })?;

                if status.success() {
                    Ok(())
                } else {
                    Err(ClipboardError::PlatformError(format!(
                        "wl-copy clear exited with code: {:?}",
                        status.code()
                    )))
                }
            }
            Self::X11(manager) => manager.clear_clipboard(),
        }
    }

    fn is_available(&self) -> bool {
        match self {
            Self::WlClipboard { .. } => true,
            Self::X11(manager) => manager.is_available(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::events::{ClipboardSequence, Event};
    use crate::core::platform::traits::NetworkState;
    use crate::platform::linux::live_test;
    use crate::platform::shared::clipboard::{ClipboardSync, compute_hash};
    use serial_test::serial;
    use std::time::Duration;

    const ROUND_TRIP_TEXT: &str = "mouse-thing-wayland-clipboard-test";

    #[test]
    fn selects_offered_plain_text_formats() {
        for target in TEXT_TARGETS {
            assert_eq!(text_target(target), Some(*target));
        }
        assert_eq!(
            text_target("text/plain\ntext/html\ntext/plain;charset=utf-8\n"),
            Some("text/plain;charset=utf-8")
        );
        assert_eq!(
            text_target("text/plain\nUTF8_STRING\n"),
            Some("UTF8_STRING")
        );
        assert_eq!(text_target("image/png\ntext/html\n"), None);
        assert_eq!(text_target(""), None);
    }

    #[test]
    fn excludes_sensitive_text_in_any_target_order() {
        for sensitive in SENSITIVE_WAYLAND_TARGETS {
            assert_eq!(text_target(&format!("{sensitive}\nUTF8_STRING\n")), None);
            assert_eq!(text_target(&format!("text/plain\n{sensitive}\n")), None);
        }
    }

    fn clipboard_available() -> bool {
        WaylandClipboardManager::check_wl_clipboard_available()
            && live_test::wayland_display_available()
    }

    #[derive(Default)]
    struct ClipboardNetwork {
        events: Mutex<Vec<Event>>,
    }

    impl NetworkState for ClipboardNetwork {
        fn broadcast_packet(&self, events: Vec<Event>) -> anyhow::Result<()> {
            self.events.lock().unwrap().extend(events);
            Ok(())
        }

        fn active_target(&self) -> Option<std::net::SocketAddr> {
            None
        }

        fn has_any_peer(&self) -> bool {
            true
        }
    }

    #[tokio::test]
    #[serial]
    async fn sync_reads_and_applies_wayland_clipboard() {
        if !live_test::require_live(
            "sync_reads_and_applies_wayland_clipboard",
            clipboard_available(),
            "no live Wayland display or wl-clipboard binary",
        ) {
            return;
        }

        let clipboard = WaylandClipboardManager::new().unwrap();
        let network = Arc::new(ClipboardNetwork::default());
        let sync = ClipboardSync::new(
            clipboard.clone(),
            network.clone(),
            Arc::new(ClipboardSequence::new()),
            tokio::runtime::Handle::current(),
            Duration::from_millis(50),
            Duration::from_millis(200),
        );
        sync.fsm().set_syncing();

        let local = "Linux → peer: café 世界\n";
        assert!(
            Command::new("wl-copy")
                .arg(local)
                .status()
                .unwrap()
                .success()
        );
        sync.poll_clipboard().await;
        let events = std::mem::take(&mut *network.events.lock().unwrap());
        let [
            Event::ClipboardText {
                content,
                sequence,
                content_hash,
                timestamp,
            },
        ] = events.as_slice()
        else {
            panic!("expected one outgoing clipboard event, got {events:?}");
        };
        assert_eq!(content, local);
        assert_eq!(*content_hash, compute_hash(local));
        sync.handle_received_ack(*sequence);

        let remote = "Peer → Linux: résumé 世界\n";
        let remote_timestamp = timestamp + 1;
        sync.handle_received_clipboard_text(
            remote.to_string(),
            42,
            compute_hash(remote),
            remote_timestamp,
        );
        let pasted = Command::new("wl-paste")
            .args(["--no-newline", "--type", "text"])
            .output()
            .unwrap();
        assert!(pasted.status.success());
        assert_eq!(String::from_utf8(pasted.stdout).unwrap(), remote);
        let events = std::mem::take(&mut *network.events.lock().unwrap());
        assert!(matches!(
            events.as_slice(),
            [Event::ClipboardAck { sequence: 42, .. }]
        ));
        sync.poll_clipboard().await;
        assert!(network.events.lock().unwrap().is_empty());

        sync.set_enabled(false);
        sync.handle_received_clipboard_text(
            "disabled".into(),
            43,
            compute_hash("disabled"),
            remote_timestamp + 1,
        );
        assert_eq!(
            clipboard.get_clipboard_text().unwrap().as_deref(),
            Some(remote)
        );
        clipboard
            .set_clipboard_text("local while disabled")
            .unwrap();
        sync.poll_clipboard().await;
        assert!(network.events.lock().unwrap().is_empty());
        sync.set_enabled(true);
        sync.poll_clipboard().await;
        assert!(
            matches!(network.events.lock().unwrap().as_slice(), [Event::ClipboardText { content, .. }] if content == "local while disabled")
        );
        sync.stop();
        clipboard.clear_clipboard().unwrap();
    }

    #[test]
    #[serial]
    fn set_get_clear_round_trip() {
        if !live_test::require_live(
            "set_get_clear_round_trip",
            clipboard_available(),
            "no live Wayland display or wl-clipboard binary",
        ) {
            return;
        }
        let clipboard = WaylandClipboardManager::new().expect("manager should build");
        clipboard
            .set_clipboard_text(ROUND_TRIP_TEXT)
            .expect("set should succeed");
        assert_eq!(
            clipboard.get_clipboard_text().expect("get should succeed"),
            Some(ROUND_TRIP_TEXT.to_string())
        );
        clipboard.clear_clipboard().expect("clear should succeed");
        assert_eq!(
            clipboard
                .get_clipboard_text()
                .expect("get after clear should succeed"),
            None
        );
    }

    #[test]
    #[serial]
    fn set_get_unicode_text() {
        if !live_test::require_live(
            "set_get_unicode_text",
            clipboard_available(),
            "no live Wayland display or wl-clipboard binary",
        ) {
            return;
        }
        let clipboard = WaylandClipboardManager::new().expect("manager should build");
        let unicode_text = "mouse-thing 🚀 世界 café résumé";
        clipboard
            .set_clipboard_text(unicode_text)
            .expect("set unicode should succeed");
        assert_eq!(
            clipboard.get_clipboard_text().expect("get should succeed"),
            Some(unicode_text.to_string())
        );
        clipboard.clear_clipboard().expect("clear should succeed");
    }
}
