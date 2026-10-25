use crate::core::api::{ConfigSnapshot, PeerRefSnapshot};
use crate::core::events::LayoutPlacement;
use crate::core::layout::resolved_layout_from_config;
use crate::core::peer_state::PeerRuntimeStateSnapshot;
use crate::core::platform::EdgeAssignments;
use crate::core::platform::engine::SessionEvent;
use anyhow::{Result, anyhow};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::sync::mpsc;
use tracing::info;

use super::clipboard_sync::ClipboardSync;
use crate::core::events::ClipboardSequence;
use crate::core::platform::ThroughputMetrics;
use crate::core::platform::traits::{ClipboardManager, NetworkState as NetworkStateTrait};
use crate::events::{Event, LayoutUpdateEvent};
use crate::platform::shared::cursor_state::CursorState;
use crate::platform::shared::layout_state::{
    LayoutState, RuntimeContact, runtime_contacts_for_peers,
};
use crate::platform::shared::runtime::{NetworkState, join_runtime_threads, spawn_runtime_threads};
use crate::platform::shared::transition::TransitionState;
use crate::platform::{DisplayBounds, DisplayManager, EventInjection, PeerEdge, TransferMode};
use crate::quic::PeerConnection;

pub(crate) use super::runtime::RuntimeConfig;

const DISCONNECT_NOTIFY_GRACE: Duration = Duration::from_millis(25);

pub(crate) type ThreadDisplayFactory =
    Arc<dyn Fn() -> Box<dyn DisplayManager + Send> + Send + Sync>;

#[derive(Clone)]
struct PeerRecord {
    peer: PeerRefSnapshot,
    addr: SocketAddr,
    edge: PeerEdge,
    display_bounds: Option<DisplayBounds>,
}

struct RunningState {
    config: RuntimeConfig,
    network: Arc<NetworkState>,
    cursor: Arc<CursorState>,
    transition: Arc<TransitionState>,
    clipboard_sync: Option<Arc<ClipboardSync>>,
    transfer_sync: Option<Arc<crate::core::transfer::TransferSync>>,
    clipboard_task: Option<tokio::task::JoinHandle<()>>,
    run_signal: Arc<AtomicBool>,
    threads: Vec<JoinHandle<()>>,
}

#[derive(Default)]
struct SessionState {
    config: Option<RuntimeConfig>,
    layout_config: Option<ConfigSnapshot>,
    local_identity: Option<PeerRefSnapshot>,
    peers: HashMap<String, PeerRecord>,
    running: Option<RunningState>,
    layout_update_rx: Option<mpsc::Receiver<LayoutUpdateEvent>>,
    session_event_rx: Option<crossbeam_channel::Receiver<SessionEvent>>,
}

pub(crate) struct SessionManager {
    display: Arc<dyn DisplayManager + Send + Sync>,
    injection: Arc<dyn EventInjection>,
    source_marker: u64,
    platform_name: &'static str,
    create_thread_display: ThreadDisplayFactory,
    cursor: Arc<CursorState>,
    layout: Arc<LayoutState>,
    layout_update_tx: mpsc::Sender<LayoutUpdateEvent>,
    clipboard: Option<Arc<dyn ClipboardManager>>,
    clipboard_sequence: Arc<ClipboardSequence>,
    state: Mutex<SessionState>,
}

impl SessionManager {
    pub(crate) fn new(
        display: Arc<dyn DisplayManager + Send + Sync>,
        injection: Arc<dyn EventInjection>,
        capture_policy: Arc<AtomicU8>,
        source_marker: u64,
        platform_name: &'static str,
        create_thread_display: ThreadDisplayFactory,
        clipboard: Option<Arc<dyn ClipboardManager>>,
    ) -> Self {
        let clipboard_sequence = Arc::new(ClipboardSequence::new());
        let (layout_update_tx, layout_update_rx) = mpsc::channel(32);
        let cursor = Arc::new(CursorState::new(Arc::clone(&capture_policy)));
        let initial_state = SessionState {
            layout_update_rx: Some(layout_update_rx),
            ..Default::default()
        };
        Self {
            display,
            injection,
            source_marker,
            platform_name,
            create_thread_display,
            cursor,
            layout: Arc::new(LayoutState::default()),
            layout_update_tx,
            clipboard,
            clipboard_sequence,
            state: Mutex::new(initial_state),
        }
    }

