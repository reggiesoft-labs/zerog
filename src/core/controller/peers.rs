use crate::core::api::{
    DiscoveryStatusSnapshot, DisplayRectSnapshot, PeerSnapshot, VirtualBoundsSnapshot,
};
use crate::core::discovery::DiscoveredPeer;
use crate::core::peer_state::{
    ConnectionStatus, PeerRuntimeStateSnapshot, TransportReachability,
};
use super::config_state::ConfigStore;
use crate::core::platform::{DisplayBounds, EdgeAssignments, types::PeerEdge};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::RwLock;
use std::time::Instant;
#[derive(Debug, Clone)]
pub(super) struct DiscoveredPeerInfo {
    pub(super) peer_id: String,
    pub(super) primary_addr: SocketAddr,
    pub(super) addrs: Vec<SocketAddr>,
    pub(super) hostname: String,
    pub(super) fullname: String,
    pub(super) platform: Option<String>,
}

/// Result of a fully connected peer after the outbound flow completes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ConnectedPeerDirection {
    Inbound,
    Outbound,
}

/// Result of a fully connected peer after the outbound flow completes.
#[derive(Debug, Clone)]
pub(super) struct ConnectedPeer {
    /// Stable identity returned by the remote handshake.
    pub(super) peer_id: String,
    /// User-visible peer name.
    pub(super) name: String,
    /// Dialable listen address of the peer.
    pub(super) addr: SocketAddr,
    /// Active transient connection socket address, if distinct from `addr`.
    pub(super) conn_addr: Option<SocketAddr>,
    /// Virtual display bounds of the peer.
    pub(super) virtual_bounds: Option<DisplayBounds>,
    /// Individual display rectangles that make up the peer's desktop shape.
    pub(super) display_rects: Vec<DisplayBounds>,
    /// Whether the canonical session was accepted inbound or initiated outbound.
    pub(super) direction: ConnectedPeerDirection,
    /// Remote operating system platform.
    pub(super) platform: Option<String>,
}

impl DiscoveredPeerInfo {
    fn from_connected(connected: &ConnectedPeer) -> Self {
        Self {
            peer_id: connected.peer_id.clone(),
            primary_addr: connected.addr,
            addrs: vec![connected.addr],
            hostname: connected.name.clone(),
            fullname: format!("{}.{}", connected.peer_id, crate::core::policies::DISCOVERY_SERVICE_TYPE),
            platform: connected.platform.clone(),
        }
    }
}

impl From<DiscoveredPeer> for DiscoveredPeerInfo {
    fn from(peer: DiscoveredPeer) -> Self {
        Self {
            peer_id: peer.peer_id,
            primary_addr: peer.primary_addr,
            addrs: peer.addrs,
            hostname: peer.hostname,
            fullname: peer.fullname,
            platform: peer.platform,
        }
    }
}

