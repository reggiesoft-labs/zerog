use super::peers::{ConnectedPeer, ConnectedPeerDirection};
use super::{CoreError, CoreResult};
use crate::core::discovery::DiscoveredPeer;
use crate::core::handshake::{HandshakeClient, HandshakeConnection, HandshakePeer};
use crate::core::platform::DisplayBounds;
use crate::quic::{PeerConnection, QuicEndpoint};
use std::sync::Arc;
pub(super) struct ConnectedPeerConnection {
    pub(super) peer: ConnectedPeer,
    pub(super) connection: Arc<PeerConnection>,
    /// Certificate presented by the remote during the QUIC/TLS handshake.
    pub(super) certificate: Vec<u8>,
    /// Whether `certificate` already matched a pinned fingerprint for the peer.
    pub(super) certificate_trusted: bool,
}

/// Controller-level coordinator for `DiscoveredPeer -> ConnectedPeer`.
///
/// Performs one outbound handshake and returns the session metadata.
pub(super) struct PeerConnector {
    client: HandshakeClient,
}

impl PeerConnector {
    pub(super) fn new(
        quic_endpoint: Arc<QuicEndpoint>,
        local_peer_id: String,
        local_name: String,
        local_display_bounds: Option<DisplayBounds>,
        local_display_rects: Vec<DisplayBounds>,
    ) -> Self {
        let listen_port = quic_endpoint.local_addr().ok().map(|addr| addr.port());
        let cert_der = quic_endpoint.self_cert_der().to_vec();
        let local_peer = HandshakePeer::local(
            local_peer_id,
            local_name,
            local_display_bounds,
            local_display_rects,
        )
        .with_listen_port(listen_port)
        .with_cert_der(Some(cert_der));
        Self {
            client: HandshakeClient::new(quic_endpoint, local_peer),
        }
    }

    /// Connects to a discovered peer and returns the remote session metadata.
    ///
    /// The controller owns connected and in-flight lifecycle state.
    pub(super) async fn connect(
        &self,
        discovered_peer: DiscoveredPeer,
        is_auto_connect: bool,
    ) -> CoreResult<ConnectedPeerConnection> {
        self.connect_inner(discovered_peer, is_auto_connect).await
    }

    async fn connect_inner(
        &self,
        discovered_peer: DiscoveredPeer,
        is_auto_connect: bool,
    ) -> CoreResult<ConnectedPeerConnection> {
        let mut candidates = vec![discovered_peer.primary_addr];
        for addr in &discovered_peer.addrs {
            if !candidates.contains(addr) {
                candidates.push(*addr);
            }
        }

        let timeout_duration = crate::core::policies::PEER_CANDIDATE_CONNECT_TIMEOUT;
        let mut last_error = None;
        for candidate_addr in candidates {
            match self
                .client
                .connect_with_connection(candidate_addr, timeout_duration, is_auto_connect)
                .await
            {
                Ok(result) => {
                    match self.attach_connection(discovered_peer.clone(), result, candidate_addr) {
                        Ok(connected) => return Ok(connected),
                        Err(error) => {
                            tracing::warn!(
                                peer_id = %discovered_peer.peer_id,
                                addr = %candidate_addr,
                                error = %error,
                                "peer candidate rejected"
                            );
                            last_error = Some(error);
                        }
                    }
                }
                Err(error) => {
                    tracing::debug!(
                        peer_id = %discovered_peer.peer_id,
                        addr = %candidate_addr,
                        error = %error,
                        "peer candidate failed"
                    );
                    last_error = Some(error);
                }
            }
        }

        Err(last_error.unwrap_or_else(|| {
            CoreError::Connection(format!(
                "all candidate addresses failed for peer {}",
                discovered_peer.peer_id
            ))
        }))
    }

    fn attach_connection(
        &self,
        discovered_peer: DiscoveredPeer,
        result: HandshakeConnection,
        connected_addr: std::net::SocketAddr,
    ) -> CoreResult<ConnectedPeerConnection> {
        let connection = Arc::clone(&result.connection);
        let remote_peer = result.peer.clone();
        let certificate = remote_peer.certificate.clone().unwrap_or_default();
        let certificate_trusted = result.certificate_trusted;
        match self.attach_peer(discovered_peer, remote_peer, connected_addr) {
            Ok(peer) => {
                result.claim();
                Ok(ConnectedPeerConnection {
                    peer,
                    connection,
                    certificate,
                    certificate_trusted,
                })
            }
            Err(error) => Err(error),
        }
    }

