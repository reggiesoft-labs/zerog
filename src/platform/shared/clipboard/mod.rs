pub(crate) mod sync_fsm;

pub use sync_fsm::{ClipboardFsm, PendingRemote, SyncState, compute_hash};

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use crate::core::events::{ClipboardSequence, Event};
use crate::core::platform::traits::{ClipboardManager, NetworkState};

pub struct ClipboardSync {
    clipboard: Arc<dyn ClipboardManager>,
    network: Arc<dyn NetworkState>,
    sequence: Arc<ClipboardSequence>,
    handle: tokio::runtime::Handle,
    fsm: ClipboardFsm,
    poll_interval: Duration,
    ack_timeout: Duration,
    stopped: AtomicBool,
    enabled: AtomicBool,
}

impl ClipboardSync {
    pub fn new(
        clipboard: Arc<dyn ClipboardManager>,
        network: Arc<dyn NetworkState>,
        sequence: Arc<ClipboardSequence>,
        handle: tokio::runtime::Handle,
        poll_interval: Duration,
        ack_timeout: Duration,
    ) -> Arc<Self> {
        Arc::new(Self {
            clipboard,
            network,
            sequence,
            handle,
            fsm: ClipboardFsm::new(),
            poll_interval,
            ack_timeout,
            stopped: AtomicBool::new(false),
            enabled: AtomicBool::new(true),
        })
    }

    pub fn set_enabled(&self, enabled: bool) {
        self.enabled.store(enabled, Ordering::Relaxed);
    }

    pub fn is_enabled(&self) -> bool {
        self.enabled.load(Ordering::Relaxed)
    }

    pub fn start(self: &Arc<Self>) -> tokio::task::JoinHandle<()> {
        let sync2 = Arc::clone(self);
        self.handle.spawn(async move {
            sync2.poll_loop().await;
        })
    }

    async fn poll_loop(self: &Arc<Self>) {
        let mut poll_timer = tokio::time::interval(self.poll_interval);

        loop {
            poll_timer.tick().await;

            if self.stopped.load(Ordering::Relaxed) {
                return;
            }

            if !self.is_enabled() {
                self.fsm.set_idle();
                continue;
            }

            if !self.network.has_any_peer() {
                self.fsm.set_idle();
                continue;
            }

            if matches!(self.fsm.state(), SyncState::Idle) {
                self.fsm.set_syncing();
            }

            self.poll_clipboard().await;
        }
    }

    pub(crate) async fn poll_clipboard(self: &Arc<Self>) {
        if !self.is_enabled() {
            return;
        }

        let state = self.fsm.state();
        match state {
            SyncState::Idle => return,
            SyncState::AwaitingAck(_) => return,
            SyncState::Syncing => {}
        }

        tracing::trace!("clipboard poll: checking for changes");

        let content = match self.clipboard.get_clipboard_text() {
            Ok(Some(text)) => text,
            Ok(None) => return,
            Err(e) => {
                tracing::warn!("Failed to read clipboard: {:?}", e);
                return;
            }
        };

        let hash = compute_hash(&content);

        if self.fsm.suppress_next_if_same_hash(hash) {
            tracing::debug!(
                hash,
                "clipboard unchanged after remote apply, suppressing echo"
            );
            return;
        }

        if self.fsm.should_skip_broadcast(hash) {
            tracing::trace!(hash, "clipboard content unchanged, skipping broadcast");
            return;
        }

        let sequence = self.sequence.next();
        let timestamp = self.fsm.prepare_broadcast(sequence, hash, self.sequence.timestamp());

        let event = Event::ClipboardText {
            content,
            sequence,
            content_hash: hash,
            timestamp,
        };

        if let Err(e) = self.network.broadcast_packet(vec![event]) {
            tracing::error!("Failed to send clipboard event: {:?}", e);
            self.fsm.set_syncing();
            return;
        }

        let sync_weak = Arc::downgrade(self);
        let sequence_for_timeout = sequence;
        let ack_timeout = self.ack_timeout;
        self.handle.spawn(async move {
            tokio::time::sleep(ack_timeout).await;
            if let Some(sync) = sync_weak.upgrade()
                && !sync.stopped.load(Ordering::Relaxed)
            {
                sync.handle_ack_timeout(sequence_for_timeout);
            }
        });
    }

    pub fn stop(&self) {
        self.stopped.store(true, Ordering::Release);
    }

    fn handle_ack_timeout(&self, sequence: u64) {
        self.fsm.handle_ack_timeout(sequence);
    }

    pub fn notify_peer_disconnected(&self) {
        self.fsm.notify_peer_disconnected();
    }

    pub fn handle_received_ack(&self, sequence: u64) {
        self.fsm.handle_received_ack(sequence);
    }

    pub fn handle_received_clipboard_text(
        &self,
        content: String,
        sequence: u64,
        hash: u64,
        timestamp: u64,
    ) {
        if !self.is_enabled() {
            tracing::debug!("ignoring remote clipboard: clipboard sharing is disabled");
            return;
        }

        if !self.fsm.check_timestamp_and_hash(timestamp, hash) {
            let last_ts = self.fsm.last_timestamp();
            let last_hash = self.fsm.last_hash();
            if timestamp < last_ts {
                tracing::debug!(
                    timestamp,
                    last_ts,
                    "ignoring remote clipboard: older timestamp"
                );
                return;
            }
            if timestamp == last_ts && Some(hash) <= last_hash {
                tracing::debug!(
                    timestamp,
                    hash,
                    "ignoring remote clipboard: equal timestamp with lower/equal hash"
                );
                return;
            }
        }

        let computed = compute_hash(&content);
        if computed != hash {
            tracing::warn!("Clipboard hash mismatch, ignoring");
            return;
        }

        self.fsm.apply_remote_content(timestamp, hash);

        if let Err(e) = self.clipboard.set_clipboard_text(&content) {
            tracing::error!("Failed to set clipboard: {:?}", e);
            self.fsm.clear_suppress();
            return;
        }

        let ack = Event::ClipboardAck {
            sequence,
            received_hash: hash,
        };
        let _ = self.network.broadcast_packet(vec![ack]);
    }

    #[cfg(test)]
    pub(crate) fn fsm(&self) -> &ClipboardFsm {
        &self.fsm
    }
}
