use std::sync::Mutex;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SyncState {
    #[default]
    Idle,
    Syncing,
    AwaitingAck(u64),
}

#[derive(Debug, Clone)]
pub struct PendingRemote {
    pub hash: u64,
}

#[derive(Debug, Default)]
struct ClipboardFsmState {
    state: SyncState,
    suppress_next: bool,
    last_local_hash: Option<u64>,
    last_timestamp: u64,
    pending_remote: Option<PendingRemote>,
}

pub struct ClipboardFsm {
    state: Mutex<ClipboardFsmState>,
}

impl ClipboardFsm {
    pub fn new() -> Self {
        Self {
            state: Mutex::new(ClipboardFsmState::default()),
        }
    }

    pub fn state(&self) -> SyncState {
        self.state.lock().unwrap_or_else(|e| e.into_inner()).state
    }

    pub fn set_idle(&self) {
        self.state.lock().unwrap_or_else(|e| e.into_inner()).state = SyncState::Idle;
    }

    pub fn set_syncing(&self) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        state.state = SyncState::Syncing;
        state.pending_remote = None;
    }

    pub fn prepare_broadcast(&self, sequence: u64, hash: u64, physical_now: u64) -> u64 {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let ts = std::cmp::max(physical_now, state.last_timestamp.saturating_add(1));
        state.last_timestamp = ts;
        state.state = SyncState::AwaitingAck(sequence);
        state.pending_remote = Some(PendingRemote { hash });
        ts
    }

    pub fn set_awaiting_ack(&self, sequence: u64, hash: u64) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        state.state = SyncState::AwaitingAck(sequence);
        state.pending_remote = Some(PendingRemote { hash });
    }

    pub fn handle_received_ack(&self, sequence: u64) -> bool {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if let SyncState::AwaitingAck(pending_seq) = state.state
            && sequence == pending_seq
        {
            state.state = SyncState::Syncing;
            if let Some(pending) = state.pending_remote.take() {
                state.last_local_hash = Some(pending.hash);
            }
            return true;
        }
        false
    }

    pub fn handle_ack_timeout(&self, sequence: u64) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if let SyncState::AwaitingAck(pending_seq) = state.state
            && sequence == pending_seq
        {
            state.state = SyncState::Syncing;
            state.pending_remote = None;
            state.last_local_hash = None;
        }
    }

    pub fn notify_peer_disconnected(&self) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        state.state = SyncState::Idle;
        state.pending_remote = None;
        state.last_local_hash = None;
        state.last_timestamp = 0;
        state.suppress_next = false;
    }

    pub fn suppress_next_if_same_hash(&self, hash: u64) -> bool {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if state.suppress_next {
            state.suppress_next = false;
            if state.last_local_hash == Some(hash) {
                return true;
            }
        }
        false
    }

    pub fn should_skip_broadcast(&self, hash: u64) -> bool {
        let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        state.last_local_hash == Some(hash)
    }

    pub fn update_hash(&self, hash: u64) {
        self.state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .last_local_hash = Some(hash);
    }

    pub fn update_timestamp(&self, timestamp: u64) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        state.last_timestamp = std::cmp::max(state.last_timestamp, timestamp);
    }

    pub fn check_timestamp_and_hash(&self, timestamp: u64, hash: u64) -> bool {
        let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if timestamp < state.last_timestamp {
            return false;
        }
        if timestamp == state.last_timestamp && Some(hash) < state.last_local_hash {
            return false;
        }
        true
    }

    pub fn apply_remote_content(&self, timestamp: u64, hash: u64) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        state.state = SyncState::Syncing;
        state.pending_remote = None;
        state.last_timestamp = std::cmp::max(state.last_timestamp, timestamp);
        state.last_local_hash = Some(hash);
        state.suppress_next = true;
    }

    pub fn clear_suppress(&self) {
        self.state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .suppress_next = false;
    }

    pub fn pending_hash(&self) -> Option<u64> {
        self.state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .pending_remote
            .as_ref()
            .map(|p| p.hash)
    }

    pub fn last_hash(&self) -> Option<u64> {
        self.state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .last_local_hash
    }

    pub fn last_timestamp(&self) -> u64 {
        self.state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .last_timestamp
    }

    #[cfg(test)]
    pub fn is_suppress_next(&self) -> bool {
        self.state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .suppress_next
    }

    #[cfg(test)]
    pub fn has_pending_remote(&self) -> bool {
        self.state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .pending_remote
            .is_some()
    }
}

impl Default for ClipboardFsm {
    fn default() -> Self {
        Self::new()
    }
}