    pub(crate) fn set_local_identity(&self, peer: PeerRefSnapshot) -> Result<()> {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        state.local_identity = Some(peer);
        Ok(())
    }

    pub(crate) fn set_runtime_config(&self, config: RuntimeConfig) -> Result<()> {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(running) = state.running.as_mut() {
            running.config = config;
        } else {
            state.config = Some(config);
        }
        Ok(())
    }

    pub(crate) fn configure_runtime(
        &self,
        endpoint: Arc<crate::platform::shared::quic::QuicEndpoint>,
        batch_window_us: u64,
        max_events_per_packet: usize,
        movement_cadence_us: u64,
        _peer_id: String,
        clipboard_poll_interval_ms: u64,
        clipboard_ack_timeout_ms: u64,
    ) -> Result<()> {
        self.set_runtime_config(RuntimeConfig {
            endpoint,
            batch_window_us,
            max_events_per_packet,
            movement_cadence_us,
            clipboard_poll_interval_ms,
            clipboard_ack_timeout_ms,
        })?;
        Ok(())
    }

    pub(crate) fn is_running(&self) -> bool {
        self.state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .running
            .is_some()
    }

    fn resolved_runtime_contacts(&self, state: &SessionState) -> Vec<RuntimeContact> {
        let local_bounds = self.display.get_virtual_bounds().ok();
        let peer_bounds = state
            .peers
            .iter()
            .filter_map(|(peer_id, record)| {
                record
                    .display_bounds
                    .map(|bounds| (peer_id.clone(), bounds))
            })
            .collect::<HashMap<_, _>>();
        let peer_addrs = state
            .peers
            .iter()
            .map(|(peer_id, record)| (peer_id.clone(), record.addr))
            .collect::<HashMap<_, _>>();
        let fallback_edges = state
            .peers
            .iter()
            .map(|(peer_id, record)| (peer_id.clone(), record.edge))
            .collect::<HashMap<_, _>>();
        let resolved = state
            .layout_config
            .as_ref()
            .map(|config| resolved_layout_from_config(config, local_bounds, &peer_bounds))
            .unwrap_or_default();
        runtime_contacts_for_peers(&resolved, &peer_addrs, &fallback_edges)
    }

    fn layout_update_for_config(
        &self,
        config: &ConfigSnapshot,
        local_peer: Option<&PeerRefSnapshot>,
    ) -> Option<LayoutUpdateEvent> {
        if config.placements.is_empty() {
            return None;
        }
        let local_peer = local_peer?;
        let mut placements = config.placements.clone();
        if !placements
            .iter()
            .any(|placement| placement.peer_id == local_peer.peer_id)
        {
            placements.push(LayoutPlacement {
                peer_id: local_peer.peer_id.clone(),
                peer_label: local_peer.name.clone(),
                x: 0,
                y: 0,
            });
        }
        Some(LayoutUpdateEvent {
            version: 0,
            editor_id: self.source_marker,
            timestamp: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis() as u64,
            placements,
        })
    }

    fn add_local_layout_origin(&self, mut update: LayoutUpdateEvent) -> LayoutUpdateEvent {
        let local_peer = self.local_identity();
        let Some(local_peer) = local_peer else {
            return update;
        };
        if !update
            .placements
            .iter()
            .any(|placement| placement.peer_id == local_peer.peer_id)
        {
            update.placements.push(LayoutPlacement {
                peer_id: local_peer.peer_id,
                peer_label: local_peer.name,
                x: 0,
                y: 0,
            });
        }
        update
    }

    fn replace_runtime_layout(&self, state: &SessionState, _active_target: Option<SocketAddr>) {
        let contacts = self.resolved_runtime_contacts(state);
        self.layout.replace(contacts);
    }

    fn local_identity(&self) -> Option<PeerRefSnapshot> {
        self.state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .local_identity
            .clone()
    }

    pub(crate) fn runtime_network(&self) -> Option<Arc<NetworkState>> {
        self.state.lock().ok().and_then(|state| {
            state
                .running
                .as_ref()
                .map(|running| Arc::clone(&running.network))
        })
    }

    pub(crate) fn runtime_transition(&self) -> Option<Arc<TransitionState>> {
        self.state.lock().ok().and_then(|state| {
            state
                .running
                .as_ref()
                .map(|running| Arc::clone(&running.transition))
        })
    }

