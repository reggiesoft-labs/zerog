use super::{ControllerEvent, RunningController, RuntimeContext};
use crate::core::controller::config_state::ConfigStore;
use crate::core::controller::events::PeerUpdate;
use crate::core::controller::peer_connector::{PeerConnector, SharedPeerConnector};
use crate::core::controller::peers::{
    ConnectedPeer, ConnectedPeerDirection, DialState, PeerStore,
};
use crate::core::controller::ConfigSnapshot;
use crate::events::LayoutUpdateEvent;
use crate::core::discovery::{DiscoveredPeer, DiscoveryEvent};
use crate::core::events::LayoutPlacement;
use crate::quic::{CertificateStore, cert_fingerprint};
use crate::core::platform::engine::SessionEvent;
use crate::core::platform::traits::{DisplayManager, PermissionChecker};
use crate::core::platform::{DisplayBounds, PeerEdge};
use crate::quic::QuicEndpoint;
use anyhow::Result;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use tokio::sync::{broadcast, mpsc};
use tokio::task::JoinSet;

fn test_connector(peer_id: &str) -> SharedPeerConnector {
    let certs_dir = tempfile::tempdir().unwrap();
    let cert_store = CertificateStore::load_or_create(certs_dir.path()).unwrap();
    let endpoint = Arc::new(QuicEndpoint::new(0, peer_id, cert_store).unwrap());
    Arc::new(PeerConnector::new(
        endpoint,
        peer_id.to_string(),
        peer_id.to_string(),
        None,
        Vec::new(),
    ))
}

struct FakeDisplay;

impl DisplayManager for FakeDisplay {
    fn discover_displays(&self) -> Result<Vec<DisplayBounds>> {
        Ok(vec![DisplayBounds {
            x: 0,
            y: 0,
            width: 1920,
            height: 1080,
        }])
    }

    fn get_virtual_bounds(&self) -> Result<DisplayBounds> {
        Ok(DisplayBounds {
            x: 0,
            y: 0,
            width: 1920,
            height: 1080,
        })
    }

    fn contains_point(&self, x: i32, y: i32) -> Result<bool> {
        Ok((0..1920).contains(&x) && (0..1080).contains(&y))
    }

    fn clamp_to_active_display(&self, x: i32, y: i32) -> Result<(i32, i32)> {
        Ok((x.clamp(0, 1919), y.clamp(0, 1079)))
    }

    fn get_signature(&self) -> Result<u64> {
        Ok(1)
    }
}

struct FakePermission {
    fail: bool,
}

impl PermissionChecker for FakePermission {
    fn check_accessibility_permission(&self) -> bool {
        !self.fail
    }

