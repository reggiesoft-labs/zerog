//! Application controller facade for mouse-thing.
//!
//! The controller keeps the app-facing API small and delegates stateful work to
//! a few concrete helpers:
//! - `ConfigStore` owns persisted configuration
//! - `LocalIdentity` owns stable local peer identity
//! - `PeerStore` owns discovered/connected peer state
//! - `RunningController` owns the live engine + discovery runtime

use std::sync::Arc;
use std::time::Duration;

pub use crate::core::api::{ConfigSnapshot, PeerSnapshot, StatusSnapshot};
use crate::core::api::{DiscoveryStatusSnapshot, DisplayRectSnapshot, VirtualBoundsSnapshot};
use crate::core::error::{CoreError, CoreResult};
use crate::core::events::LayoutUpdateEvent;
use crate::core::platform::types::PeerEdge;
use crate::core::platform::{DisplayBounds, EdgeAssignments, PlatformEngine, SessionEngine};

mod config_state;
pub mod events;
mod identity;
mod peer_connector;
mod peers;
mod runtime;

use config_state::{ConfigStore, primary_peer_edge};
use identity::LocalIdentity;
use peers::PeerStore;
use runtime::RunningController;

pub(super) type EnginePairFactory = Arc<
    dyn Fn(PeerEdge) -> CoreResult<(Arc<dyn PlatformEngine>, Arc<dyn SessionEngine>)>
        + Send
        + Sync,
>;

/// App-facing controller used by the CLI and GUI.
pub struct Controller {
    peers: Arc<PeerStore>,
    pair_factory: EnginePairFactory,
    config: ConfigStore,
    identity: LocalIdentity,
    updates: Arc<tokio::sync::broadcast::Sender<events::PeerUpdate>>,
    runtime: Option<RunningController>,
}

impl Controller {
    /// Creates a controller from a split platform/session factory sharing the same display.
    pub fn with_engine_pair(
        pair_factory: impl Fn(PeerEdge) -> CoreResult<(Arc<dyn PlatformEngine>, Arc<dyn SessionEngine>)>
            + Send
            + Sync
            + 'static,
    ) -> Self {
        let pair_factory: EnginePairFactory = Arc::new(pair_factory);
        Self::with_pair_factory_inner(pair_factory)
    }

    fn with_pair_factory_inner(pair_factory: EnginePairFactory) -> Self {
        let config = ConfigStore::load();
        let config_snapshot = config.snapshot();
        let edge_assignments = EdgeAssignments::from_config(&config_snapshot);
        let peers = Arc::new(PeerStore::new(config.clone()));
        peers.replace_configured_edges(&edge_assignments);
        let (updates_tx, _) = tokio::sync::broadcast::channel(100);
        let updates = Arc::new(updates_tx);
        Self {
            peers,
            pair_factory,
            config,
            identity: LocalIdentity::load(),
            updates,
            runtime: None,
        }
    }

    /// Starts the controller and platform engine.
    pub fn start(&mut self, port: u16) -> CoreResult<()> {
        if self.runtime.is_some() {
            return Ok(());
        }

        let config = self.config.snapshot();
        let edge_assignments = EdgeAssignments::from_config(&config);
        self.refresh_configured_peers(&edge_assignments);
        let peer_ids = self.peers.discovered_peer_ids();
        let runtime = RunningController::start(
            port,
            self.config.clone(),
            &config,
            &self.identity,
            Arc::clone(&self.peers),
            Arc::clone(&self.pair_factory),
            primary_peer_edge(&config),
            Arc::clone(&self.updates),
        )?;
        self.runtime = Some(runtime);

        if let Some(runtime) = &self.runtime {
            for peer_id in peer_ids {
                if config.should_auto_connect_peer(&peer_id) {
                    let _ = runtime.connect_peer(&peer_id);
                }
            }
        }

        let _ = self
            .updates
            .send(events::PeerUpdate::StatusChanged(self.get_status()));
        let _ = self
            .updates
            .send(events::PeerUpdate::Discovered(self.peers.snapshots(
                &self.identity.peer_id,
            )));

        Ok(())
    }

