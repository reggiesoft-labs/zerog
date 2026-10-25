use anyhow::{Context, Result, anyhow};
use futures::Future;
use socket2::{Domain, Protocol, Socket, Type};
use std::collections::HashMap;
use std::io;
use std::net::{SocketAddr, UdpSocket};
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tracing::{debug, info};

use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, ServerName, UnixTime};
use rustls::server::danger::{ClientCertVerified, ClientCertVerifier};
use rustls::{DigitallySignedStruct, DistinguishedName, SignatureScheme};

use super::cert_store::CertificateStore;
use super::connection::PeerConnection;

const MAX_DATAGRAM_SIZE: usize = 1200;
const ALPN_PROTOCOL: &[u8] = b"mouse-thing";
const BIND_RETRY_ATTEMPTS: usize = 12;
const BIND_RETRY_DELAY_MS: u64 = 25;


#[derive(Default)]
struct ConnectionState {
    connections: Vec<Arc<PeerConnection>>,
    canonical_by_peer_id: HashMap<String, Arc<PeerConnection>>,
}

impl ConnectionState {
    fn add(&mut self, peer_conn: Arc<PeerConnection>) {
        if !self
            .connections
            .iter()
            .any(|current| Arc::ptr_eq(current, &peer_conn))
        {
            self.connections.push(peer_conn);
        }
    }

    fn remove_specific(&mut self, peer_conn: &Arc<PeerConnection>) {
        let peer_id = peer_conn.peer_id();
        if !peer_id.is_empty()
            && self
                .canonical_by_peer_id
                .get(&peer_id)
                .is_some_and(|current| Arc::ptr_eq(current, peer_conn))
        {
            self.canonical_by_peer_id.remove(&peer_id);
        }
        self.connections
            .retain(|current| !Arc::ptr_eq(current, peer_conn));
    }

    fn connection_for_addr(&self, addr: SocketAddr) -> Option<Arc<PeerConnection>> {
        self.canonical_by_peer_id
            .values()
            .find(|connection| connection.addr() == addr && !connection.is_closed())
            .cloned()
    }

    fn take_peer_connections(&mut self, peer_id: &str) -> Vec<Arc<PeerConnection>> {
        let mut removed = Vec::new();
        self.connections.retain(|connection| {
            let matches = connection.peer_id() == peer_id;
            if matches {
                removed.push(Arc::clone(connection));
            }
            !matches
        });
        self.canonical_by_peer_id.remove(peer_id);
        removed
    }
}

#[derive(Debug)]
pub enum InboundMessage {
    Datagram(Arc<PeerConnection>, Vec<u8>),
    Control(Arc<PeerConnection>, Vec<u8>),
}

pub struct QuicEndpoint {
    endpoint: Option<quinn::Endpoint>,
    cert_store: Arc<Mutex<CertificateStore>>,
    self_cert_der: Vec<u8>,
    self_peer_id: String,
    state: Mutex<ConnectionState>,
    shutting_down: AtomicBool,
    inbound_tx: tokio::sync::mpsc::Sender<InboundMessage>,
    inbound_rx: Mutex<Option<tokio::sync::mpsc::Receiver<InboundMessage>>>,
}

impl QuicEndpoint {
    fn ep(&self) -> &quinn::Endpoint {
        self.endpoint.as_ref().expect("endpoint already closed")
    }
    pub fn new(port: u16, peer_id: &str, mut cert_store: CertificateStore) -> Result<Self> {
        let runtime_handle = quic_runtime_handle();

        let (cert_der, key_der) = cert_store
            .get_or_create_self_cert(peer_id)
            .context("failed to get or create self certificate")?;

        let server_config =
            configure_server(&cert_der, &key_der).context("failed to configure QUIC server")?;

        let bind_addr: SocketAddr = if port == 0 {
            "0.0.0.0:0".parse().unwrap()
        } else {
            format!("0.0.0.0:{}", port).parse().unwrap()
        };

        let socket = bind_udp_socket_with_retry(bind_addr)
            .with_context(|| format!("failed to bind QUIC UDP socket for {bind_addr}"))?;

        let actual_addr = socket.local_addr().context("failed to get local address")?;

        let mut endpoint = quinn::Endpoint::new(
            quinn::EndpointConfig::default(),
            Some(server_config),
            socket,
            Arc::new(BoundTokioRuntime {
                handle: runtime_handle,
            }),
        )
        .context("failed to create QUIC endpoint")?;

        let client_config =
            configure_client(&cert_der, &key_der).context("failed to configure QUIC client")?;
        endpoint.set_default_client_config(client_config);

        info!(
            peer_id = %peer_id,
            addr = %actual_addr,
            "QUIC endpoint created"
        );

        let (inbound_tx, inbound_rx) = tokio::sync::mpsc::channel(2048);

        Ok(Self {
            endpoint: Some(endpoint),
            cert_store: Arc::new(Mutex::new(cert_store)),
            self_cert_der: cert_der,
            self_peer_id: peer_id.to_string(),
            state: Mutex::new(ConnectionState::default()),
            shutting_down: AtomicBool::new(false),
            inbound_tx,
            inbound_rx: Mutex::new(Some(inbound_rx)),
        })
    }

    pub fn take_inbound_receiver(&self) -> Option<tokio::sync::mpsc::Receiver<InboundMessage>> {
        self.inbound_rx.lock().unwrap().take()
    }

