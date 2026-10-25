//! Archived prototype — isolated from production `RuntimeEngine`.
//! See `findings/production-path.md` and `diagrams/architecture.md`.
//! The production handshake/session authority is `core::handshake` +
//! `PeerStore`/`RunningController`; this binary is kept only as a reference
//! for deterministic tie-breaking logic.

use anyhow::{Context, Result, anyhow, bail};
use flume::{Receiver, RecvTimeoutError};
use mdns_sd::{IfKind, ServiceDaemon, ServiceEvent, ServiceInfo};
use rcgen::{
    CertificateParams, DistinguishedName, DnType, ExtendedKeyUsagePurpose, KeyPair, KeyUsagePurpose,
};
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::env;
use std::io::{self, IsTerminal};
use std::net::{IpAddr, Ipv4Addr, SocketAddr, UdpSocket};
use std::process;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};
use tracing::{error, info, warn};
use uuid::Uuid;

const DEFAULT_PORT: u16 = mouse_thing::core::policies::DEFAULT_PORT;
const DEFAULT_LOG_FILTER: &str = "info";
const DEFAULT_SERVICE: &str = mouse_thing::core::policies::DISCOVERY_SERVICE_TYPE;
const ALPN_PROTOCOL: &[u8] = b"minimal-mdns-quic";
const DISCOVERY_POLL_INTERVAL: Duration = Duration::from_millis(100);
const RETRY_BACKOFF: Duration = Duration::from_secs(1);
const TXT_KEY_PEER_ID: &str = "peer_id";
const TXT_KEY_INSTANCE_ID: &str = "instance_id";
const TXT_KEY_NAME: &str = "name";
const TXT_KEY_VERSION: &str = "version";

#[derive(Debug, Clone, PartialEq, Eq)]
struct Args {
    name: String,
    port: u16,
    connect: Option<SocketAddr>,
    run_for: Option<Duration>,
    log_filter: String,
    service: String,
}