    /// Stops the controller and platform engine.
    pub fn stop(&mut self) {
        if !self.stop_runtime() {
            return;
        }

        self.peers.clear();
        let _ = self.updates.send(events::PeerUpdate::Discovered(Vec::new()));
        let _ = self.updates.send(events::PeerUpdate::StatusChanged(self.get_status()));
    }

    pub fn restart(&mut self, port: u16) -> CoreResult<()> {
        if let Some((runtime, connected_peer_ids)) = self.take_runtime() {
            self.peers.prepare_for_restart();
            runtime.stop(connected_peer_ids);
            self.peers.prepare_for_restart();
        }

        self.start(port)
    }

    fn stop_runtime(&mut self) -> bool {
        let Some((runtime, connected_peer_ids)) = self.take_runtime() else {
            return false;
        };

        runtime.stop(connected_peer_ids);
        true
    }

    fn take_runtime(&mut self) -> Option<(RunningController, Vec<String>)> {
        let runtime = self.runtime.take()?;
        let connected_peer_ids = self.peers.connected_peer_ids();
        Some((runtime, connected_peer_ids))
    }

    /// Discovers peers on the local network.
    ///
    /// Discovery is push-driven; this method simply gives the runtime time to
    /// populate the cache and then returns the current snapshot.
    pub fn discover_peers(&mut self, wait: Duration) -> CoreResult<Vec<PeerSnapshot>> {
        std::thread::sleep(wait);
        Ok(self.get_peers_cached())
    }

    /// Returns cached discovered peers without blocking.
    pub fn get_peers_cached(&self) -> Vec<PeerSnapshot> {
        let mut peers = self
            .runtime
            .as_ref()
            .map(|runtime| {
                self.peers
                    .runtime_snapshots(&runtime.peer_runtime_state(), &self.identity.peer_id)
            })
            .unwrap_or_else(|| self.peers.snapshots(&self.identity.peer_id));
        peers.push(self.local_peer_snapshot());
        peers
    }

    /// Connects to a peer by ID.
    pub fn connect(&mut self, peer_id: &str) -> CoreResult<()> {
        if self.peers.is_connected(peer_id) {
            tracing::debug!("peer {} already connected", peer_id);
            return Ok(());
        }

        if self.runtime.is_none() {
            return Err(CoreError::Connection(
                "controller runtime is not running".to_string(),
            ));
        }

        self.config.resume_peer(peer_id)?;

        let runtime = self.runtime.as_ref().expect("runtime checked");
        runtime.connect_peer(peer_id)
    }

    /// Connects directly to a remote peer by socket address.
    pub fn connect_address(&mut self, addr: std::net::SocketAddr) -> CoreResult<()> {
        let Some(runtime) = &self.runtime else {
            return Err(CoreError::Connection(
                "controller runtime is not running".to_string(),
            ));
        };

        runtime.connect_address(addr)
    }

    /// Disconnects from a peer and pauses auto-reconnect without unpairing.
    pub fn disconnect(&mut self, peer_id: &str) -> CoreResult<()> {
        let pause_result = self.config.pause_peer(peer_id);
        if let Some(runtime) = &self.runtime {
            let _ = runtime.disconnect_peer(peer_id)?;
        }
        pause_result?;
        let _ = self
            .updates
            .send(events::PeerUpdate::Discovered(self.get_peers_cached()));
        Ok(())
    }

    /// Cancels an outbound connect attempt (connecting, backoff, or waiting
    /// for pairing approval) and pauses auto-reconnect without unpairing.
    pub fn cancel_connect(&mut self, peer_id: &str) -> CoreResult<()> {
        if let Some(runtime) = &self.runtime {
            let _ = runtime.cancel_connect(peer_id);
        } else {
            self.peers.set_idle(peer_id);
        }
        // Pause so a scheduled RetryPeer or a late ConnectFailed cannot
        // resurrect dialing after the user asked to stop. A later Connect
        // resumes via `connect()`.
        let _ = self.config.pause_peer(peer_id);
        let _ = self
            .updates
            .send(events::PeerUpdate::Discovered(self.get_peers_cached()));
        Ok(())
    }