    fn spawn_connection_readers(&self, peer_conn: &Arc<PeerConnection>) {
        if peer_conn.mark_readers_spawned() {
            return;
        }

        let conn_datagram = Arc::clone(peer_conn);
        let tx_datagram = self.inbound_tx.clone();
        tokio::spawn(async move {
            while !conn_datagram.is_closed() {
                match conn_datagram.recv_datagram().await {
                    Ok(data) => {
                        if tx_datagram
                            .send(InboundMessage::Datagram(Arc::clone(&conn_datagram), data))
                            .await
                            .is_err()
                        {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
        });

        let conn_control = Arc::clone(peer_conn);
        let tx_control = self.inbound_tx.clone();
        tokio::spawn(async move {
            while !conn_control.is_closed() {
                match conn_control.recv_control().await {
                    Ok(data) => {
                        if tx_control
                            .send(InboundMessage::Control(Arc::clone(&conn_control), data))
                            .await
                            .is_err()
                        {
                            break;
                        }
                    }
                    Err(error) => {
                        if conn_control.is_closed() {
                            break;
                        }
                        tracing::debug!(error = %error, "control stream error, waiting for next stream");
                        tokio::time::sleep(Duration::from_millis(10)).await;
                    }
                }
            }
        });
    }

    pub async fn connect(
        &self,
        addr: SocketAddr,
        server_name: &str,
    ) -> Result<Arc<PeerConnection>> {
        debug!(server_name = %server_name, addr = %addr, "initiating QUIC connection");

        let conn = self
            .ep()
            .connect(addr, server_name)
            .map_err(|e| anyhow!("failed to initiate connection: {}", e))?
            .await
            .map_err(|e| anyhow!("connection failed: {}", e))?;

        let peer_conn = Arc::new(PeerConnection::new(conn));
        self.state.lock().unwrap().add(Arc::clone(&peer_conn));

        info!(addr = %addr, "QUIC connection established");
        Ok(peer_conn)
    }

    pub async fn accept(&self) -> Result<Arc<PeerConnection>> {
        let incoming = self.accept_incoming().await?;
        self.complete_incoming(incoming).await
    }

    /// Returns the next inbound handshake attempt as soon as its first flight
    /// arrives, without waiting for the handshake to finish. Callers that
    /// serve many peers must use this and complete each attempt in its own
    /// task: awaiting the full handshake here head-of-line-blocks every other
    /// inbound attempt behind one slow or abandoned handshake.
    pub async fn accept_incoming(&self) -> Result<quinn::Incoming> {
        self.ep()
            .accept()
            .await
            .context("endpoint closed while waiting for incoming connection")
    }

    /// Drives a handshake previously obtained with [`accept_incoming`] to
    /// completion. A vanished peer fails here on its own without delaying any
    /// other attempt.
    pub async fn complete_incoming(
        &self,
        incoming: quinn::Incoming,
    ) -> Result<Arc<PeerConnection>> {
        let remote_addr = incoming.remote_address();
        debug!(addr = %remote_addr, "accepting incoming QUIC connection");

        let conn = incoming
            .await
            .map_err(|e| anyhow!("handshake failed: {}", e))?;

        let peer_conn = Arc::new(PeerConnection::new(conn));
        self.state.lock().unwrap().add(Arc::clone(&peer_conn));

        info!(addr = %remote_addr, "QUIC connection accepted (pending peer identification)");
        Ok(peer_conn)
    }

    /// Records an accepted inbound connection once the remote peer_id is known.
    pub fn register_inbound_connection(
        &self,
        peer_conn: &Arc<PeerConnection>,
        peer_id: &str,
    ) -> Result<()> {
        self.register_identified_connection(peer_conn, peer_id)
    }

    /// Records an outbound connection once the handshake has identified the peer.
    pub fn register_outbound_connection(
        &self,
        peer_conn: &Arc<PeerConnection>,
        peer_id: &str,
    ) -> Result<()> {
        self.register_identified_connection(peer_conn, peer_id)
    }

    /// Closes a failed or timed-out connection attempt and clears any transient mappings.
    pub fn abort_connection_attempt(&self, peer_conn: &Arc<PeerConnection>, reason: &[u8]) {
        self.close_connection(peer_conn, reason);
    }

    pub fn identify_connection(
        &self,
        peer_conn: &Arc<PeerConnection>,
        peer_id: &str,
    ) -> Result<()> {
        if peer_id.is_empty() {
            return Err(anyhow!("cannot identify connection with an empty peer_id"));
        }
        if !peer_conn.set_peer_id(peer_id.to_string()) {
            return Err(anyhow!(
                "connection at {} is already identified as {}",
                peer_conn.addr(),
                peer_conn.peer_id()
            ));
        }
        Ok(())
    }

    /// Registers an inbound connection after peer identification.
    fn register_identified_connection(
        &self,
        peer_conn: &Arc<PeerConnection>,
        peer_id: &str,
    ) -> Result<()> {
        self.identify_connection(peer_conn, peer_id)?;
        let mut state = self.state.lock().unwrap();
        state.add(Arc::clone(peer_conn));
        let current_is_live = state
            .canonical_by_peer_id
            .get(peer_id)
            .is_some_and(|current| !current.is_closed());
        if !current_is_live {
            state
                .canonical_by_peer_id
                .insert(peer_id.to_string(), Arc::clone(peer_conn));
        }
        drop(state);
        if !current_is_live {
            self.spawn_connection_readers(peer_conn);
        }

        info!(peer_id = %peer_id, addr = %peer_conn.addr(), "QUIC connection identified and registered");
        Ok(())
    }

    pub fn get_connection(&self, peer_id: &str) -> Option<Arc<PeerConnection>> {
        self.state
            .lock()
            .unwrap()
            .canonical_by_peer_id
            .get(peer_id)
            .cloned()
    }

    pub fn get_connection_by_addr(&self, addr: SocketAddr) -> Option<Arc<PeerConnection>> {
        self.state.lock().unwrap().connection_for_addr(addr)
    }

    pub fn is_canonical_connection(&self, peer_conn: &Arc<PeerConnection>) -> bool {
        let peer_id = peer_conn.peer_id();
        !peer_id.is_empty()
            && self
                .state
                .lock()
                .unwrap()
                .canonical_by_peer_id
                .get(&peer_id)
                .is_some_and(|current| Arc::ptr_eq(current, peer_conn))
    }

    pub fn remove_connection(&self, peer_id: &str) {
        let connections = self.state.lock().unwrap().take_peer_connections(peer_id);
        for connection in connections {
            connection.close(b"peer removed");
        }
        info!(peer_id = %peer_id, "removed QUIC connections");
    }

    /// Closes one specific duplicate connection and removes only that connection
    /// from the registries. Used during simultaneous-dial tie-breaking.
    pub fn close_duplicate_connection(&self, peer_conn: &Arc<PeerConnection>, reason: &[u8]) {
        self.close_connection(peer_conn, reason);
    }

    /// Promotes an already identified connection to be the canonical connection
    /// for the peer. Used when an accepted inbound session wins duplicate resolution.
    pub fn promote_connection(&self, peer_id: &str, peer_conn: &Arc<PeerConnection>) -> Result<()> {
        self.identify_connection(peer_conn, peer_id)?;
        if peer_conn.is_closed() {
            return Err(anyhow!(
                "cannot promote closed connection at {}",
                peer_conn.addr()
            ));
        }

        let mut state = self.state.lock().unwrap();
        state.add(Arc::clone(peer_conn));
        let replaced = if let Some(existing) = state.canonical_by_peer_id.get(peer_id).cloned()
            && !Arc::ptr_eq(&existing, peer_conn)
        {
            state.remove_specific(&existing);
            Some(existing)
        } else {
            None
        };
        state
            .canonical_by_peer_id
            .insert(peer_id.to_string(), Arc::clone(peer_conn));
        drop(state);
        self.spawn_connection_readers(peer_conn);
        if let Some(existing) = replaced {
            existing.close(b"connection replaced by preferred connection");
        }
        Ok(())
    }

    /// Removes a connection by its remote address.
    /// Returns the peer_id if the connection was found and removed.
    ///
    /// # Arguments
    /// * `addr` - The remote SocketAddr of the connection
    ///
    /// # Returns
    /// * `Some(peer_id)` - The peer_id of the removed connection
    /// * `None` - No connection found for this address
    pub fn remove_connection_by_addr(&self, addr: SocketAddr) -> Option<String> {
        let mut state = self.state.lock().unwrap();
        let conn = state.connection_for_addr(addr)?;
        let peer_id = conn.peer_id();
        state.remove_specific(&conn);
        drop(state);
        conn.close(b"peer removed");
        info!(peer_id = %peer_id, addr = %addr, "removed QUIC connection by addr");
        Some(peer_id)
    }

    pub fn all_connections(&self) -> Vec<Arc<PeerConnection>> {
        self.state.lock().unwrap().connections.clone()
    }

    pub fn prune_closed_connections(&self) {
        let mut state = self.state.lock().unwrap();
        state
            .connections
            .retain(|connection| !connection.is_closed());
        state
            .canonical_by_peer_id
            .retain(|_, connection| !connection.is_closed());
    }

    pub fn local_addr(&self) -> Result<SocketAddr> {
        self.ep()
            .local_addr()
            .context("failed to get local address")
    }

    pub fn cert_store(&self) -> &Arc<Mutex<CertificateStore>> {
        &self.cert_store
    }

    pub fn self_peer_id(&self) -> &str {
        &self.self_peer_id
    }

    pub fn self_cert_der(&self) -> &[u8] {
        &self.self_cert_der
    }

    pub fn close(&self) {
        if !self.shutting_down.swap(true, Ordering::Relaxed) {
            info!("closing QUIC endpoint");

            for conn in self.all_connections() {
                conn.close(b"endpoint shutting down");
            }

            if let Some(ep) = self.endpoint.as_ref() {
                ep.close(0u32.into(), b"endpoint shutting down");
            }
        }
        let mut state = self.state.lock().unwrap();
        state.connections.clear();
        state.canonical_by_peer_id.clear();
    }

    pub fn is_shutting_down(&self) -> bool {
        self.shutting_down.load(Ordering::Relaxed)
    }
}

impl QuicEndpoint {
    fn close_connection(&self, peer_conn: &Arc<PeerConnection>, reason: &[u8]) {
        peer_conn.close(reason);
        self.state.lock().unwrap().remove_specific(peer_conn);
    }
}

fn bind_udp_socket_with_retry(bind_addr: SocketAddr) -> Result<UdpSocket> {
    let mut last_addr_in_use: Option<io::Error> = None;

    for attempt in 0..BIND_RETRY_ATTEMPTS {
        let socket = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))?;
        let _ = socket.set_tos(0x10);

        match socket.bind(&bind_addr.into()) {
            Ok(()) => {
                let socket: UdpSocket = socket.into();
                return Ok(socket);
            }
            Err(err) if err.kind() == io::ErrorKind::AddrInUse => {
                last_addr_in_use = Some(err);
                if attempt + 1 < BIND_RETRY_ATTEMPTS {
                    std::thread::sleep(Duration::from_millis(BIND_RETRY_DELAY_MS));
                }
            }
            Err(err) => return Err(err.into()),
        }
    }

    Err(anyhow!(
        "address {bind_addr} remained in use after {} bind retries: {}",
        BIND_RETRY_ATTEMPTS,
        last_addr_in_use
            .map(|e| e.to_string())
            .unwrap_or_else(|| "unknown bind failure".to_string())
    ))
}

fn quic_runtime_handle() -> tokio::runtime::Handle {
    crate::platform::shared::tokio_runtime::global_runtime_handle()
}

#[derive(Clone, Debug)]
struct BoundTokioRuntime {
    handle: tokio::runtime::Handle,
}

impl quinn::Runtime for BoundTokioRuntime {
    fn new_timer(&self, t: std::time::Instant) -> Pin<Box<dyn quinn::AsyncTimer>> {
        let _guard = self.handle.enter();
        quinn::TokioRuntime.new_timer(t)
    }

    fn spawn(&self, future: Pin<Box<dyn Future<Output = ()> + Send>>) {
        self.handle.spawn(future);
    }

    fn wrap_udp_socket(
        &self,
        sock: std::net::UdpSocket,
    ) -> io::Result<Arc<dyn quinn::AsyncUdpSocket>> {
        let _guard = self.handle.enter();
        quinn::TokioRuntime.wrap_udp_socket(sock)
    }

    fn now(&self) -> std::time::Instant {
        let _guard = self.handle.enter();
        quinn::TokioRuntime.now()
    }
}

impl Drop for QuicEndpoint {
    fn drop(&mut self) {
        if !self.shutting_down.swap(true, Ordering::Relaxed) {
            info!("closing QUIC endpoint (drop)");

            for conn in self.all_connections() {
                conn.close(b"endpoint shutting down");
            }

            if let Some(ep) = self.endpoint.as_ref() {
                ep.close(0u32.into(), b"endpoint shutting down");
            }
        }
        // Dropping the Endpoint decrements ref_count to 0, which causes the
        // quinn driver task to exit once all connections have drained.
        self.endpoint.take();
    }
}

fn configure_transport() -> quinn::TransportConfig {
    let mut config = quinn::TransportConfig::default();
    config.max_concurrent_bidi_streams(16u32.into());
    config.max_concurrent_uni_streams(16u32.into());
    config.datagram_receive_buffer_size(Some(64 * 1024));
    config.keep_alive_interval(Some(Duration::from_millis(50)));
    config.initial_rtt(Duration::from_millis(2));
    if let Ok(timeout) = quinn::IdleTimeout::try_from(Duration::from_secs(3)) {
        config.max_idle_timeout(Some(timeout));
    }
    config
}

/// Returns the process-wide rustls crypto provider used for handshake signature checks.
fn crypto_provider() -> Arc<rustls::crypto::CryptoProvider> {
    rustls::crypto::CryptoProvider::get_default()
        .cloned()
        .unwrap_or_else(|| Arc::new(rustls::crypto::aws_lc_rs::default_provider()))
}

fn parse_private_key(key_der: &[u8]) -> Result<PrivateKeyDer<'static>> {
    PrivateKeyDer::try_from(key_der.to_vec())
        .map_err(|e| anyhow!("failed to parse private key: {}", e))
}

/// Mutual-TLS server configuration.
///
/// The server requests a client certificate and *requires* one. The certificate
/// chain is accepted trust-on-first-use here (it is self-signed), but the
/// handshake signature is verified against the presented key, so a peer cannot
/// complete the handshake by replaying a public certificate. The controller
/// pins the certificate fingerprint to the peer id after the hello exchange.
fn configure_server(cert_der: &[u8], key_der: &[u8]) -> Result<quinn::ServerConfig> {
    let cert = CertificateDer::from(cert_der.to_vec());
    let key = parse_private_key(key_der)?;

    let mut server_config = rustls::ServerConfig::builder()
        .with_client_cert_verifier(Arc::new(TofuClientCertVerifier {
            provider: crypto_provider(),
        }))
        .with_single_cert(vec![cert], key)
        .context("failed to create TLS server config")?;
    server_config.alpn_protocols = vec![ALPN_PROTOCOL.to_vec()];

    let quic_server_config = quinn::crypto::rustls::QuicServerConfig::try_from(server_config)
        .context("failed to create QUIC server config")?;

    let mut config = quinn::ServerConfig::with_crypto(Arc::new(quic_server_config));
    config.transport_config(Arc::new(configure_transport()));

    Ok(config)
}

/// Mutual-TLS client configuration.
///
/// The client presents its own self-signed certificate and accepts the server
/// certificate trust-on-first-use. The handshake signature is still verified
/// against the presented key; fingerprint pinning to the peer id happens in the
/// controller after the hello exchange.
fn configure_client(cert_der: &[u8], key_der: &[u8]) -> Result<quinn::ClientConfig> {
    let cert = CertificateDer::from(cert_der.to_vec());
    let key = parse_private_key(key_der)?;

    let mut crypto = rustls::ClientConfig::builder()
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(TofuServerCertVerifier {
            provider: crypto_provider(),
        }))
        .with_client_auth_cert(vec![cert], key)
        .context("failed to attach client certificate")?;
    crypto.alpn_protocols = vec![ALPN_PROTOCOL.to_vec()];

