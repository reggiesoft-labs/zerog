//! Lightweight concurrent /24 subnet scanner for network environments where mDNS multicast is blocked.

use std::collections::HashSet;
use std::net::{Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use futures::stream::{self, StreamExt};
use tokio::time::timeout;

use crate::core::discovery::DiscoveredPeer;
use crate::core::handshake::{HandshakePeer, read_hello_message, write_hello_message};
use crate::core::policies::{SUBNET_SCAN_CONCURRENCY, SUBNET_SCAN_PROBE_TIMEOUT};
use crate::quic::QuicEndpoint;

/// Generates candidate host addresses on the local `/24` subnets (excluding local host, network, and broadcast).
pub fn generate_subnet_candidates(port: u16) -> Vec<SocketAddr> {
    if port == 0 {
        return Vec::new();
    }

    let mut candidates = Vec::new();
    let mut local_ips = HashSet::new();

    if let Ok(ifaces) = if_addrs::get_if_addrs() {
        for iface in ifaces {
            if !iface.is_loopback() {
                if let if_addrs::IfAddr::V4(v4) = iface.addr {
                    if !v4.ip.is_loopback() && !v4.ip.is_link_local() {
                        local_ips.insert(v4.ip);
                    }
                }
            }
        }
    }

    for &local_ip in &local_ips {
        let octets = local_ip.octets();
        for host in 1..=254 {
            let candidate_ip = Ipv4Addr::new(octets[0], octets[1], octets[2], host);
            if !local_ips.contains(&candidate_ip) {
                let candidate_addr = SocketAddr::from((candidate_ip, port));
                if !candidates.contains(&candidate_addr) {
                    candidates.push(candidate_addr);
                }
            }
        }
    }

    candidates
}

/// Probes a candidate address over QUIC using a brief Hello exchange.
pub async fn probe_candidate(
    endpoint: Arc<QuicEndpoint>,
    local_peer: HandshakePeer,
    candidate_addr: SocketAddr,
    probe_timeout: Duration,
) -> Option<DiscoveredPeer> {
    let connect_fut = endpoint.connect(candidate_addr, "localhost");
    let conn = match timeout(probe_timeout, connect_fut).await {
        Ok(Ok(conn)) => conn,
        _ => return None,
    };

    let handshake_fut = async {
        let (mut send, mut recv) = conn.connection().open_bi().await.ok()?;
        write_hello_message(&mut send, &local_peer).await.ok()?;
        send.finish().ok()?;
        let remote_peer = read_hello_message(&mut recv).await.ok()?;
        Some(remote_peer)
    };

    let result = match timeout(probe_timeout, handshake_fut).await {
        Ok(Some(remote_peer)) if remote_peer.peer_id != local_peer.peer_id => {
            let discovered = DiscoveredPeer {
                peer_id: remote_peer.peer_id.clone(),
                primary_addr: candidate_addr,
                addrs: vec![candidate_addr],
                hostname: remote_peer.name.clone(),
                fullname: format!("{}-subnet.{}", remote_peer.peer_id, crate::core::policies::DISCOVERY_SERVICE_TYPE),
                platform: remote_peer.platform,
            };
            Some(discovered)
        }
        _ => None,
    };

    endpoint.abort_connection_attempt(&conn, b"subnet probe complete");
    result
}

/// Concurrently scans candidate addresses on local `/24` subnets with bounded concurrency.
pub async fn scan_subnets(
    endpoint: Arc<QuicEndpoint>,
    local_peer: HandshakePeer,
    port: u16,
) -> Vec<DiscoveredPeer> {
    let candidates = generate_subnet_candidates(port);
    if candidates.is_empty() {
        return Vec::new();
    }

    tracing::info!(
        candidate_count = candidates.len(),
        concurrency = SUBNET_SCAN_CONCURRENCY,
        "starting /24 subnet probe scan"
    );

    let peers: Vec<DiscoveredPeer> = stream::iter(candidates)
        .map(|addr| {
            let ep = Arc::clone(&endpoint);
            let lp = local_peer.clone();
            async move { probe_candidate(ep, lp, addr, SUBNET_SCAN_PROBE_TIMEOUT).await }
        })
        .buffer_unordered(SUBNET_SCAN_CONCURRENCY)
        .filter_map(|discovered| async move { discovered })
        .collect()
        .await;

    tracing::info!(
        discovered_count = peers.len(),
        "finished /24 subnet probe scan"
    );

    peers
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::handshake::HandshakeResponder;
    use crate::quic::CertificateStore;
    use tempfile::TempDir;

    #[test]
    fn test_subnet_scanner_probe_filtering() {
        let candidates = generate_subnet_candidates(crate::core::policies::DEFAULT_PORT);
        for candidate in candidates {
            assert_eq!(candidate.port(), crate::core::policies::DEFAULT_PORT);
            match candidate.ip() {
                std::net::IpAddr::V4(v4) => {
                    assert!(!v4.is_loopback());
                    let last = v4.octets()[3];
                    assert!(last >= 1 && last <= 254);
                }
                std::net::IpAddr::V6(_) => panic!("expected IPv4 candidate only"),
            }
        }
    }

    #[tokio::test]
    async fn test_subnet_scanner_probe_success() {
        crate::install_crypto_provider();

        let server_dir = TempDir::new().unwrap();
        let server_store = CertificateStore::load_or_create(server_dir.path()).unwrap();
        let server_endpoint = Arc::new(QuicEndpoint::new(0, "server-peer", server_store).unwrap());
        let server_port = server_endpoint.local_addr().unwrap().port();
        let server_addr = SocketAddr::from((Ipv4Addr::LOCALHOST, server_port));

        let responder = HandshakeResponder::new(Some(HandshakePeer::local(
            "server-peer",
            "Server Peer",
            None,
            Vec::new(),
        )));
        let server_ep_clone = Arc::clone(&server_endpoint);
        let server_task = tokio::spawn(async move {
            if let Ok(conn) = server_ep_clone.accept().await {
                let _ = responder.accept(&server_ep_clone, &conn).await;
            }
        });

        let client_dir = TempDir::new().unwrap();
        let client_store = CertificateStore::load_or_create(client_dir.path()).unwrap();
        let client_endpoint = Arc::new(QuicEndpoint::new(0, "client-peer", client_store).unwrap());
        let local_peer = HandshakePeer::local("client-peer", "Client Peer", None, Vec::new());

        let discovered = probe_candidate(
            client_endpoint,
            local_peer,
            server_addr,
            Duration::from_secs(1),
        )
        .await
        .expect("probe should discover server peer");

        assert_eq!(discovered.peer_id, "server-peer");
        assert_eq!(discovered.primary_addr, server_addr);
        assert_eq!(discovered.hostname, "Server Peer");

        server_task.abort();
    }
}