    pub(crate) fn runtime_layout(&self) -> Option<Arc<LayoutState>> {
        self.state
            .lock()
            .ok()
            .and_then(|state| state.running.as_ref().map(|_| Arc::clone(&self.layout)))
    }

    pub(crate) fn runtime_cursor(&self) -> Option<Arc<CursorState>> {
        self.state.lock().ok().and_then(|state| {
            state
                .running
                .as_ref()
                .map(|running| Arc::clone(&running.cursor))
        })
    }

    #[allow(dead_code)]
    pub(crate) fn cursor(&self) -> Arc<CursorState> {
        Arc::clone(&self.cursor)
    }

    pub(crate) fn display(&self) -> &dyn DisplayManager {
        &*self.display
    }

    pub(crate) fn injection(&self) -> &dyn crate::core::platform::traits::EventInjection {
        &*self.injection
    }

    pub(crate) fn start_network(
        &self,
        event_rx: mpsc::Receiver<crate::platform::CapturedInputEvent>,
    ) -> Result<()> {
        let (config, active_peers, layout_config, local_identity) = {
            let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
            if state.running.is_some() {
                return Ok(());
            }
            let config = state
                .config
                .clone()
                .ok_or_else(|| anyhow!("runtime config is not set"))?;
            let active_peers = state.peers.values().cloned().collect::<Vec<_>>();
            (
                config,
                active_peers,
                state.layout_config.clone(),
                state.local_identity.clone(),
            )
        };

        let (network, session_event_rx) = NetworkState::new(&config, self.source_marker)?;
        let network = Arc::new(network);
        if self.cursor.mode() != TransferMode::LocalControl {
            self.cursor
                .set_mode(TransferMode::LocalControl, self.injection.as_ref())?;
        } else {
            self.injection.set_cursor_hidden(false)?;
        }
        let transition = Arc::new(TransitionState::new(self.display.as_ref())?);
        let cursor = Arc::clone(&self.cursor);
        if let Some(cfg) = &layout_config {
            network.set_swap_alt_cmd(cfg.swap_alt_cmd);
            network.set_modifier_mapping(cfg.modifier_mapping.unwrap_or_default());
            network.set_keep_mouse_alive(cfg.keep_mouse_alive);
            network.set_release_tap_key(cfg.release_tap_key);
        }
        network.set_current_layout_update(
            layout_config
                .as_ref()
                .and_then(|cfg| self.layout_update_for_config(cfg, local_identity.as_ref())),
        );
        for peer in &active_peers {
            network.add_peer(
                &peer.peer.peer_id,
                &peer.peer.name,
                peer.addr,
                peer.display_bounds,
                peer.peer.platform.as_deref(),
            );
        }

        let clipboard_sync = self.clipboard.as_ref().map(|cb| {
            let sync = ClipboardSync::new(
                Arc::clone(cb),
                Arc::clone(&network) as Arc<dyn NetworkStateTrait>,
                Arc::clone(&self.clipboard_sequence),
                crate::platform::shared::tokio_runtime::global_runtime_handle(),
                Duration::from_millis(config.clipboard_poll_interval_ms),
                Duration::from_millis(config.clipboard_ack_timeout_ms),
            );
            if let Some(cfg) = &layout_config {
                sync.set_enabled(cfg.share_clipboard);
            }
            sync
        });
        let clipboard_task = clipboard_sync.as_ref().map(|sync| sync.start());
        let transfer_sync = Some(Arc::new(crate::core::transfer::TransferSync::new(
            Arc::clone(&network) as Arc<dyn NetworkStateTrait>,
            crate::platform::shared::tokio_runtime::global_runtime_handle(),
        )));

        let running = Arc::new(AtomicBool::new(true));
        let threads = spawn_runtime_threads(
            self.platform_name,
            &*self.create_thread_display,
            Arc::clone(&self.injection),
            event_rx,
            local_identity,
            Arc::clone(&running),
            Arc::clone(&network),
            Arc::clone(&cursor),
            Arc::clone(&transition),
            Arc::clone(&self.layout),
            self.layout_update_tx.clone(),
            config.movement_cadence_us,
            clipboard_sync.clone(),
            transfer_sync.clone(),
        )?;

        let announce_peers = active_peers
            .iter()
            .map(|peer| (peer.peer.peer_id.clone(), peer.addr))
            .collect::<Vec<_>>();
        {
            let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
            state.config = Some(config.clone());
            state.session_event_rx = Some(session_event_rx);
            state.running = Some(RunningState {
                config,
                network: Arc::clone(&network),
                cursor: Arc::clone(&cursor),
                transition: Arc::clone(&transition),
                clipboard_sync,
                transfer_sync,
                clipboard_task,
                run_signal: running,
                threads,
            });
            self.replace_runtime_layout(&state, network.active_target());
        }
        for (peer_id, _addr) in announce_peers {
            network.mark_outgoing_pending(&peer_id);
        }
        info!("{} session started", self.platform_name);
        Ok(())
    }

