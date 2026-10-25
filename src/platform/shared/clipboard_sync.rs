pub use super::clipboard::{ClipboardFsm, ClipboardSync, PendingRemote, SyncState, compute_hash};

#[cfg(test)]
mod tests {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    use std::net::SocketAddr;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use crate::core::events::{ClipboardSequence, Event};
    use crate::core::platform::traits::{ClipboardManager, NetworkState};
    use crate::platform::shared::clipboard::{ClipboardSync, SyncState};

    fn test_hash(s: &str) -> u64 {
        let mut hasher = DefaultHasher::new();
        s.hash(&mut hasher);
        hasher.finish()
    }

    struct MockClipboardManager {
        get_text: Arc<Mutex<Option<String>>>,
        set_text_calls: Arc<Mutex<Vec<String>>>,
        should_fail: AtomicBool,
        get_call_count: AtomicUsize,
        set_call_count: AtomicUsize,
    }

    impl MockClipboardManager {
        fn new(text: String) -> Self {
            Self {
                get_text: Arc::new(Mutex::new(Some(text))),
                set_text_calls: Arc::new(Mutex::new(Vec::new())),
                should_fail: AtomicBool::new(false),
                get_call_count: AtomicUsize::new(0),
                set_call_count: AtomicUsize::new(0),
            }
        }

        #[allow(dead_code)]
        fn set_text(&self, text: String) {
            *self.get_text.lock().unwrap_or_else(|e| e.into_inner()) = Some(text);
        }

        #[allow(dead_code)]
        fn get_calls(&self) -> usize {
            self.get_call_count.load(Ordering::SeqCst)
        }
    }

    impl ClipboardManager for MockClipboardManager {
        fn get_clipboard_text(
            &self,
        ) -> std::result::Result<Option<String>, crate::core::platform::traits::ClipboardError>
        {
            self.get_call_count.fetch_add(1, Ordering::SeqCst);
            if self.should_fail.load(Ordering::SeqCst) {
                return Err(
                    crate::core::platform::traits::ClipboardError::PlatformError(
                        "mock error".into(),
                    ),
                );
            }
            Ok(self
                .get_text
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clone())
        }

        fn set_clipboard_text(
            &self,
            content: &str,
        ) -> std::result::Result<(), crate::core::platform::traits::ClipboardError> {
            self.set_call_count.fetch_add(1, Ordering::SeqCst);
            if self.should_fail.load(Ordering::SeqCst) {
                return Err(
                    crate::core::platform::traits::ClipboardError::PlatformError(
                        "mock error".into(),
                    ),
                );
            }
            self.set_text_calls
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push(content.to_string());
            *self.get_text.lock().unwrap_or_else(|e| e.into_inner()) = Some(content.to_string());
            Ok(())
        }

        fn clear_clipboard(
            &self,
        ) -> std::result::Result<(), crate::core::platform::traits::ClipboardError> {
            *self.get_text.lock().unwrap_or_else(|e| e.into_inner()) = None;
            Ok(())
        }
    }

    struct MockNetworkState {
        sent_events: Arc<Mutex<Vec<Event>>>,
        has_peer: AtomicBool,
        block_send: AtomicBool,
        active_target_addr: Option<SocketAddr>,
    }

    impl MockNetworkState {
        fn new() -> Self {
            Self {
                sent_events: Arc::new(Mutex::new(Vec::new())),
                has_peer: AtomicBool::new(true),
                block_send: AtomicBool::new(false),
                active_target_addr: Some("127.0.0.1:8080".parse().unwrap()),
            }
        }

        #[allow(dead_code)]
        fn set_has_peer(&self, has_peer: bool) {
            self.has_peer.store(has_peer, Ordering::SeqCst);
        }

        fn set_block_send(&self, block: bool) {
            self.block_send.store(block, Ordering::SeqCst);
        }

        fn get_sent_events(&self) -> Vec<Event> {
            self.sent_events
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clone()
        }

        #[allow(dead_code)]
        fn clear_sent_events(&self) {
            self.sent_events
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clear();
        }
    }

