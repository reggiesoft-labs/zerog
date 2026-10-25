use anyhow::Result;
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU8, AtomicU64, Ordering};
use tracing::{debug, trace};

use crate::core::events::{Event, MotionCheckpoint};
use crate::platform::{CapturePolicy, EventInjection, PeerEdge, TransferMode};

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct HandoffRequest {
    pub(crate) edge: PeerEdge,
    pub(crate) receiver_position_bp: u16,
    pub(crate) motion_session_id: u64,
    pub(crate) return_motion: Option<MotionCheckpoint>,
    /// Caps Lock state of the sender at the moment of handoff.
    pub(crate) caps_lock: Option<bool>,
}

impl HandoffRequest {
    pub(crate) fn new(
        edge: PeerEdge,
        receiver_position_bp: u16,
        motion_session_id: u64,
        return_motion: Option<MotionCheckpoint>,
    ) -> Option<Self> {
        if motion_session_id == 0 {
            return None;
        }
        Some(Self {
            edge,
            receiver_position_bp,
            motion_session_id,
            return_motion,
            caps_lock: None,
        })
    }

    pub(crate) fn with_caps_lock(mut self, caps_lock: Option<bool>) -> Self {
        self.caps_lock = caps_lock;
        self
    }

    pub(crate) fn from_wire(
        edge: PeerEdge,
        receiver_position_bp: Option<u16>,
        motion_session_id: u64,
        return_motion: Option<MotionCheckpoint>,
        caps_lock: Option<bool>,
    ) -> Option<Self> {
        Self::new(
            edge,
            receiver_position_bp?,
            motion_session_id,
            return_motion,
        )
        .map(|request| request.with_caps_lock(caps_lock))
    }

