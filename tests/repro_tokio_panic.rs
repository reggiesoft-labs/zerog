use anyhow::Result;
use zerog::core::events::{ClipboardSequence, Event};
use zerog::core::platform::traits::{ClipboardError, ClipboardManager, NetworkState};
use std::sync::Arc;
use std::time::Duration;

struct MockClipboard;
impl ClipboardManager for MockClipboard {
    fn get_clipboard_text(&self) -> Result<Option<String>, ClipboardError> {
        Ok(None)
    }
    fn set_clipboard_text(&self, _content: &str) -> Result<(), ClipboardError> {
        Ok(())
    }
    fn clear_clipboard(&self) -> Result<(), ClipboardError> {
        Ok(())
    }
}

struct MockNetwork;
impl NetworkState for MockNetwork {
    fn broadcast_packet(&self, _events: Vec<Event>) -> Result<()> {
        Ok(())
    }
    fn active_target(&self) -> Option<std::net::SocketAddr> {
        None
    }
    fn has_any_peer(&self) -> bool {
        false
    }
}

#[test]
fn test_clipboard_sync_start_panics_without_runtime() {
    // Attempting to access ClipboardSync which might be private
    use zerog::platform::shared::clipboard_sync::ClipboardSync;

    let clipboard = Arc::new(MockClipboard);
    let network = Arc::new(MockNetwork);
    let sequence = Arc::new(ClipboardSequence::default());
    let poll_interval = Duration::from_millis(100);
    let ack_timeout = Duration::from_secs(1);

    let sync = ClipboardSync::new(
        clipboard,
        network,
        sequence,
        zerog::platform::shared::tokio_runtime::global_runtime_handle(),
        poll_interval,
        ack_timeout,
    );

    let handle = std::thread::spawn(move || {
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
            sync.start();
        }));
        result
    });

    let result = handle.join().unwrap();
    assert!(
        result.is_ok(),
        "Expected start() to succeed even when called outside of a Tokio runtime, thanks to global handle"
    );
}
