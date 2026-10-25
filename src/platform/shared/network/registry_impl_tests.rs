//! PeerRegistry implementation - tests.

use super::registry_impl::PeerRegistry;
use crate::core::peer_state::{ConnectionStatus, TransportReachability};
use std::net::{IpAddr, Ipv4Addr, SocketAddr};

fn addr(port: u16) -> SocketAddr {
    SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), port)
}

#[test]
fn timeout_marking_and_reachable_reset_round_trip() {
    let registry = PeerRegistry::new();
    let peer_addr = addr(9002);

    registry.add_peer("peer-2", "beta", peer_addr, None);
    registry.mark_packet_received(peer_addr, 55);
    assert!(!registry.mark_peer_reachable(peer_addr));

    assert_eq!(registry.peer_last_recv_us(peer_addr), 55);
    assert!(registry.mark_peer_timed_out(peer_addr));
    assert!(registry.is_peer_timed_out(peer_addr));
    assert!(registry.mark_peer_reachable(peer_addr));
    assert!(!registry.is_peer_timed_out(peer_addr));
}

#[test]
fn connection_registration_and_disconnect_update_state() {
    let registry = PeerRegistry::new();
    let peer_addr = addr(9101);

    registry.register_connected("alpha-id", "alpha", peer_addr, 0);

    let runtime = registry.get_peer_runtime_state();
    assert_eq!(runtime.len(), 1);
    assert_eq!(runtime[0].connection_status, ConnectionStatus::Connected);

    registry.mark_disconnected("alpha-id", "alpha", peer_addr, 0);

    let runtime = registry.get_peer_runtime_state();
    assert_eq!(runtime[0].connection_status, ConnectionStatus::Disconnected);
}

#[test]
fn inbound_registration_is_connected_without_a_packet_handshake() {
    let registry = PeerRegistry::new();
    let peer_addr = addr(9105);

    registry.register_connected("inbound-id", "inbound", peer_addr, 100);

    let runtime = registry.get_peer_runtime_state();
    assert_eq!(runtime.len(), 1);
    assert_eq!(runtime[0].peer_id, "inbound-id");
    assert_eq!(runtime[0].connection_status, ConnectionStatus::Connected);
    assert_eq!(runtime[0].addr, peer_addr.to_string());
}

#[test]
fn peer_id_mapping_survives_address_change() {
    let registry = PeerRegistry::new();
    let first_addr = addr(9201);
    let second_addr = addr(9202);

    registry.add_peer("peer-rebind", "rebind", first_addr, None);
    registry.mark_packet_received(first_addr, 1_500);
    registry.mark_peer_reachable(first_addr);
    registry.add_peer("peer-rebind", "rebind", second_addr, None);

    assert!(!registry.has_peer(first_addr));
    assert!(registry.has_peer(second_addr));
    assert_eq!(registry.peer_last_recv_us(second_addr), 1_500);

    let runtime = registry.get_peer_runtime_state();
    assert_eq!(runtime.len(), 1);
    assert_eq!(runtime[0].addr, second_addr.to_string());
    assert_eq!(
        runtime[0].transport_reachability,
        TransportReachability::Reachable
    );
}

#[test]
fn timeout_changes_reachability_only_not_session() {
    let registry = PeerRegistry::new();
    let peer_addr = addr(9301);

    registry.register_connected("peer-timeout", "timeout", peer_addr, 0);
    registry.mark_peer_reachable(peer_addr);
    registry.mark_peer_timed_out(peer_addr);

    let runtime = registry.get_peer_runtime_state();
    assert_eq!(runtime.len(), 1);
    assert_eq!(runtime[0].connection_status, ConnectionStatus::Connected);
    assert_eq!(
        runtime[0].transport_reachability,
        TransportReachability::TimedOut
    );
}
