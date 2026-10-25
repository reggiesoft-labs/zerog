mod coordinator;
mod dialer;
mod discovery_handler;
mod loop_runner;
mod session_handler;
#[cfg(test)]
mod tests;

pub(super) use coordinator::RunningController;

use crate::core::discovery::{DiscoveredPeer, DiscoveryEvent};
use std::time::{Duration, Instant};

pub(super) const RECONNECT_BACKOFF: Duration = Duration::from_secs(1);

#[derive(Debug)]
pub(super) enum ControllerEvent {
    ConnectPeer {
        peer_id: String,
    },
    CancelConnect {
        peer_id: String,
    },
    RetryPeer {
        peer_id: String,
        retry_at: Instant,
    },
    ConnectFailed {
        peer_id: String,
        attempt: Instant,
    },
    PeerUnpaired {
        peer_id: String,
        attempt: Option<Instant>,
    },
    PairingDecision {
        peer_id: String,
        approved: bool,
    },
    OutboundPairingPending {
        discovered: DiscoveredPeer,
        connected: crate::core::controller::peers::ConnectedPeer,
        certificate: Vec<u8>,
        connection: std::sync::Arc<crate::quic::PeerConnection>,
        attempt: Instant,
    },
    Discovery(DiscoveryEvent),
}

pub(super) enum ControllerTaskEvent {
    None,
}

pub(super) struct RuntimeContext<'a> {
    pub(super) local_peer_id: &'a str,
    pub(super) peers: std::sync::Arc<crate::core::controller::peers::PeerStore>,
    pub(super) updates: std::sync::Arc<tokio::sync::broadcast::Sender<crate::core::controller::events::PeerUpdate>>,
    pub(super) connector: crate::core::controller::peer_connector::SharedPeerConnector,
    pub(super) session: std::sync::Arc<dyn crate::core::platform::SessionEngine>,
    pub(super) controller_tx: tokio::sync::mpsc::Sender<ControllerEvent>,
    pub(super) background_tasks: &'a mut tokio::task::JoinSet<ControllerTaskEvent>,
}