pub fn compute_hash(content: &str) -> u64 {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut hasher = DefaultHasher::new();
    content.hash(&mut hasher);
    hasher.finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_initial_state_is_idle() {
        let fsm = ClipboardFsm::new();
        assert!(matches!(fsm.state(), SyncState::Idle));
    }

    #[test]
    fn test_set_syncing() {
        let fsm = ClipboardFsm::new();
        fsm.set_syncing();
        assert!(matches!(fsm.state(), SyncState::Syncing));
    }

    #[test]
    fn test_set_awaiting_ack() {
        let fsm = ClipboardFsm::new();
        fsm.set_awaiting_ack(42, 123);
        assert!(matches!(fsm.state(), SyncState::AwaitingAck(42)));
        assert_eq!(fsm.pending_hash(), Some(123));
    }

    #[test]
    fn test_ack_received_clears_pending() {
        let fsm = ClipboardFsm::new();
        fsm.set_awaiting_ack(42, 123);
        let result = fsm.handle_received_ack(42);
        assert!(result);
        assert!(matches!(fsm.state(), SyncState::Syncing));
        assert!(!fsm.has_pending_remote());
        assert_eq!(fsm.last_hash(), Some(123));
    }

    #[test]
    fn test_stale_ack_ignored() {
        let fsm = ClipboardFsm::new();
        fsm.set_awaiting_ack(42, 123);
        let result = fsm.handle_received_ack(999);
        assert!(!result);
        assert!(matches!(fsm.state(), SyncState::AwaitingAck(42)));
    }

    #[test]
    fn test_ack_timeout_clears_pending() {
        let fsm = ClipboardFsm::new();
        fsm.set_awaiting_ack(42, 123);
        fsm.handle_ack_timeout(42);
        assert!(matches!(fsm.state(), SyncState::Syncing));
        assert!(!fsm.has_pending_remote());
        assert_eq!(fsm.last_hash(), None);
    }

    #[test]
    fn test_stale_ack_timeout_ignored() {
        let fsm = ClipboardFsm::new();
        fsm.set_awaiting_ack(42, 123);
        fsm.handle_ack_timeout(999);
        assert!(matches!(fsm.state(), SyncState::AwaitingAck(42)));
        assert_eq!(fsm.pending_hash(), Some(123));
    }

    #[test]
    fn test_peer_disconnect_clears_state() {
        let fsm = ClipboardFsm::new();
        fsm.set_awaiting_ack(42, 123);
        fsm.update_timestamp(1000);
        fsm.apply_remote_content(2000, 456);

        fsm.notify_peer_disconnected();

        assert!(matches!(fsm.state(), SyncState::Idle));
        assert!(!fsm.has_pending_remote());
        assert_eq!(fsm.last_hash(), None);
        assert_eq!(fsm.last_timestamp(), 0);
        assert!(!fsm.is_suppress_next());
    }

    #[test]
    fn test_suppress_flag_prevents_echo() {
        let fsm = ClipboardFsm::new();
        fsm.update_hash(123);
        fsm.apply_remote_content(1000, 123);
        assert!(fsm.is_suppress_next());

        let suppressed = fsm.suppress_next_if_same_hash(123);
        assert!(suppressed);
        assert!(!fsm.is_suppress_next());
    }

    #[test]
    fn test_suppress_flag_allows_different_hash() {
        let fsm = ClipboardFsm::new();
        fsm.update_hash(123);
        fsm.apply_remote_content(1000, 123);

        let suppressed = fsm.suppress_next_if_same_hash(456);
        assert!(!suppressed);
    }

    #[test]
    fn test_should_skip_broadcast_when_hash_unchanged() {
        let fsm = ClipboardFsm::new();
        fsm.update_hash(123);

        assert!(fsm.should_skip_broadcast(123));
        assert!(!fsm.should_skip_broadcast(456));
    }

    #[test]
    fn test_check_timestamp_and_hash_rejects_older() {
        let fsm = ClipboardFsm::new();
        fsm.update_timestamp(1000);
        fsm.update_hash(100);

        assert!(!fsm.check_timestamp_and_hash(500, 200));
        assert!(fsm.check_timestamp_and_hash(1001, 200));
    }

    #[test]
    fn test_check_timestamp_and_hash_rejects_same_ts_lower_hash() {
        let fsm = ClipboardFsm::new();
        fsm.update_timestamp(1000);
        fsm.update_hash(200);

        assert!(!fsm.check_timestamp_and_hash(1000, 100));
        assert!(fsm.check_timestamp_and_hash(1000, 200));
        assert!(fsm.check_timestamp_and_hash(1000, 300));
    }

    #[test]
    fn test_prepare_broadcast_monotonic_with_clock_skew() {
        let fsm = ClipboardFsm::new();
        fsm.apply_remote_content(1000, 123);
        assert_eq!(fsm.last_timestamp(), 1000);

        let ts = fsm.prepare_broadcast(1, 456, 500);
        assert_eq!(ts, 1001);
        assert_eq!(fsm.last_timestamp(), 1001);
        assert!(matches!(fsm.state(), SyncState::AwaitingAck(1)));
        assert_eq!(fsm.pending_hash(), Some(456));
    }

    #[test]
    fn test_prepare_broadcast_advances_when_physical_now_is_ahead() {
        let fsm = ClipboardFsm::new();
        fsm.apply_remote_content(1000, 123);

        let ts = fsm.prepare_broadcast(1, 456, 2000);
        assert_eq!(ts, 2000);
        assert_eq!(fsm.last_timestamp(), 2000);
    }

    #[test]
    fn test_apply_remote_content_clears_pending_ack() {
        let fsm = ClipboardFsm::new();
        fsm.set_awaiting_ack(42, 123);
        assert!(matches!(fsm.state(), SyncState::AwaitingAck(42)));

        fsm.apply_remote_content(1000, 456);
        assert!(matches!(fsm.state(), SyncState::Syncing));
        assert!(!fsm.has_pending_remote());
        assert_eq!(fsm.last_hash(), Some(456));

        assert!(!fsm.handle_received_ack(42));
        assert_eq!(fsm.last_hash(), Some(456));
    }

    #[test]
    fn test_hash_computation() {
        let h1 = compute_hash("hello");
        let h2 = compute_hash("hello");
        let h3 = compute_hash("world");

        assert_eq!(h1, h2);
        assert_ne!(h1, h3);
    }
}