    /// Unpairs a peer and disconnects if connected.
    pub fn unpair_peer(&mut self, peer_id: &str) -> CoreResult<()> {
        if let Some(runtime) = &self.runtime {
            let _ = runtime.unpair_peer(peer_id);
        } else if let Ok(mut store) =
            crate::quic::CertificateStore::load_or_create(&identity::cert_path())
        {
            // Engine stopped: no loop owns the certificate store, so forget
            // the pinned certificate on disk directly (best effort).
            store.remove_peer(peer_id);
            let _ = store.persist();
        }
        let _ = self.config.unpair_peer(peer_id)?;
        let _ = self
            .updates
            .send(events::PeerUpdate::Discovered(self.get_peers_cached()));
        Ok(())
    }

    pub fn transfer_sync(&self) -> Option<Arc<crate::core::transfer::TransferSync>> {
        self.runtime.as_ref().and_then(|r| r.transfer_sync())
    }

    pub fn subscribe_to_transfer_updates(
        &self,
    ) -> Option<tokio::sync::broadcast::Receiver<crate::core::transfer::TransferEvent>> {
        self.transfer_sync().map(|sync| sync.subscribe())
    }

    /// Responds to an inbound pairing request from an unauthenticated peer.
    pub fn respond_pairing(&mut self, peer_id: &str, approved: bool) -> CoreResult<()> {
        let Some(runtime) = &self.runtime else {
            return Err(CoreError::Connection(
                "controller runtime is not running".to_string(),
            ));
        };
        runtime.respond_pairing(peer_id, approved)
    }

    /// Approves an inbound pairing request from an unauthenticated peer.
    pub fn approve_pairing(&mut self, peer_id: &str) -> CoreResult<()> {
        self.respond_pairing(peer_id, true)
    }

    /// Rejects an inbound pairing request from an unauthenticated peer.
    pub fn reject_pairing(&mut self, peer_id: &str) -> CoreResult<()> {
        self.respond_pairing(peer_id, false)
    }

    /// Emergency unlock: immediately restores local control, unhides/unlocks cursor,
    /// releases held keys, and clears active remote state.
    pub fn emergency_unlock(&mut self) -> CoreResult<()> {
        if let Some(runtime) = &self.runtime {
            runtime.emergency_unlock()?;
        }
        Ok(())
    }

    /// Force disconnect: emergency unlocks local control and disconnects all connected peers.
    pub fn force_disconnect_all(&mut self) -> CoreResult<()> {
        self.emergency_unlock()?;
        let connected_ids = self.peers.connected_peer_ids();
        for peer_id in connected_ids {
            let _ = self.disconnect(&peer_id);
        }
        Ok(())
    }

    /// Returns the current status snapshot.
    pub fn get_status(&self) -> StatusSnapshot {
        match &self.runtime {
            Some(runtime) => runtime.status(),
            None => StatusSnapshot {
                engine_running: false,
                ..Default::default()
            },
        }
    }

    /// Returns the stable local peer identity (id, display name).
    pub fn local_identity(&self) -> (String, String) {
        (self.identity.peer_id.clone(), self.identity.name.clone())
    }

    /// Returns a copy of the current config.
    pub fn get_config(&self) -> CoreResult<(ConfigSnapshot, bool)> {
        Ok((self.config.snapshot(), false))
    }

    /// Saves the configuration.
    pub fn save_config(&mut self, snapshot: ConfigSnapshot) -> CoreResult<()> {
        let edge_assignments = EdgeAssignments::from_config(&snapshot);
        self.refresh_configured_peers(&edge_assignments);
        self.config.replace(snapshot.clone());
        self.config.persist()?;

        if let Some(runtime) = &self.runtime {
            runtime.reconcile_peer_edges(&edge_assignments)?;
            runtime.reconcile_layout(&snapshot)?;

            if !snapshot.placements.is_empty() {
                runtime.send_layout_update(LayoutUpdateEvent {
                    version: 0,
                    editor_id: 0,
                    timestamp: std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_millis() as u64,
                    placements: snapshot.placements.clone(),
                })?;
            }

            for peer_id in self.peers.discovered_peer_ids() {
                if snapshot.should_auto_connect_peer(&peer_id)
                    && !self.peers.is_connected(&peer_id)
                {
                    let _ = runtime.connect_peer(&peer_id);
                }
            }
        }

        let _ = self
            .updates
            .send(events::PeerUpdate::ConfigChanged(snapshot));

        Ok(())
    }