impl From<DiscoveredPeerInfo> for DiscoveredPeer {
    fn from(peer: DiscoveredPeerInfo) -> Self {
        Self {
            peer_id: peer.peer_id,
            primary_addr: peer.primary_addr,
            addrs: peer.addrs,
            hostname: peer.hostname,
            fullname: peer.fullname,
            platform: peer.platform,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(super) enum DialState {
    #[default]
    Idle,
    Connecting(Instant),
    Pairing,
    Backoff(Instant),
}

impl DialState {
    pub(super) fn is_dial_in_flight(&self) -> bool {
        matches!(self, DialState::Connecting(_) | DialState::Pairing)
    }

    pub(super) fn is_pairing(&self) -> bool {
        matches!(self, DialState::Pairing)
    }
}

#[derive(Debug, Default)]
struct PeerLifecycle {
    discovered: Option<DiscoveredPeerInfo>,
    connected: Option<ConnectedPeer>,
    dial_state: DialState,
}

#[derive(Debug, Default)]
struct PeerRegistry {
    peers: HashMap<String, PeerLifecycle>,
    configured_edges: HashMap<String, PeerEdge>,
}

pub(super) struct PeerStore {
    registry: RwLock<PeerRegistry>,
    config_store: ConfigStore,
}

impl Default for PeerStore {
    fn default() -> Self {
        Self::new(ConfigStore::default())
    }
}

impl PeerStore {
    pub(super) fn new(config_store: ConfigStore) -> Self {
        Self {
            registry: RwLock::new(PeerRegistry::default()),
            config_store,
        }
    }

    pub(super) fn is_paired(&self, peer_id: &str) -> bool {
        self.config_store.snapshot().is_peer_paired(peer_id)
    }

    pub(super) fn remember_discovered(&self, peer: DiscoveredPeer) {
        let mut registry = self.registry.write().unwrap_or_else(|e| e.into_inner());
        let peer_id = peer.peer_id.clone();
        let entry = registry.peers.entry(peer_id).or_default();
        if let Some(existing) = &mut entry.discovered {
            for addr in peer.addrs {
                if !existing.addrs.contains(&addr) {
                    existing.addrs.push(addr);
                }
            }
            existing.primary_addr = peer.primary_addr;
            existing.hostname = peer.hostname;
            existing.fullname = peer.fullname;
            if peer.platform.is_some() {
                existing.platform = peer.platform;
            }
        } else {
            entry.discovered = Some(peer.into());
        }
    }

    pub(super) fn expire_discovered(&self, peer_id: &str) {
        let mut registry = self.registry.write().unwrap_or_else(|e| e.into_inner());
        if let Some(peer) = registry.peers.get_mut(peer_id) {
            peer.discovered = None;
            if peer_id.starts_with("direct-") {
                peer.dial_state = DialState::Idle;
            }
            if peer.connected.is_none() && matches!(peer.dial_state, DialState::Idle) {
                registry.peers.remove(peer_id);
            }
        }
    }

    pub(super) fn remove_peer(&self, peer_id: &str) {
        let mut registry = self.registry.write().unwrap_or_else(|e| e.into_inner());
        registry.peers.remove(peer_id);
    }

    pub(super) fn find_peer_by_addr(&self, addr: SocketAddr) -> Option<String> {
        let registry = self.registry.read().unwrap_or_else(|e| e.into_inner());
        for (peer_id, lifecycle) in &registry.peers {
            if peer_id.starts_with("direct-") {
                continue;
            }
            if let Some(connected) = &lifecycle.connected {
                if connected.addr == addr || connected.conn_addr == Some(addr) {
                    return Some(peer_id.clone());
                }
            }
            if let Some(discovered) = &lifecycle.discovered {
                if discovered.primary_addr == addr || discovered.addrs.contains(&addr) {
                    return Some(peer_id.clone());
                }
            }
        }
        None
    }

    pub(super) fn discovered_peer(&self, peer_id: &str) -> Option<DiscoveredPeerInfo> {
        self.registry
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .peers
            .get(peer_id)
            .and_then(|peer| peer.discovered.clone())
    }

    pub(super) fn is_discovered(&self, peer_id: &str) -> bool {
        self.registry
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .peers
            .get(peer_id)
            .is_some_and(|peer| peer.discovered.is_some())
    }

    pub(super) fn is_connected(&self, peer_id: &str) -> bool {
        self.registry
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .peers
            .get(peer_id)
            .is_some_and(|peer| peer.connected.is_some())
    }

    pub(super) fn is_empty(&self) -> bool {
        self.registry
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .peers
            .is_empty()
    }

    pub(super) fn remember_connected(&self, connected: ConnectedPeer) {
        let mut registry = self.registry.write().unwrap_or_else(|e| e.into_inner());
        let peer = registry.peers.entry(connected.peer_id.clone()).or_default();
        match &mut peer.discovered {
            Some(discovered) => discovered.primary_addr = connected.addr,
            None => peer.discovered = Some(DiscoveredPeerInfo::from_connected(&connected)),
        }
        peer.connected = Some(connected);
        peer.dial_state = DialState::Idle;
    }

    pub(super) fn remove_connected(&self, peer_id: &str) -> Option<ConnectedPeer> {
        let mut registry = self.registry.write().unwrap_or_else(|e| e.into_inner());
        let removed = registry
            .peers
            .get_mut(peer_id)
            .and_then(|peer| peer.connected.take());
        Self::prune_peer_if_idle(&mut registry, peer_id);
        removed
    }

    pub(super) fn connected(&self, peer_id: &str) -> Option<ConnectedPeer> {
        self.registry
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .peers
            .get(peer_id)
            .and_then(|peer| peer.connected.clone())
    }

    pub(super) fn connected_peer_ids(&self) -> Vec<String> {
        self.registry
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .peers
            .iter()
            .filter_map(|(peer_id, peer)| peer.connected.as_ref().map(|_| peer_id.clone()))
            .collect()
    }

    pub(super) fn discovered_peer_ids(&self) -> Vec<String> {
        self.registry
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .peers
            .iter()
            .filter_map(|(peer_id, peer)| peer.discovered.as_ref().map(|_| peer_id.clone()))
            .collect()
    }

    pub(super) fn dial_state(&self, peer_id: &str) -> DialState {
        self.registry
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .peers
            .get(peer_id)
            .map(|peer| peer.dial_state)
            .unwrap_or(DialState::Idle)
    }

    pub(super) fn set_connecting(&self, peer_id: &str) -> Instant {
        let attempt = Instant::now();
        self.set_dial_state(peer_id, DialState::Connecting(attempt));
        attempt
    }

    pub(super) fn set_pairing(&self, peer_id: &str) {
        self.set_dial_state(peer_id, DialState::Pairing);
    }

    pub(super) fn set_backoff(&self, peer_id: &str, until: Instant) {
        self.set_dial_state(peer_id, DialState::Backoff(until));
    }

    pub(super) fn set_idle(&self, peer_id: &str) {
        self.set_dial_state(peer_id, DialState::Idle);
    }

    pub(super) fn prepare_for_restart(&self) {
        let mut registry = self.registry.write().unwrap_or_else(|e| e.into_inner());
        for (_peer_id, lifecycle) in &mut registry.peers {
            if lifecycle.discovered.is_none()
                && let Some(connected) = &lifecycle.connected
            {
                lifecycle.discovered = Some(DiscoveredPeerInfo::from_connected(connected));
            }

            lifecycle.connected = None;
            lifecycle.dial_state = DialState::Idle;
        }

        registry
            .peers
            .retain(|_, lifecycle| lifecycle.discovered.is_some());
    }

    fn set_dial_state(&self, peer_id: &str, dial_state: DialState) {
        let mut registry = self.registry.write().unwrap_or_else(|e| e.into_inner());
        registry
            .peers
            .entry(peer_id.to_string())
            .or_default()
            .dial_state = dial_state;
        Self::prune_peer_if_idle(&mut registry, peer_id);
    }

    fn prune_peer_if_idle(registry: &mut PeerRegistry, peer_id: &str) {
        let should_remove = registry.peers.get(peer_id).is_some_and(|peer| {
            peer.discovered.is_none()
                && peer.connected.is_none()
                && matches!(peer.dial_state, DialState::Idle)
        });
        if should_remove {
            registry.peers.remove(peer_id);
        }
    }

    pub(super) fn snapshots(&self, local_peer_id: &str) -> Vec<PeerSnapshot> {
        let registry = self.registry.read().unwrap_or_else(|e| e.into_inner());
        let mut peers = registry
            .peers
            .iter()
            .filter(|(peer_id, _)| peer_id.as_str() != local_peer_id)
            .filter_map(|(peer_id, lifecycle)| {
                let (name, hostname, addr, virtual_bounds, display_rects, platform) =
                    if let Some(connected) = &lifecycle.connected {
                        let hostname = lifecycle
                            .discovered
                            .as_ref()
                            .map(|d| d.hostname.clone())
                            .unwrap_or_default();
                        (
                            connected.name.clone(),
                            hostname,
                            connected.addr.to_string(),
                            connected.virtual_bounds,
                            connected.display_rects.clone(),
                            connected.platform.clone().or_else(|| {
                                lifecycle
                                    .discovered
                                    .as_ref()
                                    .and_then(|d| d.platform.clone())
                            }),
                        )
                    } else if let Some(discovered) = &lifecycle.discovered {
                        (
                            discovered.hostname.clone(),
                            discovered.hostname.clone(),
                            discovered.primary_addr.to_string(),
                            None,
                            Vec::new(),
                            discovered.platform.clone(),
                        )
                    } else {
                        return None;
                    };

                let is_connected = lifecycle.connected.is_some();
                let is_paired = self.is_paired(peer_id);
                let connection_status = if is_connected {
                    Some("connected".to_string())
                } else {
                    match lifecycle.dial_state {
                        DialState::Connecting(_) => Some("connecting".to_string()),
                        DialState::Pairing => Some("pairing".to_string()),
                        DialState::Backoff(_) => Some("backoff".to_string()),
                        DialState::Idle => Some("disconnected".to_string()),
                    }
                };

                Some(PeerSnapshot {
                    peer_id: peer_id.clone(),
                    name,
                    hostname,
                    addr,
                    platform,
                    screen: None,
                    virtual_bounds: virtual_bounds.map(virtual_bounds_snapshot),
                    display_rects: display_rects
                        .into_iter()
                        .map(display_rect_snapshot)
                        .collect(),
                    last_seen_ms: 0,
                    discovery_status: DiscoveryStatusSnapshot::Online,
                    is_local: false,
                    connected: is_connected,
                    paired: is_paired,
                    connection_status,
                })
            })
            .collect::<Vec<_>>();

        let real_addrs: std::collections::HashSet<String> = peers
            .iter()
            .filter(|s| !s.peer_id.starts_with("direct-"))
            .map(|s| s.addr.clone())
            .collect();
        peers.retain(|snapshot| {
            if !snapshot.peer_id.starts_with("direct-") {
                return true;
            }
            !real_addrs.contains(&snapshot.addr)
        });

        peers.sort_by(|a, b| a.name.cmp(&b.name).then(a.peer_id.cmp(&b.peer_id)));
        peers
    }

    pub(super) fn runtime_snapshots(
        &self,
        runtime_peers: &[PeerRuntimeStateSnapshot],
        local_peer_id: &str,
    ) -> Vec<PeerSnapshot> {
        let mut peer_map: HashMap<String, PeerSnapshot> = self
            .snapshots(local_peer_id)
            .into_iter()
            .map(|snapshot| (snapshot.peer_id.clone(), snapshot))
            .collect();

        for runtime in runtime_peers {
            if runtime.peer_id == local_peer_id {
                continue;
            }

            let is_paired = self.is_paired(&runtime.peer_id);
            let is_connected = matches!(runtime.connection_status, ConnectionStatus::Connected);
            let conn_status_str = match runtime.connection_status {
                ConnectionStatus::Connected => Some("connected".to_string()),
                ConnectionStatus::Incoming => Some("incoming".to_string()),
                ConnectionStatus::Disconnected => Some("disconnected".to_string()),
            };

            let entry = peer_map.entry(runtime.peer_id.clone()).or_insert_with(|| {
                PeerSnapshot {
                    peer_id: runtime.peer_id.clone(),
                    name: runtime.name.clone(),
                    hostname: String::new(),
                    addr: runtime.addr.clone(),
                    platform: None,
                    screen: runtime.screen.clone(),
                    virtual_bounds: runtime.virtual_bounds.map(virtual_bounds_snapshot),
                    display_rects: Vec::new(),
                    last_seen_ms: runtime.last_transport_rx_ms.unwrap_or(0),
                    discovery_status: DiscoveryStatusSnapshot::Online,
                    is_local: false,
                    connected: is_connected,
                    paired: is_paired,
                    connection_status: conn_status_str.clone(),
                }
            });

            if !runtime.name.is_empty() {
                entry.name = runtime.name.clone();
            }
            if !runtime.addr.is_empty() {
                entry.addr = runtime.addr.clone();
            }
            if runtime.screen.is_some() {
                entry.screen = runtime.screen.clone();
            }
            if runtime.virtual_bounds.is_some() {
                entry.virtual_bounds = runtime.virtual_bounds.map(virtual_bounds_snapshot);
            }
            entry.last_seen_ms = runtime.last_transport_rx_ms.unwrap_or(entry.last_seen_ms);
            entry.discovery_status = match runtime.transport_reachability {
                TransportReachability::TimedOut => DiscoveryStatusSnapshot::Stale,
                TransportReachability::Unknown | TransportReachability::Reachable => {
                    DiscoveryStatusSnapshot::Online
                }
            };
            entry.paired = is_paired;
            if is_connected {
                entry.connected = true;
                entry.connection_status = Some("connected".to_string());
            } else if matches!(runtime.connection_status, ConnectionStatus::Incoming) {
                entry.connection_status = Some("incoming".to_string());
            }
        }

        let real_addrs: std::collections::HashSet<String> = peer_map
            .values()
            .filter(|s| !s.peer_id.starts_with("direct-"))
            .map(|s| s.addr.clone())
            .collect();
        peer_map.retain(|peer_id, snapshot| {
            if !peer_id.starts_with("direct-") {
                return true;
            }
            !real_addrs.contains(&snapshot.addr)
        });

        let mut peers: Vec<_> = peer_map.into_values().collect();
        peers.sort_by(|a, b| a.name.cmp(&b.name).then(a.peer_id.cmp(&b.peer_id)));
        peers
    }

    pub(super) fn replace_configured_edges(&self, assignments: &EdgeAssignments) {
        let mut registry = self.registry.write().unwrap_or_else(|e| e.into_inner());
        registry.configured_edges.clear();
        for (peer_id, edge) in assignments.iter() {
            registry.configured_edges.insert(peer_id.to_string(), edge);
        }
    }

    pub(super) fn configured_edge(&self, peer_id: &str) -> PeerEdge {
        self.registry
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .configured_edges
            .get(peer_id)
            .copied()
            .unwrap_or(PeerEdge::Right)
    }

    pub(super) fn clear(&self) {
        let mut registry = self.registry.write().unwrap_or_else(|e| e.into_inner());
        registry.peers.clear();
        registry.configured_edges.clear();
    }
}

fn virtual_bounds_snapshot(bounds: DisplayBounds) -> VirtualBoundsSnapshot {
    VirtualBoundsSnapshot {
        x: bounds.x,
        y: bounds.y,
        width: bounds.width,
        height: bounds.height,
    }
}

fn display_rect_snapshot(bounds: DisplayBounds) -> DisplayRectSnapshot {
    DisplayRectSnapshot {
        x: bounds.x,
        y: bounds.y,
        width: bounds.width,
        height: bounds.height,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::{ConnectedPeer, ConnectedPeerDirection};
    use crate::core::peer_state::{ConnectionStatus, TransportReachability};
    use crate::core::platform::DisplayBounds;
    use std::net::{Ipv4Addr, SocketAddr};

    fn discovered_peer(peer_id: &str, addr: SocketAddr, hostname: &str) -> DiscoveredPeer {
        DiscoveredPeer {
            peer_id: peer_id.to_string(),
            primary_addr: addr,
            addrs: vec![addr],
            hostname: hostname.to_string(),
            fullname: format!("{hostname}.{}", crate::core::policies::DISCOVERY_SERVICE_TYPE),
            platform: None,
        }
    }

    #[test]
    fn runtime_snapshots_include_inbound_peers_using_discovery_hostname() {
        let store = PeerStore::default();
        let addr = SocketAddr::from((Ipv4Addr::new(192, 168, 1, 77), 54321));
        store.remember_discovered(discovered_peer("peer-b", addr, "macbook-pro.local"));

        let runtime = vec![PeerRuntimeStateSnapshot {
            peer_id: "peer-b".to_string(),
            name: "Machine B".to_string(),
            addr: addr.to_string(),
            transport_reachability: TransportReachability::Reachable,
            connection_status: ConnectionStatus::Incoming,
            last_transport_rx_ms: Some(42),
            screen: Some("Built-in".to_string()),
            virtual_bounds: Some(DisplayBounds {
                x: 10,
                y: 20,
                width: 2560,
                height: 1600,
            }),
        }];

        let snapshots = store.runtime_snapshots(&runtime, "local-peer");

        assert_eq!(snapshots.len(), 1);
        let snapshot = &snapshots[0];
        assert_eq!(snapshot.peer_id, "peer-b");
        assert_eq!(snapshot.hostname, "macbook-pro.local");
        assert_eq!(snapshot.addr, "192.168.1.77:54321");
        assert_eq!(snapshot.screen.as_deref(), Some("Built-in"));
        assert_eq!(snapshot.discovery_status, DiscoveryStatusSnapshot::Online);
        assert_eq!(snapshot.last_seen_ms, 42);
        assert_eq!(snapshot.display_rects, Vec::<DisplayRectSnapshot>::new());
        assert_eq!(
            snapshot.virtual_bounds.as_ref().map(|bounds| (
                bounds.x,
                bounds.y,
                bounds.width,
                bounds.height
            )),
            Some((10, 20, 2560, 1600))
        );
    }

    #[test]
    fn runtime_snapshots_mark_timed_out_peers_stale_and_preserve_display_rects() {
        let store = PeerStore::default();
        let addr = SocketAddr::from((Ipv4Addr::new(10, 0, 0, 2), crate::core::policies::DEFAULT_PORT));
        store.remember_discovered(discovered_peer("peer-a", addr, "alpha.local"));
        store.remember_connected(ConnectedPeer {
            peer_id: "peer-a".to_string(),
            name: "Alpha".to_string(),
            addr,
            conn_addr: None,
            virtual_bounds: None,
            display_rects: vec![DisplayBounds {
                x: 0,
                y: 0,
                width: 1920,
                height: 1080,
            }],
            direction: ConnectedPeerDirection::Inbound,
            platform: None,
        });

        let runtime = vec![PeerRuntimeStateSnapshot {
            peer_id: "peer-a".to_string(),
            name: "Alpha".to_string(),
            addr: addr.to_string(),
            transport_reachability: TransportReachability::TimedOut,
            connection_status: ConnectionStatus::Connected,
            last_transport_rx_ms: None,
            screen: None,
            virtual_bounds: None,
        }];

        let snapshots = store.runtime_snapshots(&runtime, "local-peer");

        assert_eq!(snapshots.len(), 1);
        let snapshot = &snapshots[0];
        assert_eq!(snapshot.discovery_status, DiscoveryStatusSnapshot::Stale);
        assert_eq!(
            snapshot.display_rects,
            vec![DisplayRectSnapshot {
                x: 0,
                y: 0,
                width: 1920,
                height: 1080,
            }]
        );
    }

    #[test]
    fn lifecycle_transitions_keep_discovery_and_reset_dial_state_on_connect() {
        let store = PeerStore::default();
        let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, crate::core::policies::DEFAULT_PORT));
        store.remember_discovered(discovered_peer("peer-a", addr, "alpha.local"));

        store.set_connecting("peer-a");
        assert!(matches!(
            store.dial_state("peer-a"),
            DialState::Connecting(_)
        ));

        store.remember_connected(ConnectedPeer {
            peer_id: "peer-a".to_string(),
            name: "Alpha".to_string(),
            addr,
            conn_addr: None,
            virtual_bounds: None,
            display_rects: Vec::new(),
            direction: ConnectedPeerDirection::Outbound,
            platform: None,
        });

        assert!(store.is_discovered("peer-a"));
        assert!(store.is_connected("peer-a"));
        assert_eq!(store.dial_state("peer-a"), DialState::Idle);
    }

    #[test]
    fn prepare_for_restart_preserves_peers_and_allows_reconnection() {
        let store = PeerStore::default();
        let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, crate::core::policies::DEFAULT_PORT));
        store.remember_discovered(discovered_peer("peer-a", addr, "alpha.local"));
        store.remember_connected(ConnectedPeer {
            peer_id: "peer-a".to_string(),
            name: "Alpha".to_string(),
            addr,
            conn_addr: None,
            virtual_bounds: None,
            display_rects: Vec::new(),
            direction: ConnectedPeerDirection::Outbound,
            platform: None,
        });
        store.set_connecting("peer-a");

        store.prepare_for_restart();

        assert_eq!(store.discovered_peer_ids(), vec!["peer-a"]);
        assert_eq!(store.discovered_peer("peer-a").unwrap().hostname, "alpha.local");
        assert!(!store.is_connected("peer-a"));
        assert_eq!(store.dial_state("peer-a"), DialState::Idle);
    }

