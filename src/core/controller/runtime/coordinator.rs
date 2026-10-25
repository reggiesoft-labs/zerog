use super::ControllerEvent;
use super::loop_runner::{ControllerLoop, ControllerLoopInput};
use crate::core::api::StatusSnapshot;
use crate::core::controller::config_state::ConfigStore;
use crate::core::controller::events::PeerUpdate;
use crate::core::controller::identity::{LocalIdentity, cert_path};
use crate::core::controller::peer_connector::{PeerConnector, SharedPeerConnector};
use crate::core::controller::peers::{ConnectedPeer, PeerStore};
use crate::core::controller::{ConfigSnapshot, CoreError, CoreResult, EnginePairFactory};
use crate::core::discovery::{DiscoveredPeer, DiscoveryClient, DiscoveryEvent};
use crate::core::handshake::HandshakePeer;
use crate::core::peer_state::{ConnectionStatus, PeerRuntimeStateSnapshot};
use crate::core::platform::types::PeerEdge;
use crate::core::platform::{DisplayBounds, EdgeAssignments};
use crate::events::LayoutUpdateEvent;
use crate::quic::{CertificateStore, QuicEndpoint};
use std::future::Future;
use std::sync::Arc;
use tokio::sync::mpsc;
use tokio::sync::{broadcast, oneshot};
use tokio::task::JoinSet;

pub(crate) struct RunningController {
    pub(super) platform: Arc<dyn crate::core::platform::PlatformEngine>,
    pub(super) session: Arc<dyn crate::core::platform::SessionEngine>,
    pub(super) peers: Arc<PeerStore>,
    pub(super) discovery: DiscoveryClient,
    pub(super) updates: Arc<broadcast::Sender<PeerUpdate>>,
    pub(super) controller_tx: mpsc::Sender<ControllerEvent>,
    pub(super) shutdown_tx: oneshot::Sender<()>,
    pub(super) shutdown_ack_rx: oneshot::Receiver<()>,
    pub(super) runtime_handle: tokio::runtime::Handle,
    pub(super) join_handle: tokio::task::JoinHandle<()>,
}

pub(crate) struct StartupGuard {
    discovery: Option<DiscoveryClient>,
    platform: Option<Arc<dyn crate::core::platform::PlatformEngine>>,
    session: Option<Arc<dyn crate::core::platform::SessionEngine>>,
}

impl StartupGuard {
    pub(super) fn new(discovery: DiscoveryClient) -> Self {
        Self {
            discovery: Some(discovery),
            platform: None,
            session: None,
        }
    }

    pub(super) fn complete(mut self) -> DiscoveryClient {
        self.platform.take();
        self.session.take();
        self.discovery
            .take()
            .expect("startup guard must own discovery")
    }
}

impl Drop for StartupGuard {
    fn drop(&mut self) {
        if let Some(platform) = self.platform.take() {
            let _ = platform.stop();
        }
        if let Some(session) = self.session.take() {
            let _ = session.shutdown();
        }

        if let Some(mut discovery) = self.discovery.take() {
            discovery.stop();
        }
    }
}