    impl NetworkState for MockNetworkState {
        fn broadcast_packet(&self, events: Vec<Event>) -> anyhow::Result<()> {
            if self.block_send.load(Ordering::SeqCst) {
                return Err(anyhow::anyhow!("send blocked"));
            }
            self.sent_events
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .extend(events);
            Ok(())
        }

        fn active_target(&self) -> Option<SocketAddr> {
            if self.has_peer.load(Ordering::SeqCst) {
                self.active_target_addr
            } else {
                None
            }
        }

        fn has_any_peer(&self) -> bool {
            self.has_peer.load(Ordering::SeqCst)
        }
    }

    #[test]
    fn test_suppress_flag_prevents_echo() {
        let clipboard = Arc::new(MockClipboardManager::new("initial".to_string()));
        let network = Arc::new(MockNetworkState::new());
        let sequence = Arc::new(ClipboardSequence::new());
        let rt = tokio::runtime::Runtime::new().unwrap();
        let sync = ClipboardSync::new(
            Arc::clone(&clipboard) as Arc<dyn ClipboardManager>,
            Arc::clone(&network) as Arc<dyn NetworkState>,
            Arc::clone(&sequence),
            rt.handle().clone(),
            Duration::from_millis(100),
            Duration::from_secs(2),
        );

        sync.handle_received_clipboard_text(
            "remote content".to_string(),
            1,
            test_hash("remote content"),
            1000,
        );

        assert!(sync.fsm().is_suppress_next());

        rt.block_on(async {
            sync.poll_clipboard().await;
        });

        let sent = network.get_sent_events();
        let clipboard_texts: Vec<_> = sent
            .iter()
            .filter(|e| matches!(e, Event::ClipboardText { .. }))
            .collect();
        assert!(
            clipboard_texts.is_empty(),
            "Expected no ClipboardText events due to suppress flag"
        );
    }

    #[test]
    fn test_hash_tracking_prevents_duplicate_send() {
        let clipboard = Arc::new(MockClipboardManager::new("stable content".to_string()));
        let network = Arc::new(MockNetworkState::new());
        let sequence = Arc::new(ClipboardSequence::new());
        let rt = tokio::runtime::Runtime::new().unwrap();
        let sync = ClipboardSync::new(
            Arc::clone(&clipboard) as Arc<dyn ClipboardManager>,
            Arc::clone(&network) as Arc<dyn NetworkState>,
            Arc::clone(&sequence),
            rt.handle().clone(),
            Duration::from_millis(100),
            Duration::from_secs(2),
        );

        sync.fsm().update_hash(test_hash("stable content"));
        rt.block_on(async {
            sync.poll_clipboard().await;
        });

        let sent = network.get_sent_events();
        assert!(sent.is_empty(), "Expected no events for unchanged content");
    }

    #[test]
    fn test_hash_change_triggers_send() {
        let clipboard = Arc::new(MockClipboardManager::new("old content".to_string()));
        let network = Arc::new(MockNetworkState::new());
        let sequence = Arc::new(ClipboardSequence::new());
        let rt = tokio::runtime::Runtime::new().unwrap();
        let sync = ClipboardSync::new(
            Arc::clone(&clipboard) as Arc<dyn ClipboardManager>,
            Arc::clone(&network) as Arc<dyn NetworkState>,
            Arc::clone(&sequence),
            rt.handle().clone(),
            Duration::from_millis(100),
            Duration::from_secs(2),
        );

        sync.fsm().update_hash(test_hash("different content"));
        sync.fsm().set_syncing();
        rt.block_on(async {
            sync.poll_clipboard().await;
        });

        let sent = network.get_sent_events();
        assert_eq!(sent.len(), 1, "Expected one event for changed content");

        if let Event::ClipboardText { content, .. } = &sent[0] {
            assert_eq!(content, "old content");
        } else {
            panic!("Expected ClipboardText event");
        }
    }