    fn local_peer_snapshot(&self) -> PeerSnapshot {
        let (virtual_bounds, display_rects) = self.local_display_topology();
        let addr = crate::core::discovery::DiscoveryClient::local_ipv4_addrs()
            .first()
            .map(|ip| ip.to_string())
            .unwrap_or_else(|| "local".to_string());

        build_local_peer_snapshot(
            &self.identity.peer_id,
            &self.identity.name,
            &self.identity.discovery_hostname(),
            &addr,
            virtual_bounds,
            display_rects,
        )
    }

    fn local_display_topology(&self) -> (Option<DisplayBounds>, Vec<DisplayBounds>) {
        if let Some(runtime) = &self.runtime {
            return runtime.local_display_topology();
        }

        let config = self.config.snapshot();
        let Ok((platform, _)) = (self.pair_factory)(primary_peer_edge(&config)) else {
            return (None, Vec::new());
        };

        let display = platform.get_display_manager();
        let virtual_bounds = display.get_virtual_bounds().ok();
        let display_rects = display.discover_displays().unwrap_or_default();
        (virtual_bounds, display_rects)
    }

    /// Returns a subscriber to peer update events.
    pub fn subscribe_to_peer_updates(
        &self,
    ) -> Option<tokio::sync::broadcast::Receiver<events::PeerUpdate>> {
        Some(self.updates.subscribe())
    }

    fn refresh_configured_peers(&self, assignments: &EdgeAssignments) {
        self.peers.replace_configured_edges(assignments);
    }
}