    fn prompt_accessibility_permission(&self) -> Result<()> {
        if self.fail {
            anyhow::bail!("injected permission failure");
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum StartupFailure {
    None,
    Permission,
    ConfigureRuntime,
    SetLocalIdentity,
    ReconcileLayout,
    TakeLayoutUpdateReceiver,
    PlatformStart,
    SessionStart,
    TakeSessionEventReceiver,
}

struct FakeEngine {
    reconciled: Mutex<Vec<ConfigSnapshot>>,
    start_count: Arc<AtomicUsize>,
    stop_count: Arc<AtomicUsize>,
    shutdown_count: Arc<AtomicUsize>,
    layout_update_tx: Mutex<Option<mpsc::Sender<LayoutUpdateEvent>>>,
    session_event_rx: Mutex<Option<crossbeam_channel::Receiver<SessionEvent>>>,
    session_event_tx: Mutex<Option<crossbeam_channel::Sender<SessionEvent>>>,
    display: FakeDisplay,
    permission: FakePermission,
    failure: StartupFailure,
    endpoint: Option<Arc<QuicEndpoint>>,
}

impl FakeEngine {
    fn new() -> Self {
        Self {
            reconciled: Mutex::new(Vec::new()),
            start_count: Arc::new(AtomicUsize::new(0)),
            stop_count: Arc::new(AtomicUsize::new(0)),
            shutdown_count: Arc::new(AtomicUsize::new(0)),
            layout_update_tx: Mutex::new(None),
            session_event_rx: Mutex::new(None),
            session_event_tx: Mutex::new(None),
            display: FakeDisplay,
            permission: FakePermission { fail: false },
            failure: StartupFailure::None,
            endpoint: None,
        }
    }

    fn with_lifecycle_counts(
        start_count: Arc<AtomicUsize>,
        stop_count: Arc<AtomicUsize>,
    ) -> Self {
        Self {
            reconciled: Mutex::new(Vec::new()),
            start_count,
            stop_count,
            shutdown_count: Arc::new(AtomicUsize::new(0)),
            layout_update_tx: Mutex::new(None),
            session_event_rx: Mutex::new(None),
            session_event_tx: Mutex::new(None),
            display: FakeDisplay,
            permission: FakePermission { fail: false },
            failure: StartupFailure::None,
            endpoint: None,
        }
    }

    fn with_failure(
        start_count: Arc<AtomicUsize>,
        stop_count: Arc<AtomicUsize>,
        shutdown_count: Arc<AtomicUsize>,
        failure: StartupFailure,
    ) -> Self {
        let (session_event_tx, session_event_rx) = crossbeam_channel::bounded(1);
        Self {
            reconciled: Mutex::new(Vec::new()),
            start_count,
            stop_count,
            shutdown_count,
            layout_update_tx: Mutex::new(None),
            session_event_rx: Mutex::new(Some(session_event_rx)),
            session_event_tx: Mutex::new(Some(session_event_tx)),
            display: FakeDisplay,
            permission: FakePermission {
                fail: failure == StartupFailure::Permission,
            },
            failure,
            endpoint: None,
        }
    }

    fn with_session_events(
        start_count: Arc<AtomicUsize>,
        stop_count: Arc<AtomicUsize>,
    ) -> Self {
        let (session_event_tx, session_event_rx) = crossbeam_channel::bounded(1);
        Self {
            reconciled: Mutex::new(Vec::new()),
            start_count,
            stop_count,
            shutdown_count: Arc::new(AtomicUsize::new(0)),
            layout_update_tx: Mutex::new(None),
            session_event_rx: Mutex::new(Some(session_event_rx)),
            session_event_tx: Mutex::new(Some(session_event_tx)),
            display: FakeDisplay,
            permission: FakePermission { fail: false },
            failure: StartupFailure::None,
            endpoint: None,
        }
    }

    fn reconciled(&self) -> Vec<ConfigSnapshot> {
        self.reconciled.lock().unwrap().clone()
    }
}

impl crate::core::platform::PlatformEngine for FakeEngine {
    fn start(&self) -> Result<tokio::sync::mpsc::Receiver<crate::platform::CapturedInputEvent>> {
        self.start_count.fetch_add(1, Ordering::SeqCst);
        if self.failure == StartupFailure::PlatformStart {
            anyhow::bail!("injected platform start failure");
        }
        let (_tx, rx) = tokio::sync::mpsc::channel(1);
        Ok(rx)
    }

    fn stop(&self) -> Result<()> {
        self.stop_count.fetch_add(1, Ordering::SeqCst);
        self.layout_update_tx.lock().unwrap().take();
        self.session_event_tx.lock().unwrap().take();
        Ok(())
    }

    fn get_display_manager(&self) -> &dyn DisplayManager {
        &self.display
    }

    fn get_permission_checker(&self) -> &dyn PermissionChecker {
        &self.permission
    }
}

impl crate::core::platform::SessionEngine for FakeEngine {
    fn start(
        &self,
        _event_rx: tokio::sync::mpsc::Receiver<crate::platform::CapturedInputEvent>,
    ) -> Result<()> {
        if self.failure == StartupFailure::SessionStart {
            anyhow::bail!("injected session start failure");
        }
        Ok(())
    }

    fn set_local_identity(&self, _peer: crate::core::api::PeerRefSnapshot) -> Result<()> {
        if self.failure == StartupFailure::SetLocalIdentity {
            anyhow::bail!("injected identity failure");
        }
        Ok(())
    }

    fn configure_runtime(
        &self,
        _endpoint: Arc<QuicEndpoint>,
        _batch_window_us: u64,
        _max_events_per_packet: usize,
        _movement_cadence_us: u64,
        _peer_id: String,
        _clipboard_poll_interval_ms: u64,
        _clipboard_ack_timeout_ms: u64,
    ) -> Result<()> {
        if self.failure == StartupFailure::ConfigureRuntime {
            anyhow::bail!("injected configure failure");
        }
        Ok(())
    }

    fn shutdown(&self) -> Result<()> {
        self.shutdown_count.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }

    fn take_layout_update_rx(&self) -> Result<Option<mpsc::Receiver<LayoutUpdateEvent>>> {
        if self.failure == StartupFailure::TakeLayoutUpdateReceiver {
            anyhow::bail!("injected layout receiver failure");
        }
        let (tx, rx) = mpsc::channel(1);
        *self.layout_update_tx.lock().unwrap() = Some(tx);
        Ok(Some(rx))
    }

    fn take_session_event_rx(
        &self,
    ) -> Result<Option<crossbeam_channel::Receiver<SessionEvent>>> {
        if self.failure == StartupFailure::TakeSessionEventReceiver {
            anyhow::bail!("injected session receiver failure");
        }
        if self.session_event_tx.lock().unwrap().is_none() {
            return Ok(None);
        }
        Ok(self.session_event_rx.lock().unwrap().take())
    }

    fn reconcile_layout(&self, config: &ConfigSnapshot) -> Result<()> {
        if self.failure == StartupFailure::ReconcileLayout {
            anyhow::bail!("injected layout reconciliation failure");
        }
        self.reconciled.lock().unwrap().push(config.clone());
        Ok(())
    }

    fn promote_connection(
        &self,
        peer_id: &str,
        conn: Arc<crate::quic::PeerConnection>,
    ) -> Result<()> {
        if let Some(endpoint) = &self.endpoint {
            endpoint.promote_connection(peer_id, &conn)?;
        }
        Ok(())
    }

    fn close_duplicate_connection(&self, conn: Arc<crate::quic::PeerConnection>, reason: &[u8]) {
        if let Some(endpoint) = &self.endpoint {
            endpoint.close_duplicate_connection(&conn, reason);
        }
    }

    fn remove_peer(&self, peer_id: &str) -> Result<()> {
        if let Some(endpoint) = &self.endpoint {
            endpoint.remove_connection(peer_id);
        }
        Ok(())
    }

    fn send_pairing_pending(&self, addr: SocketAddr, _peer_id: &str) -> Result<()> {
        if let Some(endpoint) = &self.endpoint {
            assert!(endpoint.get_connection_by_addr(addr).is_some());
        }
        Ok(())
    }

    fn send_pairing_approved(&self, addr: SocketAddr, _peer_id: &str) -> Result<()> {
        if let Some(endpoint) = &self.endpoint {
            assert!(endpoint.get_connection_by_addr(addr).is_some());
        }
        Ok(())
    }
}

impl crate::core::platform::PlatformEngineTestExt for FakeEngine {}

fn layout_update(timestamp: u64, x: i32) -> LayoutUpdateEvent {
    LayoutUpdateEvent {
        version: 1,
        editor_id: 7,
        timestamp,
        placements: vec![LayoutPlacement {
            peer_id: "peer-a".to_string(),
            peer_label: "Peer A".to_string(),
            x,
            y: 0,
        }],
    }
}

use crate::platform::shared::test_support::with_temp_home;

fn startup_failure_counts(failure: StartupFailure) -> (usize, usize, usize) {
    with_temp_home(|| {
        crate::install_crypto_provider();
        let start_count = Arc::new(AtomicUsize::new(0));
        let stop_count = Arc::new(AtomicUsize::new(0));
        let shutdown_count = Arc::new(AtomicUsize::new(0));
        let factory_start_count = Arc::clone(&start_count);
        let factory_stop_count = Arc::clone(&stop_count);
        let factory_shutdown_count = Arc::clone(&shutdown_count);
        let pair_factory: super::super::EnginePairFactory = Arc::new(move |_| {
            let engine: Arc<dyn crate::core::platform::RuntimeEngine> =
                Arc::new(FakeEngine::with_failure(
                    Arc::clone(&factory_start_count),
                    Arc::clone(&factory_stop_count),
                    Arc::clone(&factory_shutdown_count),
                    failure,
                ));
            let platform =
                Arc::clone(&engine) as Arc<dyn crate::core::platform::PlatformEngine>;
            let session = Arc::clone(&engine) as Arc<dyn crate::core::platform::SessionEngine>;
            Ok((platform, session))
        });
        let config_store = ConfigStore::load();
        let config = config_store.snapshot();
        let identity = super::super::identity::LocalIdentity {
            peer_id: format!("startup-failure-{failure:?}"),
            name: "startup-failure-test".to_string(),
        };
        let peers = Arc::new(super::super::peers::PeerStore::default());
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .unwrap();

        let result = runtime.block_on(async {
            RunningController::start(
                0,
                config_store,
                &config,
                &identity,
                peers,
                pair_factory,
                PeerEdge::Right,
                Arc::new(broadcast::channel(100).0),
            )
        });

        assert!(result.is_err());
        (
            start_count.load(Ordering::SeqCst),
            stop_count.load(Ordering::SeqCst),
            shutdown_count.load(Ordering::SeqCst),
        )
    })
}

#[test]
fn startup_failures_clean_up_every_created_resource() {
    for (failure, expected_start_count) in [
        (StartupFailure::Permission, 0),
        (StartupFailure::ConfigureRuntime, 0),
        (StartupFailure::SetLocalIdentity, 0),
        (StartupFailure::ReconcileLayout, 0),
        (StartupFailure::TakeLayoutUpdateReceiver, 0),
        (StartupFailure::PlatformStart, 1),
        (StartupFailure::SessionStart, 1),
        (StartupFailure::TakeSessionEventReceiver, 1),
    ] {
        let (start_count, stop_count, shutdown_count) = startup_failure_counts(failure);
        assert_eq!(start_count, expected_start_count, "{failure:?}");
        assert_eq!(stop_count, 1, "{failure:?}");
        assert_eq!(shutdown_count, 1, "{failure:?}");
    }
}

#[test]
fn startup_failure_stops_engine_before_returning() {
    with_temp_home(|| {
        crate::install_crypto_provider();
        let start_count = Arc::new(AtomicUsize::new(0));
        let stop_count = Arc::new(AtomicUsize::new(0));
        let factory_start_count = Arc::clone(&start_count);
        let factory_stop_count = Arc::clone(&stop_count);
        let pair_factory: super::super::EnginePairFactory = Arc::new(move |_| {
            let engine: Arc<dyn crate::core::platform::RuntimeEngine> =
                Arc::new(FakeEngine::with_lifecycle_counts(
                    Arc::clone(&factory_start_count),
                    Arc::clone(&factory_stop_count),
                ));
            let platform =
                Arc::clone(&engine) as Arc<dyn crate::core::platform::PlatformEngine>;
            let session = Arc::clone(&engine) as Arc<dyn crate::core::platform::SessionEngine>;
            Ok((platform, session))
        });
        let config_store = ConfigStore::load();
        let config = config_store.snapshot();
        let identity = super::super::identity::LocalIdentity {
            peer_id: "startup-test-peer".to_string(),
            name: "startup-test-peer".to_string(),
        };
        let peers = Arc::new(super::super::peers::PeerStore::default());
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .unwrap();

        let result = runtime.block_on(async {
            RunningController::start(
                0,
                config_store,
                &config,
                &identity,
                peers,
                pair_factory,
                PeerEdge::Right,
                Arc::new(broadcast::channel(100).0),
            )
        });

        assert!(result.is_err());
        assert_eq!(start_count.load(Ordering::SeqCst), 1);
        assert_eq!(stop_count.load(Ordering::SeqCst), 1);
    });
}

#[test]
fn stop_waits_for_owned_controller_tasks() {
    with_temp_home(|| {
        crate::install_crypto_provider();
        let start_count = Arc::new(AtomicUsize::new(0));
        let stop_count = Arc::new(AtomicUsize::new(0));
        let factory_start_count = Arc::clone(&start_count);
        let factory_stop_count = Arc::clone(&stop_count);
        let pair_factory: super::super::EnginePairFactory = Arc::new(move |_| {
            let engine: Arc<dyn crate::core::platform::RuntimeEngine> =
                Arc::new(FakeEngine::with_session_events(
                    Arc::clone(&factory_start_count),
                    Arc::clone(&factory_stop_count),
                ));
            let platform =
                Arc::clone(&engine) as Arc<dyn crate::core::platform::PlatformEngine>;
            let session = Arc::clone(&engine) as Arc<dyn crate::core::platform::SessionEngine>;
            Ok((platform, session))
        });
        let config_store = ConfigStore::load();
        let config = config_store.snapshot();
        let identity = super::super::identity::LocalIdentity {
            peer_id: "shutdown-test-peer".to_string(),
            name: "shutdown-test-peer".to_string(),
        };
        let peers = Arc::new(super::super::peers::PeerStore::default());
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .unwrap();

        let running = runtime.block_on(async {
            RunningController::start(
                0,
                config_store,
                &config,
                &identity,
                peers,
                pair_factory,
                PeerEdge::Right,
                Arc::new(broadcast::channel(100).0),
            )
            .unwrap()
        });

        running.stop(Vec::new());

        assert_eq!(start_count.load(Ordering::SeqCst), 1);
        assert_eq!(stop_count.load(Ordering::SeqCst), 1);
    });
}

#[test]
fn handle_layout_update_applies_latest_pending_snapshot_and_reconciles() {
    with_temp_home(|| {
        let config_store = ConfigStore::load();
        let engine_impl = Arc::new(FakeEngine::new());
        let engine: Arc<dyn crate::core::platform::SessionEngine> =
            engine_impl.clone() as Arc<dyn crate::core::platform::SessionEngine>;
        let (updates, mut updates_rx) = broadcast::channel(1);
        let (tx, mut rx) = mpsc::channel(4);

        tx.try_send(layout_update(2, 2)).unwrap();
        tx.try_send(layout_update(3, 3)).unwrap();

        RunningController::handle_layout_update(
            &config_store,
            &engine,
            &updates,
            layout_update(1, 1),
            &mut rx,
        );

        let snapshot = config_store.snapshot();
        assert_eq!(snapshot.placements.len(), 1);
        assert_eq!(snapshot.placements[0].x, 3);

        let reconciled = engine_impl.reconciled();
        assert_eq!(reconciled.len(), 1);
        assert_eq!(reconciled[0].placements[0].x, 3);
        let PeerUpdate::ConfigChanged(snapshot) = updates_rx.try_recv().unwrap() else {
            panic!("remote layout update should publish the changed config");
        };
        assert_eq!(snapshot.placements[0].x, 3);
    });
}

#[test]
fn handle_layout_update_skips_reconcile_when_layout_is_unchanged() {
    with_temp_home(|| {
        let config_store = ConfigStore::load();
        let initial = ConfigSnapshot {
            placements: vec![LayoutPlacement {
                peer_id: "peer-a".to_string(),
                peer_label: "Peer A".to_string(),
                x: 5,
                y: 0,
            }],
            ..Default::default()
        };
        config_store.replace(initial);

        let engine_impl = Arc::new(FakeEngine::new());
        let engine: Arc<dyn crate::core::platform::SessionEngine> =
            engine_impl.clone() as Arc<dyn crate::core::platform::SessionEngine>;
        let (updates, _updates_rx) = broadcast::channel(1);
        let (_tx, mut rx) = mpsc::channel(1);

        RunningController::handle_layout_update(
            &config_store,
            &engine,
            &updates,
            layout_update(10, 5),
            &mut rx,
        );

        assert!(engine_impl.reconciled().is_empty());
    });
}

#[test]
fn outbound_commit_updates_peer_state_and_publishes_update() {
    let engine_impl = Arc::new(FakeEngine::new());
    let engine: Arc<dyn crate::core::platform::SessionEngine> =
        engine_impl as Arc<dyn crate::core::platform::SessionEngine>;
    let peers = PeerStore::default();
    let addr = SocketAddr::from(([127, 0, 0, 1], crate::core::policies::DEFAULT_PORT));
    peers.remember_discovered(DiscoveredPeer {
        peer_id: "peer-b".to_string(),
        primary_addr: addr,
        addrs: vec![addr],
        hostname: "peer-b.local".to_string(),
        fullname: "peer-b._zerog._udp.local.".to_string(),
        platform: None,
    });
    peers.set_connecting("peer-b");
    let (updates, mut receiver) = broadcast::channel(1);
    let connected = ConnectedPeer {
        peer_id: "peer-b".to_string(),
        name: "Peer B".to_string(),
        addr,
        conn_addr: Some(addr),
        virtual_bounds: None,
        display_rects: Vec::new(),
        direction: ConnectedPeerDirection::Outbound,
        platform: None,
    };

    RunningController::commit_outbound_peer(
        &DiscoveredPeer {
            peer_id: "peer-b".to_string(),
            primary_addr: addr,
            addrs: vec![addr],
            hostname: "peer-b.local".to_string(),
            fullname: "peer-b._zerog._udp.local.".to_string(),
            platform: None,
        },
        connected,
        &engine,
        &peers,
        &updates,
    )
    .unwrap();

    assert!(peers.is_connected("peer-b"));
    assert_eq!(peers.dial_state("peer-b"), DialState::Idle);
    let PeerUpdate::Discovered(snapshots) = receiver.try_recv().unwrap() else {
        panic!("outbound registration should publish a discovered update");
    };
    assert_eq!(snapshots[0].peer_id, "peer-b");
    assert_eq!(snapshots[0].hostname, "peer-b.local");
}

#[tokio::test]
async fn session_loss_removes_peer_and_publishes_one_update() {
    let peers = Arc::new(PeerStore::default());
    let addr = SocketAddr::from(([127, 0, 0, 1], crate::core::policies::DEFAULT_PORT));
    peers.remember_connected(ConnectedPeer {
        peer_id: "peer-b".to_string(),
        name: "Peer B".to_string(),
        addr,
        conn_addr: None,
        virtual_bounds: None,
        display_rects: Vec::new(),
        direction: ConnectedPeerDirection::Inbound,
        platform: None,
    });
    let (updates, mut receiver) = broadcast::channel(1);
    let engine: Arc<dyn crate::core::platform::SessionEngine> =
        Arc::new(FakeEngine::new()) as Arc<dyn crate::core::platform::SessionEngine>;
    let (controller_tx, _controller_rx) = mpsc::channel(1);
    let mut background_tasks = JoinSet::new();

    let mut ctx = RuntimeContext {
        local_peer_id: "local-peer",
        peers: Arc::clone(&peers),
        updates: Arc::new(updates),
        connector: test_connector("local-peer"),
        session: engine,
        controller_tx,
        background_tasks: &mut background_tasks,
    };

    RunningController::handle_session_event(
        &mut ctx,
        SessionEvent::SessionLost {
            peer_id: "peer-b".to_string(),
        },
        true,
    );

    assert!(!peers.is_connected("peer-b"));
    assert!(matches!(
        receiver.try_recv().unwrap(),
        PeerUpdate::ConnectionLost { peer_id } if peer_id == "peer-b"
    ));
    assert!(receiver.try_recv().is_err());
}

#[test]
fn duplicate_tie_break_prefers_new_connection_when_directions_match() {
    assert!(RunningController::should_keep_new_connection(
        "peer-a",
        "peer-b",
        ConnectedPeerDirection::Inbound,
        ConnectedPeerDirection::Inbound,
    ));
}

#[test]
fn duplicate_tie_break_matches_prototype_direction_rule() {
    assert!(!RunningController::should_keep_new_connection(
        "peer-a",
        "peer-b",
        ConnectedPeerDirection::Outbound,
        ConnectedPeerDirection::Inbound,
    ));
    assert!(RunningController::should_keep_new_connection(
        "peer-b",
        "peer-a",
        ConnectedPeerDirection::Outbound,
        ConnectedPeerDirection::Inbound,
    ));
}

#[tokio::test]
async fn discovery_defers_dial_when_remote_is_initiator() {
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    let peers = Arc::new(PeerStore::default());
    let certs_dir = tempfile::tempdir().unwrap();
    let cert_store = CertificateStore::load_or_create(certs_dir.path()).unwrap();
    let endpoint = Arc::new(QuicEndpoint::new(0, "peer-b", cert_store).unwrap());
    let connector: SharedPeerConnector = Arc::new(PeerConnector::new(
        endpoint,
        "peer-b".to_string(),
        "Peer B".to_string(),
        None,
        Vec::new(),
    ));
    let (updates, mut updates_rx) = broadcast::channel(10);
    let engine: Arc<dyn crate::core::platform::SessionEngine> =
        Arc::new(FakeEngine::new()) as Arc<dyn crate::core::platform::SessionEngine>;
    let (controller_tx, _controller_rx) = mpsc::channel(1);
    let mut background_tasks = JoinSet::new();

    let remote = DiscoveredPeer {
        peer_id: "peer-a".to_string(),
        primary_addr: SocketAddr::from(([127, 0, 0, 1], crate::core::policies::DEFAULT_PORT)),
        addrs: vec![SocketAddr::from(([127, 0, 0, 1], crate::core::policies::DEFAULT_PORT))],
        hostname: "peer-a.local".to_string(),
        fullname: "peer-a._zerog._udp.local.".to_string(),
        platform: None,
    };

    let mut ctx = RuntimeContext {
        local_peer_id: "peer-b",
        peers: Arc::clone(&peers),
        updates: Arc::new(updates),
        connector,
        session: engine,
        controller_tx,
        background_tasks: &mut background_tasks,
    };

    RunningController::handle_discovery_event(
        &mut ctx,
        DiscoveryEvent::Discovered(remote),
        true,
    );

    assert!(matches!(peers.dial_state("peer-a"), DialState::Backoff(_)));
    assert_eq!(background_tasks.len(), 1);
    let mut published_peers = Vec::new();
    while let Ok(PeerUpdate::Discovered(snapshot)) = updates_rx.try_recv() {
        published_peers = snapshot;
    }
    assert_eq!(
        published_peers
            .iter()
            .find(|peer| peer.peer_id == "peer-a")
            .unwrap()
            .connection_status
            .as_deref(),
        Some("backoff")
    );
}

#[test]
fn session_disconnected_sets_dial_state_idle_without_retry() {
    let peers = Arc::new(PeerStore::default());
    peers.remember_discovered(DiscoveredPeer {
        peer_id: "peer-b".to_string(),
        primary_addr: SocketAddr::from(([127, 0, 0, 1], 54322)),
        addrs: vec![SocketAddr::from(([127, 0, 0, 1], 54322))],
        hostname: "peer-b".to_string(),
        fullname: "peer-b._zerog._udp.local.".to_string(),
        platform: None,
    });
    peers.remember_connected(ConnectedPeer {
        peer_id: "peer-b".to_string(),
        name: "Beta".to_string(),
        addr: SocketAddr::from(([127, 0, 0, 1], 54322)),
        conn_addr: None,
        virtual_bounds: None,
        display_rects: Vec::new(),
        direction: ConnectedPeerDirection::Inbound,
        platform: None,
    });
    let (updates, mut receiver) = broadcast::channel(1);
    let engine: Arc<dyn crate::core::platform::SessionEngine> =
        Arc::new(FakeEngine::new()) as Arc<dyn crate::core::platform::SessionEngine>;
    let (controller_tx, _controller_rx) = mpsc::channel(1);
    let mut background_tasks = JoinSet::new();

    let mut ctx = RuntimeContext {
        local_peer_id: "local-peer",
        peers: Arc::clone(&peers),
        updates: Arc::new(updates),
        connector: test_connector("local-peer"),
        session: engine,
        controller_tx,
        background_tasks: &mut background_tasks,
    };

    RunningController::handle_session_event(
        &mut ctx,
        SessionEvent::SessionDisconnected {
            peer_id: "peer-b".to_string(),
        },
        true,
    );

    assert!(!peers.is_connected("peer-b"));
    assert_eq!(peers.dial_state("peer-b"), DialState::Idle);
    assert!(background_tasks.is_empty());
    assert!(matches!(
        receiver.try_recv().unwrap(),
        PeerUpdate::ConnectionLost { peer_id } if peer_id == "peer-b"
    ));
}

#[test]
fn session_unpaired_sets_dial_state_idle_and_removes_connected() {
    let peers = Arc::new(PeerStore::default());
    peers.remember_discovered(DiscoveredPeer {
        peer_id: "peer-b".to_string(),
        primary_addr: SocketAddr::from(([127, 0, 0, 1], 54322)),
        addrs: vec![SocketAddr::from(([127, 0, 0, 1], 54322))],
        hostname: "peer-b".to_string(),
        fullname: "peer-b._zerog._udp.local.".to_string(),
        platform: None,
    });
    peers.remember_connected(ConnectedPeer {
        peer_id: "peer-b".to_string(),
        name: "Beta".to_string(),
        addr: SocketAddr::from(([127, 0, 0, 1], 54322)),
        conn_addr: None,
        virtual_bounds: None,
        display_rects: Vec::new(),
        direction: ConnectedPeerDirection::Inbound,
        platform: None,
    });
    let (updates, mut receiver) = broadcast::channel(1);
    let engine: Arc<dyn crate::core::platform::SessionEngine> =
        Arc::new(FakeEngine::new()) as Arc<dyn crate::core::platform::SessionEngine>;
    let (controller_tx, _controller_rx) = mpsc::channel(1);
    let mut background_tasks = JoinSet::new();

    let mut ctx = RuntimeContext {
        local_peer_id: "local-peer",
        peers: Arc::clone(&peers),
        updates: Arc::new(updates),
        connector: test_connector("local-peer"),
        session: engine,
        controller_tx,
        background_tasks: &mut background_tasks,
    };

    RunningController::handle_session_event(
        &mut ctx,
        SessionEvent::SessionUnpaired {
            peer_id: "peer-b".to_string(),
        },
        false,
    );

    assert!(!peers.is_connected("peer-b"));
    assert_eq!(peers.dial_state("peer-b"), DialState::Idle);
    assert!(background_tasks.is_empty());
    assert!(matches!(
        receiver.try_recv().unwrap(),
        PeerUpdate::ConnectionLost { peer_id } if peer_id == "peer-b"
    ));
}

#[tokio::test]
async fn discovered_paired_peer_auto_connects() {
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    let peers = Arc::new(PeerStore::default());
    let certs_dir = tempfile::tempdir().unwrap();
    let cert_store = CertificateStore::load_or_create(certs_dir.path()).unwrap();
    let endpoint = Arc::new(QuicEndpoint::new(0, "peer-a", cert_store).unwrap());
    let connector: SharedPeerConnector = Arc::new(PeerConnector::new(
        endpoint,
        "peer-a".to_string(),
        "Peer A".to_string(),
        None,
        Vec::new(),
    ));
    let (updates, mut updates_rx) = broadcast::channel(10);
    let engine: Arc<dyn crate::core::platform::SessionEngine> =
        Arc::new(FakeEngine::new()) as Arc<dyn crate::core::platform::SessionEngine>;
    let (controller_tx, _controller_rx) = mpsc::channel(1);
    let mut background_tasks = JoinSet::new();

    let remote = DiscoveredPeer {
        peer_id: "peer-b".to_string(),
        primary_addr: "127.0.0.1:54322".parse().unwrap(),
        addrs: vec!["127.0.0.1:54322".parse().unwrap()],
        hostname: "peer-b.local".to_string(),
        fullname: "peer-b._zerog._udp.local.".to_string(),
        platform: None,
    };

    let mut ctx = RuntimeContext {
        local_peer_id: "peer-a",
        peers: Arc::clone(&peers),
        updates: Arc::new(updates),
        connector,
        session: engine,
        controller_tx,
        background_tasks: &mut background_tasks,
    };

    RunningController::handle_discovery_event(
        &mut ctx,
        DiscoveryEvent::Discovered(remote),
        true,
    );

    assert!(matches!(
        peers.dial_state("peer-b"),
        DialState::Connecting(_)
    ));
    assert_eq!(background_tasks.len(), 1);
    let mut published_peers = Vec::new();
    while let Ok(PeerUpdate::Discovered(snapshot)) = updates_rx.try_recv() {
        published_peers = snapshot;
    }
    assert_eq!(
        published_peers
            .iter()
            .find(|peer| peer.peer_id == "peer-b")
            .unwrap()
            .connection_status
            .as_deref(),
        Some("connecting")
    );
}

#[tokio::test]
async fn discovered_paired_peer_does_not_auto_connect_when_paused() {
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    let peers = Arc::new(PeerStore::default());
    let certs_dir = tempfile::tempdir().unwrap();
    let cert_store = CertificateStore::load_or_create(certs_dir.path()).unwrap();
    let endpoint = Arc::new(QuicEndpoint::new(0, "peer-a", cert_store).unwrap());
    let connector: SharedPeerConnector = Arc::new(PeerConnector::new(
        endpoint,
        "peer-a".to_string(),
        "Peer A".to_string(),
        None,
        Vec::new(),
    ));
    let (updates, _rx) = broadcast::channel(10);
    let engine: Arc<dyn crate::core::platform::SessionEngine> =
        Arc::new(FakeEngine::new()) as Arc<dyn crate::core::platform::SessionEngine>;
    let (controller_tx, _controller_rx) = mpsc::channel(1);
    let mut background_tasks = JoinSet::new();

    let remote = DiscoveredPeer {
        peer_id: "peer-b".to_string(),
        primary_addr: "127.0.0.1:54322".parse().unwrap(),
        addrs: vec!["127.0.0.1:54322".parse().unwrap()],
        hostname: "peer-b.local".to_string(),
        fullname: "peer-b._zerog._udp.local.".to_string(),
        platform: None,
    };

    let mut ctx = RuntimeContext {
        local_peer_id: "peer-a",
        peers: Arc::clone(&peers),
        updates: Arc::new(updates),
        connector,
        session: engine,
        controller_tx,
        background_tasks: &mut background_tasks,
    };

    // auto_connect flag passed to handle_discovery_event is false when paused
    RunningController::handle_discovery_event(
        &mut ctx,
        DiscoveryEvent::Discovered(remote),
        false,
    );

    assert_eq!(peers.dial_state("peer-b"), DialState::Idle);
    assert_eq!(background_tasks.len(), 0);
}

struct TestHarness {
    peers: Arc<PeerStore>,
    updates_rx: broadcast::Receiver<PeerUpdate>,
    controller_rx: mpsc::Receiver<ControllerEvent>,
    endpoint: Arc<QuicEndpoint>,
    controller_loop: super::loop_runner::ControllerLoop,
    _certs_dir: tempfile::TempDir,
    remote_endpoints: std::collections::HashMap<String, (Arc<QuicEndpoint>, tempfile::TempDir)>,
}

impl TestHarness {
    fn new(local_peer_id: &str) -> Self {
        let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
        let peers = Arc::new(PeerStore::default());
        let certs_dir = tempfile::tempdir().unwrap();
        let cert_store = CertificateStore::load_or_create(certs_dir.path()).unwrap();
        let endpoint = Arc::new(QuicEndpoint::new(0, local_peer_id, cert_store).unwrap());
        let (updates_tx, updates_rx) = broadcast::channel(16);
        let updates = Arc::new(updates_tx);
        let engine: Arc<dyn crate::core::platform::SessionEngine> = Arc::new(FakeEngine {
            endpoint: Some(Arc::clone(&endpoint)),
            ..FakeEngine::new()
        })
            as Arc<dyn crate::core::platform::SessionEngine>;
        let (controller_tx, controller_rx) = mpsc::channel(16);
        let connector: SharedPeerConnector = Arc::new(PeerConnector::new(
            Arc::clone(&endpoint),
            local_peer_id.to_string(),
            "Local".to_string(),
            None,
            Vec::new(),
        ));
        let config_store = ConfigStore::load();

        let controller_loop = super::loop_runner::ControllerLoop {
            config_store,
            local_peer_id: local_peer_id.to_string(),
            peers: Arc::clone(&peers),
            updates,
            connector,
            session: engine,
            controller_tx,
            background_tasks: JoinSet::new(),
            endpoint: Arc::clone(&endpoint),
            pending_pairings: std::collections::HashMap::new(),
            pending_outbound_pairings: std::collections::HashMap::new(),
        };

        Self {
            peers,
            updates_rx,
            controller_rx,
            endpoint,
            controller_loop,
            _certs_dir: certs_dir,
            remote_endpoints: std::collections::HashMap::new(),
        }
    }

    async fn connect_inbound(
        &mut self,
        peer_id: &str,
        name: &str,
        platform: Option<&str>,
    ) -> Arc<crate::quic::PeerConnection> {
        let entry = self
            .remote_endpoints
            .entry(peer_id.to_string())
            .or_insert_with(|| {
                let remote_dir = tempfile::tempdir().unwrap();
                let remote_store = CertificateStore::load_or_create(remote_dir.path()).unwrap();
                let remote_endpoint = Arc::new(QuicEndpoint::new(0, peer_id, remote_store).unwrap());
                (remote_endpoint, remote_dir)
            });
        let remote_endpoint = Arc::clone(&entry.0);
        let cert_der = remote_endpoint.self_cert_der().to_vec();

        let local_port = self.endpoint.local_addr().unwrap().port();
        let server_addr = SocketAddr::new(
            std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST),
            local_port,
        );

        let (inbound_conn_res, _outbound_conn_res) = tokio::join!(
            self.endpoint.accept(),
            remote_endpoint.connect(server_addr, "localhost")
        );
        let inbound_conn = inbound_conn_res.unwrap();

        let inbound_event = SessionEvent::InboundIdentified {
            peer_id: peer_id.to_string(),
            name: name.to_string(),
            addr: inbound_conn.addr(),
            conn: Arc::clone(&inbound_conn),
            display_bounds: None,
            display_rects: Vec::new(),
            platform: platform.map(|s| s.to_string()),
            is_auto_connect: false,
            listen_port: Some(remote_endpoint.local_addr().unwrap().port()),
            certificate: cert_der,
        };

        self.controller_loop.handle_session_event(inbound_event);
        inbound_conn
    }

    async fn connect_outbound(
        &mut self,
        peer_id: &str,
    ) -> (Arc<crate::quic::PeerConnection>, std::time::Instant) {
        let entry = self
            .remote_endpoints
            .entry(peer_id.to_string())
            .or_insert_with(|| {
                let certs = tempfile::tempdir().unwrap();
                let store = CertificateStore::load_or_create(certs.path()).unwrap();
                (
                    Arc::new(QuicEndpoint::new(0, peer_id, store).unwrap()),
                    certs,
                )
            });
        let remote = Arc::clone(&entry.0);
        let addr = SocketAddr::from((
            std::net::Ipv4Addr::LOCALHOST,
            remote.local_addr().unwrap().port(),
        ));
        let (inbound, outbound) =
            tokio::join!(remote.accept(), self.endpoint.connect(addr, "localhost"));
        inbound.unwrap();
        let attempt = self.peers.set_connecting(peer_id);
        (outbound.unwrap(), attempt)
    }

    fn respond_pairing(&mut self, peer_id: &str, approved: bool) {
        self.controller_loop.handle_controller_event(ControllerEvent::PairingDecision {
            peer_id: peer_id.to_string(),
            approved,
        });
    }

    fn has_pending_pairing(&self, peer_id: &str) -> bool {
        self.controller_loop.pending_pairings.contains_key(peer_id)
    }

    fn is_connected(&self, peer_id: &str) -> bool {
        self.peers.is_connected(peer_id)
    }

    fn is_paired(&self, peer_id: &str) -> bool {
        self.controller_loop.config_store.snapshot().is_peer_paired(peer_id)
    }

    fn is_cert_trusted(&self, peer_id: &str) -> bool {
        self.endpoint.cert_store().lock().unwrap().is_known_peer(peer_id)
    }

    fn drain_updates(&mut self) -> Vec<PeerUpdate> {
        let mut updates = Vec::new();
        while let Ok(event) = self.updates_rx.try_recv() {
            updates.push(event);
        }
        updates
    }
}

#[test]
fn inbound_pairing_request_includes_authentication_code() {
    with_temp_home(|| {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let mut harness = TestHarness::new("peer-local");
            harness
                .connect_inbound("peer-remote", "Remote Machine", Some("macos"))
                .await;

            let updates = harness.drain_updates();
            let code = updates
                .iter()
                .find_map(|update| match update {
                    PeerUpdate::PairingRequested { peer_id, code, .. }
                        if peer_id == "peer-remote" =>
                    {
                        code.clone()
                    }
                    _ => None,
                })
                .expect("pairing request must carry an authentication code");

            assert_eq!(code.len(), 6);
            assert!(
                code.chars().all(|c| c.is_ascii_digit()),
                "code must be numeric: {code}"
            );

            let remote_der = harness
                .remote_endpoints
                .get("peer-remote")
                .unwrap()
                .0
                .self_cert_der();
            let expected = crate::core::pairing_code::pairing_code(
                &cert_fingerprint(harness.endpoint.self_cert_der()),
                &cert_fingerprint(remote_der),
            );
            assert_eq!(
                code, expected,
                "code must be derived from both certificates so a relay shows a different value"
            );
        });
    });
}