    #[test]
    fn test_peer_disconnect_clears_state() {
        let clipboard = Arc::new(MockClipboardManager::new("content".to_string()));
        let network = Arc::new(MockNetworkState::new());
        let sequence = Arc::new(ClipboardSequence::new());
        let rt = tokio::runtime::Runtime::new().unwrap();
        let sync = ClipboardSync::new(
            Arc::clone(&clipboard) as Arc<dyn ClipboardManager>,
            Arc::clone(&network) as Arc<dyn NetworkState>,
            Arc::clone(&sequence),
            rt.handle().clone(),
            Duration::from_millis(100),
            Duration::from_secs(2),
        );

        sync.notify_peer_disconnected();

        assert!(matches!(sync.fsm().state(), SyncState::Idle));
        assert!(!sync.fsm().has_pending_remote());
        assert!(sync.fsm().last_hash().is_none());
    }

    #[test]
    fn test_ack_received_clears_pending() {
        let clipboard = Arc::new(MockClipboardManager::new("content".to_string()));
        let network = Arc::new(MockNetworkState::new());
        let sequence = Arc::new(ClipboardSequence::new());
        let rt = tokio::runtime::Runtime::new().unwrap();
        let sync = ClipboardSync::new(
            Arc::clone(&clipboard) as Arc<dyn ClipboardManager>,
            Arc::clone(&network) as Arc<dyn NetworkState>,
            Arc::clone(&sequence),
            rt.handle().clone(),
            Duration::from_millis(100),
            Duration::from_secs(2),
        );

        sync.fsm().set_awaiting_ack(42, 123);

        sync.handle_received_ack(42);

        assert!(matches!(sync.fsm().state(), SyncState::Syncing));
        assert!(!sync.fsm().has_pending_remote());
    }

    #[test]
    fn test_stale_ack_ignored() {
        let clipboard = Arc::new(MockClipboardManager::new("content".to_string()));
        let network = Arc::new(MockNetworkState::new());
        let sequence = Arc::new(ClipboardSequence::new());
        let rt = tokio::runtime::Runtime::new().unwrap();
        let sync = ClipboardSync::new(
            Arc::clone(&clipboard) as Arc<dyn ClipboardManager>,
            Arc::clone(&network) as Arc<dyn NetworkState>,
            Arc::clone(&sequence),
            rt.handle().clone(),
            Duration::from_millis(100),
            Duration::from_secs(2),
        );

        sync.fsm().set_awaiting_ack(42, 123);

        sync.handle_received_ack(999);

        assert!(matches!(sync.fsm().state(), SyncState::AwaitingAck(42)));
    }

    #[test]
    fn test_hash_integrity_check() {
        let clipboard = Arc::new(MockClipboardManager::new("content".to_string()));
        let network = Arc::new(MockNetworkState::new());
        let sequence = Arc::new(ClipboardSequence::new());
        let rt = tokio::runtime::Runtime::new().unwrap();
        let sync = ClipboardSync::new(
            Arc::clone(&clipboard) as Arc<dyn ClipboardManager>,
            Arc::clone(&network) as Arc<dyn NetworkState>,
            Arc::clone(&sequence),
            rt.handle().clone(),
            Duration::from_millis(100),
            Duration::from_secs(2),
        );

        sync.handle_received_clipboard_text(
            "content".to_string(),
            1,
            test_hash("different content"),
            1000,
        );

        let current = clipboard
            .get_text
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        assert_eq!(current, Some("content".to_string()));
    }

    #[test]
    fn test_send_failure_reverts_state() {
        let clipboard = Arc::new(MockClipboardManager::new("content".to_string()));
        let network = Arc::new(MockNetworkState::new());
        let sequence = Arc::new(ClipboardSequence::new());
        let rt = tokio::runtime::Runtime::new().unwrap();
        let sync = ClipboardSync::new(
            Arc::clone(&clipboard) as Arc<dyn ClipboardManager>,
            Arc::clone(&network) as Arc<dyn NetworkState>,
            Arc::clone(&sequence),
            rt.handle().clone(),
            Duration::from_millis(100),
            Duration::from_secs(2),
        );

        network.set_block_send(true);
        sync.fsm().set_syncing();

        rt.block_on(async {
            sync.poll_clipboard().await;
        });

        assert!(matches!(sync.fsm().state(), SyncState::Syncing));
    }

