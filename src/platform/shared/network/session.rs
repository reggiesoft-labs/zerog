//! Session state types for peer connections.

use crate::core::peer_state::ConnectionStatus;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SessionState {
    None,
    OutgoingPending,
    Connected,
}

pub(crate) fn session_state_to_connection_status(state: SessionState) -> ConnectionStatus {
    match state {
        SessionState::Connected => ConnectionStatus::Connected,
        SessionState::OutgoingPending | SessionState::None => ConnectionStatus::Disconnected,
    }
}
