//! Event types for peer updates emitted by the controller.

use crate::core::api::{ConfigSnapshot, PeerId, PeerSnapshot, StatusSnapshot};
pub use tokio::sync::broadcast::error::RecvError;

/// Peer update events emitted by the controller.
#[derive(Debug, Clone)]
pub enum PeerUpdate {
    /// New peers discovered on the network.
    Discovered(Vec<PeerSnapshot>),
    /// A peer was removed from the network.
    Removed(PeerId),
    /// A previously established connection was lost.
    ConnectionLost { peer_id: PeerId },
    /// Status changed (e.g., connection state, metrics).
    StatusChanged(StatusSnapshot),
    ConfigChanged(ConfigSnapshot),
    PairingRequested {
        peer_id: PeerId,
        name: String,
        platform: Option<String>,
        /// Short authentication string derived from both certificates. The user
        /// compares it out of band before approving, which is what detects a
        /// man in the middle during first-time pairing.
        code: Option<String>,
        /// True when this side must approve the pairing. The peer that dialed
        /// out only waits for the other side, but still shows the code so the
        /// two users have something to compare.
        can_respond: bool,
    },
    PairingResolved {
        peer_id: PeerId,
        approved: bool,
    },
}