    pub(crate) fn stop_network(&self) -> Result<()> {
        let running = {
            let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
            state.session_event_rx = None;
            let running = state.running.take();
            if let Some(running) = running.as_ref() {
                state.config = Some(running.config.clone());
            }
            running
        };
        let Some(running) = running else {
            let _ = self.injection.release_all_keys();
            let _ = self.injection.unlock_cursor_position();
            let _ = self.injection.set_cursor_hidden(false);
            return Ok(());
        };
        if let Some(sync) = running.transfer_sync.as_ref() {
            sync.stop();
        }
        if let Some(sync) = running.clipboard_sync.as_ref() {
            sync.stop();
        }
        if let Some(task) = running.clipboard_task.as_ref() {
            task.abort();
        }
        let mut stop_error: Option<anyhow::Error> = None;
        running.run_signal.store(false, Ordering::SeqCst);
        running.network.stop_outbound_input();
        let mut restore_error = running
            .cursor
            .transition_to_local_control(self.injection.as_ref(), None)
            .err();
        running.network.shutdown_endpoint();
        if let Err(err) = join_runtime_threads(running.threads)
            && stop_error.is_none()
        {
            stop_error = Some(err);
        }
        if running.cursor.mode() != TransferMode::LocalControl {
            restore_error = running
                .cursor
                .transition_to_local_control(self.injection.as_ref(), None)
                .err();
        }
        if stop_error.is_none() {
            stop_error = restore_error;
        }
        if let Some(err) = stop_error {
            return Err(err);
        }
        Ok(())
    }

    pub(crate) fn emergency_unlock(&self) -> Result<()> {
        let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(running) = state.running.as_ref() {
            running.network.stop_outbound_input();
            let (pending_handoff, runtime_recovery) =
                running.cursor.take_pending_handoff_and_recovery();
            let recovery = runtime_recovery
                .or_else(|| running.cursor.last_edge_position())
                .or_else(|| {
                    running
                        .transition
                        .topology_snapshot()
                        .ok()
                        .map(|t| crate::platform::shared::topology::recovery_point(&t))
                });
            running
                .cursor
                .transition_to_local_control(self.injection.as_ref(), recovery)?;
            running.cursor.record_transition(running.network.mono_us());
            tracing::info!(
                domain = "runtime.transition",
                transition_key = "handoff_recovery",
                handoff_id = pending_handoff
                    .as_ref()
                    .map(|value| value.motion_session_id),
                recovery_x = recovery.map(|value| value.0),
                recovery_y = recovery.map(|value| value.1),
                "restoring local cursor during emergency unlock"
            );
            tracing::warn!("emergency unlock invoked: local control restored");
            return Ok(());
        }
        let _ = self.injection.release_all_keys();
        let _ = self.injection.unlock_cursor_position();
        let _ = self.injection.set_cursor_hidden(false);
        if let Some(display) = self.display.discover_displays().ok().and_then(|displays| {
            displays
                .iter()
                .find(|display| {
                    0 >= display.x
                        && 0 < display.x.saturating_add(display.width as i32)
                        && 0 >= display.y
                        && 0 < display.y.saturating_add(display.height as i32)
                })
                .or_else(|| displays.first())
                .copied()
        }) {
            let x = display.x + (display.width / 2) as i32;
            let y = display.y + (display.height / 2) as i32;
            let _ = self.injection.warp_cursor(x, y);
        }
        tracing::warn!("emergency unlock invoked: local control restored");
        Ok(())
    }