#[test]
fn outbound_pairing_request_carries_the_authentication_code() {
    with_temp_home(|| {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let mut harness = TestHarness::new("peer-local");
            let (connection, attempt) = harness.connect_outbound("peer-remote").await;
            let addr = connection.addr();
            let certificate = vec![7u8; 64];

            harness
                .controller_loop
                .handle_controller_event(ControllerEvent::OutboundPairingPending {
                    discovered: DiscoveredPeer {
                        peer_id: "peer-remote".to_string(),
                        primary_addr: addr,
                        addrs: vec![addr],
                        hostname: "remote.local".to_string(),
                        fullname: "peer-remote._zerog._udp.local.".to_string(),
                        platform: Some("macos".to_string()),
                    },
                    connected: ConnectedPeer {
                        peer_id: "peer-remote".to_string(),
                        name: "Remote Machine".to_string(),
                        addr,
                        conn_addr: Some(addr),
                        virtual_bounds: None,
                        display_rects: Vec::new(),
                        direction: ConnectedPeerDirection::Outbound,
                        platform: Some("macos".to_string()),
                    },
                    certificate: certificate.clone(),
                    connection,
                    attempt,
                });

            let updates = harness.drain_updates();
            let (code, can_respond) = updates
                .iter()
                .find_map(|update| match update {
                    PeerUpdate::PairingRequested {
                        peer_id,
                        code,
                        can_respond,
                        ..
                    } if peer_id == "peer-remote" => Some((code.clone(), *can_respond)),
                    _ => None,
                })
                .expect("outbound pairing must ask the GUI to show the code");

            assert!(
                !can_respond,
                "the dialing side waits for the peer and must not offer an approve button"
            );
            let code = code.expect("outbound pairing must carry a code");
            assert_eq!(
                code,
                crate::core::pairing_code::pairing_code(
                    &cert_fingerprint(harness.endpoint.self_cert_der()),
                    &cert_fingerprint(&certificate),
                )
            );
            assert!(
                harness
                    .controller_loop
                    .pending_outbound_pairings
                    .contains_key("peer-remote")
            );
        });
    });
}