impl RunningController {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn start(
        port: u16,
        config_store: ConfigStore,
        config: &ConfigSnapshot,
        identity: &LocalIdentity,
        peers: Arc<PeerStore>,
        pair_factory: EnginePairFactory,
        primary_edge: PeerEdge,
        updates: Arc<broadcast::Sender<PeerUpdate>>,
    ) -> CoreResult<Self> {
        let mut discovery = DiscoveryClient::new();
        let discovery_rx = discovery
            .start(
                port,
                Some(&identity.discovery_hostname()),
                Some(&identity.peer_id),
                None,
            )
            .map_err(|e| CoreError::Discovery(e.to_string()))?;
        let mut startup_guard = StartupGuard::new(discovery);

        let (platform, session) = pair_factory(primary_edge)?;
        startup_guard.platform = Some(Arc::clone(&platform));
        startup_guard.session = Some(Arc::clone(&session));
        Self::ensure_permissions(&platform)?;

        let quic_endpoint = Arc::new(
            QuicEndpoint::new(
                port,
                &identity.peer_id,
                CertificateStore::load_or_create(&cert_path()).map_err(|e| {
                    CoreError::Engine(format!("failed to create certificate store: {}", e))
                })?,
            )
            .map_err(|e| CoreError::Engine(format!("failed to create QUIC endpoint: {}", e)))?,
        );

        Self::configure_engine(&session, Arc::clone(&quic_endpoint), config, identity)?;
        session
            .reconcile_layout(config)
            .map_err(|e| CoreError::Engine(e.to_string()))?;
        let layout_update_rx = session
            .take_layout_update_rx()
            .map_err(|e| CoreError::Engine(e.to_string()))?
            .ok_or_else(|| CoreError::Engine("layout update receiver already taken".to_string()))?;

        let peer_connector = Self::build_peer_connector(
            Arc::clone(&quic_endpoint),
            identity,
            Arc::clone(&platform),
        )?;
        let (shutdown_tx, shutdown_rx) = oneshot::channel();
        let (shutdown_ack_tx, shutdown_ack_rx) = oneshot::channel();

        let event_rx = platform
            .start()
            .map_err(|e| CoreError::Engine(e.to_string()))?;
        session
            .start(event_rx)
            .map_err(|e| CoreError::Engine(e.to_string()))?;
        let session_event_rx = session
            .take_session_event_rx()
            .map_err(|e| CoreError::Engine(e.to_string()))?
            .ok_or_else(|| CoreError::Engine("session event receiver already taken".to_string()))?;
        let (controller_tx, controller_rx) = mpsc::channel(32);
        let display_manager = platform.get_display_manager();
        let display_bounds = display_manager.get_virtual_bounds().ok();
        let display_rects = display_manager.discover_displays().unwrap_or_default();
        let scan_local_peer = HandshakePeer::local(
            &identity.peer_id,
            &identity.name,
            display_bounds,
            display_rects,
        );
        let controller_loop = ControllerLoop {
            config_store,
            local_peer_id: identity.peer_id.clone(),
            peers: Arc::clone(&peers),
            updates: Arc::clone(&updates),
            connector: peer_connector,
            session: Arc::clone(&session),
            controller_tx: controller_tx.clone(),
            background_tasks: JoinSet::new(),
            endpoint: Arc::clone(&quic_endpoint),
            pending_pairings: std::collections::HashMap::new(),
            pending_outbound_pairings: std::collections::HashMap::new(),
        };
        let loop_input = ControllerLoopInput {
            discovery_rx,
            layout_update_rx,
            session_event_rx,
            controller_rx,
            shutdown_rx,
            shutdown_ack_tx,
            scan_endpoint: quic_endpoint,
            scan_local_peer,
            scan_port: port,
        };
        let runtime_handle = tokio::runtime::Handle::current();
        let join_handle = tokio::spawn(controller_loop.run(loop_input));

        let discovery = startup_guard.complete();

        Ok(Self {
            platform,
            session,
            peers,
            discovery,
            updates,
            controller_tx,
            shutdown_tx,
            shutdown_ack_rx,
            runtime_handle,
            join_handle,
        })
    }

    pub(crate) fn transfer_sync(&self) -> Option<Arc<crate::core::transfer::TransferSync>> {
        self.session.transfer_sync()
    }

    pub(crate) fn connect_peer(&self, peer_id: &str) -> CoreResult<()> {
        if self.peers.is_connected(peer_id) {
            return Ok(());
        }
        let Some(_peer) = self.peers.discovered_peer(peer_id) else {
            return Err(CoreError::Connection(format!(
                "peer {} not found in discovered peers",
                peer_id
            )));
        };

        self.controller_tx
            .try_send(ControllerEvent::ConnectPeer {
                peer_id: peer_id.to_string(),
            })
            .map_err(|error| CoreError::Connection(error.to_string()))
    }

    pub(crate) fn connect_address(&self, addr: std::net::SocketAddr) -> CoreResult<()> {
        if let Some(existing_peer_id) = self.peers.find_peer_by_addr(addr) {
            return self.connect_peer(&existing_peer_id);
        }

        let fake_peer_id = format!("direct-{}", addr);
        let peer = DiscoveredPeer {
            peer_id: fake_peer_id,
            primary_addr: addr,
            addrs: vec![addr],
            hostname: addr.to_string(),
            fullname: format!("{}-direct.{}", addr, crate::core::policies::DISCOVERY_SERVICE_TYPE),
            platform: None,
        };

        self.controller_tx
            .try_send(ControllerEvent::Discovery(DiscoveryEvent::Discovered(peer)))
            .map_err(|error| CoreError::Connection(error.to_string()))
    }

    pub(crate) fn respond_pairing(&self, peer_id: &str, approved: bool) -> CoreResult<()> {
        self.controller_tx
            .try_send(ControllerEvent::PairingDecision {
                peer_id: peer_id.to_string(),
                approved,
            })
            .map_err(|error| CoreError::Connection(error.to_string()))
    }