    #[allow(dead_code)]
    pub(crate) fn get_display_manager(&self) -> &dyn DisplayManager {
        &*self.display
    }

    pub(crate) fn take_layout_update_rx(
        &self,
    ) -> Result<Option<mpsc::Receiver<LayoutUpdateEvent>>> {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        Ok(state.layout_update_rx.take())
    }

    pub(crate) fn take_session_event_rx(
        &self,
    ) -> Result<Option<crossbeam_channel::Receiver<SessionEvent>>> {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        Ok(state.session_event_rx.take())
    }
}

impl crate::core::platform::SessionEngine for SessionManager {
    fn start(&self, event_rx: mpsc::Receiver<crate::platform::CapturedInputEvent>) -> Result<()> {
        SessionManager::start_network(self, event_rx)
    }

    fn set_local_identity(&self, peer: PeerRefSnapshot) -> Result<()> {
        SessionManager::set_local_identity(self, peer)
    }

    fn configure_runtime(
        &self,
        endpoint: Arc<crate::platform::shared::quic::QuicEndpoint>,
        batch_window_us: u64,
        max_events_per_packet: usize,
        movement_cadence_us: u64,
        peer_id: String,
        clipboard_poll_interval_ms: u64,
        clipboard_ack_timeout_ms: u64,
    ) -> Result<()> {
        SessionManager::configure_runtime(
            self,
            endpoint,
            batch_window_us,
            max_events_per_packet,
            movement_cadence_us,
            peer_id,
            clipboard_poll_interval_ms,
            clipboard_ack_timeout_ms,
        )
    }

    fn emergency_unlock(&self) -> Result<()> {
        SessionManager::emergency_unlock(self)
    }

    fn shutdown(&self) -> Result<()> {
        SessionManager::stop_network(self)
    }

    fn send_layout_update(&self, update: LayoutUpdateEvent) -> Result<()> {
        let network = self
            .runtime_network()
            .ok_or_else(|| anyhow!("network runtime is not active"))?;
        let update = self.add_local_layout_origin(update);
        network.set_current_layout_update(Some(update.clone()));
        network.flush_pending_input_now()?;
        network.broadcast_packet(vec![Event::LayoutUpdate(update)])?;
        Ok(())
    }

    fn take_layout_update_rx(&self) -> Result<Option<mpsc::Receiver<LayoutUpdateEvent>>> {
        SessionManager::take_layout_update_rx(self)
    }

    fn take_session_event_rx(&self) -> Result<Option<crossbeam_channel::Receiver<SessionEvent>>> {
        SessionManager::take_session_event_rx(self)
    }

    fn transfer_sync(&self) -> Option<Arc<crate::core::transfer::TransferSync>> {
        self.state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .running
            .as_ref()
            .and_then(|r| r.transfer_sync.clone())
    }

    fn reconcile_peer_edges(&self, assignments: &EdgeAssignments) -> Result<()> {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let mut next_edges = HashMap::new();
        let mut assigned_edges = HashMap::new();
        for (peer_id, record) in &state.peers {
            let edge = assignments.edge_for_peer(peer_id).unwrap_or(record.edge);
            if let Some(existing_peer_id) = assigned_edges.insert(edge, peer_id.clone()) {
                anyhow::bail!(
                    "cannot assign edge {} to both {} and {}",
                    edge.as_str(),
                    existing_peer_id,
                    peer_id
                );
            }
            next_edges.insert(peer_id.clone(), edge);
        }
        for (peer_id, record) in state.peers.iter_mut() {
            let next_edge = *next_edges
                .get(peer_id)
                .expect("every known peer should have a reconciled edge");
            if record.edge != next_edge {
                record.edge = next_edge;
            }
        }
        if let Some(active_target) = state
            .running
            .as_ref()
            .map(|running| running.network.active_target())
        {
            self.replace_runtime_layout(&state, active_target);
        }
        Ok(())
    }