    fn attach_peer(
        &self,
        discovered_peer: DiscoveredPeer,
        peer: HandshakePeer,
        connected_addr: std::net::SocketAddr,
    ) -> CoreResult<ConnectedPeer> {
        if !discovered_peer.peer_id.starts_with("direct-")
            && peer.peer_id != discovered_peer.peer_id
        {
            return Err(CoreError::Connection(format!(
                "handshake peer_id {} did not match discovered peer_id {}",
                peer.peer_id, discovered_peer.peer_id
            )));
        }

        Ok(ConnectedPeer {
            peer_id: peer.peer_id,
            name: peer.name,
            addr: connected_addr,
            conn_addr: Some(connected_addr),
            virtual_bounds: peer.display_bounds,
            display_rects: peer.display_rects,
            direction: ConnectedPeerDirection::Outbound,
            platform: peer.platform.or(discovered_peer.platform),
        })
    }
}

pub(super) type SharedPeerConnector = Arc<PeerConnector>;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::controller::peers::PeerStore;
    use crate::quic::CertificateStore;
    use std::net::SocketAddr;
    use std::sync::Arc;
    use tempfile::TempDir;

    fn test_connector() -> PeerConnector {
        crate::install_crypto_provider();
        let cert_dir = TempDir::new().unwrap();
        let endpoint = Arc::new(
            QuicEndpoint::new(
                0,
                "local-peer",
                CertificateStore::load_or_create(cert_dir.path()).unwrap(),
            )
            .unwrap(),
        );
        PeerConnector::new(
            endpoint,
            "local-peer".to_string(),
            "Local".to_string(),
            None,
            Vec::new(),
        )
    }

    #[tokio::test]
    async fn connector_returns_session_without_registering_peer_state() {
        let peers = Arc::new(PeerStore::default());
        let connector = test_connector();
        let discovered_peer = DiscoveredPeer {
            peer_id: "peer-b".to_string(),
            primary_addr: std::net::SocketAddr::from(([127, 0, 0, 1], crate::core::policies::DEFAULT_PORT)),
            addrs: vec![std::net::SocketAddr::from(([127, 0, 0, 1], crate::core::policies::DEFAULT_PORT))],
            hostname: "peer-b.local".to_string(),
            fullname: format!("peer-b.{}", crate::core::policies::DISCOVERY_SERVICE_TYPE),
            platform: None,
        };

        let connected = connector
            .attach_peer(
                discovered_peer.clone(),
                HandshakePeer::remote("peer-b", "Peer B", None, Vec::new(), None, false),
                discovered_peer.primary_addr,
            )
            .expect("attach_peer should succeed");

        assert_eq!(connected.peer_id, "peer-b");
        assert_eq!(connected.name, "Peer B");
        assert_eq!(connected.addr, discovered_peer.primary_addr);
        assert!(!peers.is_connected("peer-b"));
    }

    #[tokio::test]
    async fn test_peer_connector_candidate_fallback() {
        let peers = Arc::new(PeerStore::default());
        let connector = test_connector();

        let server_dir = TempDir::new().unwrap();
        let server_store = CertificateStore::load_or_create(server_dir.path()).unwrap();
        let server_endpoint = Arc::new(QuicEndpoint::new(0, "peer-b", server_store).unwrap());
        let server_port = server_endpoint.local_addr().unwrap().port();
        let server_addr = SocketAddr::from((std::net::Ipv4Addr::LOCALHOST, server_port));
        let server_responder = crate::core::handshake::HandshakeResponder::new(Some(
            HandshakePeer::local("peer-b", "Peer B", None, Vec::new()),
        ));
        let server_endpoint_clone = Arc::clone(&server_endpoint);
        let server_task = tokio::spawn(async move {
            if let Ok(conn) = server_endpoint_clone.accept().await {
                let _ = server_responder.accept(&server_endpoint_clone, &conn).await;
            }
        });

        // Unreachable candidate followed by the real server address
        let dead_port_addr: SocketAddr = "127.0.0.1:1".parse().unwrap();
        let discovered_peer = DiscoveredPeer {
            peer_id: "peer-b".to_string(),
            primary_addr: dead_port_addr,
            addrs: vec![dead_port_addr, server_addr],
            hostname: "peer-b.local".to_string(),
            fullname: format!("peer-b.{}", crate::core::policies::DISCOVERY_SERVICE_TYPE),
            platform: None,
        };

        let connected = connector
            .connect(discovered_peer, false)
            .await
            .expect("connect should succeed on secondary candidate");

        assert_eq!(connected.peer.peer_id, "peer-b");
        assert_eq!(connected.peer.name, "Peer B");
        assert_eq!(connected.peer.addr, server_addr);
        assert!(!peers.is_connected("peer-b"));

        server_task.abort();
    }

    #[tokio::test]
    async fn identity_mismatch_falls_back_and_closes_rejected_connection() {
        let connector = test_connector();
        let certs = TempDir::new().unwrap();
        let wrong_endpoint = Arc::new(
            QuicEndpoint::new(
                0,
                "wrong-peer",
                CertificateStore::load_or_create(&certs.path().join("wrong")).unwrap(),
            )
            .unwrap(),
        );
        let right_endpoint = Arc::new(
            QuicEndpoint::new(
                0,
                "peer-b",
                CertificateStore::load_or_create(&certs.path().join("right")).unwrap(),
            )
            .unwrap(),
        );
        let wrong_addr = SocketAddr::from((
            std::net::Ipv4Addr::LOCALHOST,
            wrong_endpoint.local_addr().unwrap().port(),
        ));
        let right_addr = SocketAddr::from((
            std::net::Ipv4Addr::LOCALHOST,
            right_endpoint.local_addr().unwrap().port(),
        ));
        let wrong_server = Arc::clone(&wrong_endpoint);
        let wrong_task = tokio::spawn(async move {
            let conn = wrong_server.accept().await.unwrap();
            let responder = crate::core::handshake::HandshakeResponder::new(Some(
                HandshakePeer::local("wrong-peer", "Wrong", None, Vec::new()),
            ));
            responder.accept(&wrong_server, &conn).await.unwrap();
            conn
        });
        let right_server = Arc::clone(&right_endpoint);
        let right_task = tokio::spawn(async move {
            let conn = right_server.accept().await.unwrap();
            let responder = crate::core::handshake::HandshakeResponder::new(Some(
                HandshakePeer::local("peer-b", "Peer B", None, Vec::new()),
            ));
            responder.accept(&right_server, &conn).await.unwrap();
            conn
        });
        let connected = connector
            .connect(
                DiscoveredPeer {
                    peer_id: "peer-b".to_string(),
                    primary_addr: wrong_addr,
                    addrs: vec![wrong_addr, right_addr],
                    hostname: "peer-b.local".to_string(),
                    fullname: "peer-b._zerog._udp.local.".to_string(),
                    platform: Some("macos".to_string()),
                },
                false,
            )
            .await
            .unwrap();

        assert_eq!(connected.peer.peer_id, "peer-b");
        assert_eq!(connected.peer.addr, right_addr);
        let rejected = wrong_task.await.unwrap();
        tokio::time::timeout(
            std::time::Duration::from_secs(2),
            rejected.connection().closed(),
        )
        .await
        .unwrap();
        assert!(!right_task.await.unwrap().is_closed());
    }

    #[tokio::test]
    async fn test_peer_connector_direct_address() {
        let peers = Arc::new(PeerStore::default());
        let connector = test_connector();

        let server_dir = TempDir::new().unwrap();
        let server_store = CertificateStore::load_or_create(server_dir.path()).unwrap();
        let server_endpoint =
            Arc::new(QuicEndpoint::new(0, "direct-remote", server_store).unwrap());
        let server_port = server_endpoint.local_addr().unwrap().port();
        let server_addr = SocketAddr::from((std::net::Ipv4Addr::LOCALHOST, server_port));
        let server_responder = crate::core::handshake::HandshakeResponder::new(Some(
            HandshakePeer::local("direct-remote", "Remote Machine", None, Vec::new()),
        ));
        let server_endpoint_clone = Arc::clone(&server_endpoint);
        let server_task = tokio::spawn(async move {
            if let Ok(conn) = server_endpoint_clone.accept().await {
                let _ = server_responder.accept(&server_endpoint_clone, &conn).await;
            }
        });

        let direct_peer = DiscoveredPeer {
            peer_id: format!("direct-{}", server_addr),
            primary_addr: server_addr,
            addrs: vec![server_addr],
            hostname: server_addr.to_string(),
            fullname: format!("{}-direct.{}", server_addr, crate::core::policies::DISCOVERY_SERVICE_TYPE),
            platform: None,
        };

        let connected = connector
            .connect(direct_peer, false)
            .await
            .expect("direct connect should succeed");

        assert_eq!(connected.peer.peer_id, "direct-remote");
        assert_eq!(connected.peer.name, "Remote Machine");
        assert_eq!(connected.peer.addr, server_addr);
        assert!(!peers.is_connected("direct-remote"));

        server_task.abort();
    }
}
