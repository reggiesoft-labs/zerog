use crate::events::Event;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum EventClass {
    InputUnreliable,
    InputReliable,
    ReliableControl,
    UnreliableControl,
}

pub(crate) fn classify(event: &Event) -> EventClass {
    if is_input_event(event) {
        return if needs_reliable_transport(event) {
            EventClass::InputReliable
        } else {
            EventClass::InputUnreliable
        };
    }
    if needs_reliable_transport(event) {
        return EventClass::ReliableControl;
    }
    EventClass::UnreliableControl
}

pub(crate) fn is_input_event(event: &Event) -> bool {
    matches!(
        event,
        Event::MouseMove { .. }
            | Event::MouseButton { .. }
            | Event::MouseWheel { .. }
            | Event::KeyPress { .. }
            | Event::KeyRelease { .. }
    )
}

pub(crate) fn needs_reliable_transport(event: &Event) -> bool {
    matches!(
        event,
        Event::TakeControl { .. }
            | Event::TakeControlAck { .. }
            | Event::ReleaseControl
            | Event::KeyPress { .. }
            | Event::KeyRelease { .. }
            | Event::MouseButton { .. }
            | Event::ConnectionRequest { .. }
            | Event::ConnectionAccept { .. }
            | Event::ConnectionReject { .. }
            | Event::DisconnectNotify { .. }
            | Event::UnpairNotify { .. }
            | Event::PairingPending { .. }
            | Event::PairingApproved { .. }
            | Event::LayoutUpdate(_)
            | Event::ClipboardText { .. }
            | Event::ClipboardAck { .. }
            | Event::ClipboardSyncRequest { .. }
            | Event::FileOffer { .. }
            | Event::FileAccept { .. }
            | Event::FileReject { .. }
            | Event::FileChunk { .. }
            | Event::FileAck { .. }
            | Event::FileCancel { .. }
    )
}

pub(crate) fn is_idempotent_control_event(event: &Event) -> bool {
    matches!(
        event,
        Event::TakeControl { .. }
            | Event::ReleaseControl
            | Event::TakeControlAck { .. }
            | Event::DisconnectNotify { .. }
            | Event::UnpairNotify { .. }
            | Event::PairingPending { .. }
            | Event::PairingApproved { .. }
            | Event::LayoutUpdate(_)
            | Event::ClipboardText { .. }
            | Event::ClipboardAck { .. }
            | Event::ClipboardSyncRequest { .. }
    )
}

#[cfg(test)]
mod tests {
    use super::{EventClass, classify};
    use crate::events::Event;

    #[test]
    fn classification_keeps_motion_unreliable_and_controls_reliable() {
        assert_eq!(
            classify(&Event::MouseMove {
                dx: 1,
                dy: 0,
                total_dx: 1,
                total_dy: 0,
                motion_session_id: 1,
            }),
            EventClass::InputUnreliable
        );
        assert_eq!(
            classify(&Event::MouseButton {
                button: 0,
                pressed: true,
            }),
            EventClass::InputReliable
        );
        assert_eq!(
            classify(&Event::TakeControl {
                edge: crate::platform::PeerEdge::Right,
                initial_x: 0,
                initial_y: 0,
                receiver_position_bp: Some(5_000),
                motion_session_id: 1,
                return_motion: None,
                caps_lock: None,
            }),
            EventClass::ReliableControl
        );
        assert_eq!(
            classify(&Event::Heartbeat {
                state: crate::events::PeerState::LocalControl,
            }),
            EventClass::UnreliableControl
        );
    }
}