    fn reconcile_layout(&self, config: &ConfigSnapshot) -> Result<()> {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        state.layout_config = Some(config.clone());
        if let Some(running) = state.running.as_ref() {
            running.network.set_swap_alt_cmd(config.swap_alt_cmd);
            running
                .network
                .set_modifier_mapping(config.modifier_mapping.unwrap_or_default());
            running.network.set_keep_mouse_alive(config.keep_mouse_alive);
            running.network.set_release_tap_key(config.release_tap_key);
            if let Some(sync) = running.clipboard_sync.as_ref() {
                sync.set_enabled(config.share_clipboard);
            }
        }
        let layout_update = self.layout_update_for_config(config, state.local_identity.as_ref());
        if let Some(active_target) = state
            .running
            .as_ref()
            .map(|running| running.network.active_target())
        {
            if let Some(running) = state.running.as_ref() {
                running.network.set_current_layout_update(layout_update);
            }
            self.replace_runtime_layout(&state, active_target);
        }
        Ok(())
    }

    fn get_throughput_metrics(&self) -> ThroughputMetrics {
        self.runtime_network()
            .map(|network| network.get_throughput_metrics())
            .unwrap_or_default()
    }

    fn get_peer_runtime_state(&self) -> Vec<PeerRuntimeStateSnapshot> {
        self.runtime_network()
            .map(|network| network.get_peer_runtime_state())
            .unwrap_or_default()
    }

    fn close_duplicate_connection(&self, conn: Arc<PeerConnection>, reason: &[u8]) {
        if let Some(network) = self.runtime_network() {
            network.endpoint().close_duplicate_connection(&conn, reason);
        }
    }

    fn promote_connection(&self, peer_id: &str, conn: Arc<PeerConnection>) -> Result<()> {
        if let Some(network) = self.runtime_network() {
            network.endpoint().promote_connection(peer_id, &conn)?;
            network.register_conn_addr(conn.addr(), peer_id);
        }
        Ok(())
    }

    fn add_peer(
        &self,
        peer: PeerRefSnapshot,
        addr: SocketAddr,
        edge: PeerEdge,
        display_bounds: Option<DisplayBounds>,
        is_inbound: bool,
    ) -> Result<()> {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        state.peers.insert(
            peer.peer_id.clone(),
            PeerRecord {
                peer: peer.clone(),
                addr,
                edge,
                display_bounds,
            },
        );
        if let Some(running) = state.running.as_ref() {
            running.network.add_peer(
                &peer.peer_id,
                &peer.name,
                addr,
                display_bounds,
                peer.platform.as_deref(),
            );
            self.replace_runtime_layout(&state, running.network.active_target());
            if !is_inbound {
                running.network.mark_outgoing_pending(&peer.peer_id);
            } else {
                running.network.mark_connected(&peer.peer_id);
            }
        }
        info!(peer_id = %peer.peer_id, peer_name = %peer.name, peer_addr = %addr, "peer added");
        Ok(())
    }

    fn mark_connection_established(&self, peer_id: &str) {
        if let Some(network) = self.runtime_network() {
            network.mark_connected(peer_id);
        }
    }

    fn remove_peer(&self, peer_id: &str) -> Result<()> {
        self.remove_peer_internal(peer_id, false)
    }

    fn unpair_peer(&self, peer_id: &str) -> Result<()> {
        self.remove_peer_internal(peer_id, true)
    }

    fn send_pairing_pending(&self, addr: SocketAddr, _peer_id: &str) -> Result<()> {
        let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(running) = state.running.as_ref() {
            let local_id = state
                .local_identity
                .as_ref()
                .map(|p| p.peer_id.clone())
                .unwrap_or_default();
            let events = vec![Event::PairingPending { peer_id: local_id }];
            if let Some(conn) = running.network.connection_for_addr(addr) {
                let _ = running.network.send_packet_on_connection_blocking(&conn, events);
            } else {
                let _ = running.network.send_packet_to(addr, events);
            }
        }
        Ok(())
    }

    fn send_pairing_approved(&self, addr: SocketAddr, _peer_id: &str) -> Result<()> {
        let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(running) = state.running.as_ref() {
            let local_id = state
                .local_identity
                .as_ref()
                .map(|p| p.peer_id.clone())
                .unwrap_or_default();
            let events = vec![Event::PairingApproved { peer_id: local_id }];
            if let Some(conn) = running.network.connection_for_addr(addr) {
                let _ = running.network.send_packet_on_connection_blocking(&conn, events);
            } else {
                let _ = running.network.send_packet_to(addr, events);
            }
        }
        Ok(())
    }

