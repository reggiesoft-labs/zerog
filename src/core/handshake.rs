//! Reusable QUIC connection handshake helpers.
//!
//! This module centralizes the wire-level request/accept handshake so callers
//! can compose discovery, direct-connect, or smoke-test flows without pulling
//! in the full runtime packet handler.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::core::error::{CoreError, CoreResult};
use crate::core::peer_state::canonical_peer_id;
use crate::core::platform::DisplayBounds;
use crate::quic::{PeerConnection, QuicEndpoint};
use tokio::time::timeout;
use tracing::debug;

/// Upper bound for a hello message. The payload only carries identity metadata
/// and display geometry, so anything larger is malformed or hostile. Bounding it
/// prevents a pre-authentication allocation attack via the length prefix.
const MAX_HELLO_MESSAGE_SIZE: usize = 64 * 1024;

fn validate_hello_message_len(len: usize) -> CoreResult<()> {
    if len > MAX_HELLO_MESSAGE_SIZE {
        return Err(CoreError::Connection(format!(
            "hello message length {} exceeds maximum {}",
            len, MAX_HELLO_MESSAGE_SIZE
        )));
    }
    Ok(())
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct HelloMessage {
    peer_id: String,
    instance_id: String,
    name: String,
    #[serde(default)]
    display_bounds: Option<DisplayBounds>,
    #[serde(default)]
    display_rects: Vec<DisplayBounds>,
    #[serde(default)]
    platform: Option<String>,
    #[serde(default)]
    is_auto_connect: bool,
    #[serde(default)]
    listen_port: Option<u16>,
    #[serde(default)]
    cert_der: Option<Vec<u8>>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[allow(dead_code)]
struct LegacyHelloMessage {
    peer_id: String,
    instance_id: String,
    name: String,
    display_bounds: Option<DisplayBounds>,
    display_rects: Vec<DisplayBounds>,
    platform: Option<String>,
    is_auto_connect: bool,
}

/// Canonical peer identity exchanged during the connection handshake.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct HandshakePeer {
    pub peer_id: String,
    pub name: String,
    pub display_bounds: Option<DisplayBounds>,
    pub display_rects: Vec<DisplayBounds>,
    pub platform: Option<String>,
    pub is_auto_connect: bool,
    pub listen_port: Option<u16>,
    #[serde(default)]
    pub cert_der: Option<Vec<u8>>,
    /// End-entity certificate actually presented during the QUIC/TLS
    /// handshake. Populated by the handshake layer, never from the wire hello.
    #[serde(skip)]
    pub certificate: Option<Vec<u8>>,
}

impl HandshakePeer {
    /// Creates a local identity without altering the provided peer ID.
    pub fn local(
        peer_id: impl Into<String>,
        name: impl Into<String>,
        display_bounds: Option<DisplayBounds>,
        display_rects: Vec<DisplayBounds>,
    ) -> Self {
        Self {
            peer_id: peer_id.into(),
            name: name.into(),
            display_bounds,
            display_rects,
            platform: Some(std::env::consts::OS.to_string()),
            is_auto_connect: false,
            listen_port: None,
            cert_der: None,
            certificate: None,
        }
    }

    pub fn with_auto_connect(mut self, is_auto_connect: bool) -> Self {
        self.is_auto_connect = is_auto_connect;
        self
    }

    pub fn with_listen_port(mut self, port: Option<u16>) -> Self {
        self.listen_port = port;
        self
    }

    pub fn with_cert_der(mut self, cert_der: Option<Vec<u8>>) -> Self {
        self.cert_der = cert_der;
        self
    }

    /// Creates a remote identity using canonical peer ID normalization.
    pub fn remote(
        peer_id: impl AsRef<str>,
        name: impl Into<String>,
        display_bounds: Option<DisplayBounds>,
        display_rects: Vec<DisplayBounds>,
        platform: Option<String>,
        is_auto_connect: bool,
    ) -> Self {
        Self {
            peer_id: canonical_peer_id(peer_id.as_ref()),
            name: name.into(),
            display_bounds,
            display_rects,
            platform,
            is_auto_connect,
            listen_port: None,
            cert_der: None,
            certificate: None,
        }
    }
}

/// Stateless outbound handshake flow for establishing a peer identity.
#[derive(Clone)]
pub struct HandshakeClient {
    endpoint: Arc<QuicEndpoint>,
    local_peer: HandshakePeer,
}

pub(crate) struct HandshakeConnection {
    pub(crate) peer: HandshakePeer,
    pub(crate) connection: Arc<PeerConnection>,
    pub(crate) certificate_trusted: bool,
    endpoint: Arc<QuicEndpoint>,
    claimed: bool,
}

impl HandshakeConnection {
    pub(crate) fn claim(mut self) {
        self.claimed = true;
    }
}

impl Drop for HandshakeConnection {
    fn drop(&mut self) {
        if !self.claimed {
            self.endpoint
                .abort_connection_attempt(&self.connection, b"unclaimed handshake connection");
        }
    }
}

struct ConnectionAttempt {
    endpoint: Arc<QuicEndpoint>,
    connection: Arc<PeerConnection>,
    completed: bool,
}

impl ConnectionAttempt {
    fn new(endpoint: Arc<QuicEndpoint>, connection: Arc<PeerConnection>) -> Self {
        Self {
            endpoint,
            connection,
            completed: false,
        }
    }

    fn connection(&self) -> &Arc<PeerConnection> {
        &self.connection
    }

    fn abort(&mut self, reason: &[u8]) {
        self.endpoint
            .abort_connection_attempt(&self.connection, reason);
        self.completed = true;
    }

    fn complete(&mut self) {
        self.completed = true;
    }
}

impl Drop for ConnectionAttempt {
    fn drop(&mut self) {
        if !self.completed {
            self.endpoint
                .abort_connection_attempt(&self.connection, b"connection handshake cancelled");
        }
    }
}

impl HandshakeClient {
    pub fn new(endpoint: Arc<QuicEndpoint>, local_peer: HandshakePeer) -> Self {
        Self {
            endpoint,
            local_peer,
        }
    }

    pub async fn connect(
        &self,
        addr: SocketAddr,
        timeout_duration: Duration,
        is_auto_connect: bool,
    ) -> CoreResult<HandshakePeer> {
        let result = self
            .connect_with_connection(addr, timeout_duration, is_auto_connect)
            .await?;
        if let Err(error) = self
            .endpoint
            .register_outbound_connection(&result.connection, &result.peer.peer_id)
        {
            return Err(CoreError::Connection(error.to_string()));
        }
        let peer = result.peer.clone();
        result.claim();
        Ok(peer)
    }

    pub(crate) async fn connect_with_connection(
        &self,
        addr: SocketAddr,
        timeout_duration: Duration,
        is_auto_connect: bool,
    ) -> CoreResult<HandshakeConnection> {
        debug!(addr = %addr, "initiating connection handshake");

        let deadline = Instant::now() + timeout_duration;
        let connect_timeout = deadline.saturating_duration_since(Instant::now());
        let conn = match timeout(connect_timeout, self.endpoint.connect(addr, "localhost")).await {
            Ok(Ok(conn)) => conn,
            Ok(Err(error)) => {
                return Err(CoreError::Connection(format!(
                    "QUIC connect failed: {}",
                    error
                )));
            }
            Err(_) => {
                tracing::warn!(addr = %addr, "connection handshake timed out");
                return Err(CoreError::Connection(format!(
                    "handshake timed out after {:?}",
                    timeout_duration
                )));
            }
        };

        let mut attempt = ConnectionAttempt::new(Arc::clone(&self.endpoint), conn);
        let handshake_timeout = deadline.saturating_duration_since(Instant::now());
        match timeout(
            handshake_timeout,
            self.perform_handshake(Arc::clone(attempt.connection()), is_auto_connect),
        )
        .await
        {
            Ok(Ok(peer)) => {
                let Some(certificate) = peer.certificate.clone() else {
                    attempt.abort(b"remote did not present a TLS certificate");
                    return Err(CoreError::Connection(
                        "remote did not present a TLS certificate".to_string(),
                    ));
                };
                let certificate_trusted = {
                    let cert_store = self.endpoint.cert_store();
                    let store = cert_store.lock().unwrap();
                    let known = store.is_known_peer(&peer.peer_id);
                    let matches = store.verify_fingerprint(&peer.peer_id, &certificate);
                    if known && !matches {
                        drop(store);
                        attempt.abort(b"certificate fingerprint mismatch");
                        return Err(CoreError::Connection(format!(
                            "certificate fingerprint mismatch for peer {}",
                            peer.peer_id
                        )));
                    }
                    matches
                };
                if let Err(error) = self
                    .endpoint
                    .identify_connection(attempt.connection(), &peer.peer_id)
                {
                    attempt.abort(b"failed to identify connected peer");
                    return Err(CoreError::Connection(format!(
                        "failed to identify connected peer {}: {}",
                        peer.peer_id, error
                    )));
                }
                let connection = Arc::clone(attempt.connection());
                attempt.complete();
                tracing::info!(
                    peer_id = %peer.peer_id,
                    name = %peer.name,
                    certificate_trusted,
                    "connection handshake successful"
                );
                Ok(HandshakeConnection {
                    peer,
                    connection,
                    certificate_trusted,
                    endpoint: Arc::clone(&self.endpoint),
                    claimed: false,
                })
            }
            Ok(Err(error)) => {
                attempt.abort(b"connection handshake failed");
                tracing::warn!(addr = %addr, error = %error, "connection handshake failed");
                Err(error)
            }
            Err(_) => {
                attempt.abort(b"connection handshake timed out");
                tracing::warn!(addr = %addr, "connection handshake timed out");
                Err(CoreError::Connection(format!(
                    "handshake timed out after {:?}",
                    timeout_duration
                )))
            }
        }
    }

    async fn perform_handshake(
        &self,
        conn: Arc<PeerConnection>,
        is_auto_connect: bool,
    ) -> CoreResult<HandshakePeer> {
        let (mut send, mut recv) =
            conn.connection().open_bi().await.map_err(|e| {
                CoreError::Connection(format!("failed to open hello stream: {}", e))
            })?;

        let local_peer = self.local_peer.clone().with_auto_connect(is_auto_connect);
        write_hello_message(&mut send, &local_peer).await?;
        send.finish()
            .map_err(|e| CoreError::Connection(format!("failed to finish hello stream: {}", e)))?;

        let mut peer = read_hello_message(&mut recv).await?;
        peer.certificate = conn.peer_certificate();
        Ok(peer)
    }
}

/// Reusable inbound responder for the connection handshake.
pub struct HandshakeResponder {
    local_peer: Option<HandshakePeer>,
}

impl HandshakeResponder {
    pub fn new(local_peer: Option<HandshakePeer>) -> Self {
        Self { local_peer }
    }

    /// Accepts an inbound hello exchange on the same bidirectional stream used
    /// by the remote outbound connector and responds with the local identity.
    pub async fn accept(
        &self,
        endpoint: &QuicEndpoint,
        conn: &Arc<PeerConnection>,
    ) -> CoreResult<HandshakePeer> {
        let (mut send, mut recv) =
            conn.connection().accept_bi().await.map_err(|e| {
                CoreError::Connection(format!("failed to accept hello stream: {}", e))
            })?;

        let mut request = read_hello_message(&mut recv).await?;

        let certificate = conn.peer_certificate().ok_or_else(|| {
            CoreError::Connection(
                "inbound connection did not present a TLS certificate".to_string(),
            )
        })?;
        {
            let cert_store = endpoint.cert_store();
            let store = cert_store.lock().unwrap();
            if store.is_known_peer(&request.peer_id)
                && !store.verify_fingerprint(&request.peer_id, &certificate)
            {
                return Err(CoreError::Connection(format!(
                    "inbound certificate fingerprint mismatch for peer {}",
                    request.peer_id
                )));
            }
        }
        request.certificate = Some(certificate);

        endpoint.register_inbound_connection(conn, &request.peer_id)?;

        let local_peer = self.local_peer.as_ref().ok_or_else(|| {
            CoreError::Connection("received connection request without local identity".to_string())
        })?;

        write_hello_message(&mut send, local_peer).await?;
        send.finish().map_err(|e| {
            CoreError::Connection(format!("failed to finish hello response stream: {}", e))
        })?;

        Ok(request)
    }
}

pub(crate) async fn write_hello_message(
    stream: &mut quinn::SendStream,
    peer: &HandshakePeer,
) -> CoreResult<()> {
    let hello = HelloMessage {
        peer_id: peer.peer_id.clone(),
        // The GUI/runtime does not yet track a separate instance id. Use a
        // stable peer-scoped string so the payload matches the bin protocol.
        instance_id: peer.peer_id.clone(),
        name: peer.name.clone(),
        display_bounds: peer.display_bounds,
        display_rects: peer.display_rects.clone(),
        platform: peer
            .platform
            .clone()
            .or_else(|| Some(std::env::consts::OS.to_string())),
        is_auto_connect: peer.is_auto_connect,
        listen_port: peer.listen_port,
        // Legacy wire field kept for protocol compatibility. Peer identity is
        // authenticated by the QUIC/TLS handshake, not by this value.
        cert_der: peer.cert_der.clone(),
    };
    let data = bincode::serialize(&hello)
        .map_err(|e| CoreError::Connection(format!("failed to serialize hello message: {}", e)))?;
    if data.len() > MAX_HELLO_MESSAGE_SIZE {
        return Err(CoreError::Connection(format!(
            "hello message too large: {} > {}",
            data.len(),
            MAX_HELLO_MESSAGE_SIZE
        )));
    }
    let len = u32::try_from(data.len())
        .map_err(|_| CoreError::Connection("hello message too large".to_string()))?;
    tokio::io::AsyncWriteExt::write_all(stream, &len.to_le_bytes())
        .await
        .map_err(|e| CoreError::Connection(format!("failed to write hello length: {}", e)))?;
    tokio::io::AsyncWriteExt::write_all(stream, &data)
        .await
        .map_err(|e| CoreError::Connection(format!("failed to write hello payload: {}", e)))?;
    Ok(())
}

pub(crate) async fn read_hello_message(
    stream: &mut quinn::RecvStream,
) -> CoreResult<HandshakePeer> {
    let mut len_buf = [0u8; 4];
    tokio::io::AsyncReadExt::read_exact(stream, &mut len_buf)
        .await
        .map_err(|e| CoreError::Connection(format!("failed to read hello length: {}", e)))?;
    let len = u32::from_le_bytes(len_buf) as usize;
    validate_hello_message_len(len)?;
    let mut data = vec![0u8; len];
    tokio::io::AsyncReadExt::read_exact(stream, &mut data)
        .await
        .map_err(|e| CoreError::Connection(format!("failed to read hello payload: {}", e)))?;

    let (peer_id, name, bounds, rects, platform, auto_connect, listen_port, cert_der) =
        if let Ok(hello) = bincode::deserialize::<HelloMessage>(&data) {
            (
                hello.peer_id,
                hello.name,
                hello.display_bounds,
                hello.display_rects,
                hello.platform,
                hello.is_auto_connect,
                hello.listen_port,
                hello.cert_der,
            )
        } else if let Ok(legacy) = bincode::deserialize::<LegacyHelloMessage>(&data) {
            (
                legacy.peer_id,
                legacy.name,
                legacy.display_bounds,
                legacy.display_rects,
                legacy.platform,
                legacy.is_auto_connect,
                None,
                None,
            )
        } else {
            let hello: HelloMessage = serde_json::from_slice(&data)
                .map_err(|e| CoreError::Connection(format!("failed to decode hello message: {}", e)))?;
            (
                hello.peer_id,
                hello.name,
                hello.display_bounds,
                hello.display_rects,
                hello.platform,
                hello.is_auto_connect,
                hello.listen_port,
                hello.cert_der,
            )
        };

    let mut peer = HandshakePeer::remote(
        peer_id,
        name,
        bounds,
        rects,
        platform,
        auto_connect,
    );
    peer.listen_port = listen_port;
    // The certificate in the hello body is untrusted metadata. The handshake
    // layer overwrites `peer.certificate` with the certificate presented during
    // the QUIC/TLS handshake, which is the only value used for authentication.
    peer.cert_der = cert_der;
    Ok(peer)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::quic::CertificateStore;
    use anyhow::Result;
    use std::net::{Ipv4Addr, SocketAddr};
    use tempfile::TempDir;

    #[test]
    fn handshake_client_completes_stream_hello_exchange() -> Result<()> {
        let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();

        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("tokio runtime should build");

        rt.block_on(async {
            let dir = TempDir::new()?;
            let server_store = CertificateStore::load_or_create(&dir.path().join("server"))?;
            let client_store = CertificateStore::load_or_create(&dir.path().join("client"))?;

            let server = Arc::new(QuicEndpoint::new(0, "server-peer", server_store)?);
            let client = Arc::new(QuicEndpoint::new(0, "client-peer", client_store)?);
            let server_addr = SocketAddr::from((Ipv4Addr::LOCALHOST, server.local_addr()?.port()));

            let accept = tokio::time::timeout(Duration::from_secs(2), server.accept());
            let connect = tokio::time::timeout(
                Duration::from_secs(2),
                client.connect(server_addr, "localhost"),
            );
            let (accept_res, connect_res) = tokio::join!(accept, connect);

            let server_conn = accept_res??;
            let client_conn = connect_res??;

            let handshake_client = HandshakeClient::new(
                client,
                HandshakePeer::local(
                    "client-peer",
                    "Client Peer",
                    Some(DisplayBounds {
                        x: -1920,
                        y: 0,
                        width: 1920,
                        height: 1080,
                    }),
                    vec![DisplayBounds {
                        x: -1920,
                        y: 0,
                        width: 1920,
                        height: 1080,
                    }],
                ),
            );

            let responder = HandshakeResponder::new(Some(HandshakePeer::local(
                "server-peer",
                "Server Peer",
                Some(DisplayBounds {
                    x: 0,
                    y: 0,
                    width: 2560,
                    height: 1440,
                }),
                vec![DisplayBounds {
                    x: 0,
                    y: 0,
                    width: 2560,
                    height: 1440,
                }],
            )));
            let server = Arc::clone(&server);
            let server_task =
                tokio::spawn(async move { responder.accept(&server, &server_conn).await });

            let result = timeout(
                Duration::from_secs(2),
                handshake_client.perform_handshake(client_conn, false),
            )
            .await??;
            let inbound = timeout(Duration::from_secs(2), server_task).await???;

            assert_eq!(result.peer_id, "server-peer");
            assert_eq!(result.name, "Server Peer");
            assert_eq!(
                result.display_bounds,
                Some(DisplayBounds {
                    x: 0,
                    y: 0,
                    width: 2560,
                    height: 1440,
                })
            );
            assert_eq!(
                result.display_rects,
                vec![DisplayBounds {
                    x: 0,
                    y: 0,
                    width: 2560,
                    height: 1440,
                }]
            );
            assert!(
                result.certificate.is_some(),
                "outbound handshake must capture the server TLS certificate"
            );
            assert!(
                inbound.certificate.is_some(),
                "inbound handshake must capture the client TLS certificate"
            );
            assert_eq!(inbound.peer_id, "client-peer");
            assert_eq!(inbound.name, "Client Peer");
            assert_eq!(
                inbound.display_bounds,
                Some(DisplayBounds {
                    x: -1920,
                    y: 0,
                    width: 1920,
                    height: 1080,
                })
            );
            assert_eq!(
                inbound.display_rects,
                vec![DisplayBounds {
                    x: -1920,
                    y: 0,
                    width: 1920,
                    height: 1080,
                }]
            );

            Ok::<(), anyhow::Error>(())
        })
    }

    #[test]
    fn handshake_timeout_aborts_connection_attempt() -> Result<()> {
        let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();

        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("tokio runtime should build");

        rt.block_on(async {
            let dir = TempDir::new()?;
            let server_store = CertificateStore::load_or_create(&dir.path().join("server"))?;
            let client_store = CertificateStore::load_or_create(&dir.path().join("client"))?;

            let server = Arc::new(QuicEndpoint::new(0, "server-peer", server_store)?);
            let client_endpoint = Arc::new(QuicEndpoint::new(0, "client-peer", client_store)?);
            let server_addr = SocketAddr::from((Ipv4Addr::LOCALHOST, server.local_addr()?.port()));
            let server_task = tokio::spawn(async move {
                let _connection = server.accept().await?;
                tokio::time::sleep(Duration::from_secs(1)).await;
                Ok::<(), anyhow::Error>(())
            });

            let client = HandshakeClient::new(
                Arc::clone(&client_endpoint),
                HandshakePeer::local("client-peer", "Client Peer", None, Vec::new()),
            );
            let error = client
                .connect(server_addr, Duration::from_millis(100), false)
                .await
                .expect_err("handshake should time out");

            assert!(error.to_string().contains("timed out"));
            assert!(client_endpoint.all_connections().is_empty());

            server_task.abort();
            let _ = server_task.await;
            Ok::<(), anyhow::Error>(())
        })
    }

    #[test]
    fn hello_message_wire_encoding_supports_both_bincode_and_json() {
        let original = HelloMessage {
            peer_id: "test-peer".to_string(),
            instance_id: "test-peer".to_string(),
            name: "Test Peer".to_string(),
            display_bounds: None,
            display_rects: Vec::new(),
            platform: Some("windows".to_string()),
            is_auto_connect: true,
            listen_port: Some(54321),
            cert_der: None,
        };

        let bincode_bytes = bincode::serialize(&original).unwrap();
        let from_bincode: HelloMessage = bincode::deserialize(&bincode_bytes).unwrap();
        assert_eq!(from_bincode.peer_id, "test-peer");
        assert!(from_bincode.is_auto_connect);
        assert_eq!(from_bincode.listen_port, Some(54321));

        let json_bytes = serde_json::to_vec(&original).unwrap();
        assert!(bincode::deserialize::<HelloMessage>(&json_bytes).is_err());
        let from_json: HelloMessage = serde_json::from_slice(&json_bytes).unwrap();
        assert_eq!(from_json.peer_id, "test-peer");
        assert!(from_json.is_auto_connect);
        assert_eq!(from_json.listen_port, Some(54321));
    }

    #[test]
    fn inbound_rejects_known_peer_presenting_a_different_certificate() -> Result<()> {
        let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();

        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("tokio runtime should build");

        rt.block_on(async {
            let dir = TempDir::new()?;
            let other_store = CertificateStore::load_or_create(&dir.path().join("other"))?;
            let other = Arc::new(QuicEndpoint::new(0, "other-peer", other_store)?);

            let mut server_store = CertificateStore::load_or_create(&dir.path().join("server"))?;
            // Pin the wrong certificate for the client peer.
            server_store.trust_peer("client-peer", other.self_cert_der())?;
            let client_store = CertificateStore::load_or_create(&dir.path().join("client"))?;

            let server = Arc::new(QuicEndpoint::new(0, "server-peer", server_store)?);
            let client = Arc::new(QuicEndpoint::new(0, "client-peer", client_store)?);
            let server_addr = SocketAddr::from((Ipv4Addr::LOCALHOST, server.local_addr()?.port()));

            let accept = server.accept();
            let connect = client.connect(server_addr, "localhost");
            let (server_conn, client_conn) = tokio::join!(accept, connect);
            let server_conn = server_conn?;
            let client_conn = client_conn?;

            let responder = HandshakeResponder::new(Some(HandshakePeer::local(
                "server-peer",
                "Server Peer",
                None,
                Vec::new(),
            )));
            let server = Arc::clone(&server);
            let server_task =
                tokio::spawn(async move { responder.accept(&server, &server_conn).await });

            let client_handshake = HandshakeClient::new(
                Arc::clone(&client),
                HandshakePeer::local("client-peer", "Client Peer", None, Vec::new()),
            );
            let _ = timeout(
                Duration::from_secs(2),
                client_handshake.perform_handshake(client_conn, false),
            )
            .await;

            let inbound = timeout(Duration::from_secs(2), server_task).await??;
            assert!(
                inbound.is_err(),
                "inbound handshake must reject a pinned peer presenting a different certificate"
            );

            Ok::<(), anyhow::Error>(())
        })
    }

    #[test]
    fn outbound_rejects_known_peer_presenting_a_different_certificate() -> Result<()> {
        let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();

        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("tokio runtime should build");

        rt.block_on(async {
            let dir = TempDir::new()?;
            let other_store = CertificateStore::load_or_create(&dir.path().join("other"))?;
            let other = Arc::new(QuicEndpoint::new(0, "other-peer", other_store)?);

            let mut client_store = CertificateStore::load_or_create(&dir.path().join("client"))?;
            // Pin the wrong certificate for the server peer.
            client_store.trust_peer("server-peer", other.self_cert_der())?;
            let server_store = CertificateStore::load_or_create(&dir.path().join("server"))?;

            let server = Arc::new(QuicEndpoint::new(0, "server-peer", server_store)?);
            let client = Arc::new(QuicEndpoint::new(0, "client-peer", client_store)?);
            let server_addr = SocketAddr::from((Ipv4Addr::LOCALHOST, server.local_addr()?.port()));

            let server_responder = HandshakeResponder::new(Some(HandshakePeer::local(
                "server-peer",
                "Server Peer",
                None,
                Vec::new(),
            )));
            let server_endpoint = Arc::clone(&server);
            let server_task = tokio::spawn(async move {
                if let Ok(conn) = server_endpoint.accept().await {
                    let _ = server_responder.accept(&server_endpoint, &conn).await;
                }
            });

            let handshake = HandshakeClient::new(
                Arc::clone(&client),
                HandshakePeer::local("client-peer", "Client Peer", None, Vec::new()),
            );
            let error = handshake
                .connect(server_addr, Duration::from_secs(2), false)
                .await
                .expect_err("outbound handshake must reject a pinned fingerprint mismatch");
            assert!(
                error.to_string().contains("fingerprint mismatch"),
                "unexpected error: {error}"
            );

            server_task.abort();
            let _ = server_task.await;
            Ok::<(), anyhow::Error>(())
        })
    }

    #[test]
    fn hello_length_validation_rejects_oversized_prefixes() {
        assert!(validate_hello_message_len(0).is_ok());
        assert!(validate_hello_message_len(MAX_HELLO_MESSAGE_SIZE).is_ok());
        assert!(validate_hello_message_len(MAX_HELLO_MESSAGE_SIZE + 1).is_err());
        // A hostile u32 length must be rejected before any allocation happens.
        assert!(validate_hello_message_len(u32::MAX as usize).is_err());
    }

    #[test]
    fn legacy_hello_message_without_listen_port_decodes() {
        let legacy = LegacyHelloMessage {
            peer_id: "legacy-peer".to_string(),
            instance_id: "legacy-peer".to_string(),
            name: "Legacy Peer".to_string(),
            display_bounds: None,
            display_rects: Vec::new(),
            platform: Some("macos".to_string()),
            is_auto_connect: false,
        };

        let bincode_bytes = bincode::serialize(&legacy).unwrap();
        let legacy_decoded: LegacyHelloMessage = bincode::deserialize(&bincode_bytes).unwrap();
        assert_eq!(legacy_decoded.peer_id, "legacy-peer");
    }
}