    #[test]
    fn prepare_for_restart_keeps_connected_peers_without_discovery() {
        let store = PeerStore::default();
        let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, crate::core::policies::DEFAULT_PORT));
        store.remember_connected(ConnectedPeer {
            peer_id: "peer-a".to_string(),
            name: "Alpha".to_string(),
            addr,
            conn_addr: None,
            virtual_bounds: None,
            display_rects: Vec::new(),
            direction: ConnectedPeerDirection::Inbound,
            platform: None,
        });

        store.prepare_for_restart();

        let peer = store.discovered_peer("peer-a").unwrap();
        assert_eq!(peer.primary_addr, addr);
        assert_eq!(peer.hostname, "Alpha");
        assert!(!store.is_connected("peer-a"));
    }

    #[test]
    fn failed_connection_can_back_off_then_expire_without_leaving_peer_state() {
        let store = PeerStore::default();
        let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, crate::core::policies::DEFAULT_PORT));
        store.remember_discovered(discovered_peer("peer-a", addr, "alpha.local"));
        store.set_connecting("peer-a");

        let retry_at = Instant::now() + std::time::Duration::from_secs(1);
        store.set_backoff("peer-a", retry_at);
        assert!(matches!(store.dial_state("peer-a"), DialState::Backoff(_)));

        store.expire_discovered("peer-a");
        assert!(!store.is_discovered("peer-a"));
        assert!(matches!(store.dial_state("peer-a"), DialState::Backoff(_)));

        store.set_idle("peer-a");
        assert_eq!(store.dial_state("peer-a"), DialState::Idle);
        assert!(store.is_empty());
    }

    #[test]
    fn removing_a_connection_preserves_discovery_for_reconnect() {
        let store = PeerStore::default();
        let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, crate::core::policies::DEFAULT_PORT));
        store.remember_discovered(discovered_peer("peer-a", addr, "alpha.local"));
        store.remember_connected(ConnectedPeer {
            peer_id: "peer-a".to_string(),
            name: "Alpha".to_string(),
            addr,
            conn_addr: None,
            virtual_bounds: None,
            display_rects: Vec::new(),
            direction: ConnectedPeerDirection::Inbound,
            platform: None,
        });

        assert!(store.remove_connected("peer-a").is_some());
        assert!(!store.is_connected("peer-a"));
        assert!(store.is_discovered("peer-a"));
        assert_eq!(store.dial_state("peer-a"), DialState::Idle);
    }

    #[test]
    fn reconnect_uses_successful_listen_address_without_transient_port() {
        let store = PeerStore::default();
        let advertised: SocketAddr = "10.0.0.5:54321".parse().unwrap();
        let listen_addr: SocketAddr = "192.168.1.5:54321".parse().unwrap();
        let transient: SocketAddr = "192.168.1.5:60123".parse().unwrap();
        store.remember_connected(ConnectedPeer {
            peer_id: "peer-a".to_string(),
            name: "Alpha".to_string(),
            addr: listen_addr,
            conn_addr: Some(transient),
            virtual_bounds: None,
            display_rects: Vec::new(),
            direction: ConnectedPeerDirection::Inbound,
            platform: None,
        });
        let discovered = store.discovered_peer("peer-a").unwrap();
        assert_eq!(discovered.primary_addr, listen_addr);
        assert_eq!(discovered.addrs, vec![listen_addr]);
        assert_eq!(
            store.find_peer_by_addr(transient).as_deref(),
            Some("peer-a")
        );
        store.remember_discovered(discovered_peer("peer-a", advertised, "alpha.local"));
        let connected = store.connected("peer-a").unwrap();
        store.remember_connected(connected);
        store.remove_connected("peer-a");

        let discovered = store.discovered_peer("peer-a").unwrap();
        assert_eq!(discovered.primary_addr, listen_addr);
        assert!(discovered.addrs.contains(&listen_addr));
        assert!(discovered.addrs.contains(&advertised));
        assert!(!discovered.addrs.contains(&transient));
        assert!(store.find_peer_by_addr(transient).is_none());
    }

    #[test]
    fn peer_store_is_paired_reflects_config_store_single_source_of_truth() {
        crate::platform::shared::test_support::with_temp_home(|| {
            let config_store = ConfigStore::default();
            let store = PeerStore::new(config_store.clone());

            assert!(!store.is_paired("peer-a"));

            let _ = config_store.pair_peer("peer-a").unwrap();
            assert!(store.is_paired("peer-a"));

            let _ = config_store.unpair_peer("peer-a").unwrap();
            assert!(!store.is_paired("peer-a"));
        });
    }
}