    pub(crate) fn stop(self, connected_peer_ids: Vec<String>) {
        let RunningController {
            platform,
            session,
            discovery,
            shutdown_tx,
            shutdown_ack_rx,
            runtime_handle,
            join_handle,
            ..
        } = self;

        let _ = shutdown_tx.send(());
        for peer_id in connected_peer_ids {
            let _ = session.remove_peer(&peer_id);
        }

        let _ = session.shutdown();
        let _ = platform.stop();

        let mut discovery = discovery;
        discovery.stop();

        let can_wait = Self::wait_for_runtime(&runtime_handle, async {
            let _ = tokio::time::timeout(std::time::Duration::from_millis(500), shutdown_ack_rx).await;
        });

        if can_wait {
            let mut join_handle = join_handle;
            let _ = Self::wait_for_runtime(&runtime_handle, async {
                let _ = tokio::time::timeout(std::time::Duration::from_millis(500), &mut join_handle).await;
            });
            join_handle.abort();
        } else {
            join_handle.abort();
        }
    }

    fn wait_for_runtime<F>(runtime_handle: &tokio::runtime::Handle, future: F) -> bool
    where
        F: Future<Output = ()>,
    {
        if tokio::runtime::Handle::try_current().is_err() {
            runtime_handle.block_on(future);
            return true;
        }

        if matches!(
            runtime_handle.runtime_flavor(),
            tokio::runtime::RuntimeFlavor::MultiThread
        ) {
            tokio::task::block_in_place(|| runtime_handle.block_on(future));
            return true;
        }

        false
    }

    pub(crate) fn remove_peer(&self, peer_id: &str) -> CoreResult<()> {
        self.session
            .remove_peer(peer_id)
            .map_err(|e| CoreError::Connection(e.to_string()))
    }

    pub(crate) fn cancel_connect(&self, peer_id: &str) -> CoreResult<()> {
        self.peers.set_idle(peer_id);
        // Notify the loop so a pending outbound pairing waiter is cleared
        // and its promoted connection is closed. Best effort: the loop may
        // already be gone during shutdown.
        let _ = self.controller_tx.try_send(ControllerEvent::CancelConnect {
            peer_id: peer_id.to_string(),
        });
        // Best effort: drop any session record for a peer that never fully
        // connected (outbound pairing pending has no record yet, but a
        // racing inbound might). The loop closes the pending QUIC conn via
        // its stored handle.
        let _ = self.session.remove_peer(peer_id);
        Ok(())
    }

    pub(crate) fn disconnect_peer(&self, peer_id: &str) -> CoreResult<Option<ConnectedPeer>> {
        self.peers.set_idle(peer_id);
        // A Disconnect click doubles as Cancel while dialing or waiting for
        // pairing approval: clear the loop's outbound waiter so a late dial
        // cannot resurrect pairing state after we go idle.
        let _ = self.controller_tx.try_send(ControllerEvent::CancelConnect {
            peer_id: peer_id.to_string(),
        });
        if self.peers.connected(peer_id).is_none() {
            return Ok(None);
        }

        self.remove_peer(peer_id)?;
        if let Some(sync) = self.transfer_sync() {
            sync.notify_peer_disconnected(Some(peer_id));
        }
        let connected = self.peers.remove_connected(peer_id);
        if connected.is_some() {
            let _ = self.updates.send(PeerUpdate::ConnectionLost {
                peer_id: peer_id.to_string(),
            });
        }
        Ok(connected)
    }

    pub(crate) fn unpair_peer(&self, peer_id: &str) -> CoreResult<Option<ConnectedPeer>> {
        self.peers.set_idle(peer_id);
        // Notify the controller loop so it drops the pinned certificate and
        // clears pairing waiters even when no session peer is active. Without
        // this, forgetting a disconnected peer leaves trust behind and a
        // later Connect can deadlock waiting for an approval that never comes.
        let _ = self.controller_tx.try_send(ControllerEvent::PeerUnpaired {
            peer_id: peer_id.to_string(),
            attempt: None,
        });
        if let Some(sync) = self.transfer_sync() {
            sync.notify_peer_disconnected(Some(peer_id));
        }
        // Always attempt the session notify: the session may still hold a
        // record (and a notify address) even when the peerstore shows idle.
        let session_result = self
            .session
            .unpair_peer(peer_id)
            .map_err(|e| CoreError::Connection(e.to_string()));
        let connected = self.peers.remove_connected(peer_id);
        if connected.is_some() {
            let _ = self.updates.send(PeerUpdate::ConnectionLost {
                peer_id: peer_id.to_string(),
            });
        }
        session_result?;
        Ok(connected)
    }