#[test]
fn inbound_unpaired_connection_requires_pairing_approval() {
    with_temp_home(|| {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let mut harness = TestHarness::new("peer-local");
            harness.connect_inbound("peer-remote", "Remote Machine", Some("macos")).await;

            assert!(!harness.is_connected("peer-remote"));
            assert!(harness.has_pending_pairing("peer-remote"));

            let updates = harness.drain_updates();
            assert!(updates.iter().any(|u| matches!(
                u,
                PeerUpdate::PairingRequested { peer_id, name, .. }
                if peer_id == "peer-remote" && name == "Remote Machine"
            )));

            harness.respond_pairing("peer-remote", true);

            assert!(harness.is_connected("peer-remote"));
            assert!(!harness.has_pending_pairing("peer-remote"));
            assert!(harness.is_paired("peer-remote"));
            assert!(harness.is_cert_trusted("peer-remote"));

            let updates = harness.drain_updates();
            assert!(updates.iter().any(|u| matches!(
                u,
                PeerUpdate::PairingResolved { peer_id, approved: true }
                if peer_id == "peer-remote"
            )));
        });
    });
}

#[test]
fn inbound_unpaired_connection_rejection_drops_connection() {
    with_temp_home(|| {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let mut harness = TestHarness::new("peer-local-2");
            harness.connect_inbound("peer-attacker", "Attacker Machine", Some("linux")).await;

            assert!(harness.has_pending_pairing("peer-attacker"));

            harness.respond_pairing("peer-attacker", false);

            assert!(!harness.is_connected("peer-attacker"));
            assert!(!harness.has_pending_pairing("peer-attacker"));
            assert!(!harness.is_paired("peer-attacker"));

            let updates = harness.drain_updates();
            assert!(updates.iter().any(|u| matches!(
                u,
                PeerUpdate::PairingResolved { peer_id, approved: false }
                if peer_id == "peer-attacker"
            )));
        });
    });
}