    let quic_client_config = quinn::crypto::rustls::QuicClientConfig::try_from(crypto)
        .context("failed to create QUIC client config")?;

    let mut config = quinn::ClientConfig::new(Arc::new(quic_client_config));
    config.transport_config(Arc::new(configure_transport()));

    Ok(config)
}

#[derive(Debug)]
struct TofuClientCertVerifier {
    provider: Arc<rustls::crypto::CryptoProvider>,
}

impl ClientCertVerifier for TofuClientCertVerifier {
    fn client_auth_mandatory(&self) -> bool {
        true
    }

    fn root_hint_subjects(&self) -> &[DistinguishedName] {
        &[]
    }

    fn verify_client_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _now: UnixTime,
    ) -> std::result::Result<ClientCertVerified, rustls::Error> {
        // Trust-on-first-use: the controller pins the fingerprint to the peer id.
        Ok(ClientCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> std::result::Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> std::result::Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.provider
            .signature_verification_algorithms
            .supported_schemes()
    }
}

#[derive(Debug)]
struct TofuServerCertVerifier {
    provider: Arc<rustls::crypto::CryptoProvider>,
}

impl ServerCertVerifier for TofuServerCertVerifier {
    fn verify_server_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> std::result::Result<ServerCertVerified, rustls::Error> {
        // Trust-on-first-use: the controller pins the fingerprint to the peer id.
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> std::result::Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> std::result::Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.provider
            .signature_verification_algorithms
            .supported_schemes()
    }
}