    #[test]
    fn test_disabled_sync_skips_poll_and_receive() {
        let clipboard = Arc::new(MockClipboardManager::new("local content".to_string()));
        let network = Arc::new(MockNetworkState::new());
        let sequence = Arc::new(ClipboardSequence::new());
        let rt = tokio::runtime::Runtime::new().unwrap();
        let sync = ClipboardSync::new(
            Arc::clone(&clipboard) as Arc<dyn ClipboardManager>,
            Arc::clone(&network) as Arc<dyn NetworkState>,
            Arc::clone(&sequence),
            rt.handle().clone(),
            Duration::from_millis(100),
            Duration::from_secs(2),
        );

        assert!(sync.is_enabled());
        sync.set_enabled(false);
        assert!(!sync.is_enabled());

        sync.fsm().set_syncing();
        rt.block_on(async {
            sync.poll_clipboard().await;
        });
        assert!(network.get_sent_events().is_empty());

        sync.handle_received_clipboard_text(
            "remote content".to_string(),
            1,
            test_hash("remote content"),
            1000,
        );
        let current = clipboard
            .get_text
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        assert_eq!(current, Some("local content".to_string()));
        assert!(network.get_sent_events().is_empty());

        sync.set_enabled(true);
        sync.handle_received_clipboard_text(
            "remote content".to_string(),
            1,
            test_hash("remote content"),
            1000,
        );
        let current = clipboard
            .get_text
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        assert_eq!(current, Some("remote content".to_string()));
        assert!(!network.get_sent_events().is_empty());
    }

    #[test]
    fn test_clipboard_sync_advances_timestamp_past_skewed_remote() {
        let clipboard = Arc::new(MockClipboardManager::new("initial".to_string()));
        let network = Arc::new(MockNetworkState::new());
        let sequence = Arc::new(ClipboardSequence::new());
        let rt = tokio::runtime::Runtime::new().unwrap();
        let sync = ClipboardSync::new(
            Arc::clone(&clipboard) as Arc<dyn ClipboardManager>,
            Arc::clone(&network) as Arc<dyn NetworkState>,
            Arc::clone(&sequence),
            rt.handle().clone(),
            Duration::from_millis(100),
            Duration::from_secs(2),
        );

        let future_remote_ts = sequence.timestamp() + 1_000_000_000;
        sync.handle_received_clipboard_text(
            "mac remote text".to_string(),
            1,
            test_hash("mac remote text"),
            future_remote_ts,
        );

        clipboard.set_text("windows local text".to_string());
        sync.fsm().set_syncing();

        rt.block_on(async {
            sync.poll_clipboard().await;
        });

        let sent = network.get_sent_events();
        let outbound = sent
            .iter()
            .find_map(|e| match e {
                Event::ClipboardText {
                    content, timestamp, ..
                } => Some((content.clone(), *timestamp)),
                _ => None,
            })
            .expect("Expected outbound ClipboardText event");

        assert_eq!(outbound.0, "windows local text");
        assert!(
            outbound.1 > future_remote_ts,
            "Expected outbound timestamp ({}) > future_remote_ts ({})",
            outbound.1,
            future_remote_ts
        );
    }

    #[test]
    fn test_sensitive_clipboard_none_is_silently_dropped() {
        let clipboard = Arc::new(MockClipboardManager::new("initial".to_string()));
        let network = Arc::new(MockNetworkState::new());
        let sequence = Arc::new(ClipboardSequence::new());
        let rt = tokio::runtime::Runtime::new().unwrap();
        let sync = ClipboardSync::new(
            Arc::clone(&clipboard) as Arc<dyn ClipboardManager>,
            Arc::clone(&network) as Arc<dyn NetworkState>,
            Arc::clone(&sequence),
            rt.handle().clone(),
            Duration::from_millis(100),
            Duration::from_secs(2),
        );

        *clipboard.get_text.lock().unwrap() = None;
        sync.fsm().set_syncing();

        rt.block_on(async {
            sync.poll_clipboard().await;
        });

        assert!(network.get_sent_events().is_empty());
    }
}
