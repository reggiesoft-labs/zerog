//! PeerRegistry implementation.

use crate::core::config::ReleaseTapKey;
use crate::core::peer_state::{PeerRuntimeStateSnapshot, TransportReachability};
use crate::core::platform::{DisplayBounds, ModifierMapping, PlatformFamily};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use super::peer_info::PeerInfo;
use super::session::{SessionState, session_state_to_connection_status};

#[derive(Default)]
struct PeerRegistryState {
    peers: HashMap<String, PeerInfo>,
    peer_id_by_addr: HashMap<SocketAddr, String>,
}

impl PeerRegistryState {
    fn peer_for_addr_mut(&mut self, addr: SocketAddr) -> Option<&mut PeerInfo> {
        let peer_id = self.peer_id_by_addr.get(&addr)?.clone();
        self.peers.get_mut(&peer_id)
    }

    fn peer_for_addr(&self, addr: SocketAddr) -> Option<&PeerInfo> {
        let peer_id = self.peer_id_by_addr.get(&addr)?;
        self.peers.get(peer_id)
    }
}

pub(crate) struct PeerRegistry {
    state: Mutex<PeerRegistryState>,
    swap_alt_cmd: AtomicBool,
    modifier_mapping: Mutex<ModifierMapping>,
    keep_mouse_alive: AtomicBool,
    release_tap_keycode: AtomicU32,
}

impl PeerRegistry {
    pub(crate) fn new() -> Self {
        Self {
            state: Mutex::new(PeerRegistryState::default()),
            swap_alt_cmd: AtomicBool::new(cfg!(any(target_os = "windows", target_os = "macos"))),
            modifier_mapping: Mutex::new(ModifierMapping::default()),
            keep_mouse_alive: AtomicBool::new(false),
            release_tap_keycode: AtomicU32::new(ReleaseTapKey::default().hid_code()),
        }
    }

    pub(crate) fn set_swap_alt_cmd(&self, swap: bool) {
        self.swap_alt_cmd.store(swap, Ordering::Relaxed);
    }

    pub(crate) fn swap_alt_cmd(&self) -> bool {
        self.swap_alt_cmd.load(Ordering::Relaxed)
    }

    pub(crate) fn set_modifier_mapping(&self, mapping: ModifierMapping) {
        *self.modifier_mapping.lock().unwrap() = mapping;
    }

    pub(crate) fn modifier_mapping(&self) -> ModifierMapping {
        *self.modifier_mapping.lock().unwrap()
    }

    pub(crate) fn set_keep_mouse_alive(&self, keep: bool) {
        self.keep_mouse_alive.store(keep, Ordering::Relaxed);
    }

    pub(crate) fn keep_mouse_alive(&self) -> bool {
        self.keep_mouse_alive.load(Ordering::Relaxed)
    }

    pub(crate) fn set_release_tap_key(&self, key: ReleaseTapKey) {
        self.release_tap_keycode.store(key.hid_code(), Ordering::Relaxed);
    }

    pub(crate) fn release_tap_keycode(&self) -> u32 {
        self.release_tap_keycode.load(Ordering::Relaxed)
    }

    pub(crate) fn platform_for_addr(&self, addr: SocketAddr) -> PlatformFamily {
        self.state
            .lock()
            .unwrap()
            .peer_for_addr(addr)
            .map(|peer| peer.platform)
            .unwrap_or(PlatformFamily::LOCAL)
    }

    pub(crate) fn add_peer(
        &self,
        peer_id: &str,
        name: &str,
        addr: SocketAddr,
        platform: Option<&str>,
    ) {
        let platform_family = platform
            .map(PlatformFamily::from_os_str)
            .unwrap_or(PlatformFamily::LOCAL);
        let mut state = self.state.lock().unwrap();
        let previous_addr = state.peers.get(peer_id).and_then(|peer| peer.addr);
        if let Some(old_addr) = previous_addr {
            state.peer_id_by_addr.remove(&old_addr);
        }

        if let Some(previous_peer_id) = state.peer_id_by_addr.insert(addr, peer_id.to_string())
            && previous_peer_id != peer_id
            && let Some(previous_peer) = state.peers.get_mut(&previous_peer_id)
            && previous_peer.addr == Some(addr)
        {
            previous_peer.addr = None;
        }

        let peer = state.peers.entry(peer_id.to_string()).or_insert_with(|| PeerInfo {
            peer_id: peer_id.to_string(),
            name: name.to_string(),
            addr: Some(addr),
            last_recv_us: 0,
            timed_out: false,
            session_state: SessionState::None,
            session_state_changed_at_us: 0,
            screen: None,
            display_bounds: None,
            platform: platform_family,
        });

        peer.peer_id = peer_id.to_string();
        peer.name = name.to_string();
        peer.addr = Some(addr);
        peer.platform = platform_family;

    }