#[test]
fn inbound_fingerprint_mismatch_drops_connection() {
    with_temp_home(|| {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let mut harness = TestHarness::new("peer-local-3");
            harness
                .endpoint
                .cert_store()
                .lock()
                .unwrap()
                .trust_peer("peer-imposter", b"trusted-original-cert")
                .unwrap();
            let _ = harness.controller_loop.config_store.pair_peer("peer-imposter");

            harness.connect_inbound("peer-imposter", "Imposter Machine", Some("linux")).await;

            assert!(!harness.is_connected("peer-imposter"));
            assert!(!harness.has_pending_pairing("peer-imposter"));
        });
    });
}

#[test]
fn forget_clears_trust_and_outbound_pairing_waiter() {
    with_temp_home(|| {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let mut harness = TestHarness::new("peer-local-forget");
            let _ = harness.controller_loop.config_store.pair_peer("peer-remote");
            let certificate = vec![9u8; 64];
            harness
                .endpoint
                .cert_store()
                .lock()
                .unwrap()
                .trust_peer("peer-remote", &certificate)
                .unwrap();
            let (connection, attempt) = harness.connect_outbound("peer-remote").await;
            let addr = connection.addr();
            harness.controller_loop.handle_controller_event(
                ControllerEvent::OutboundPairingPending {
                    discovered: DiscoveredPeer {
                        peer_id: "peer-remote".to_string(),
                        primary_addr: addr,
                        addrs: vec![addr],
                        hostname: "remote.local".to_string(),
                        fullname: "peer-remote._zerog._udp.local.".to_string(),
                        platform: None,
                    },
                    connected: ConnectedPeer {
                        peer_id: "peer-remote".to_string(),
                        name: "Remote".to_string(),
                        addr,
                        conn_addr: Some(addr),
                        virtual_bounds: None,
                        display_rects: Vec::new(),
                        direction: ConnectedPeerDirection::Outbound,
                        platform: None,
                    },
                    certificate: certificate.clone(),
                    connection,
                    attempt,
                },
            );
            assert!(harness
                .controller_loop
                .pending_outbound_pairings
                .contains_key("peer-remote"));
            let _ = harness.drain_updates();

            harness
                .controller_loop
                .handle_controller_event(ControllerEvent::PeerUnpaired {
                    peer_id: "peer-remote".to_string(),
                    attempt: None,
                });

            assert!(!harness.is_paired("peer-remote"));
            assert!(!harness.is_cert_trusted("peer-remote"));
            assert!(!harness
                .controller_loop
                .pending_outbound_pairings
                .contains_key("peer-remote"));
            assert_eq!(harness.peers.dial_state("peer-remote"), DialState::Idle);
            let updates = harness.drain_updates();
            assert!(updates.iter().any(|u| matches!(
                u,
                PeerUpdate::PairingResolved { peer_id, approved: false }
                if peer_id == "peer-remote"
            )));
        });
    });
}

#[test]
fn remote_forget_clears_trust_and_outbound_pairing_waiter() {
    with_temp_home(|| {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let mut harness = TestHarness::new("peer-local-remote-forget");
            let _ = harness.controller_loop.config_store.pair_peer("peer-remote");
            let certificate = vec![9u8; 64];
            harness
                .endpoint
                .cert_store()
                .lock()
                .unwrap()
                .trust_peer("peer-remote", &certificate)
                .unwrap();
            let (connection, attempt) = harness.connect_outbound("peer-remote").await;
            let addr = connection.addr();
            harness.controller_loop.handle_controller_event(
                ControllerEvent::OutboundPairingPending {
                    discovered: DiscoveredPeer {
                        peer_id: "peer-remote".to_string(),
                        primary_addr: addr,
                        addrs: vec![addr],
                        hostname: "remote.local".to_string(),
                        fullname: "peer-remote._zerog._udp.local.".to_string(),
                        platform: None,
                    },
                    connected: ConnectedPeer {
                        peer_id: "peer-remote".to_string(),
                        name: "Remote".to_string(),
                        addr,
                        conn_addr: Some(addr),
                        virtual_bounds: None,
                        display_rects: Vec::new(),
                        direction: ConnectedPeerDirection::Outbound,
                        platform: None,
                    },
                    certificate,
                    connection,
                    attempt,
                },
            );
            let _ = harness.drain_updates();

            harness
                .controller_loop
                .handle_session_event(SessionEvent::SessionUnpaired {
                    peer_id: "peer-remote".to_string(),
                });

            assert!(!harness.is_paired("peer-remote"));
            assert!(!harness.is_cert_trusted("peer-remote"));
            assert!(!harness
                .controller_loop
                .pending_outbound_pairings
                .contains_key("peer-remote"));
            assert_eq!(harness.peers.dial_state("peer-remote"), DialState::Idle);
        });
    });
}