pub const fn max_datagram_size() -> usize {
    MAX_DATAGRAM_SIZE
}

#[cfg(test)]
mod tests {
    use super::{InboundMessage, QuicEndpoint, bind_udp_socket_with_retry};
    use crate::platform::shared::quic::CertificateStore;
    use crate::platform::shared::quic::max_datagram_size;
    use anyhow::Result;
    use std::sync::Arc;
    use std::time::Duration;
    use tempfile::TempDir;

    #[test]
    fn listening_socket_cannot_be_shared_and_can_be_rebound_after_release() -> Result<()> {
        let socket = bind_udp_socket_with_retry("0.0.0.0:0".parse()?)?;
        let addr = socket.local_addr()?;

        assert!(bind_udp_socket_with_retry(addr).is_err());
        drop(socket);
        let rebound = bind_udp_socket_with_retry(addr)?;
        assert_eq!(rebound.local_addr()?, addr);
        Ok(())
    }

    #[test]
    fn listening_port_can_be_rebound_after_connected_endpoint_shutdown() -> Result<()> {
        let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        rt.block_on(async {
            let certs = TempDir::new()?;
            let server = QuicEndpoint::new(
                0,
                "server",
                CertificateStore::load_or_create(&certs.path().join("server"))?,
            )?;
            let client = QuicEndpoint::new(
                0,
                "client",
                CertificateStore::load_or_create(&certs.path().join("client"))?,
            )?;
            let port = server.local_addr()?.port();
            let addr = std::net::SocketAddr::from((std::net::Ipv4Addr::LOCALHOST, port));
            let (inbound, outbound) =
                tokio::join!(server.accept(), client.connect(addr, "localhost"));
            drop(inbound?);
            drop(outbound?);
            server.close();
            client.close();
            drop(server);
            drop(client);

            let restarted = QuicEndpoint::new(
                port,
                "server",
                CertificateStore::load_or_create(&certs.path().join("server"))?,
            )?;
            assert_eq!(restarted.local_addr()?.port(), port);
            Ok(())
        })
    }