    pub(crate) fn register_conn_addr(&self, addr: SocketAddr, peer_id: &str) {
        self.state
            .lock()
            .unwrap()
            .peer_id_by_addr
            .insert(addr, peer_id.to_string());
    }

    pub(crate) fn clear_conn_addr_for_peer(&self, peer_id: &str) {
        let mut state = self.state.lock().unwrap();
        let primary_addr = state.peers.get(peer_id).and_then(|p| p.addr);
        state
            .peer_id_by_addr
            .retain(|addr, id| id != peer_id || Some(*addr) == primary_addr);
    }

    pub(crate) fn remove_conn_addr(&self, addr: SocketAddr) -> Option<String> {
        self.state.lock().unwrap().peer_id_by_addr.remove(&addr)
    }

    pub(crate) fn remove_peer(&self, peer_id: &str) {
        let mut state = self.state.lock().unwrap();
        state.peer_id_by_addr.retain(|_, id| id != peer_id);
        state.peers.remove(peer_id);
    }

    pub(crate) fn peer_addrs(&self) -> Vec<SocketAddr> {
        self.state
            .lock()
            .unwrap()
            .peer_id_by_addr
            .keys()
            .copied()
            .collect()
    }

    pub(crate) fn peer_id_for_addr(&self, addr: SocketAddr) -> Option<String> {
        self.state
            .lock()
            .unwrap()
            .peer_id_by_addr
            .get(&addr)
            .cloned()
    }

    pub(crate) fn mark_packet_received(&self, addr: SocketAddr, now_us: u64) {
        if let Some(peer) = self.state.lock().unwrap().peer_for_addr_mut(addr) {
            peer.last_recv_us = now_us;
        }
    }

    pub(crate) fn mark_peer_reachable(&self, addr: SocketAddr) -> bool {
        let mut state = self.state.lock().unwrap();
        let Some(peer) = state.peer_for_addr_mut(addr) else {
            return false;
        };
        let was_timed_out = peer.timed_out;
        peer.timed_out = false;
        was_timed_out
    }

    pub(crate) fn mark_packet_received_by_peer_id(&self, peer_id: &str, now_us: u64) {
        if let Some(peer) = self.state.lock().unwrap().peers.get_mut(peer_id) {
            peer.last_recv_us = now_us;
        }
    }

    pub(crate) fn mark_peer_reachable_by_peer_id(&self, peer_id: &str) -> bool {
        let mut state = self.state.lock().unwrap();
        let Some(peer) = state.peers.get_mut(peer_id) else {
            return false;
        };
        let was_timed_out = peer.timed_out;
        peer.timed_out = false;
        was_timed_out
    }

    pub(crate) fn mark_peer_timed_out(&self, addr: SocketAddr) -> bool {
        let mut state = self.state.lock().unwrap();
        let Some(peer) = state.peer_for_addr_mut(addr) else {
            return false;
        };
        let was_timed_out = peer.timed_out;
        peer.timed_out = true;
        !was_timed_out
    }

    pub(crate) fn is_peer_timed_out(&self, addr: SocketAddr) -> bool {
        self.state
            .lock()
            .unwrap()
            .peer_for_addr(addr)
            .is_some_and(|peer| peer.timed_out)
    }

    pub(crate) fn initialize_peer_timeout(&self, addr: SocketAddr, now_us: u64) {
        if let Some(peer) = self.state.lock().unwrap().peer_for_addr_mut(addr)
            && peer.last_recv_us == 0
            && peer.session_state_changed_at_us == 0
        {
            peer.session_state_changed_at_us = now_us.max(1);
        }
    }

    pub(crate) fn peer_timeout_reference_us(&self, addr: SocketAddr) -> u64 {
        self.state
            .lock()
            .unwrap()
            .peer_for_addr(addr)
            .map(|peer| {
                if peer.last_recv_us > 0 {
                    peer.last_recv_us
                } else {
                    peer.session_state_changed_at_us
                }
            })
            .unwrap_or(0)
    }

    pub(crate) fn mark_outgoing_pending(&self, peer_id: &str, now_us: u64) {
        let mut state = self.state.lock().unwrap();
        if let Some(peer) = state.peers.get_mut(peer_id)
            && peer.session_state != SessionState::Connected
        {
            peer.session_state = SessionState::OutgoingPending;
            peer.session_state_changed_at_us = now_us;
        }
    }