#[test]
fn stray_pairing_approved_does_not_spam_config_change() {
    with_temp_home(|| {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let mut harness = TestHarness::new("peer-local-stray-approved");
            let _ = harness.controller_loop.config_store.pair_peer("peer-remote");
            harness
                .endpoint
                .cert_store()
                .lock()
                .unwrap()
                .trust_peer("peer-remote", b"stray-cert")
                .unwrap();
            let _ = harness.drain_updates();

            harness
                .controller_loop
                .handle_session_event(SessionEvent::SessionPairingApproved {
                    peer_id: "peer-remote".to_string(),
                });

            assert!(harness.is_paired("peer-remote"));
            assert!(harness.is_cert_trusted("peer-remote"));
            let updates = harness.drain_updates();
            assert!(
                !updates.iter().any(|u| matches!(u, PeerUpdate::ConfigChanged(_))),
                "stray approval must not rebroadcast config: {updates:?}"
            );
        });
    });
}

#[test]
fn can_repair_after_forget() {
    with_temp_home(|| {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let mut harness = TestHarness::new("peer-local-repair");
            let _ = harness.controller_loop.config_store.pair_peer("peer-remote");
            harness
                .endpoint
                .cert_store()
                .lock()
                .unwrap()
                .trust_peer("peer-remote", b"old-cert")
                .unwrap();
            harness
                .controller_loop
                .handle_controller_event(ControllerEvent::PeerUnpaired {
                    peer_id: "peer-remote".to_string(),
                    attempt: None,
                });
            assert!(!harness.is_paired("peer-remote"));
            assert!(!harness.is_cert_trusted("peer-remote"));
            let _ = harness.drain_updates();

            harness.connect_inbound("peer-remote", "Remote Machine", Some("macos")).await;
            assert!(!harness.is_connected("peer-remote"));
            assert!(harness.has_pending_pairing("peer-remote"));

            harness.respond_pairing("peer-remote", true);
            assert!(harness.is_connected("peer-remote"));
            assert!(harness.is_paired("peer-remote"));
            assert!(harness.is_cert_trusted("peer-remote"));
        });
    });
}

#[test]
fn reconnect_after_disconnect_succeeds_without_re_pairing() {
    with_temp_home(|| {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let mut harness = TestHarness::new("peer-local-reconnect");
            harness.connect_inbound("peer-remote", "Remote Machine", Some("macos")).await;
            harness.respond_pairing("peer-remote", true);

            assert!(harness.is_connected("peer-remote"));
            assert!(harness.is_paired("peer-remote"));

            // Disconnect peer and pause auto-connect
            harness.controller_loop.handle_session_event(SessionEvent::SessionDisconnected {
                peer_id: "peer-remote".to_string(),
            });
            assert!(!harness.is_connected("peer-remote"));
            assert!(harness.peers.discovered_peer("peer-remote").is_some());

            let mut cfg = harness.controller_loop.config_store.snapshot();
            cfg.pause_peer("peer-remote");
            let _ = harness.controller_loop.config_store.replace(cfg);
            assert!(harness.controller_loop.config_store.snapshot().is_peer_paused("peer-remote"));

            // Reconnect inbound from remote peer
            harness.connect_inbound("peer-remote", "Remote Machine", Some("macos")).await;

            // Should connect directly without triggering a pending pairing modal
            assert!(harness.is_connected("peer-remote"));
            assert!(!harness.has_pending_pairing("peer-remote"));
            assert!(!harness.controller_loop.config_store.snapshot().is_peer_paused("peer-remote"));
        });
    });
}


fn pairing_discovered_peer(peer_id: &str) -> DiscoveredPeer {
    let addr = SocketAddr::from(([127, 0, 0, 1], crate::core::policies::DEFAULT_PORT));
    DiscoveredPeer {
        peer_id: peer_id.to_string(),
        primary_addr: addr,
        addrs: vec![addr],
        hostname: format!("{peer_id}.local"),
        fullname: format!("{peer_id}._zerog._udp.local."),
        platform: None,
    }
}

#[test]
fn outbound_pairing_registers_waiter_before_reading_early_approval() {
    with_temp_home(|| {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let mut harness = TestHarness::new("peer-local-early-approval");
            let certs = tempfile::tempdir().unwrap();
            let remote = Arc::new(QuicEndpoint::new(
                0,
                "peer-remote",
                CertificateStore::load_or_create(certs.path()).unwrap(),
            ).unwrap());
            let addr = SocketAddr::from((std::net::Ipv4Addr::LOCALHOST, remote.local_addr().unwrap().port()));
            let mut inbound_rx = harness.endpoint.take_inbound_receiver().unwrap();
            let server = Arc::clone(&remote);
            let server_task = tokio::spawn(async move {
                let conn = server.accept().await.unwrap();
                let responder = crate::core::handshake::HandshakeResponder::new(Some(
                    crate::core::handshake::HandshakePeer::local("peer-remote", "Remote", None, Vec::new()),
                ));
                responder.accept(&server, &conn).await.unwrap();
                conn.send_control(vec![42]).await.unwrap();
            });
            harness.peers.remember_discovered(DiscoveredPeer {
                primary_addr: addr,
                addrs: vec![addr],
                ..pairing_discovered_peer("peer-remote")
            });
            harness.controller_loop.handle_controller_event(ControllerEvent::ConnectPeer {
                peer_id: "peer-remote".to_string(),
            });
            let event = tokio::time::timeout(std::time::Duration::from_secs(2), harness.controller_rx.recv()).await.unwrap().unwrap();

            assert!(harness.endpoint.get_connection("peer-remote").is_none());
            assert!(inbound_rx.try_recv().is_err());
            harness.controller_loop.handle_controller_event(event);
            let message = tokio::time::timeout(std::time::Duration::from_secs(2), inbound_rx.recv()).await.unwrap().unwrap();
            assert!(matches!(message, crate::platform::shared::quic::InboundMessage::Control(_, bytes) if bytes == vec![42]));
            assert!(harness.controller_loop.pending_outbound_pairings.contains_key("peer-remote"));
            harness.controller_loop.handle_session_event(SessionEvent::SessionPairingApproved {
                peer_id: "peer-remote".to_string(),
            });

            assert!(harness.is_connected("peer-remote"));
            assert!(harness.is_paired("peer-remote"));
            assert!(!harness.controller_loop.pending_outbound_pairings.contains_key("peer-remote"));
            assert!(harness.endpoint.cert_store().lock().unwrap().verify_fingerprint("peer-remote", remote.self_cert_der()));
            server_task.await.unwrap();
        });
    });
}

#[test]
fn queued_outbound_pairing_after_cancel_does_not_resurrect_connection() {
    with_temp_home(|| {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let mut harness = TestHarness::new("peer-local-queued-cancel");
            let event = outbound_pairing_event(&mut harness, "peer-remote").await;
            let connection = match &event {
                ControllerEvent::OutboundPairingPending { connection, .. } => {
                    Arc::clone(connection)
                }
                _ => unreachable!(),
            };
            harness
                .controller_loop
                .handle_controller_event(ControllerEvent::CancelConnect {
                    peer_id: "peer-remote".to_string(),
                });
            harness.controller_loop.handle_controller_event(event);

            assert!(connection.is_closed());
            assert_eq!(harness.peers.dial_state("peer-remote"), DialState::Idle);
            assert!(
                !harness
                    .controller_loop
                    .pending_outbound_pairings
                    .contains_key("peer-remote")
            );
            assert!(!harness.is_paired("peer-remote"));
            assert!(
                !harness
                    .drain_updates()
                    .iter()
                    .any(|update| matches!(update, PeerUpdate::PairingRequested { .. }))
            );
        });
    });
}

#[test]
fn old_dial_results_cannot_change_an_attempt_started_after_cancel() {
    with_temp_home(|| {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let mut harness = TestHarness::new("peer-local-restarted-dial");
            let event = outbound_pairing_event(&mut harness, "peer-remote").await;
            let (connection, old_attempt) = match &event {
                ControllerEvent::OutboundPairingPending {
                    connection,
                    attempt,
                    ..
                } => (Arc::clone(connection), *attempt),
                _ => unreachable!(),
            };
            harness
                .controller_loop
                .handle_controller_event(ControllerEvent::CancelConnect {
                    peer_id: "peer-remote".to_string(),
                });
            let new_attempt = harness.peers.set_connecting("peer-remote");
            assert_ne!(old_attempt, new_attempt);
            harness.controller_loop.handle_controller_event(event);
            harness
                .controller_loop
                .handle_controller_event(ControllerEvent::ConnectFailed {
                    peer_id: "peer-remote".to_string(),
                    attempt: old_attempt,
                });
            harness
                .controller_loop
                .handle_controller_event(ControllerEvent::PeerUnpaired {
                    peer_id: "peer-remote".to_string(),
                    attempt: Some(old_attempt),
                });
            harness
                .controller_loop
                .handle_session_event(SessionEvent::SessionPairingPending {
                    peer_id: "peer-remote".to_string(),
                });

            assert!(connection.is_closed());
            assert_eq!(
                harness.peers.dial_state("peer-remote"),
                DialState::Connecting(new_attempt)
            );
            assert!(
                !harness
                    .controller_loop
                    .pending_outbound_pairings
                    .contains_key("peer-remote")
            );
            assert!(harness.controller_loop.background_tasks.is_empty());
            assert!(!harness.drain_updates().iter().any(|update| matches!(
                update,
                PeerUpdate::ConfigChanged(_) | PeerUpdate::PairingRequested { .. }
            )));
        });
    });
}