    #[test]
    fn endpoints_can_connect_and_accept() -> Result<()> {
        let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();

        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("tokio runtime should build");

        rt.block_on(async {
            let dir = TempDir::new()?;
            let server_store = CertificateStore::load_or_create(&dir.path().join("server"))?;
            let client_store = CertificateStore::load_or_create(&dir.path().join("client"))?;

            let server = QuicEndpoint::new(0, "server", server_store)?;
            let client = QuicEndpoint::new(0, "client", client_store)?;
            let server_addr = std::net::SocketAddr::from((
                std::net::Ipv4Addr::LOCALHOST,
                server.local_addr()?.port(),
            ));

            let accept = tokio::time::timeout(std::time::Duration::from_secs(2), server.accept());
            let connect = tokio::time::timeout(
                std::time::Duration::from_secs(2),
                client.connect(server_addr, "localhost"),
            );

            let (accept_res, connect_res) = tokio::join!(accept, connect);
            let _server_conn = accept_res??;
            let _client_conn = connect_res??;

            Ok::<(), anyhow::Error>(())
        })
    }

    #[test]
    fn quic_endpoint_connect_and_exchange_datagram() -> Result<()> {
        let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();

        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("tokio runtime should build");

        rt.block_on(async {
            let dir = TempDir::new()?;
            let server_store = CertificateStore::load_or_create(&dir.path().join("server"))?;
            let client_store = CertificateStore::load_or_create(&dir.path().join("client"))?;

            let server = QuicEndpoint::new(0, "server", server_store)?;
            let client = QuicEndpoint::new(0, "client", client_store)?;
            let server_addr = std::net::SocketAddr::from((
                std::net::Ipv4Addr::LOCALHOST,
                server.local_addr()?.port(),
            ));

            let accept = tokio::time::timeout(Duration::from_secs(2), server.accept());
            let connect = tokio::time::timeout(
                Duration::from_secs(2),
                client.connect(server_addr, "localhost"),
            );
            let (accept_res, connect_res) = tokio::join!(accept, connect);

            let server_conn = accept_res??;
            let client_conn = connect_res??;

            let payload = b"hello-datagram".to_vec();
            client_conn.send_datagram(payload.clone())?;
            let received =
                tokio::time::timeout(Duration::from_secs(2), server_conn.recv_datagram()).await??;
            assert_eq!(received, payload);

            Ok::<(), anyhow::Error>(())
        })
    }

    #[test]
    fn quic_endpoint_control_stream_reliable_delivery() -> Result<()> {
        let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();

        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("tokio runtime should build");

        rt.block_on(async {
            let dir = TempDir::new()?;
            let server_store = CertificateStore::load_or_create(&dir.path().join("server"))?;
            let client_store = CertificateStore::load_or_create(&dir.path().join("client"))?;

            let server = QuicEndpoint::new(0, "server", server_store)?;
            let client = QuicEndpoint::new(0, "client", client_store)?;
            let server_addr = std::net::SocketAddr::from((
                std::net::Ipv4Addr::LOCALHOST,
                server.local_addr()?.port(),
            ));

            let accept = tokio::time::timeout(Duration::from_secs(2), server.accept());
            let connect = tokio::time::timeout(
                Duration::from_secs(2),
                client.connect(server_addr, "localhost"),
            );
            let (accept_res, connect_res) = tokio::join!(accept, connect);

            let server_conn = accept_res??;
            let client_conn = connect_res??;

            let payload = b"reliable-control-message".to_vec();
            client_conn.send_control(payload.clone()).await?;

            let received = tokio::time::timeout(Duration::from_secs(2), async {
                loop {
                    match server_conn.try_recv_control().await {
                        Ok(data) => break Ok::<Vec<u8>, anyhow::Error>(data),
                        Err(err) if err.to_string().contains("would block") => {
                            tokio::time::sleep(Duration::from_millis(5)).await;
                        }
                        Err(err) => break Err(err),
                    }
                }
            })
            .await??;

            assert_eq!(received, payload);
            let idle_result =
                tokio::time::timeout(Duration::from_millis(20), server_conn.try_recv_control())
                    .await;
            assert!(matches!(
                idle_result,
                Ok(Err(error)) if error.to_string().contains("would block")
            ));
            Ok::<(), anyhow::Error>(())
        })
    }