    pub(crate) fn into_wire_event(self) -> Event {
        Event::TakeControl {
            edge: self.edge,
            initial_x: 0,
            initial_y: 0,
            receiver_position_bp: Some(self.receiver_position_bp),
            motion_session_id: self.motion_session_id,
            return_motion: self.return_motion,
            caps_lock: self.caps_lock,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct HandoffContext {
    pub(crate) peer_id: String,
    pub(crate) addr: SocketAddr,
    pub(crate) request_seq: u32,
    pub(crate) motion_session_id: u64,
    pub(crate) edge: PeerEdge,
    pub(crate) receiver_position_bp: u16,
    pub(crate) source_position: (i32, i32),
    pub(crate) started_us: u64,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) enum HandoffPhase {
    #[default]
    Idle,
    AwaitingAck {
        context: HandoffContext,
        timeout_reported: bool,
    },
    RemoteSession {
        peer_id: String,
        peer: SocketAddr,
        motion_session_id: u64,
    },
    ReceivingSession {
        peer_id: String,
        peer: SocketAddr,
        motion_session_id: u64,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum HandoffAck {
    Accepted,
    Rejected,
    Stale,
}

#[derive(Default)]
struct HandoffState {
    phase: HandoffPhase,
    handoff_source_position: Option<(i32, i32)>,
    last_edge_position: Option<(i32, i32)>,
}

pub(crate) struct CursorState {
    mode: AtomicU8,
    capture_policy: Arc<AtomicU8>,
    pressed_buttons: AtomicU8,
    last_transition_us: AtomicU64,
    handoff: Mutex<HandoffState>,
}

impl CursorState {
    pub(crate) fn new(capture_policy: Arc<AtomicU8>) -> Self {
        capture_policy.store(
            CapturePolicy::from_transfer_mode(TransferMode::LocalControl) as u8,
            Ordering::Relaxed,
        );
        Self {
            mode: AtomicU8::new(TransferMode::LocalControl as u8),
            capture_policy,
            pressed_buttons: AtomicU8::new(0),
            last_transition_us: AtomicU64::new(0),
            handoff: Mutex::new(HandoffState::default()),
        }
    }

    pub(crate) fn set_button_state(&self, button: u8, pressed: bool) {
        if button < 8 {
            let mask = 1u8 << button;
            if pressed {
                self.pressed_buttons.fetch_or(mask, Ordering::Relaxed);
            } else {
                self.pressed_buttons.fetch_and(!mask, Ordering::Relaxed);
            }
        }
    }

    pub(crate) fn has_pressed_buttons(&self) -> bool {
        self.pressed_buttons.load(Ordering::Relaxed) != 0
    }

    pub(crate) fn clear_pressed_buttons(&self) {
        self.pressed_buttons.store(0, Ordering::Relaxed);
    }

    pub(crate) fn mode(&self) -> TransferMode {
        TransferMode::from_u8(self.mode.load(Ordering::Relaxed))
    }

    pub(crate) fn last_transition_us(&self) -> u64 {
        self.last_transition_us.load(Ordering::Relaxed)
    }

    pub(crate) fn record_transition(&self, sample_us: u64) {
        self.last_transition_us.store(sample_us, Ordering::Release);
    }

    pub(crate) fn phase_name(&self) -> &'static str {
        let guard = self.handoff.lock().expect("handoff lock poisoned");
        match guard.phase {
            HandoffPhase::Idle => "Idle",
            HandoffPhase::AwaitingAck { .. } => "AwaitingAck",
            HandoffPhase::RemoteSession { .. } => "RemoteSession",
            HandoffPhase::ReceivingSession { .. } => "ReceivingSession",
        }
    }

    pub(crate) fn is_session_peer(&self, peer_id: Option<&str>, addr: SocketAddr) -> bool {
        let guard = self.handoff.lock().expect("handoff lock poisoned");
        match &guard.phase {
            HandoffPhase::AwaitingAck { context, .. } => {
                context.addr == addr || peer_id == Some(context.peer_id.as_str())
            }
            HandoffPhase::RemoteSession {
                peer_id: session_peer_id,
                peer,
                ..
            }
            | HandoffPhase::ReceivingSession {
                peer_id: session_peer_id,
                peer,
                ..
            } => *peer == addr || peer_id == Some(session_peer_id.as_str()),
            HandoffPhase::Idle => false,
        }
    }

    pub(crate) fn can_begin_outbound(&self) -> bool {
        if !self.mode().owns_cursor() {
            return false;
        }
        let guard = self.handoff.lock().expect("handoff lock poisoned");
        matches!(
            guard.phase,
            HandoffPhase::Idle | HandoffPhase::ReceivingSession { .. }
        )
    }

    pub(crate) fn begin_outbound(&self, context: HandoffContext) -> bool {
        if !self.mode().owns_cursor() {
            return false;
        }
        let mut guard = self.handoff.lock().expect("handoff lock poisoned");
        if !matches!(
            guard.phase,
            HandoffPhase::Idle | HandoffPhase::ReceivingSession { .. }
        ) {
            return false;
        }
        guard.last_edge_position = Some(context.source_position);
        guard.handoff_source_position = Some(context.source_position);
        guard.phase = HandoffPhase::AwaitingAck {
            context,
            timeout_reported: false,
        };
        true
    }

    #[allow(dead_code)]
    pub(crate) fn recovery_position(&self) -> Option<(i32, i32)> {
        self.handoff
            .lock()
            .expect("handoff lock poisoned")
            .handoff_source_position
    }

    #[allow(dead_code)]
    pub(crate) fn last_edge_position(&self) -> Option<(i32, i32)> {
        self.handoff
            .lock()
            .expect("handoff lock poisoned")
            .last_edge_position
    }

    #[allow(dead_code)]
    pub(crate) fn pending_handoff(&self) -> Option<HandoffContext> {
        match &self.handoff.lock().expect("handoff lock poisoned").phase {
            HandoffPhase::AwaitingAck { context, .. } => Some(context.clone()),
            _ => None,
        }
    }

    pub(crate) fn take_pending_handoff_and_recovery(
        &self,
    ) -> (Option<HandoffContext>, Option<(i32, i32)>) {
        let mut guard = self.handoff.lock().expect("handoff lock poisoned");
        let pending = match &guard.phase {
            HandoffPhase::AwaitingAck { context, .. } => Some(context.clone()),
            _ => None,
        };
        let recovery = guard.handoff_source_position.or(guard.last_edge_position);
        guard.phase = HandoffPhase::Idle;
        guard.handoff_source_position = None;
        (pending, recovery)
    }

    pub(crate) fn take_pending_handoff_for_peer(
        &self,
        peer_id: Option<&str>,
        addr: SocketAddr,
    ) -> (Option<HandoffContext>, Option<(i32, i32)>) {
        let mut guard = self.handoff.lock().expect("handoff lock poisoned");
        let pending = match &guard.phase {
            HandoffPhase::AwaitingAck { context, .. }
                if context.addr == addr || peer_id == Some(context.peer_id.as_str()) =>
            {
                Some(context.clone())
            }
            _ => None,
        };
        let recovery = guard.handoff_source_position.or(guard.last_edge_position);
        guard.phase = HandoffPhase::Idle;
        guard.handoff_source_position = None;
        (pending, recovery)
    }

    pub(crate) fn clear_handoff(&self) {
        let mut guard = self.handoff.lock().expect("handoff lock poisoned");
        if matches!(guard.phase, HandoffPhase::AwaitingAck { .. }) {
            guard.phase = HandoffPhase::Idle;
            guard.handoff_source_position = None;
        }
    }

    pub(crate) fn report_delayed_handoff(
        &self,
        now_us: u64,
        timeout_us: u64,
    ) -> Option<HandoffContext> {
        let mut guard = self.handoff.lock().expect("handoff lock poisoned");
        let HandoffPhase::AwaitingAck {
            context,
            timeout_reported,
        } = &mut guard.phase
        else {
            return None;
        };

        if *timeout_reported || now_us.saturating_sub(context.started_us) < timeout_us {
            return None;
        }

        *timeout_reported = true;
        Some(context.clone())
    }

    pub(crate) fn resolve_outbound(
        &self,
        peer_id: Option<&str>,
        addr: SocketAddr,
        request_seq: u32,
        accepted: bool,
    ) -> (HandoffAck, Option<HandoffContext>) {
        let mut guard = self.handoff.lock().expect("handoff lock poisoned");
        let HandoffPhase::AwaitingAck { context, .. } = &guard.phase else {
            return (HandoffAck::Stale, None);
        };

        let matches = context.request_seq == request_seq
            && (context.addr == addr || peer_id == Some(context.peer_id.as_str()));
        if !matches {
            return (HandoffAck::Stale, None);
        }

        let HandoffPhase::AwaitingAck { context, .. } =
            std::mem::replace(&mut guard.phase, HandoffPhase::Idle)
        else {
            unreachable!();
        };

        if accepted {
            guard.phase = HandoffPhase::RemoteSession {
                peer_id: context.peer_id.clone(),
                peer: context.addr,
                motion_session_id: context.motion_session_id,
            };
            (HandoffAck::Accepted, Some(context))
        } else {
            guard.phase = HandoffPhase::Idle;
            guard.handoff_source_position = None;
            (HandoffAck::Rejected, Some(context))
        }
    }

    pub(crate) fn begin_inbound(
        &self,
        peer_id: &str,
        peer: SocketAddr,
        motion_session_id: u64,
    ) -> bool {
        let mut guard = self.handoff.lock().expect("handoff lock poisoned");
        if let HandoffPhase::ReceivingSession {
            peer_id: current_peer_id,
            peer: current_peer,
            motion_session_id: current_id,
        } = &guard.phase
        {
            if current_peer_id == peer_id || *current_peer == peer {
                guard.phase = HandoffPhase::ReceivingSession {
                    peer_id: peer_id.to_string(),
                    peer,
                    motion_session_id: motion_session_id.max(*current_id),
                };
                return true;
            }
            return false;
        }

        guard.phase = HandoffPhase::ReceivingSession {
            peer_id: peer_id.to_string(),
            peer,
            motion_session_id,
        };
        true
    }

    pub(crate) fn recover(&self) {
        let mut guard = self.handoff.lock().expect("handoff lock poisoned");
        guard.phase = HandoffPhase::Idle;
        guard.handoff_source_position = None;
        self.clear_pressed_buttons();
    }

    pub(crate) fn set_mode(
        &self,
        next: TransferMode,
        injection: &dyn EventInjection,
    ) -> Result<()> {
        trace!(target: "mouse_thing::platform::shared::transition",
            domain = "runtime.transition",
            transition_key = "os_call",
            operation = "set_mode",
            from_mode = ?self.mode(),
            to_mode = ?next,
            "starting cursor mode change"
        );
        match next {
            TransferMode::RemoteControl => {
                trace!(target: "mouse_thing::platform::shared::transition",
                    domain = "runtime.transition",
                    transition_key = "os_call",
                    operation = "set_cursor_hidden",
                    hidden = true,
                    "changing cursor visibility"
                );
                injection.set_cursor_hidden(true)?;
                trace!(target: "mouse_thing::platform::shared::transition",
                    domain = "runtime.transition",
                    transition_key = "os_call",
                    operation = "lock_cursor_position",
                    "locking cursor position"
                );
                if let Err(error) = injection.lock_cursor_position() {
                    debug!(
                        domain = "runtime.transition",
                        transition_key = "os_call_failed",
                        operation = "lock_cursor_position",
                        error = %error,
                        "failed to lock cursor position"
                    );
                    let _ = injection.set_cursor_hidden(false);
                    return Err(error);
                }
            }
            TransferMode::LocalControl => {
                trace!(target: "mouse_thing::platform::shared::transition",
                    domain = "runtime.transition",
                    transition_key = "os_call",
                    operation = "unlock_cursor_position",
                    "unlocking cursor position"
                );
                injection.unlock_cursor_position()?;
                trace!(target: "mouse_thing::platform::shared::transition",
                    domain = "runtime.transition",
                    transition_key = "os_call",
                    operation = "set_cursor_hidden",
                    hidden = false,
                    "changing cursor visibility"
                );
                if let Err(error) = injection.set_cursor_hidden(false) {
                    debug!(
                        domain = "runtime.transition",
                        transition_key = "os_call_failed",
                        operation = "set_cursor_hidden",
                        error = %error,
                        "failed to show cursor"
                    );
                    let _ = injection.lock_cursor_position();
                    return Err(error);
                }
            }
        }
        self.mode.store(next as u8, Ordering::Release);
        self.capture_policy.store(
            CapturePolicy::from_transfer_mode(next) as u8,
            Ordering::Release,
        );
        Ok(())
    }

    pub(crate) fn transition_to_remote_control(
        &self,
        injection: &dyn EventInjection,
    ) -> Result<()> {
        let _ = injection.release_all_keys();
        self.clear_pressed_buttons();
        let result = self.set_mode(TransferMode::RemoteControl, injection);
        if result.is_err() {
            self.recover();
        }
        result
    }

    pub(crate) fn transition_to_local_control(
        &self,
        injection: &dyn EventInjection,
        initial_pos: Option<(i32, i32)>,
    ) -> Result<()> {
        if let Some(pos) = initial_pos {
            let mut guard = self.handoff.lock().expect("handoff lock poisoned");
            guard.last_edge_position = Some(pos);
        }
        self.recover();
        trace!(target: "mouse_thing::platform::shared::transition",
            domain = "runtime.transition",
            transition_key = "os_call",
            operation = "release_all_keys",
            "releasing keys before local control"
        );
        if let Err(error) = injection.release_all_keys() {
            debug!(
                domain = "runtime.transition",
                transition_key = "os_call_failed",
                operation = "release_all_keys",
                error = %error,
                "failed to release all keys"
            );
        }
        if let Some((x, y)) = initial_pos {
            trace!(target: "mouse_thing::platform::shared::transition",
                domain = "runtime.transition",
                transition_key = "os_call",
                operation = "restore_cursor_position",
                x,
                y,
                "restoring cursor position before exposing local control"
            );
            if let Err(error) = injection.restore_cursor_position(x, y) {
                debug!(
                    domain = "runtime.transition",
                    transition_key = "os_call_failed",
                    operation = "restore_cursor_position",
                    x,
                    y,
                    error = %error,
                    "failed to restore cursor position before exposing local control"
                );
                let _ = injection.unlock_cursor_position();
            }
        } else {
            trace!(target: "mouse_thing::platform::shared::transition",
                domain = "runtime.transition",
                transition_key = "os_call",
                operation = "unlock_cursor_position",
                "unlocking cursor position"
            );
            let _ = injection.unlock_cursor_position();
        }
        trace!(target: "mouse_thing::platform::shared::transition",
            domain = "runtime.transition",
            transition_key = "os_call",
            operation = "set_cursor_hidden",
            hidden = false,
            "changing cursor visibility"
        );
        if let Err(error) = injection.set_cursor_hidden(false) {
            debug!(
                domain = "runtime.transition",
                transition_key = "os_call_failed",
                operation = "set_cursor_hidden",
                error = %error,
                "failed to show cursor"
            );
        }
        self.mode.store(TransferMode::LocalControl as u8, Ordering::Release);
        self.capture_policy.store(
            CapturePolicy::from_transfer_mode(TransferMode::LocalControl) as u8,
            Ordering::Release,
        );
        if let Some((x, y)) = initial_pos {
            trace!(target: "mouse_thing::platform::shared::transition",
                domain = "runtime.transition",
                transition_key = "os_call",
                operation = "inject_mouse_move",
                x,
                y,
                "injecting initial mouse move to wake cursor rendering"
            );
            if let Err(error) = injection.inject_mouse_move(x, y) {
                debug!(
                    domain = "runtime.transition",
                    transition_key = "os_call_failed",
                    operation = "inject_mouse_move",
                    x,
                    y,
                    error = %error,
                    "failed to inject initial mouse move after local transition"
                );
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::platform::TransferMode;

    #[derive(Debug, PartialEq, Eq)]
    enum Operation {
        Hide(bool),
        Warp(i32, i32),
        Move(i32, i32),
        Lock,
        Unlock,
        ReleaseAllKeys,
    }

    #[derive(Default)]
    struct MockInjection {
        hidden_calls: Mutex<Vec<bool>>,
        warp_calls: Mutex<Vec<(i32, i32)>>,
        move_calls: Mutex<Vec<(i32, i32)>>,
        lock_calls: AtomicU64,
        unlock_calls: AtomicU64,
        keys_released: Mutex<bool>,
        operations: Mutex<Vec<Operation>>,
    }

    impl EventInjection for MockInjection {
        fn inject_mouse_move(&self, x: i32, y: i32) -> Result<()> {
            self.move_calls
                .lock()
                .expect("move_calls lock poisoned")
                .push((x, y));
            self.operations
                .lock()
                .expect("operations lock poisoned")
                .push(Operation::Move(x, y));
            Ok(())
        }
        fn inject_mouse_button(&self, _button: u8, _pressed: bool) -> Result<()> {
            Ok(())
        }
        fn inject_mouse_wheel(&self, _delta_x: i32, _delta_y: i32) -> Result<()> {
            Ok(())
        }
        fn inject_key_press(&self, _keycode: u32) -> Result<()> {
            Ok(())
        }
        fn inject_key_release(&self, _keycode: u32) -> Result<()> {
            Ok(())
        }
        fn set_cursor_hidden(&self, hidden: bool) -> Result<()> {
            self.hidden_calls
                .lock()
                .expect("hidden_calls lock poisoned")
                .push(hidden);
            self.operations
                .lock()
                .expect("operations lock poisoned")
                .push(Operation::Hide(hidden));
            Ok(())
        }
        fn warp_cursor(&self, x: i32, y: i32) -> Result<()> {
            self.warp_calls
                .lock()
                .expect("warp_calls lock poisoned")
                .push((x, y));
            self.operations
                .lock()
                .expect("operations lock poisoned")
                .push(Operation::Warp(x, y));
            Ok(())
        }
        fn lock_cursor_position(&self) -> Result<()> {
            self.lock_calls.fetch_add(1, Ordering::Relaxed);
            self.operations
                .lock()
                .expect("operations lock poisoned")
                .push(Operation::Lock);
            Ok(())
        }
        fn unlock_cursor_position(&self) -> Result<()> {
            self.unlock_calls.fetch_add(1, Ordering::Relaxed);
            self.operations
                .lock()
                .expect("operations lock poisoned")
                .push(Operation::Unlock);
            Ok(())
        }
        fn restore_cursor_position(&self, x: i32, y: i32) -> Result<()> {
            self.warp_cursor(x, y)?;
            self.unlock_cursor_position()
        }
        fn release_all_keys(&self) -> Result<()> {
            *self
                .keys_released
                .lock()
                .expect("keys_released lock poisoned") = true;
            self.operations
                .lock()
                .expect("operations lock poisoned")
                .push(Operation::ReleaseAllKeys);
            Ok(())
        }
    }

    fn peer() -> SocketAddr {
        "127.0.0.1:5000".parse().expect("valid test address")
    }

    fn sample_handoff(request_seq: u32, motion_session_id: u64) -> HandoffContext {
        HandoffContext {
            peer_id: "peer-b".to_string(),
            addr: peer(),
            request_seq,
            motion_session_id,
            edge: PeerEdge::Right,
            receiver_position_bp: 5_000,
            source_position: (1_919, 500),
            started_us: 100,
        }
    }

    #[test]
    fn new_mirrors_local_control_to_shared() {
        let shared = Arc::new(AtomicU8::new(255));
        let cursor = CursorState::new(Arc::clone(&shared));
        assert_eq!(cursor.mode(), TransferMode::LocalControl);
        assert_eq!(
            shared.load(Ordering::Relaxed),
            TransferMode::LocalControl as u8
        );
    }

    #[test]
    fn transition_to_remote_hides_and_locks() {
        let shared = Arc::new(AtomicU8::new(TransferMode::LocalControl as u8));
        let cursor = CursorState::new(Arc::clone(&shared));
        let injection = MockInjection::default();
        cursor
            .transition_to_remote_control(&injection)
            .expect("remote transition should succeed");
        assert_eq!(cursor.mode(), TransferMode::RemoteControl);
        assert_eq!(
            shared.load(Ordering::Relaxed),
            TransferMode::RemoteControl as u8
        );
        assert_eq!(*injection.hidden_calls.lock().unwrap(), vec![true]);
        assert_eq!(injection.lock_calls.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn transition_to_local_releases_warps_and_restores() {
        let shared = Arc::new(AtomicU8::new(TransferMode::LocalControl as u8));
        let cursor = CursorState::new(Arc::clone(&shared));
        let injection = MockInjection::default();
        cursor
            .transition_to_remote_control(&injection)
            .expect("remote transition should succeed");
        cursor
            .transition_to_local_control(&injection, Some((90, 45)))
            .expect("local transition should succeed");
        assert_eq!(cursor.mode(), TransferMode::LocalControl);
        assert_eq!(
            shared.load(Ordering::Relaxed),
            TransferMode::LocalControl as u8
        );
        assert_eq!(*injection.warp_calls.lock().unwrap(), vec![(90, 45)]);
        assert_eq!(*injection.move_calls.lock().unwrap(), vec![(90, 45)]);
        assert_eq!(*injection.hidden_calls.lock().unwrap(), vec![true, false]);
        assert_eq!(injection.unlock_calls.load(Ordering::Relaxed), 1);
        assert!(*injection.keys_released.lock().unwrap());
        assert_eq!(
            *injection.operations.lock().unwrap(),
            vec![
                Operation::ReleaseAllKeys,
                Operation::Hide(true),
                Operation::Lock,
                Operation::ReleaseAllKeys,
                Operation::Warp(90, 45),
                Operation::Unlock,
                Operation::Hide(false),
                Operation::Move(90, 45),
            ]
        );
    }

    #[test]
    fn button_tracking_sets_and_clears_state() {
        let shared = Arc::new(AtomicU8::new(TransferMode::LocalControl as u8));
        let cursor = CursorState::new(shared);

        assert!(!cursor.has_pressed_buttons());
        cursor.set_button_state(0, true);
        assert!(cursor.has_pressed_buttons());
        cursor.set_button_state(1, true);
        assert!(cursor.has_pressed_buttons());

        cursor.set_button_state(0, false);
        assert!(cursor.has_pressed_buttons());
        cursor.set_button_state(1, false);
        assert!(!cursor.has_pressed_buttons());

        cursor.set_button_state(2, true);
        assert!(cursor.has_pressed_buttons());
        cursor.recover();
        assert!(!cursor.has_pressed_buttons());
    }

    #[test]
    fn outbound_handoff_has_an_explicit_pending_phase() {
        let shared = Arc::new(AtomicU8::new(TransferMode::LocalControl as u8));
        let cursor = CursorState::new(shared);
        let handoff = sample_handoff(3, 8);

        assert!(cursor.begin_outbound(handoff.clone()));
        assert_eq!(cursor.phase_name(), "AwaitingAck");
        assert_eq!(cursor.pending_handoff(), Some(handoff));
    }

    #[test]
    fn rejected_or_stale_ack_never_leaves_remote_control_pending() {
        let shared = Arc::new(AtomicU8::new(TransferMode::LocalControl as u8));
        let cursor = CursorState::new(shared);
        assert!(cursor.begin_outbound(sample_handoff(3, 8)));

        assert_eq!(
            cursor.resolve_outbound(Some("peer-b"), peer(), 2, true).0,
            HandoffAck::Stale
        );
        assert_eq!(
            cursor.resolve_outbound(Some("peer-b"), peer(), 3, false).0,
            HandoffAck::Rejected
        );
        assert_eq!(cursor.phase_name(), "Idle");
    }

    #[test]
    fn accepted_ack_enters_remote_session() {
        let shared = Arc::new(AtomicU8::new(TransferMode::LocalControl as u8));
        let cursor = CursorState::new(shared);
        assert!(cursor.begin_outbound(sample_handoff(3, 8)));

        let (ack, context) = cursor.resolve_outbound(Some("peer-b"), peer(), 3, true);
        assert_eq!(ack, HandoffAck::Accepted);
        assert!(context.is_some());
        assert_eq!(cursor.phase_name(), "RemoteSession");
        assert_eq!(cursor.recovery_position(), Some((1_919, 500)));
        cursor.recover();
        assert_eq!(cursor.recovery_position(), None);
    }

    #[test]
    fn inbound_session_keeps_local_ownership_and_blocks_competing_handoffs() {
        let shared = Arc::new(AtomicU8::new(TransferMode::LocalControl as u8));
        let cursor = CursorState::new(shared);
        let competing_peer: SocketAddr = "127.0.0.1:5001".parse().unwrap();

        assert!(cursor.begin_inbound("peer-b", peer(), 8));
        assert!(cursor.begin_inbound("peer-b", peer(), 9));
        assert!(!cursor.begin_inbound("peer-c", competing_peer, 10));
        assert_eq!(cursor.mode(), TransferMode::LocalControl);
        assert_eq!(cursor.phase_name(), "ReceivingSession");
    }

    #[test]
    fn recovery_returns_to_idle_local_control_from_any_phase() {
        let shared = Arc::new(AtomicU8::new(TransferMode::LocalControl as u8));
        let cursor = CursorState::new(shared);
        assert!(cursor.begin_outbound(sample_handoff(3, 8)));
        cursor.recover();

        assert_eq!(cursor.mode(), TransferMode::LocalControl);
        assert_eq!(cursor.phase_name(), "Idle");
    }

    #[test]
    fn return_handoff_can_follow_an_inbound_session() {
        let shared = Arc::new(AtomicU8::new(TransferMode::LocalControl as u8));
        let cursor = CursorState::new(shared);
        assert!(cursor.begin_outbound(sample_handoff(3, 8)));
        assert_eq!(
            cursor.resolve_outbound(Some("peer-b"), peer(), 3, true).0,
            HandoffAck::Accepted
        );
        assert!(cursor.begin_inbound("peer-b", peer(), 9));
        assert_eq!(cursor.mode(), TransferMode::LocalControl);
        assert!(cursor.begin_outbound(sample_handoff(4, 10)));
    }

    #[test]
    fn inbound_handoff_accepted_during_awaiting_ack() {
        let shared = Arc::new(AtomicU8::new(TransferMode::LocalControl as u8));
        let cursor = CursorState::new(shared);
        assert!(cursor.begin_outbound(sample_handoff(3, 8)));
        assert_eq!(cursor.phase_name(), "AwaitingAck");

        assert!(cursor.begin_inbound("peer-b", peer(), 9));
        assert_eq!(cursor.phase_name(), "ReceivingSession");
    }

    #[test]
    fn last_edge_position_preserved_across_recovery() {
        let shared = Arc::new(AtomicU8::new(TransferMode::LocalControl as u8));
        let cursor = CursorState::new(shared);
        let injection = MockInjection::default();

        cursor
            .transition_to_local_control(&injection, Some((100, 200)))
            .unwrap();
        assert_eq!(cursor.last_edge_position(), Some((100, 200)));

        cursor.recover();
        assert_eq!(cursor.last_edge_position(), Some((100, 200)));

        let (pending, recovery) = cursor.take_pending_handoff_for_peer(Some("peer-b"), peer());
        assert_eq!(pending, None);
        assert_eq!(recovery, Some((100, 200)));
    }
}