#[test]
fn inbound_connections_reconnect_using_source_ip_and_listening_port() {
    with_temp_home(|| {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let mut harness = TestHarness::new("peer-local-listen-addr");
            let stale: SocketAddr = "10.0.0.5:54321".parse().unwrap();
            harness.peers.remember_discovered(DiscoveredPeer {
                primary_addr: stale,
                addrs: vec![stale],
                ..pairing_discovered_peer("peer-remote")
            });
            let conn = harness
                .connect_inbound("peer-remote", "Remote", Some("macos"))
                .await;
            let remote_port = harness.remote_endpoints["peer-remote"]
                .0
                .local_addr()
                .unwrap()
                .port();
            let listen_addr = SocketAddr::from((std::net::Ipv4Addr::LOCALHOST, remote_port));
            harness.respond_pairing("peer-remote", true);

            let connected = harness.peers.connected("peer-remote").unwrap();
            assert_eq!(connected.addr, listen_addr);
            assert_eq!(connected.conn_addr, Some(conn.addr()));
            assert_eq!(
                harness
                    .peers
                    .discovered_peer("peer-remote")
                    .unwrap()
                    .primary_addr,
                listen_addr
            );
            harness
                .controller_loop
                .handle_session_event(SessionEvent::SessionDisconnected {
                    peer_id: "peer-remote".to_string(),
                });
            harness
                .connect_inbound("peer-remote", "Remote", Some("macos"))
                .await;

            assert_eq!(
                harness.peers.connected("peer-remote").unwrap().addr,
                listen_addr
            );
            assert!(!harness.has_pending_pairing("peer-remote"));
        });
    });
}

#[test]
fn closed_outbound_pairing_connection_clears_waiter_and_dial_state() {
    with_temp_home(|| {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let mut harness = TestHarness::new("peer-local-closed-outbound");
            let event = outbound_pairing_event(&mut harness, "peer-remote").await;
            if let ControllerEvent::OutboundPairingPending { connection, .. } = &event {
                connection.close(b"remote closed before promotion");
            }
            harness.controller_loop.handle_controller_event(event);

            assert_eq!(harness.peers.dial_state("peer-remote"), DialState::Idle);
            assert!(
                !harness
                    .controller_loop
                    .pending_outbound_pairings
                    .contains_key("peer-remote")
            );
            assert!(!harness.is_connected("peer-remote"));
            assert!(
                !harness
                    .drain_updates()
                    .iter()
                    .any(|update| matches!(update, PeerUpdate::PairingRequested { .. }))
            );
        });
    });
}

#[tokio::test]
async fn delayed_retry_does_not_cross_state_boundaries() {
    crate::install_crypto_provider();
    let peers = Arc::new(PeerStore::default());
    peers.remember_discovered(pairing_discovered_peer("peer-remote"));
    let (updates, _) = broadcast::channel(16);
    let (controller_tx, mut controller_rx) = mpsc::channel(16);
    let session = Arc::new(FakeEngine::new()) as Arc<dyn crate::core::platform::SessionEngine>;
    let connector = test_connector("peer-local-timer");
    let mut background_tasks = JoinSet::new();
    for mode in 0..6 {
        peers.remove_connected("peer-remote");
        let retry_at = std::time::Instant::now() + std::time::Duration::from_millis(5);
        peers.set_backoff("peer-remote", retry_at);
        RunningController::schedule_retry(
            controller_tx.clone(),
            "peer-remote".to_string(),
            retry_at,
            &mut background_tasks,
        );
        match mode {
            0 => peers.set_idle("peer-remote"),
            1 => {
                peers.set_connecting("peer-remote");
            }
            2 => peers.set_pairing("peer-remote"),
            3 => peers.set_backoff("peer-remote", retry_at + std::time::Duration::from_secs(1)),
            4 => peers.remember_connected(ConnectedPeer {
                peer_id: "peer-remote".to_string(),
                name: "Remote".to_string(),
                addr: "127.0.0.1:54321".parse().unwrap(),
                conn_addr: None,
                virtual_bounds: None,
                display_rects: Vec::new(),
                direction: ConnectedPeerDirection::Inbound,
                platform: None,
            }),
            5 => {}
            _ => unreachable!(),
        }
        let state = peers.dial_state("peer-remote");
        let event = tokio::time::timeout(std::time::Duration::from_secs(1), controller_rx.recv())
            .await
            .unwrap()
            .unwrap();
        background_tasks.join_next().await.unwrap().unwrap();
        let mut ctx = RuntimeContext {
            local_peer_id: "peer-local-timer",
            peers: Arc::clone(&peers),
            updates: Arc::new(updates.clone()),
            connector: Arc::clone(&connector),
            session: Arc::clone(&session),
            controller_tx: controller_tx.clone(),
            background_tasks: &mut background_tasks,
        };
        RunningController::handle_controller_event(&mut ctx, event, true);

        if mode == 5 {
            assert!(matches!(
                peers.dial_state("peer-remote"),
                DialState::Connecting(_)
            ));
            assert_eq!(background_tasks.len(), 1);
            background_tasks.abort_all();
            while background_tasks.join_next().await.is_some() {}
        } else {
            assert_eq!(peers.dial_state("peer-remote"), state);
            assert!(background_tasks.is_empty());
        }
    }
}

#[test]
fn connect_peer_while_pairing_does_not_start_duplicate_dial() {
    with_temp_home(|| {
        let mut harness = TestHarness::new("peer-local-dup-connect");
        harness.peers.remember_discovered(pairing_discovered_peer("peer-remote"));
        harness.peers.set_pairing("peer-remote");

        harness.controller_loop.handle_controller_event(ControllerEvent::ConnectPeer {
            peer_id: "peer-remote".to_string(),
        });

        assert_eq!(harness.peers.dial_state("peer-remote"), DialState::Pairing);
        assert!(harness.controller_loop.background_tasks.is_empty());
    });
}

#[test]
fn retry_peer_while_pairing_leaves_pairing_wait_untouched() {
    with_temp_home(|| {
        let mut harness = TestHarness::new("peer-local-dup-retry");
        harness.peers.remember_discovered(pairing_discovered_peer("peer-remote"));
        harness.peers.set_pairing("peer-remote");

        harness
            .controller_loop
            .handle_controller_event(ControllerEvent::RetryPeer {
                peer_id: "peer-remote".to_string(),
                retry_at: std::time::Instant::now(),
            });

        assert_eq!(harness.peers.dial_state("peer-remote"), DialState::Pairing);
        assert!(harness.controller_loop.background_tasks.is_empty());
    });
}

#[test]
fn retry_peer_on_larger_side_defers_to_inbound_instead_of_dialing() {
    with_temp_home(|| {
        // "peer-zzz-listener" > "peer-remote": the larger side must wait
        // for inbound so simultaneous fixed-port redials cannot livelock.
        let mut harness = TestHarness::new("peer-zzz-listener");
        let peer_id = "peer-remote";
        harness.peers.remember_discovered(pairing_discovered_peer(peer_id));
        harness.controller_loop.config_store.pair_peer(peer_id).unwrap();

        let retry_at = std::time::Instant::now();
        harness.peers.set_backoff(peer_id, retry_at);
        harness
            .controller_loop
            .handle_controller_event(ControllerEvent::RetryPeer {
                peer_id: peer_id.to_string(),
                retry_at,
            });

        let updates = harness.drain_updates();
        assert!(updates.iter().any(|update| matches!(
            update,
            PeerUpdate::Discovered(peers)
                if peers.iter().any(|peer| peer.peer_id == peer_id
                    && peer.connection_status.as_deref() == Some("disconnected"))
        )));

        assert_eq!(harness.peers.dial_state(peer_id), DialState::Idle);
        assert!(harness.controller_loop.background_tasks.is_empty());
    });
}

#[tokio::test]
async fn retry_peer_on_smaller_side_still_dials() {
    with_temp_home(|| {
        // "peer-aaa-dialer" < "peer-remote": the smaller side dials.
        let mut harness = TestHarness::new("peer-aaa-dialer");
        let peer_id = "peer-remote";
        harness.peers.remember_discovered(pairing_discovered_peer(peer_id));
        harness.controller_loop.config_store.pair_peer(peer_id).unwrap();

        let retry_at = std::time::Instant::now();
        harness.peers.set_backoff(peer_id, retry_at);
        harness
            .controller_loop
            .handle_controller_event(ControllerEvent::RetryPeer {
                peer_id: peer_id.to_string(),
                retry_at,
            });

        let updates = harness.drain_updates();
        assert!(updates.iter().any(|update| matches!(
            update,
            PeerUpdate::Discovered(peers)
                if peers.iter().any(|peer| peer.peer_id == peer_id
                    && peer.connection_status.as_deref() == Some("connecting"))
        )));

        assert!(matches!(
            harness.peers.dial_state(peer_id),
            DialState::Connecting(_)
        ));
        assert_eq!(harness.controller_loop.background_tasks.len(), 1);
        harness.controller_loop.background_tasks.abort_all();
    });
}

#[test]
fn manual_disconnect_pause_suppresses_late_loss_and_retry_events() {
    with_temp_home(|| {
        let mut harness = TestHarness::new("peer-local-manual-disconnect");
        let peer_id = "peer-remote";
        let addr = SocketAddr::from(([127, 0, 0, 1], crate::core::policies::DEFAULT_PORT));
        harness.peers.remember_discovered(pairing_discovered_peer(peer_id));
        harness.controller_loop.config_store.pair_peer(peer_id).unwrap();
        harness.peers.remember_connected(ConnectedPeer {
            peer_id: peer_id.to_string(),
            name: "Remote".to_string(),
            addr,
            conn_addr: None,
            virtual_bounds: None,
            display_rects: Vec::new(),
            direction: ConnectedPeerDirection::Inbound,
            platform: None,
        });

        harness.controller_loop.config_store.pause_peer(peer_id).unwrap();
        harness.controller_loop.handle_session_event(SessionEvent::SessionLost {
            peer_id: peer_id.to_string(),
        });

        assert_eq!(harness.peers.dial_state(peer_id), DialState::Idle);
        assert!(harness.controller_loop.background_tasks.is_empty());

        let retry_at = std::time::Instant::now();
        harness.peers.set_backoff(peer_id, retry_at);
        harness
            .controller_loop
            .handle_controller_event(ControllerEvent::RetryPeer {
                peer_id: peer_id.to_string(),
                retry_at,
            });

        assert_eq!(harness.peers.dial_state(peer_id), DialState::Idle);
        assert!(harness.controller_loop.background_tasks.is_empty());
    });
}