    #[test]
    fn registered_reader_receives_first_control_message() -> Result<()> {
        let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();

        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("tokio runtime should build");

        rt.block_on(async {
            let dir = TempDir::new()?;
            let server_store = CertificateStore::load_or_create(&dir.path().join("server"))?;
            let client_store = CertificateStore::load_or_create(&dir.path().join("client"))?;
            let server = QuicEndpoint::new(0, "server", server_store)?;
            let client = QuicEndpoint::new(0, "client", client_store)?;
            let mut inbound = server
                .take_inbound_receiver()
                .expect("server inbound receiver should be available");
            let server_addr = std::net::SocketAddr::from((
                std::net::Ipv4Addr::LOCALHOST,
                server.local_addr()?.port(),
            ));

            let accept = tokio::time::timeout(Duration::from_secs(2), server.accept());
            let connect = tokio::time::timeout(
                Duration::from_secs(2),
                client.connect(server_addr, "localhost"),
            );
            let (accept_res, connect_res) = tokio::join!(accept, connect);
            let server_conn = accept_res??;
            let client_conn = connect_res??;
            server.register_inbound_connection(&server_conn, "client")?;
            client.register_outbound_connection(&client_conn, "server")?;

            let payload = b"first-control-message".to_vec();
            client_conn.send_control(payload.clone()).await?;
            let received = tokio::time::timeout(Duration::from_secs(2), inbound.recv())
                .await?
                .expect("server inbound channel should remain open");

            match received {
                InboundMessage::Control(connection, data) => {
                    assert_eq!(connection.peer_id(), "client");
                    assert_eq!(data, payload);
                }
                InboundMessage::Datagram(_, _) => panic!("expected reliable control message"),
            }
            Ok::<(), anyhow::Error>(())
        })
    }

    #[test]
    fn quic_datagram_rejects_oversized() -> Result<()> {
        let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();

        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("tokio runtime should build");

        rt.block_on(async {
            let dir = TempDir::new()?;
            let server_store = CertificateStore::load_or_create(&dir.path().join("server"))?;
            let client_store = CertificateStore::load_or_create(&dir.path().join("client"))?;

            let server = QuicEndpoint::new(0, "server", server_store)?;
            let client = QuicEndpoint::new(0, "client", client_store)?;
            let server_addr = std::net::SocketAddr::from((
                std::net::Ipv4Addr::LOCALHOST,
                server.local_addr()?.port(),
            ));

            let accept = tokio::time::timeout(Duration::from_secs(2), server.accept());
            let connect = tokio::time::timeout(
                Duration::from_secs(2),
                client.connect(server_addr, "localhost"),
            );
            let (accept_res, connect_res) = tokio::join!(accept, connect);

            let _server_conn = accept_res??;
            let client_conn = connect_res??;

            let oversized = vec![0u8; max_datagram_size() + 1];
            assert!(client_conn.send_datagram(oversized).is_err());

            Ok::<(), anyhow::Error>(())
        })
    }

    #[test]
    fn outbound_connection_can_be_identified_after_connect() -> Result<()> {
        let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();

        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("tokio runtime should build");

        rt.block_on(async {
            let dir = TempDir::new()?;
            let server_store = CertificateStore::load_or_create(&dir.path().join("server"))?;
            let client_store = CertificateStore::load_or_create(&dir.path().join("client"))?;

            let server = QuicEndpoint::new(0, "server", server_store)?;
            let client = QuicEndpoint::new(0, "client", client_store)?;
            let server_addr = std::net::SocketAddr::from((
                std::net::Ipv4Addr::LOCALHOST,
                server.local_addr()?.port(),
            ));

            let accept = tokio::time::timeout(Duration::from_secs(2), server.accept());
            let connect = tokio::time::timeout(
                Duration::from_secs(2),
                client.connect(server_addr, "localhost"),
            );
            let (accept_res, connect_res) = tokio::join!(accept, connect);

            let _server_conn = accept_res??;
            let client_conn = connect_res??;

            assert!(client_conn.peer_id().is_empty());
            assert!(
                client.get_connection_by_addr(server_addr).is_none(),
                "outbound connection should not be exposed before peer identification"
            );

            client.register_outbound_connection(&client_conn, "server")?;

            assert_eq!(client_conn.peer_id(), "server");
            assert!(client.get_connection("server").is_some());

            Ok::<(), anyhow::Error>(())
        })
    }

    #[test]
    fn duplicate_outbound_registration_keeps_existing_connection_primary() -> Result<()> {
        let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();

        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("tokio runtime should build");

        rt.block_on(async {
            let dir = TempDir::new()?;
            let server_store = CertificateStore::load_or_create(&dir.path().join("server"))?;
            let client_store = CertificateStore::load_or_create(&dir.path().join("client"))?;
            let server = QuicEndpoint::new(0, "server", server_store)?;
            let client = QuicEndpoint::new(0, "client", client_store)?;
            let server_addr = std::net::SocketAddr::from((
                std::net::Ipv4Addr::LOCALHOST,
                server.local_addr()?.port(),
            ));

            let first_accept = tokio::time::timeout(Duration::from_secs(2), server.accept());
            let first_connect = tokio::time::timeout(
                Duration::from_secs(2),
                client.connect(server_addr, "localhost"),
            );
            let (first_accept_res, first_connect_res) = tokio::join!(first_accept, first_connect);
            let _first_server_conn = first_accept_res??;
            let first_client_conn = first_connect_res??;
            client.register_outbound_connection(&first_client_conn, "server")?;

            let second_accept = tokio::time::timeout(Duration::from_secs(2), server.accept());
            let second_connect = tokio::time::timeout(
                Duration::from_secs(2),
                client.connect(server_addr, "localhost"),
            );
            let (second_accept_res, second_connect_res) =
                tokio::join!(second_accept, second_connect);
            let _second_server_conn = second_accept_res??;
            let second_client_conn = second_connect_res??;
            client.register_outbound_connection(&second_client_conn, "server")?;

            let primary = client
                .get_connection("server")
                .expect("first connection should remain primary");
            assert!(Arc::ptr_eq(&primary, &first_client_conn));
            assert!(client.is_canonical_connection(&first_client_conn));
            assert!(!client.is_canonical_connection(&second_client_conn));
            assert_eq!(second_client_conn.peer_id(), "server");
            assert!(!second_client_conn.is_closed());

            client.abort_connection_attempt(&second_client_conn, b"duplicate rejected");
            assert!(client.get_connection("server").is_some());

            Ok::<(), anyhow::Error>(())
        })
    }