fn build_local_peer_snapshot(
    peer_id: &str,
    name: &str,
    hostname: &str,
    addr: &str,
    virtual_bounds: Option<DisplayBounds>,
    display_rects: Vec<DisplayBounds>,
) -> PeerSnapshot {
    PeerSnapshot {
        peer_id: peer_id.to_string(),
        name: name.to_string(),
        hostname: hostname.to_string(),
        addr: addr.to_string(),
        platform: Some(std::env::consts::OS.to_string()),
        screen: None,
        virtual_bounds: virtual_bounds.map(virtual_bounds_snapshot),
        display_rects: display_rects
            .into_iter()
            .map(display_rect_snapshot)
            .collect(),
        last_seen_ms: 0,
        discovery_status: DiscoveryStatusSnapshot::Online,
        is_local: true,
        connected: true,
        paired: false,
        connection_status: Some("connected".to_string()),
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
    use crate::platform::shared::test_support::{
        create_test_engine_pair, with_temp_home, with_tokio_runtime,
    };
    use std::sync::{Arc, Mutex};

    #[test]
    fn build_local_peer_snapshot_preserves_display_topology() {
        let snapshot = build_local_peer_snapshot(
            "local-peer",
            "Local",
            "local-host",
            "local",
            Some(DisplayBounds {
                x: 0,
                y: 0,
                width: 2560,
                height: 2880,
            }),
            vec![DisplayBounds {
                x: 0,
                y: 0,
                width: 2560,
                height: 2880,
            }],
        );

        assert_eq!(snapshot.peer_id, "local-peer");
        assert_eq!(snapshot.name, "Local");
        assert_eq!(snapshot.hostname, "local-host");
        assert_eq!(snapshot.addr, "local");
        assert_eq!(snapshot.platform.as_deref(), Some(std::env::consts::OS));
        assert!(snapshot.is_local);
        let virtual_bounds = snapshot
            .virtual_bounds
            .expect("local peer should expose bounds");
        assert_eq!(virtual_bounds.x, 0);
        assert_eq!(virtual_bounds.y, 0);
        assert_eq!(virtual_bounds.width, 2560);
        assert_eq!(virtual_bounds.height, 2880);
        assert_eq!(
            snapshot.display_rects,
            vec![DisplayRectSnapshot {
                x: 0,
                y: 0,
                width: 2560,
                height: 2880,
            }]
        );
    }

    #[test]
    fn start_and_stop_owns_the_controller_runtime() {
        with_temp_home(|| {
            with_tokio_runtime(|| {
                crate::install_crypto_provider();
                let pair = create_test_engine_pair(
                    "controller-test-peer",
                    "Controller Test",
                    PeerEdge::Right,
                    0,
                    1,
                )
                .unwrap();
                let pair_cell = Arc::new(Mutex::new(Some(pair)));
                let factory = {
                    let pair_cell = Arc::clone(&pair_cell);
                    move |_| {
                        Ok(pair_cell
                            .lock()
                            .unwrap()
                            .take()
                            .expect("controller engine factory called more than once"))
                    }
                };
                let mut controller = Controller::with_engine_pair(factory);

                controller.start(0).unwrap();
                assert!(controller.get_status().engine_running);

                controller.stop();
                assert!(!controller.get_status().engine_running);
            });
        });
    }

    #[test]
    fn transfer_subscribers_release_old_engine_across_restart_and_stop() {
        with_temp_home(|| {
            with_tokio_runtime(|| {
                use crate::core::transfer::{FileManifest, TransferEvent};
                use crate::platform::shared::test_support::wait_until;
                use std::net::UdpSocket;
                use tokio::sync::broadcast::error::TryRecvError;

                let socket = UdpSocket::bind("127.0.0.1:0").unwrap();
                let port = socket.local_addr().unwrap().port();
                drop(socket);
                let mut controller = Controller::with_engine_pair(|edge| {
                    create_test_engine_pair("transfer-test-peer", "Transfer Test", edge, 0, 1)
                        .map_err(|error| CoreError::Engine(error.to_string()))
                });
                assert!(controller.subscribe_to_transfer_updates().is_none());
                controller.start(port).unwrap();

                for generation in 0..3 {
                    let silent_peer = UdpSocket::bind("127.0.0.1:0").unwrap();
                    silent_peer.set_nonblocking(true).unwrap();
                    let mut receiver = controller.subscribe_to_transfer_updates().unwrap();
                    let sync = controller.transfer_sync().unwrap();
                    let old_sync = Arc::downgrade(&sync);
                    let manifest = FileManifest {
                        transfer_id: format!("offer-{generation}"),
                        sender_peer_id: "sender".to_string(),
                        sender_name: "Sender".to_string(),
                        target_peer_id: None,
                        files: Vec::new(),
                        total_size: 0,
                    };
                    sync.handle_received_offer(manifest.clone());
                    assert!(matches!(
                        receiver.try_recv().unwrap(),
                        TransferEvent::OfferReceived(received) if received == manifest
                    ));
                    drop(sync);

                    controller
                        .connect_address(silent_peer.local_addr().unwrap())
                        .unwrap();
                    wait_until(
                        "outbound handshake started",
                        Duration::from_secs(2),
                        Duration::from_millis(5),
                        || silent_peer.recv_from(&mut [0u8; 2048]).is_ok(),
                        || "silent peer received no handshake packet".to_string(),
                    )
                    .unwrap();
                    controller.restart(port).unwrap();
                    assert!(controller.get_status().engine_running);
                    wait_until(
                        "old transfer channel closed",
                        Duration::from_secs(2),
                        Duration::from_millis(5),
                        || matches!(receiver.try_recv(), Err(TryRecvError::Closed)),
                        || "old transfer engine is still retained".to_string(),
                    )
                    .unwrap();
                    assert!(old_sync.upgrade().is_none());
                }

                let mut receiver = controller.subscribe_to_transfer_updates().unwrap();
                controller.stop();
                assert!(controller.subscribe_to_transfer_updates().is_none());
                wait_until(
                    "stopped transfer channel closed",
                    Duration::from_secs(2),
                    Duration::from_millis(5),
                    || matches!(receiver.try_recv(), Err(TryRecvError::Closed)),
                    || "stopped transfer engine is still retained".to_string(),
                )
                .unwrap();
                controller.start(port).unwrap();
                controller.stop();
            });
        });
    }

    #[test]
    fn failed_start_does_not_leave_a_runtime_installed() {
        with_temp_home(|| {
            crate::install_crypto_provider();
            let mut controller = Controller::with_engine_pair(|_| {
                Err(CoreError::Engine("test startup failure".to_string()))
            });

            assert!(controller.start(0).is_err());
            assert!(!controller.get_status().engine_running);
        });
    }
}

impl Drop for Controller {
    fn drop(&mut self) {
        self.stop();
    }
}