    pub(crate) fn mark_connected(&self, peer_id: &str, now_us: u64) {
        let mut state = self.state.lock().unwrap();
        if let Some(peer) = state.peers.get_mut(peer_id) {
            peer.session_state = SessionState::Connected;
            peer.session_state_changed_at_us = now_us;
            peer.last_recv_us = now_us;
            peer.timed_out = false;
        }
    }

    #[cfg(test)]
    pub(crate) fn register_connected(
        &self,
        peer_id: &str,
        peer_name: &str,
        addr: SocketAddr,
        now_us: u64,
    ) {
        self.add_peer(peer_id, peer_name, addr, None);
        self.mark_connected(peer_id, now_us);
    }

    pub(crate) fn mark_disconnected(
        &self,
        peer_id: &str,
        peer_name: &str,
        addr: SocketAddr,
        now_us: u64,
    ) {
        self.add_peer(peer_id, peer_name, addr, None);
        let mut state = self.state.lock().unwrap();
        let Some(peer) = state.peers.get_mut(peer_id) else {
            return;
        };

        if matches!(
            peer.session_state,
            SessionState::Connected | SessionState::OutgoingPending
        ) {
            peer.session_state = SessionState::None;
            peer.session_state_changed_at_us = now_us;
        }

    }

    pub(crate) fn get_peer_runtime_state(&self) -> Vec<PeerRuntimeStateSnapshot> {
        let state = self.state.lock().unwrap();
        let mut runtime: Vec<_> = state
            .peers
            .values()
            .filter_map(|peer| {
                let addr = peer.addr?;
                Some(PeerRuntimeStateSnapshot {
                    peer_id: peer.peer_id.clone(),
                    name: peer.name.clone(),
                    addr: addr.to_string(),
                    transport_reachability: if peer.timed_out {
                        TransportReachability::TimedOut
                    } else {
                        TransportReachability::Reachable
                    },
                    connection_status: session_state_to_connection_status(peer.session_state),
                    last_transport_rx_ms: (peer.last_recv_us > 0)
                        .then_some(peer.last_recv_us / 1000),
                    screen: peer.screen.clone(),
                    virtual_bounds: peer.display_bounds,
                })
            })
            .collect();

        runtime.sort_by(|a, b| a.name.cmp(&b.name).then(a.peer_id.cmp(&b.peer_id)));
        runtime
    }

    pub(crate) fn update_peer_display_bounds(&self, peer_id: &str, bounds: Option<DisplayBounds>) {
        if let Some(peer) = self.state.lock().unwrap().peers.get_mut(peer_id) {
            peer.display_bounds = bounds;
        }
    }

    pub(crate) fn peer_display_bounds(&self, peer_id: &str) -> Option<DisplayBounds> {
        self.state
            .lock()
            .unwrap()
            .peers
            .get(peer_id)
            .and_then(|peer| peer.display_bounds)
    }
}

#[cfg(test)]
impl PeerRegistry {
    pub(crate) fn has_peer(&self, addr: SocketAddr) -> bool {
        self.state
            .lock()
            .unwrap()
            .peer_id_by_addr
            .contains_key(&addr)
    }

    pub(crate) fn peer_last_recv_us(&self, addr: SocketAddr) -> u64 {
        self.state
            .lock()
            .unwrap()
            .peer_for_addr(addr)
            .map(|p| p.last_recv_us)
            .unwrap_or(0)
    }
}

#[cfg(test)]
mod tests {
    use super::PeerRegistry;
    use std::net::SocketAddr;

    #[test]
    fn register_conn_addr_preserves_listening_addr_for_peer() {
        let registry = PeerRegistry::new();
        let listening_addr: SocketAddr = "192.168.1.50:51820".parse().unwrap();
        let connection_addr: SocketAddr = "192.168.1.50:58123".parse().unwrap();

        // 1. Peer discovered via mDNS with listening port
        registry.add_peer("peer-mac", "Mac", listening_addr, Some("macos"));
        assert_eq!(
            registry.peer_id_for_addr(listening_addr).as_deref(),
            Some("peer-mac")
        );

        // 2. Peer connects inbound from ephemeral connection port
        registry.register_conn_addr(connection_addr, "peer-mac");

        // Both listening address (used by layout) and ephemeral address (used by QUIC conn) resolve
        assert_eq!(
            registry.peer_id_for_addr(listening_addr).as_deref(),
            Some("peer-mac")
        );
        assert_eq!(
            registry.peer_id_for_addr(connection_addr).as_deref(),
            Some("peer-mac")
        );
    }
}