impl Default for Args {
    fn default() -> Self {
        Self {
            name: default_name(),
            port: DEFAULT_PORT,
            connect: None,
            run_for: None,
            log_filter: DEFAULT_LOG_FILTER.to_string(),
            service: DEFAULT_SERVICE.to_string(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ParsedArgs {
    Run(Args),
    Help(String),
}

#[derive(Debug, Clone)]
struct LocalIdentity {
    peer_id: String,
    instance_id: String,
    name: String,
}

impl LocalIdentity {
    fn generate(name: String) -> Self {
        Self {
            peer_id: Uuid::new_v4().to_string(),
            instance_id: Uuid::new_v4().to_string(),
            name,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct HelloMessage {
    peer_id: String,
    instance_id: String,
    name: String,
}

#[derive(Debug)]
struct DiscoveryHandle {
    stop_tx: Option<mpsc::Sender<()>>,
    join_handle: Option<thread::JoinHandle<()>>,
}

impl DiscoveryHandle {
    fn start(
        identity: Arc<LocalIdentity>,
        port: u16,
        service_type: String,
        event_tx: tokio::sync::mpsc::UnboundedSender<AppEvent>,
    ) -> Result<Self> {
        let (stop_tx, stop_rx) = mpsc::channel::<()>();
        let (startup_tx, startup_rx) = mpsc::sync_channel::<Result<()>>(1);

        let handle =
            thread::spawn(
                move || match DiscoveryRuntime::new(&identity, port, &service_type) {
                    Ok(runtime) => {
                        if startup_tx.send(Ok(())).is_err() {
                            return;
                        }
                        run_discovery_loop(runtime, identity, event_tx, stop_rx);
                    }
                    Err(error) => {
                        let _ = startup_tx.send(Err(error));
                    }
                },
            );

        match startup_rx.recv() {
            Ok(Ok(())) => Ok(Self {
                stop_tx: Some(stop_tx),
                join_handle: Some(handle),
            }),
            Ok(Err(error)) => {
                let _ = handle.join();
                Err(error)
            }
            Err(_) => {
                let _ = handle.join();
                bail!("discovery thread exited before startup completed");
            }
        }
    }

    fn stop(&mut self) {
        if let Some(stop_tx) = self.stop_tx.take() {
            let _ = stop_tx.send(());
        }
        if let Some(join_handle) = self.join_handle.take() {
            let _ = join_handle.join();
        }
    }
}

impl Drop for DiscoveryHandle {
    fn drop(&mut self) {
        self.stop();
    }
}

struct DiscoveryRuntime {
    daemon: ServiceDaemon,
    local_service_fullname: String,
    browse_rx: Receiver<ServiceEvent>,
}

impl DiscoveryRuntime {
    fn new(identity: &LocalIdentity, port: u16, service_type: &str) -> Result<Self> {
        let daemon = ServiceDaemon::new().context("failed to create mDNS daemon")?;
        let local_ip = IpAddr::V4(local_ipv4()?);
        daemon
            .disable_interface(IfKind::All)
            .context("failed to disable default mDNS interfaces")?;
        daemon
            .enable_interface(local_ip)
            .with_context(|| format!("failed to enable mDNS interface for {local_ip}"))?;

        let service_info = build_service_info(identity, port, service_type, local_ip)?;
        let local_service_fullname = service_info.get_fullname().to_string();

        daemon
            .register(service_info)
            .context("failed to register mDNS service")?;
        info!(
            service_type,
            fullname = %local_service_fullname,
            local_ip = %local_ip,
            port,
            "mDNS service registered"
        );

        let browse_rx = match daemon.browse(service_type) {
            Ok(browse_rx) => browse_rx,
            Err(error) => {
                let _ = daemon.unregister(&local_service_fullname);
                let _ = daemon.shutdown();
                return Err(error).context(format!("failed to browse {service_type}"));
            }
        };

        info!(service_type, "mDNS browse started");
        Ok(Self {
            daemon,
            local_service_fullname,
            browse_rx,
        })
    }
}

impl Drop for DiscoveryRuntime {
    fn drop(&mut self) {
        let _ = self.daemon.unregister(&self.local_service_fullname);
        let _ = self.daemon.shutdown();
    }
}

#[derive(Debug, Clone)]
struct DiscoveredPeer {
    peer_id: String,
    instance_id: String,
    name: String,
    version: String,
    addr: SocketAddr,
    fullname: String,
}

#[derive(Debug)]
enum AppEvent {
    Shutdown,
    PeerSeen(DiscoveredPeer),
    PeerExpired(DiscoveredPeer),
    HelloComplete(IdentifiedSession),
    HelloFailed {
        direction: Direction,
        peer_hint: Option<DiscoveredPeer>,
        addr: SocketAddr,
        error: String,
    },
    SessionClosed {
        peer_id: String,
        session_id: String,
        direction: Direction,
    },
    RetryPeer {
        peer_id: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Direction {
    Inbound,
    Outbound,
}

#[derive(Debug)]
struct IdentifiedSession {
    session_id: String,
    direction: Direction,
    peer: RemotePeer,
    connection: quinn::Connection,
}

#[derive(Debug, Clone)]
struct RemotePeer {
    peer_id: String,
    instance_id: String,
    name: String,
    addr: SocketAddr,
}

#[derive(Debug, Clone)]
enum DialState {
    Idle,
    Connecting,
    Backoff(Instant),
}

#[derive(Debug)]
struct PeerState {
    discovered: Option<DiscoveredPeer>,
    dial_state: DialState,
    session_ids: Vec<String>,
    canonical_session_id: Option<String>,
}

impl Default for PeerState {
    fn default() -> Self {
        Self {
            discovered: None,
            dial_state: DialState::Idle,
            session_ids: Vec::new(),
            canonical_session_id: None,
        }
    }
}

#[derive(Debug)]
struct SessionEntry {
    session_id: String,
    direction: Direction,
    peer: RemotePeer,
    connection: quinn::Connection,
}

impl HelloMessage {
    fn from_identity(identity: &LocalIdentity) -> Self {
        Self {
            peer_id: identity.peer_id.clone(),
            instance_id: identity.instance_id.clone(),
            name: identity.name.clone(),
        }
    }
}

fn main() -> Result<()> {
    let program_name = env::args()
        .next()
        .unwrap_or_else(|| "minimal-mdns-quic".to_string());
    let args = match parse_args_from(env::args().skip(1), &program_name)? {
        ParsedArgs::Run(args) => args,
        ParsedArgs::Help(help) => {
            println!("{help}");
            return Ok(());
        }
    };

    mouse_thing::install_crypto_provider();
    init_tracing(&args.log_filter)?;

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("failed to build tokio runtime")?;

    runtime.block_on(async_main(args))
}

async fn async_main(args: Args) -> Result<()> {
    let identity = Arc::new(LocalIdentity::generate(args.name));
    let endpoint = make_endpoint(args.port)?;
    let listen_addr = endpoint
        .local_addr()
        .context("failed to get local listen address")?;
    let (event_tx, event_rx) = tokio::sync::mpsc::unbounded_channel();
    let shutting_down = Arc::new(AtomicBool::new(false));

    info!(
        peer_id = %identity.peer_id,
        instance_id = %identity.instance_id,
        name = %identity.name,
        listen_addr = %listen_addr,
        connect = args.connect.map(|addr| addr.to_string()),
        service_type = %args.service,
        "minimal QUIC prototype started"
    );
    if run_interactive_shutdown_hint(args.run_for) {
        info!("press Ctrl+C or type 'q' then Enter to stop");
    }

    let mut discovery = DiscoveryHandle::start(
        Arc::clone(&identity),
        listen_addr.port(),
        args.service.clone(),
        event_tx.clone(),
    )?;

    let manager_identity = Arc::clone(&identity);
    let manager_endpoint = endpoint.clone();
    let manager_tx = event_tx.clone();
    let manager_shutdown = Arc::clone(&shutting_down);
    let event_task = tokio::spawn(async move {
        run_session_manager(
            manager_identity,
            manager_endpoint,
            manager_tx,
            manager_shutdown,
            event_rx,
        )
        .await;
    });

    let accept_endpoint = endpoint.clone();
    let accept_identity = Arc::clone(&identity);
    let accept_tx = event_tx.clone();
    let accept_shutdown = Arc::clone(&shutting_down);
    let accept_task = tokio::spawn(async move {
        if let Err(error) =
            run_accept_loop(accept_endpoint, accept_identity, accept_tx, accept_shutdown).await
        {
            error!(error = %error, "accept loop exited");
        }
    });

    if let Some(remote_addr) = args.connect {
        start_outbound_connect(
            event_tx.clone(),
            endpoint.clone(),
            Arc::clone(&identity),
            Arc::clone(&shutting_down),
            remote_addr,
            None,
        );
    }

    wait_for_shutdown(args.run_for).await;
    info!("shutting down minimal QUIC prototype");
    shutting_down.store(true, Ordering::Relaxed);
    discovery.stop();
    endpoint.close(0u32.into(), b"prototype shutdown");
    let _ = event_tx.send(AppEvent::Shutdown);

    drop(event_tx);
    let _ = accept_task.await;
    let _ = event_task.await;

    Ok(())
}

async fn run_session_manager(
    identity: Arc<LocalIdentity>,
    endpoint: quinn::Endpoint,
    event_tx: tokio::sync::mpsc::UnboundedSender<AppEvent>,
    shutting_down: Arc<AtomicBool>,
    mut event_rx: tokio::sync::mpsc::UnboundedReceiver<AppEvent>,
) {
    let mut peers = HashMap::<String, PeerState>::new();
    let mut sessions = HashMap::<String, SessionEntry>::new();

    while let Some(event) = event_rx.recv().await {
        match event {
            AppEvent::Shutdown => {
                info!("stopping session manager");
                break;
            }
            AppEvent::PeerSeen(peer) => {
                info!(
                    remote_peer_id = %peer.peer_id,
                    remote_instance_id = %peer.instance_id,
                    remote_name = %peer.name,
                    remote_version = %peer.version,
                    remote_addr = %peer.addr,
                    fullname = %peer.fullname,
                    "peer discovered over mDNS"
                );

                let state = peers.entry(peer.peer_id.clone()).or_default();
                state.discovered = Some(peer.clone());

                if state.session_ids.is_empty() {
                    let should_connect = match state.dial_state {
                        DialState::Idle => true,
                        DialState::Connecting => false,
                        DialState::Backoff(until) => Instant::now() >= until,
                    };

                    if should_connect {
                        info!(
                            remote_peer_id = %peer.peer_id,
                            remote_addr = %peer.addr,
                            "starting outbound connect from discovery"
                        );
                        state.dial_state = DialState::Connecting;
                        start_outbound_connect(
                            event_tx.clone(),
                            endpoint.clone(),
                            Arc::clone(&identity),
                            Arc::clone(&shutting_down),
                            peer.addr,
                            Some(peer),
                        );
                    }
                }
            }
            AppEvent::PeerExpired(peer) => {
                info!(
                    remote_peer_id = %peer.peer_id,
                    remote_instance_id = %peer.instance_id,
                    remote_name = %peer.name,
                    remote_addr = %peer.addr,
                    fullname = %peer.fullname,
                    "peer expired from mDNS"
                );

                if let Some(state) = peers.get_mut(&peer.peer_id) {
                    if state
                        .discovered
                        .as_ref()
                        .is_some_and(|current| current.instance_id == peer.instance_id)
                    {
                        state.discovered = None;
                    }
                }
            }
            AppEvent::HelloComplete(identified) => {
                let state = peers.entry(identified.peer.peer_id.clone()).or_default();
                state.dial_state = DialState::Idle;
                if let Some(existing_id) = state.canonical_session_id.clone() {
                    if let Some(existing) = sessions.get(&existing_id) {
                        let existing_session_id = existing.session_id.clone();
                        let existing_direction = existing.direction;
                        let existing_connection = existing.connection.clone();
                        let keep_new = should_keep_new_session(
                            &identity.peer_id,
                            &identified.peer.peer_id,
                            existing_direction,
                            identified.direction,
                        );

                        if keep_new {
                            info!(
                                remote_peer_id = %identified.peer.peer_id,
                                keep_direction = ?identified.direction,
                                drop_direction = ?existing_direction,
                                "duplicate connection resolved in favor of new session"
                            );
                            close_session(existing_connection, b"duplicate connection replaced");
                            state
                                .session_ids
                                .retain(|session_id| session_id != &existing_session_id);
                            sessions.remove(&existing_session_id);
                            state.canonical_session_id = Some(identified.session_id.clone());
                        } else {
                            info!(
                                remote_peer_id = %identified.peer.peer_id,
                                keep_direction = ?existing_direction,
                                drop_direction = ?identified.direction,
                                "duplicate connection resolved in favor of existing session"
                            );
                            close_session(identified.connection, b"duplicate connection rejected");
                            continue;
                        }
                    } else {
                        state.canonical_session_id = Some(identified.session_id.clone());
                    }
                } else {
                    state.canonical_session_id = Some(identified.session_id.clone());
                }

                state.session_ids.push(identified.session_id.clone());
                info!(
                    session_id = %identified.session_id,
                    direction = ?identified.direction,
                    remote_peer_id = %identified.peer.peer_id,
                    remote_instance_id = %identified.peer.instance_id,
                    remote_name = %identified.peer.name,
                    remote_addr = %identified.peer.addr,
                    canonical = state.canonical_session_id.as_deref() == Some(identified.session_id.as_str()),
                    "session established"
                );

                watch_connection_close(
                    event_tx.clone(),
                    identified.connection.clone(),
                    identified.peer.peer_id.clone(),
                    identified.session_id.clone(),
                    identified.direction,
                );

                sessions.insert(
                    identified.session_id.clone(),
                    SessionEntry {
                        session_id: identified.session_id,
                        direction: identified.direction,
                        peer: identified.peer,
                        connection: identified.connection,
                    },
                );
            }
            AppEvent::HelloFailed {
                direction,
                peer_hint,
                addr,
                error,
            } => {
                warn!(direction = ?direction, remote_addr = %addr, error = %error, "hello exchange failed");
                if let Some(peer) = peer_hint
                    && let Some(state) = peers.get_mut(&peer.peer_id)
                {
                    state.dial_state = DialState::Backoff(Instant::now() + RETRY_BACKOFF);
                    schedule_retry(
                        event_tx.clone(),
                        Arc::clone(&shutting_down),
                        peer.peer_id.clone(),
                        RETRY_BACKOFF,
                    );
                }
            }
            AppEvent::SessionClosed {
                peer_id,
                session_id,
                direction,
            } => {
                let removed = sessions.remove(&session_id);
                info!(
                    session_id = %session_id,
                    remote_peer_id = %peer_id,
                    remote_instance_id = removed.as_ref().map(|entry| entry.peer.instance_id.as_str()),
                    remote_name = removed.as_ref().map(|entry| entry.peer.name.as_str()),
                    direction = ?direction,
                    had_entry = removed.is_some(),
                    "session closed"
                );

                if let Some(state) = peers.get_mut(&peer_id) {
                    state.session_ids.retain(|existing| existing != &session_id);
                    if state.canonical_session_id.as_deref() == Some(session_id.as_str()) {
                        state.canonical_session_id = state.session_ids.first().cloned();
                    }
                    if state.session_ids.is_empty() {
                        if state.discovered.is_some() && !shutting_down.load(Ordering::Relaxed) {
                            state.dial_state = DialState::Backoff(Instant::now() + RETRY_BACKOFF);
                            schedule_retry(
                                event_tx.clone(),
                                Arc::clone(&shutting_down),
                                peer_id.clone(),
                                RETRY_BACKOFF,
                            );
                        } else {
                            state.dial_state = DialState::Idle;
                        }
                    }
                }
            }
            AppEvent::RetryPeer { peer_id } => {
                if let Some(state) = peers.get_mut(&peer_id) {
                    if !state.session_ids.is_empty()
                        || matches!(state.dial_state, DialState::Connecting)
                    {
                        continue;
                    }

                    let Some(peer) = state.discovered.clone() else {
                        state.dial_state = DialState::Idle;
                        continue;
                    };

                    if let DialState::Backoff(until) = state.dial_state
                        && Instant::now() < until
                    {
                        schedule_retry(
                            event_tx.clone(),
                            Arc::clone(&shutting_down),
                            peer_id.clone(),
                            until.saturating_duration_since(Instant::now()),
                        );
                        continue;
                    }

                    info!(
                        remote_peer_id = %peer.peer_id,
                        remote_addr = %peer.addr,
                        "retrying outbound connect after backoff"
                    );
                    state.dial_state = DialState::Connecting;
                    start_outbound_connect(
                        event_tx.clone(),
                        endpoint.clone(),
                        Arc::clone(&identity),
                        Arc::clone(&shutting_down),
                        peer.addr,
                        Some(peer),
                    );
                }
            }
        }
    }
}

fn should_keep_new_session(
    local_peer_id: &str,
    remote_peer_id: &str,
    existing_direction: Direction,
    new_direction: Direction,
) -> bool {
    if existing_direction == new_direction {
        return false;
    }

    let preferred = preferred_direction(local_peer_id, remote_peer_id);
    new_direction == preferred
}

fn preferred_direction(local_peer_id: &str, remote_peer_id: &str) -> Direction {
    if local_peer_id < remote_peer_id {
        Direction::Outbound
    } else {
        Direction::Inbound
    }
}

fn close_session(connection: quinn::Connection, reason: &[u8]) {
    connection.close(0u32.into(), reason);
}

async fn run_accept_loop(
    endpoint: quinn::Endpoint,
    identity: Arc<LocalIdentity>,
    event_tx: tokio::sync::mpsc::UnboundedSender<AppEvent>,
    shutting_down: Arc<AtomicBool>,
) -> Result<()> {
    while let Some(incoming) = endpoint.accept().await {
        let remote_addr = incoming.remote_address();
        let accept_identity = Arc::clone(&identity);
        let accept_tx = event_tx.clone();
        let accept_shutdown = Arc::clone(&shutting_down);

        tokio::spawn(async move {
            if accept_shutdown.load(Ordering::Relaxed) {
                return;
            }
            match incoming.accept() {
                Ok(connecting) => match connecting.await {
                    Ok(connection) => {
                        match identify_inbound_connection(connection, accept_identity).await {
                            Ok(identified) => {
                                if accept_tx.send(AppEvent::HelloComplete(identified)).is_err() {
                                    warn!("session manager dropped before inbound hello result");
                                }
                            }
                            Err(error) => {
                                warn!(remote_addr = %remote_addr, error = %error, "inbound connection failed");
                            }
                        }
                    }
                    Err(error) => {
                        warn!(remote_addr = %remote_addr, error = %error, "inbound QUIC handshake failed");
                    }
                },
                Err(error) => {
                    warn!(remote_addr = %remote_addr, error = %error, "failed to accept inbound connection");
                }
            }
        });
    }

    Ok(())
}

fn start_outbound_connect(
    event_tx: tokio::sync::mpsc::UnboundedSender<AppEvent>,
    endpoint: quinn::Endpoint,
    identity: Arc<LocalIdentity>,
    shutting_down: Arc<AtomicBool>,
    remote_addr: SocketAddr,
    peer_hint: Option<DiscoveredPeer>,
) {
    tokio::spawn(async move {
        if shutting_down.load(Ordering::Relaxed) {
            return;
        }
        match connect_and_identify(endpoint, identity, remote_addr).await {
            Ok(identified) => {
                if event_tx.send(AppEvent::HelloComplete(identified)).is_err() {
                    warn!("session manager dropped before outbound hello result");
                }
            }
            Err(error) => {
                let _ = event_tx.send(AppEvent::HelloFailed {
                    direction: Direction::Outbound,
                    peer_hint,
                    addr: remote_addr,
                    error: error.to_string(),
                });
            }
        }
    });
}

fn watch_connection_close(
    event_tx: tokio::sync::mpsc::UnboundedSender<AppEvent>,
    connection: quinn::Connection,
    peer_id: String,
    session_id: String,
    direction: Direction,
) {
    tokio::spawn(async move {
        connection.closed().await;
        let _ = event_tx.send(AppEvent::SessionClosed {
            peer_id,
            session_id,
            direction,
        });
    });
}

fn schedule_retry(
    event_tx: tokio::sync::mpsc::UnboundedSender<AppEvent>,
    shutting_down: Arc<AtomicBool>,
    peer_id: String,
    delay: Duration,
) {
    tokio::spawn(async move {
        tokio::time::sleep(delay).await;
        if shutting_down.load(Ordering::Relaxed) {
            return;
        }
        let _ = event_tx.send(AppEvent::RetryPeer { peer_id });
    });
}

async fn connect_and_identify(
    endpoint: quinn::Endpoint,
    identity: Arc<LocalIdentity>,
    remote_addr: SocketAddr,
) -> Result<IdentifiedSession> {
    info!(remote_addr = %remote_addr, "starting outbound QUIC connect");
    let connection = endpoint
        .connect(remote_addr, "localhost")
        .map_err(|error| anyhow!("failed to initiate connect: {error}"))?
        .await
        .map_err(|error| anyhow!("connect failed: {error}"))?;

    info!(remote_addr = %remote_addr, "outbound QUIC connection established");
    let peer = perform_outbound_hello(connection.clone(), identity).await?;

    Ok(IdentifiedSession {
        session_id: Uuid::new_v4().to_string(),
        direction: Direction::Outbound,
        peer,
        connection,
    })
}

async fn identify_inbound_connection(
    connection: quinn::Connection,
    identity: Arc<LocalIdentity>,
) -> Result<IdentifiedSession> {
    let remote_addr = connection.remote_address();
    let peer = perform_inbound_hello(connection.clone(), identity).await?;
    info!(remote_addr = %remote_addr, "accepted inbound QUIC connection");

    Ok(IdentifiedSession {
        session_id: Uuid::new_v4().to_string(),
        direction: Direction::Inbound,
        peer,
        connection,
    })
}

async fn perform_outbound_hello(
    connection: quinn::Connection,
    identity: Arc<LocalIdentity>,
) -> Result<RemotePeer> {
    let remote_addr = connection.remote_address();
    let (mut send, mut recv) = connection
        .open_bi()
        .await
        .map_err(|error| anyhow!("failed to open hello stream: {error}"))?;

    let local_hello = HelloMessage::from_identity(&identity);
    write_message(&mut send, &local_hello).await?;
    send.finish()
        .map_err(|error| anyhow!("failed to finish hello stream: {error}"))?;

    let remote_hello = read_message(&mut recv).await?;
    info!(
        remote_addr = %remote_addr,
        remote_peer_id = %remote_hello.peer_id,
        remote_instance_id = %remote_hello.instance_id,
        remote_name = %remote_hello.name,
        "outbound hello exchange completed"
    );

    Ok(RemotePeer {
        peer_id: remote_hello.peer_id,
        instance_id: remote_hello.instance_id,
        name: remote_hello.name,
        addr: remote_addr,
    })
}

async fn perform_inbound_hello(
    connection: quinn::Connection,
    identity: Arc<LocalIdentity>,
) -> Result<RemotePeer> {
    let remote_addr = connection.remote_address();
    let (mut send, mut recv) = connection
        .accept_bi()
        .await
        .map_err(|error| anyhow!("failed to accept hello stream: {error}"))?;

    let remote_hello = read_message(&mut recv).await?;
    info!(
        remote_addr = %remote_addr,
        remote_peer_id = %remote_hello.peer_id,
        remote_instance_id = %remote_hello.instance_id,
        remote_name = %remote_hello.name,
        "inbound hello received"
    );

    let local_hello = HelloMessage::from_identity(&identity);
    write_message(&mut send, &local_hello).await?;
    send.finish()
        .map_err(|error| anyhow!("failed to finish hello response: {error}"))?;

    info!(
        remote_addr = %remote_addr,
        remote_peer_id = %remote_hello.peer_id,
        "inbound hello exchange completed"
    );

    Ok(RemotePeer {
        peer_id: remote_hello.peer_id,
        instance_id: remote_hello.instance_id,
        name: remote_hello.name,
        addr: remote_addr,
    })
}

async fn write_message(stream: &mut quinn::SendStream, message: &HelloMessage) -> Result<()> {
    let data = serde_json::to_vec(message).context("failed to serialize hello message")?;
    let len = u32::try_from(data.len()).context("hello message too large")?;

    tokio::io::AsyncWriteExt::write_all(stream, &len.to_le_bytes())
        .await
        .context("failed to write hello length")?;
    tokio::io::AsyncWriteExt::write_all(stream, &data)
        .await
        .context("failed to write hello payload")?;

    Ok(())
}

async fn read_message(stream: &mut quinn::RecvStream) -> Result<HelloMessage> {
    let mut len_buf = [0u8; 4];
    tokio::io::AsyncReadExt::read_exact(stream, &mut len_buf)
        .await
        .context("failed to read hello length")?;
    let len = u32::from_le_bytes(len_buf) as usize;
    let mut data = vec![0u8; len];
    tokio::io::AsyncReadExt::read_exact(stream, &mut data)
        .await
        .context("failed to read hello payload")?;

    serde_json::from_slice(&data).context("failed to decode hello message")
}

async fn wait_for_shutdown(run_for: Option<Duration>) {
    if let Some(duration) = run_for {
        tokio::time::sleep(duration).await;
        return;
    }

    let stdin_shutdown_rx = spawn_stdin_shutdown_listener();

    #[cfg(unix)]
    {
        let mut terminate =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                .expect("failed to install SIGTERM handler");
        tokio::select! {
            result = tokio::signal::ctrl_c() => {
                if let Err(error) = result {
                    warn!(error = %error, "failed while waiting for Ctrl+C");
                }
            }
            _ = terminate.recv() => {}
            _ = wait_for_stdin_shutdown(stdin_shutdown_rx) => {}
        }
    }

    #[cfg(not(unix))]
    {
        tokio::select! {
            result = tokio::signal::ctrl_c() => {
                if let Err(error) = result {
                    warn!(error = %error, "failed while waiting for Ctrl+C");
                }
            }
            _ = wait_for_stdin_shutdown(stdin_shutdown_rx) => {}
        }
    }
}

fn run_interactive_shutdown_hint(run_for: Option<Duration>) -> bool {
    run_for.is_none() && io::stdin().is_terminal()
}

fn spawn_stdin_shutdown_listener() -> Option<tokio::sync::oneshot::Receiver<()>> {
    if !io::stdin().is_terminal() {
        return None;
    }

    let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel();
    thread::spawn(move || {
        if read_stdin_shutdown_command() {
            let _ = shutdown_tx.send(());
        }
    });
    Some(shutdown_rx)
}

async fn wait_for_stdin_shutdown(stdin_shutdown_rx: Option<tokio::sync::oneshot::Receiver<()>>) {
    match stdin_shutdown_rx {
        Some(shutdown_rx) => {
            let _ = shutdown_rx.await;
        }
        None => std::future::pending::<()>().await,
    }
}

fn read_stdin_shutdown_command() -> bool {
    let stdin = io::stdin();
    let mut line = String::new();

    loop {
        line.clear();
        match stdin.read_line(&mut line) {
            Ok(0) => return false,
            Ok(_) => match line.trim() {
                "q" | "quit" | "exit" => return true,
                _ => {}
            },
            Err(error) => {
                warn!(error = %error, "failed to read shutdown command from stdin");
                return false;
            }
        }
    }
}

fn make_endpoint(port: u16) -> Result<quinn::Endpoint> {
    let bind_addr = SocketAddr::from(([0, 0, 0, 0], port));
    let (cert_der, key_der) = generate_self_signed_cert("minimal-mdns-quic")?;

    let server_config = configure_server(&cert_der, &key_der)?;
    let client_config = configure_client()?;

    let mut endpoint = quinn::Endpoint::server(server_config, bind_addr)
        .context("failed to bind QUIC endpoint")?;
    endpoint.set_default_client_config(client_config);
    Ok(endpoint)
}

fn run_discovery_loop(
    runtime: DiscoveryRuntime,
    identity: Arc<LocalIdentity>,
    event_tx: tokio::sync::mpsc::UnboundedSender<AppEvent>,
    stop_rx: mpsc::Receiver<()>,
) {
    let mut peers_by_fullname = HashMap::<String, DiscoveredPeer>::new();

    loop {
        if stop_rx.try_recv().is_ok() {
            info!("stopping mDNS discovery loop");
            break;
        }

        match runtime.browse_rx.recv_timeout(DISCOVERY_POLL_INTERVAL) {
            Ok(ServiceEvent::ServiceResolved(info)) => {
                let fullname = info.get_fullname().to_string();
                if fullname == runtime.local_service_fullname {
                    continue;
                }

                if let Some(peer) = discovered_peer_from_service_info(&info, &identity) {
                    peers_by_fullname.insert(fullname, peer.clone());
                    if event_tx.send(AppEvent::PeerSeen(peer)).is_err() {
                        break;
                    }
                }
            }
            Ok(ServiceEvent::ServiceRemoved(fullname, _)) => {
                if fullname == runtime.local_service_fullname {
                    continue;
                }
                if let Some(peer) = peers_by_fullname.remove(&fullname) {
                    if event_tx.send(AppEvent::PeerExpired(peer)).is_err() {
                        break;
                    }
                }
            }
            Ok(ServiceEvent::SearchStopped(service_type)) => {
                warn!(service_type, "mDNS browse stopped");
                break;
            }
            Ok(ServiceEvent::ServiceFound(_, _)) => {}
            Ok(_) => {}
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => {
                warn!("mDNS browse channel disconnected");
                break;
            }
        }
    }
}

fn build_service_info(
    identity: &LocalIdentity,
    port: u16,
    service_type: &str,
    local_ip: IpAddr,
) -> Result<ServiceInfo> {
    let hostname = default_name();
    let host_fqdn = format!("{}.local.", sanitize_host(&hostname));
    let instance_name = format!(
        "{}-{}",
        sanitize_instance(&identity.name),
        identity.instance_id
    );
    let properties = vec![
        (TXT_KEY_PEER_ID.to_string(), identity.peer_id.clone()),
        (
            TXT_KEY_INSTANCE_ID.to_string(),
            identity.instance_id.clone(),
        ),
        (TXT_KEY_NAME.to_string(), identity.name.clone()),
        (
            TXT_KEY_VERSION.to_string(),
            env!("CARGO_PKG_VERSION").to_string(),
        ),
    ];

    ServiceInfo::new(
        service_type,
        &instance_name,
        &host_fqdn,
        local_ip,
        port,
        &properties[..],
    )
    .context("failed to build mDNS service info")
}

fn discovered_peer_from_service_info(
    info: &ServiceInfo,
    identity: &LocalIdentity,
) -> Option<DiscoveredPeer> {
    let peer_id = trimmed_property(info, TXT_KEY_PEER_ID)?;
    let instance_id = trimmed_property(info, TXT_KEY_INSTANCE_ID)?;
    if peer_id == identity.peer_id && instance_id == identity.instance_id {
        return None;
    }

    let addr = preferred_socket_addr(info)?;
    let name = trimmed_property(info, TXT_KEY_NAME).unwrap_or_else(|| "unknown".to_string());
    let version = trimmed_property(info, TXT_KEY_VERSION)
        .unwrap_or_else(|| env!("CARGO_PKG_VERSION").to_string());

    Some(DiscoveredPeer {
        peer_id,
        instance_id,
        name,
        version,
        addr,
        fullname: info.get_fullname().to_string(),
    })
}

fn trimmed_property(info: &ServiceInfo, key: &str) -> Option<String> {
    info.get_property_val_str(key)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
}

fn preferred_socket_addr(info: &ServiceInfo) -> Option<SocketAddr> {
    info.get_addresses()
        .iter()
        .copied()
        .filter(|ip| !ip.is_loopback())
        .min_by_key(|ip| match ip {
            IpAddr::V4(_) => 0,
            IpAddr::V6(_) => 1,
        })
        .map(|ip| SocketAddr::new(ip, info.get_port()))
}

fn local_ipv4() -> Result<Ipv4Addr> {
    let socket = UdpSocket::bind("0.0.0.0:0").context("failed to bind UDP socket for local IP")?;
    socket
        .connect("224.0.0.251:5353")
        .context("failed to derive local IP via multicast connect")?;

    match socket
        .local_addr()
        .context("failed to read local socket address")?
        .ip()
    {
        IpAddr::V4(ip) => Ok(ip),
        IpAddr::V6(_) => bail!("unable to determine IPv4 address for mDNS advertisement"),
    }
}

fn sanitize_instance(value: &str) -> String {
    value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '-' {
                ch
            } else {
                '-'
            }
        })
        .collect::<String>()
        .trim_matches('-')
        .to_string()
}

fn sanitize_host(value: &str) -> String {
    sanitize_instance(value).to_lowercase()
}

fn configure_server(cert_der: &[u8], key_der: &[u8]) -> Result<quinn::ServerConfig> {
    let cert = rustls::pki_types::CertificateDer::from(cert_der.to_vec());
    let key = rustls::pki_types::PrivateKeyDer::try_from(key_der.to_vec())
        .map_err(|error| anyhow!("failed to parse private key: {error}"))?;

    let mut tls_config = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(vec![cert], key)
        .context("failed to create TLS server config")?;
    tls_config.alpn_protocols = vec![ALPN_PROTOCOL.to_vec()];

    let quic_config = quinn::crypto::rustls::QuicServerConfig::try_from(tls_config)
        .context("failed to create QUIC server config")?;
    let mut server_config = quinn::ServerConfig::with_crypto(Arc::new(quic_config));
    server_config.transport_config(Arc::new(configure_transport()));
    Ok(server_config)
}

fn configure_client() -> Result<quinn::ClientConfig> {
    let mut tls_config = rustls::ClientConfig::builder()
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(SkipServerVerification))
        .with_no_client_auth();
    tls_config.alpn_protocols = vec![ALPN_PROTOCOL.to_vec()];

    let quic_config = quinn::crypto::rustls::QuicClientConfig::try_from(tls_config)
        .context("failed to create QUIC client config")?;
    let mut client_config = quinn::ClientConfig::new(Arc::new(quic_config));
    client_config.transport_config(Arc::new(configure_transport()));
    Ok(client_config)
}

fn configure_transport() -> quinn::TransportConfig {
    let mut config = quinn::TransportConfig::default();
    config.max_concurrent_bidi_streams(4u32.into());
    config.keep_alive_interval(Some(Duration::from_secs(1)));
    config
}

fn generate_self_signed_cert(common_name: &str) -> Result<(Vec<u8>, Vec<u8>)> {
    let mut params = CertificateParams::new(vec![])
        .map_err(|error| anyhow!("failed to create certificate params: {error}"))?;
    params.distinguished_name = DistinguishedName::new();
    params
        .distinguished_name
        .push(DnType::CommonName, common_name);
    params.key_usages.push(KeyUsagePurpose::DigitalSignature);
    params
        .extended_key_usages
        .push(ExtendedKeyUsagePurpose::ServerAuth);
    params
        .extended_key_usages
        .push(ExtendedKeyUsagePurpose::ClientAuth);

    let key_pair =
        KeyPair::generate().map_err(|error| anyhow!("failed to generate key pair: {error}"))?;
    let cert = params
        .self_signed(&key_pair)
        .map_err(|error| anyhow!("failed to sign certificate: {error}"))?;

    Ok((cert.der().to_vec(), key_pair.serialize_der()))
}

fn init_tracing(log_filter: &str) -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_new(log_filter)
                .with_context(|| format!("invalid log filter: {log_filter}"))?,
        )
        .with_target(false)
        .try_init()
        .map_err(|error| anyhow!("failed to initialize tracing subscriber: {error}"))
}

fn parse_args_from<I>(args: I, program_name: &str) -> Result<ParsedArgs>
where
    I: IntoIterator<Item = String>,
{
    let mut parsed = Args::default();
    let mut args = args.into_iter();

    while let Some(arg) = args.next() {
        match arg.as_str() {
            "-h" | "--help" => return Ok(ParsedArgs::Help(usage(program_name))),
            "--name" => {
                parsed.name = non_empty_arg(args.next(), "--name")?;
            }
            "--port" => {
                parsed.port = parse_value::<u16>(args.next(), "--port")?;
            }
            "--connect" => {
                parsed.connect = Some(parse_value::<SocketAddr>(args.next(), "--connect")?);
            }
            "--run-for-secs" => {
                let seconds = parse_value::<u64>(args.next(), "--run-for-secs")?;
                if seconds == 0 {
                    bail!("--run-for-secs must be greater than 0");
                }
                parsed.run_for = Some(Duration::from_secs(seconds));
            }
            "--log" => {
                parsed.log_filter = non_empty_arg(args.next(), "--log")?;
            }
            "--service" => {
                parsed.service = non_empty_arg(args.next(), "--service")?;
            }
            unknown => bail!("unknown argument: {unknown}\n\n{}", usage(program_name)),
        }
    }

    Ok(ParsedArgs::Run(parsed))
}

fn parse_value<T>(value: Option<String>, flag: &str) -> Result<T>
where
    T: std::str::FromStr,
    T::Err: std::fmt::Display,
{
    let raw = next_arg(value, flag)?;
    raw.parse::<T>()
        .map_err(|error| anyhow!("{flag} expects a valid value: {error}"))
}

fn non_empty_arg(value: Option<String>, flag: &str) -> Result<String> {
    let value = next_arg(value, flag)?;
    let trimmed = value.trim();
    if trimmed.is_empty() {
        bail!("{flag} must not be empty");
    }
    Ok(trimmed.to_string())
}

fn next_arg(value: Option<String>, flag: &str) -> Result<String> {
    value.ok_or_else(|| anyhow!("{flag} requires a value"))
}

fn default_name() -> String {
    hostname::get()
        .ok()
        .and_then(|hostname| hostname.into_string().ok())
        .map(|hostname| hostname.trim().to_string())
        .filter(|hostname| !hostname.is_empty())
        .unwrap_or_else(|| format!("minimal-mdns-quic-{}", process::id()))
}

fn usage(program_name: &str) -> String {
    format!(
        "Minimal QUIC prototype for phase 1.\n\n\
Usage:\n  {program_name} [options]\n\n\
Options:\n\
  --name <display-name>         Local display name (default: hostname)\n\
  --port <udp-port>             Local UDP listen port (default: {DEFAULT_PORT})\n\
  --connect <host:port>         Directly dial a peer after startup\n\
  --run-for-secs <seconds>      Exit automatically after the given duration\n\
  --log <filter>                Tracing filter (default: {DEFAULT_LOG_FILTER})\n\
  --service <dns-sd-type>       mDNS service type (default: {DEFAULT_SERVICE})\n\
  -h, --help                    Show this help\n"
    )
}

#[derive(Debug)]
struct SkipServerVerification;

impl ServerCertVerifier for SkipServerVerification {
    fn verify_server_cert(
        &self,
        _end_entity: &rustls::pki_types::CertificateDer<'_>,
        _intermediates: &[rustls::pki_types::CertificateDer<'_>],
        _server_name: &rustls::pki_types::ServerName<'_>,
        _ocsp_response: &[u8],
        _now: rustls::pki_types::UnixTime,
    ) -> std::result::Result<ServerCertVerified, rustls::Error> {
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &rustls::pki_types::CertificateDer<'_>,
        _dss: &rustls::DigitallySignedStruct,
    ) -> std::result::Result<HandshakeSignatureValid, rustls::Error> {
        Ok(HandshakeSignatureValid::assertion())
    }

    fn verify_tls13_signature(
        &self,
        _message: &[u8],
        _cert: &rustls::pki_types::CertificateDer<'_>,
        _dss: &rustls::DigitallySignedStruct,
    ) -> std::result::Result<HandshakeSignatureValid, rustls::Error> {
        Ok(HandshakeSignatureValid::assertion())
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        vec![
            rustls::SignatureScheme::RSA_PKCS1_SHA256,
            rustls::SignatureScheme::RSA_PKCS1_SHA384,
            rustls::SignatureScheme::RSA_PKCS1_SHA512,
            rustls::SignatureScheme::ECDSA_NISTP256_SHA256,
            rustls::SignatureScheme::ECDSA_NISTP384_SHA384,
            rustls::SignatureScheme::ECDSA_NISTP521_SHA512,
            rustls::SignatureScheme::RSA_PSS_SHA256,
            rustls::SignatureScheme::RSA_PSS_SHA384,
            rustls::SignatureScheme::RSA_PSS_SHA512,
            rustls::SignatureScheme::ED25519,
        ]
    }
}