    fn notify_pairing_cancelled(&self, addr: SocketAddr) -> Result<()> {
        // Outbound pairing has no session record yet, so the remove path
        // below cannot notify. Send explicitly so the remote drops its
        // inbound waiter (hiding its dialog) instead of lingering until
        // a retry replaces it.
        let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(running) = state.running.as_ref() {
            if let Some(local_peer) = state.local_identity.as_ref() {
                if notify_peer_disconnected(&running.network, addr, local_peer) {
                    std::thread::sleep(DISCONNECT_NOTIFY_GRACE);
                }
            }
        }
        Ok(())
    }
}

impl SessionManager {
    fn remove_peer_internal(&self, peer_id: &str, unpair: bool) -> Result<()> {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let Some(record) = state.peers.remove(peer_id) else {
            if let Some(running) = state.running.as_ref() {
                // No session record yet (e.g. outbound pairing pending whose
                // QUIC connection was already promoted). Still drop any
                // canonical endpoint connection so Cancel actually closes it.
                running.network.endpoint().remove_connection(peer_id);
                running.network.clear_conn_addr_for_peer(peer_id);
                running.network.remove_peer(peer_id);
            }
            return Ok(());
        };
        let local_identity = state.local_identity.clone();
        if let Some(running) = state.running.as_ref() {
            let active_target = running.network.active_target();
            let active_peer_id = active_target
                .and_then(|target| running.network.peer_id_for_addr(target));
            let controls_cursor = active_target == Some(record.addr)
                || active_peer_id.as_deref() == Some(peer_id)
                || running.cursor.is_session_peer(Some(peer_id), record.addr);
            if controls_cursor {
                running.network.stop_outbound_input();
                let was_remote = running.cursor.mode().is_remote_control();
                let (_, saved_position) = running
                    .cursor
                    .take_pending_handoff_for_peer(Some(peer_id), record.addr);
                let recovery = if was_remote { saved_position } else { None };
                if let Err(error) = running
                    .cursor
                    .transition_to_local_control(self.injection.as_ref(), recovery)
                {
                    tracing::warn!(
                        peer_id,
                        error = %error,
                        "failed to restore local control while removing peer"
                    );
                }
            }
            if let Some(local_peer) = &local_identity {
                let notified = if unpair {
                    notify_peer_unpaired(&running.network, record.addr, local_peer)
                } else {
                    notify_peer_disconnected(&running.network, record.addr, local_peer)
                };
                if notified {
                    std::thread::sleep(DISCONNECT_NOTIFY_GRACE);
                }
            }
            running.network.endpoint().remove_connection(peer_id);
            running
                .network
                .endpoint()
                .remove_connection_by_addr(record.addr);
            running.network.clear_conn_addr_for_peer(peer_id);
            running.network.remove_peer(peer_id);
            self.replace_runtime_layout(&state, running.network.active_target());
        }
        info!(
            peer_id = %peer_id,
            peer_name = %record.peer.name,
            peer_addr = %record.addr,
            unpair,
            "peer removed"
        );
        Ok(())
    }
}

fn notify_peer_disconnected(
    network: &NetworkState,
    addr: SocketAddr,
    local_peer: &PeerRefSnapshot,
) -> bool {
    let events = vec![Event::DisconnectNotify {
        peer_id: local_peer.peer_id.clone(),
        peer_name: local_peer.name.clone(),
    }];
    let result = if let Some(conn) = network.connection_for_addr(addr) {
        network.send_packet_on_connection_blocking(&conn, events)
    } else {
        network.send_packet_to(addr, events)
    };
    if let Err(error) = result {
        tracing::warn!(peer_addr = %addr, error = %error, "failed to send disconnect notification");
        return false;
    }
    true
}

fn notify_peer_unpaired(
    network: &NetworkState,
    addr: SocketAddr,
    local_peer: &PeerRefSnapshot,
) -> bool {
    let events = vec![Event::UnpairNotify {
        peer_id: local_peer.peer_id.clone(),
        peer_name: local_peer.name.clone(),
    }];
    let result = if let Some(conn) = network.connection_for_addr(addr) {
        network.send_packet_on_connection_blocking(&conn, events)
    } else {
        network.send_packet_to(addr, events)
    };
    if let Err(error) = result {
        tracing::warn!(peer_addr = %addr, error = %error, "failed to send unpair notification");
        return false;
    }
    true
}