#[test]
fn connect_failed_while_pairing_does_not_clear_pairing_wait() {
    with_temp_home(|| {
        let mut harness = TestHarness::new("peer-local-stale-failure");
        harness.peers.remember_discovered(pairing_discovered_peer("peer-remote"));
        let attempt = harness.peers.set_connecting("peer-remote");
        harness.peers.set_pairing("peer-remote");

        harness
            .controller_loop
            .handle_controller_event(ControllerEvent::ConnectFailed {
                peer_id: "peer-remote".to_string(),
                attempt,
            });

        assert_eq!(harness.peers.dial_state("peer-remote"), DialState::Pairing);
        assert!(harness.controller_loop.background_tasks.is_empty());
    });
}

#[tokio::test]
async fn rediscovery_while_pairing_does_not_start_duplicate_dial() {
    let peers = Arc::new(PeerStore::default());
    peers.remember_discovered(pairing_discovered_peer("peer-remote"));
    peers.set_pairing("peer-remote");
    let (updates, _rx) = broadcast::channel(10);
    let engine: Arc<dyn crate::core::platform::SessionEngine> =
        Arc::new(FakeEngine::new()) as Arc<dyn crate::core::platform::SessionEngine>;
    let (controller_tx, _controller_rx) = mpsc::channel(1);
    let mut background_tasks = JoinSet::new();

    let mut ctx = RuntimeContext {
        local_peer_id: "local-peer",
        peers: Arc::clone(&peers),
        updates: Arc::new(updates),
        connector: test_connector("local-peer"),
        session: engine,
        controller_tx,
        background_tasks: &mut background_tasks,
    };

    RunningController::handle_discovery_event(
        &mut ctx,
        DiscoveryEvent::Discovered(pairing_discovered_peer("peer-remote")),
        false,
    );

    assert_eq!(peers.dial_state("peer-remote"), DialState::Pairing);
    assert!(background_tasks.is_empty());
}

#[test]
fn second_inbound_while_pairing_replaces_stale_pending() {
    with_temp_home(|| {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let mut harness = TestHarness::new("peer-local-dup-inbound");
            let _first = harness.connect_inbound("peer-remote", "Remote Machine", Some("macos")).await;
            assert!(harness.has_pending_pairing("peer-remote"));
            let _ = harness.drain_updates();

            // The remote cancelled and dialed again: the fresh handshake
            // must replace the stale waiter so the pairing dialog shows
            // again instead of the retry being rejected as a duplicate.
            let second = harness.connect_inbound("peer-remote", "Remote Machine", Some("macos")).await;

            assert!(harness.has_pending_pairing("peer-remote"));
            let pending = harness.controller_loop.pending_pairings.get("peer-remote").unwrap();
            assert!(Arc::ptr_eq(&pending.conn, &second));
            let updates = harness.drain_updates();
            assert!(updates.iter().any(|u| matches!(
                u,
                PeerUpdate::PairingRequested { peer_id, .. } if peer_id == "peer-remote"
            )));

            harness.respond_pairing("peer-remote", true);
            assert!(harness.is_connected("peer-remote"));
            assert!(!harness.has_pending_pairing("peer-remote"));
        });
    });
}

async fn outbound_pairing_event(harness: &mut TestHarness, peer_id: &str) -> ControllerEvent {
    let (connection, attempt) = harness.connect_outbound(peer_id).await;
    let addr = connection.addr();
    ControllerEvent::OutboundPairingPending {
        discovered: DiscoveredPeer {
            peer_id: peer_id.to_string(),
            primary_addr: addr,
            addrs: vec![addr],
            hostname: "remote.local".to_string(),
            fullname: format!("{peer_id}._zerog._udp.local."),
            platform: None,
        },
        connected: ConnectedPeer {
            peer_id: peer_id.to_string(),
            name: "Remote".to_string(),
            addr,
            conn_addr: Some(addr),
            virtual_bounds: None,
            display_rects: Vec::new(),
            direction: ConnectedPeerDirection::Outbound,
            platform: None,
        },
        certificate: vec![9u8; 64],
        connection,
        attempt,
    }
}

#[test]
fn cancel_connect_clears_outbound_pairing_waiter() {
    with_temp_home(|| {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let mut harness = TestHarness::new("peer-local-cancel");
            harness.peers.remember_discovered(pairing_discovered_peer("peer-remote"));
            let event = outbound_pairing_event(&mut harness, "peer-remote").await;
            harness.controller_loop.handle_controller_event(event);
            assert!(harness
                .controller_loop
                .pending_outbound_pairings
                .contains_key("peer-remote"));
            let _ = harness.drain_updates();

            harness
                .controller_loop
                .handle_controller_event(ControllerEvent::CancelConnect {
                    peer_id: "peer-remote".to_string(),
                });

            assert!(!harness
                .controller_loop
                .pending_outbound_pairings
                .contains_key("peer-remote"));
            assert_eq!(harness.peers.dial_state("peer-remote"), DialState::Idle);
            assert!(!harness.is_paired("peer-remote"));
            let updates = harness.drain_updates();
            assert!(updates.iter().any(|u| matches!(
                u,
                PeerUpdate::PairingResolved { peer_id, approved: false }
                if peer_id == "peer-remote"
            )));
        });
    });
}

#[test]
fn cancelled_outbound_ignores_late_pairing_approval() {
    with_temp_home(|| {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let mut harness = TestHarness::new("peer-local-cancel-race");
            harness.peers.remember_discovered(pairing_discovered_peer("peer-remote"));
            let event = outbound_pairing_event(&mut harness, "peer-remote").await;
            harness.controller_loop.handle_controller_event(event);
            let _ = harness.drain_updates();
            harness
                .controller_loop
                .handle_controller_event(ControllerEvent::CancelConnect {
                    peer_id: "peer-remote".to_string(),
                });
            let _ = harness.drain_updates();

            harness
                .controller_loop
                .handle_session_event(SessionEvent::SessionPairingApproved {
                    peer_id: "peer-remote".to_string(),
                });

            assert!(!harness.is_paired("peer-remote"));
            assert!(!harness.is_connected("peer-remote"));
            assert!(!harness.is_cert_trusted("peer-remote"));
            let updates = harness.drain_updates();
            assert!(
                !updates.iter().any(|u| matches!(u, PeerUpdate::ConfigChanged(_))),
                "late approval after cancel must not pair: {updates:?}"
            );
        });
    });
}

#[test]
fn connect_failed_after_cancel_stays_idle_without_retry() {
    with_temp_home(|| {
        let mut harness = TestHarness::new("peer-local-cancel-failed");
        harness.peers.remember_discovered(pairing_discovered_peer("peer-remote"));
        let attempt = harness.peers.set_connecting("peer-remote");
        harness
            .controller_loop
            .handle_controller_event(ControllerEvent::CancelConnect {
                peer_id: "peer-remote".to_string(),
            });
        let _ = harness.drain_updates();
        assert_eq!(harness.peers.dial_state("peer-remote"), DialState::Idle);

        harness
            .controller_loop
            .handle_controller_event(ControllerEvent::ConnectFailed {
                peer_id: "peer-remote".to_string(),
                attempt,
            });

        assert_eq!(harness.peers.dial_state("peer-remote"), DialState::Idle);
        assert!(harness.controller_loop.background_tasks.is_empty());
    });
}

#[test]
fn cancel_connect_clears_connecting_state() {
    with_temp_home(|| {
        let mut harness = TestHarness::new("peer-local-cancel-connecting");
        harness.peers.remember_discovered(pairing_discovered_peer("peer-remote"));
        harness.peers.set_connecting("peer-remote");
        let _ = harness.drain_updates();

        harness
            .controller_loop
            .handle_controller_event(ControllerEvent::CancelConnect {
                peer_id: "peer-remote".to_string(),
            });

        assert_eq!(harness.peers.dial_state("peer-remote"), DialState::Idle);
        let updates = harness.drain_updates();
        assert!(updates.iter().any(|u| matches!(
            u,
            PeerUpdate::Discovered(_)
        )));
    });
}

#[test]
fn disconnect_while_deciding_clears_inbound_pairing_waiter() {
    with_temp_home(|| {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let mut harness = TestHarness::new("peer-local-cancel-notify");
            harness
                .connect_inbound("peer-remote", "Remote Machine", Some("macos"))
                .await;
            assert!(harness.has_pending_pairing("peer-remote"));
            let _ = harness.drain_updates();

            // The remote cancelled its request: the disconnect notify must
            // drop our inbound waiter and hide the dialog.
            harness.controller_loop.handle_session_event(
                crate::core::platform::engine::SessionEvent::SessionDisconnected {
                    peer_id: "peer-remote".to_string(),
                },
            );

            assert!(!harness.has_pending_pairing("peer-remote"));
            let updates = harness.drain_updates();
            assert!(updates.iter().any(|u| matches!(
                u,
                PeerUpdate::PairingResolved { peer_id, approved: false }
                if peer_id == "peer-remote"
            )));
        });
    });
}

#[test]
fn accepting_pairing_on_closed_connection_does_not_pair() {
    with_temp_home(|| {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let mut harness = TestHarness::new("peer-local-stale-accept");
            harness
                .connect_inbound("peer-remote", "Remote Machine", Some("macos"))
                .await;
            assert!(harness.has_pending_pairing("peer-remote"));
            let _ = harness.drain_updates();

            // The remote went away after we showed the dialog (e.g. it
            // cancelled while our Allow click was in flight).
            {
                let pending = harness
                    .controller_loop
                    .pending_pairings
                    .get("peer-remote")
                    .unwrap();
                pending.conn.close(b"remote cancelled");
            }
            let _ = harness.drain_updates();

            harness.respond_pairing("peer-remote", true);

            assert!(!harness.is_paired("peer-remote"));
            assert!(!harness.is_connected("peer-remote"));
            assert!(!harness.is_cert_trusted("peer-remote"));
            let updates = harness.drain_updates();
            assert!(updates.iter().any(|u| matches!(
                u,
                PeerUpdate::PairingResolved { peer_id, approved: false }
                if peer_id == "peer-remote"
            )));
            assert!(
                !updates.iter().any(|u| matches!(u, PeerUpdate::ConfigChanged(_))),
                "accepting a dead pairing must not pair: {updates:?}"
            );
        });
    });
}