    #[test]
    fn aborting_outbound_attempt_does_not_remove_established_inbound_for_same_addr() -> Result<()> {
        let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();

        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("tokio runtime should build");

        rt.block_on(async {
            let dir = TempDir::new()?;
            let endpoint_a_store = CertificateStore::load_or_create(&dir.path().join("a"))?;
            let endpoint_b_store = CertificateStore::load_or_create(&dir.path().join("b"))?;

            let endpoint_a = QuicEndpoint::new(0, "endpoint-a", endpoint_a_store)?;
            let endpoint_b = QuicEndpoint::new(0, "endpoint-b", endpoint_b_store)?;
            let endpoint_a_addr = std::net::SocketAddr::from((
                std::net::Ipv4Addr::LOCALHOST,
                endpoint_a.local_addr()?.port(),
            ));
            let endpoint_b_addr = std::net::SocketAddr::from((
                std::net::Ipv4Addr::LOCALHOST,
                endpoint_b.local_addr()?.port(),
            ));

            let first_accept = tokio::time::timeout(Duration::from_secs(2), endpoint_b.accept());
            let first_connect = tokio::time::timeout(
                Duration::from_secs(2),
                endpoint_a.connect(endpoint_b_addr, "localhost"),
            );
            let (first_accept_res, first_connect_res) = tokio::join!(first_accept, first_connect);
            let established_inbound = first_accept_res??;
            let _established_outbound = first_connect_res??;

            endpoint_b.register_inbound_connection(&established_inbound, "endpoint-a")?;
            assert!(endpoint_b.get_connection("endpoint-a").is_some());
            assert!(endpoint_b.get_connection_by_addr(endpoint_a_addr).is_some());

            let second_accept = tokio::time::timeout(Duration::from_secs(2), endpoint_a.accept());
            let second_connect = tokio::time::timeout(
                Duration::from_secs(2),
                endpoint_b.connect(endpoint_a_addr, "localhost"),
            );
            let (second_accept_res, second_connect_res) =
                tokio::join!(second_accept, second_connect);
            let _accepted_on_a = second_accept_res??;
            let failed_outbound_attempt = second_connect_res??;

            endpoint_b.abort_connection_attempt(&failed_outbound_attempt, b"test abort");

            assert!(
                endpoint_b.get_connection("endpoint-a").is_some(),
                "aborting the retry must not remove the established inbound connection"
            );
            assert!(
                endpoint_b.get_connection_by_addr(endpoint_a_addr).is_some(),
                "address mapping for the established inbound connection must remain"
            );

            Ok::<(), anyhow::Error>(())
        })
    }

    #[test]
    fn closing_specific_duplicate_preserves_primary_connection() -> Result<()> {
        let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();

        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("tokio runtime should build");

        rt.block_on(async {
            let dir = TempDir::new()?;
            let endpoint_a_store = CertificateStore::load_or_create(&dir.path().join("a"))?;
            let endpoint_b_store = CertificateStore::load_or_create(&dir.path().join("b"))?;

            let endpoint_a = QuicEndpoint::new(0, "endpoint-a", endpoint_a_store)?;
            let endpoint_b = QuicEndpoint::new(0, "endpoint-b", endpoint_b_store)?;
            let endpoint_a_addr = std::net::SocketAddr::from((
                std::net::Ipv4Addr::LOCALHOST,
                endpoint_a.local_addr()?.port(),
            ));
            let endpoint_b_addr = std::net::SocketAddr::from((
                std::net::Ipv4Addr::LOCALHOST,
                endpoint_b.local_addr()?.port(),
            ));

            let first_accept = tokio::time::timeout(Duration::from_secs(2), endpoint_b.accept());
            let first_connect = tokio::time::timeout(
                Duration::from_secs(2),
                endpoint_a.connect(endpoint_b_addr, "localhost"),
            );
            let (first_accept_res, first_connect_res) = tokio::join!(first_accept, first_connect);
            let _accepted_on_b = first_accept_res??;
            let established_outbound = first_connect_res??;
            endpoint_a.register_outbound_connection(&established_outbound, "endpoint-b")?;

            let second_accept = tokio::time::timeout(Duration::from_secs(2), endpoint_a.accept());
            let second_connect = tokio::time::timeout(
                Duration::from_secs(2),
                endpoint_b.connect(endpoint_a_addr, "localhost"),
            );
            let (second_accept_res, second_connect_res) =
                tokio::join!(second_accept, second_connect);
            let accepted_inbound = second_accept_res??;
            let _outbound_from_b = second_connect_res??;
            endpoint_a.register_inbound_connection(&accepted_inbound, "endpoint-b")?;

            endpoint_a.close_duplicate_connection(&accepted_inbound, b"duplicate rejected");

            let canonical = endpoint_a
                .get_connection("endpoint-b")
                .expect("primary outbound should still be registered");
            assert!(Arc::ptr_eq(&canonical, &established_outbound));

            let by_addr = endpoint_a
                .get_connection_by_addr(endpoint_b_addr)
                .expect("address mapping should still point at the primary outbound");
            assert!(Arc::ptr_eq(&by_addr, &established_outbound));
            assert!(accepted_inbound.is_closed());

            Ok::<(), anyhow::Error>(())
        })
    }

