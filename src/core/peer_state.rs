use crate::core::platform::DisplayBounds;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransportReachability {
    Unknown,
    Reachable,
    TimedOut,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ConnectionStatus {
    #[default]
    Disconnected,
    Incoming,
    Connected,
}

#[derive(Debug, Clone)]
pub struct PeerRuntimeStateSnapshot {
    pub peer_id: String,
    pub name: String,
    pub addr: String,
    pub transport_reachability: TransportReachability,
    pub connection_status: ConnectionStatus,
    pub last_transport_rx_ms: Option<u64>,
    pub screen: Option<String>,
    pub virtual_bounds: Option<DisplayBounds>,
}

pub fn canonical_peer_id(peer_id: &str) -> String {
    peer_id.trim().to_ascii_lowercase()
}