    pub(crate) fn emergency_unlock(&self) -> CoreResult<()> {
        self.session
            .emergency_unlock()
            .map_err(|e| CoreError::Engine(e.to_string()))
    }

    pub(crate) fn status(&self) -> StatusSnapshot {
        let metrics = self.session.get_throughput_metrics();
        let runtime_peers = self.session.get_peer_runtime_state();
        let mut connected_peers = Vec::new();
        let mut incoming_connections = Vec::new();

        for peer in runtime_peers {
            if peer.transport_reachability == crate::core::peer_state::TransportReachability::TimedOut {
                continue;
            }

            let platform = self
                .peers
                .connected(&peer.peer_id)
                .and_then(|c| c.platform)
                .or_else(|| {
                    self.peers
                        .discovered_peer(&peer.peer_id)
                        .and_then(|d| d.platform)
                });
            let peer_ref = crate::core::api::PeerRefSnapshot {
                peer_id: peer.peer_id,
                name: peer.name,
                platform,
            };

            match peer.connection_status {
                ConnectionStatus::Connected => connected_peers.push(peer_ref),
                ConnectionStatus::Incoming => incoming_connections.push(peer_ref),
                ConnectionStatus::Disconnected => {}
            }
        }

        StatusSnapshot {
            engine_running: true,
            connected_peers,
            incoming_connections,
            out_events_per_sec: metrics.out_events_per_sec,
            in_events_per_sec: metrics.in_events_per_sec,
        }
    }

    pub(crate) fn local_display_topology(&self) -> (Option<DisplayBounds>, Vec<DisplayBounds>) {
        let display = self.platform.get_display_manager();
        let virtual_bounds = display.get_virtual_bounds().ok();
        let display_rects = display.discover_displays().unwrap_or_default();
        (virtual_bounds, display_rects)
    }

    pub(crate) fn peer_runtime_state(&self) -> Vec<PeerRuntimeStateSnapshot> {
        self.session.get_peer_runtime_state()
    }

    pub(crate) fn reconcile_peer_edges(&self, assignments: &EdgeAssignments) -> CoreResult<()> {
        self.session
            .reconcile_peer_edges(assignments)
            .map_err(|e| CoreError::Engine(e.to_string()))
    }

    pub(crate) fn reconcile_layout(
        &self,
        config: &crate::core::api::ConfigSnapshot,
    ) -> CoreResult<()> {
        self.session
            .reconcile_layout(config)
            .map_err(|e| CoreError::Engine(e.to_string()))
    }

    pub(crate) fn send_layout_update(&self, update: LayoutUpdateEvent) -> CoreResult<()> {
        self.session
            .send_layout_update(update)
            .map_err(|e| CoreError::Engine(e.to_string()))
    }

    fn ensure_permissions(
        engine: &Arc<dyn crate::core::platform::PlatformEngine>,
    ) -> CoreResult<()> {
        let checker = engine.get_permission_checker();
        if checker.check_accessibility_permission() {
            return Ok(());
        }

        checker
            .prompt_accessibility_permission()
            .map_err(|e| CoreError::PermissionDenied(e.to_string()))
    }

    fn configure_engine(
        engine: &Arc<dyn crate::core::platform::SessionEngine>,
        endpoint: Arc<QuicEndpoint>,
        config: &ConfigSnapshot,
        identity: &LocalIdentity,
    ) -> CoreResult<()> {
        engine
            .configure_runtime(
                endpoint,
                config.batch_window_us,
                config.max_events_per_packet,
                config.movement_cadence_us,
                identity.peer_id.clone(),
                500,
                2000,
            )
            .map_err(|e| CoreError::Engine(e.to_string()))?;

        engine
            .set_local_identity(identity.peer_ref())
            .map_err(|e| CoreError::Engine(e.to_string()))
    }

    fn build_peer_connector(
        quic_endpoint: Arc<QuicEndpoint>,
        identity: &LocalIdentity,
        platform: Arc<dyn crate::core::platform::PlatformEngine>,
    ) -> CoreResult<SharedPeerConnector> {
        let local_display_bounds = platform.get_display_manager().get_virtual_bounds().ok();
        let local_display_rects = platform
            .get_display_manager()
            .discover_displays()
            .unwrap_or_default();
        Ok(Arc::new(PeerConnector::new(
            quic_endpoint,
            identity.peer_id.clone(),
            identity.name.clone(),
            local_display_bounds,
            local_display_rects,
        )))
    }
}