    #[test]
    fn promoted_inbound_connection_becomes_canonical_after_primary_close() -> Result<()> {
        let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();

        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("tokio runtime should build");

        rt.block_on(async {
            let dir = TempDir::new()?;
            let endpoint_a_store = CertificateStore::load_or_create(&dir.path().join("a"))?;
            let endpoint_b_store = CertificateStore::load_or_create(&dir.path().join("b"))?;

            let endpoint_a = QuicEndpoint::new(0, "endpoint-a", endpoint_a_store)?;
            let endpoint_b = QuicEndpoint::new(0, "endpoint-b", endpoint_b_store)?;
            let endpoint_a_addr = std::net::SocketAddr::from((
                std::net::Ipv4Addr::LOCALHOST,
                endpoint_a.local_addr()?.port(),
            ));
            let endpoint_b_addr = std::net::SocketAddr::from((
                std::net::Ipv4Addr::LOCALHOST,
                endpoint_b.local_addr()?.port(),
            ));

            let first_accept = tokio::time::timeout(Duration::from_secs(2), endpoint_b.accept());
            let first_connect = tokio::time::timeout(
                Duration::from_secs(2),
                endpoint_a.connect(endpoint_b_addr, "localhost"),
            );
            let (first_accept_res, first_connect_res) = tokio::join!(first_accept, first_connect);
            let _accepted_on_b = first_accept_res??;
            let established_outbound = first_connect_res??;
            endpoint_a.register_outbound_connection(&established_outbound, "endpoint-b")?;

            let second_accept = tokio::time::timeout(Duration::from_secs(2), endpoint_a.accept());
            let second_connect = tokio::time::timeout(
                Duration::from_secs(2),
                endpoint_b.connect(endpoint_a_addr, "localhost"),
            );
            let (second_accept_res, second_connect_res) =
                tokio::join!(second_accept, second_connect);
            let accepted_inbound = second_accept_res??;
            let _outbound_from_b = second_connect_res??;
            endpoint_a.register_inbound_connection(&accepted_inbound, "endpoint-b")?;

            endpoint_a.promote_connection("endpoint-b", &accepted_inbound)?;

            let canonical = endpoint_a
                .get_connection("endpoint-b")
                .expect("promoted inbound should be registered as canonical");
            assert!(Arc::ptr_eq(&canonical, &accepted_inbound));

            let by_addr = endpoint_a
                .get_connection_by_addr(endpoint_b_addr)
                .expect("address mapping should point at the promoted inbound");
            assert!(Arc::ptr_eq(&by_addr, &accepted_inbound));
            assert!(established_outbound.is_closed());

            Ok::<(), anyhow::Error>(())
        })
    }

    #[test]
    fn stranded_handshake_does_not_delay_later_incoming() -> Result<()> {
        let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();

        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("tokio runtime should build");

        rt.block_on(async {
            let dir = TempDir::new()?;
            let server = QuicEndpoint::new(
                0,
                "server",
                CertificateStore::load_or_create(&dir.path().join("server"))?,
            )?;
            let server_addr = std::net::SocketAddr::from((
                std::net::Ipv4Addr::LOCALHOST,
                server.local_addr()?.port(),
            ));

            // A slow peer whose handshake is never completed must not delay
            // any other inbound attempt.
            let slow = new_raw_client(&dir, "slow")?;
            let slow_port = slow.local_addr()?.port();
            let slow_task = tokio::spawn(async move {
                let connecting = slow
                    .connect(server_addr, "localhost")
                    .expect("slow connect should start");
                let _ = connecting.await;
            });
            let slow_arrived =
                tokio::time::timeout(Duration::from_secs(2), server.accept_incoming()).await??;
            assert_eq!(slow_arrived.remote_address().port(), slow_port);

            let victim = QuicEndpoint::new(
                0,
                "victim",
                CertificateStore::load_or_create(&dir.path().join("victim"))?,
            )?;
            let victim_port = victim.local_addr()?.port();
            let victim_task =
                tokio::spawn(async move { victim.connect(server_addr, "localhost").await });
            let prompt =
                tokio::time::timeout(Duration::from_secs(2), server.accept_incoming()).await?;
            let prompt = prompt.expect("later dial must yield an incoming promptly");
            assert_eq!(prompt.remote_address().port(), victim_port);
            let established = server.complete_incoming(prompt).await?;
            assert_eq!(established.addr().port(), victim_port);
            let victim_conn = tokio::time::timeout(Duration::from_secs(2), victim_task).await???;
            assert_eq!(victim_conn.addr(), server_addr);
            victim_conn.close(b"test done");
            slow_task.abort();
            Ok::<(), anyhow::Error>(())
        })
    }

    fn new_raw_client(
        dir: &TempDir,
        peer_id: &str,
    ) -> Result<quinn::Endpoint, anyhow::Error> {
        let mut store =
            CertificateStore::load_or_create(&dir.path().join(format!("raw-{peer_id}")))?;
        let (cert_der, key_der) = store.get_or_create_self_cert(peer_id)?;
        let mut endpoint = quinn::Endpoint::new(
            quinn::EndpointConfig::default(),
            None,
            std::net::UdpSocket::bind("127.0.0.1:0")?,
            std::sync::Arc::new(super::BoundTokioRuntime {
                handle: super::quic_runtime_handle(),
            }),
        )?;
        endpoint.set_default_client_config(super::configure_client(&cert_der, &key_der)?);
        Ok(endpoint)
    }
}
