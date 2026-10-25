//! Network runtime for QUIC communication and event batching.
//!
//! This module provides the core networking layer for peer-to-peer communication.
//! It handles:
//! - QUIC endpoint management with configurable listen port
//! - Event batching for efficient network utilization
//! - Peer connection state tracking with timeout detection
//! - Throughput metrics collection for monitoring
//!
//! # Threading Model
//!
//! `NetworkState` is designed for concurrent access from multiple threads:
//! - Atomic types are used for counters and flags
//! - Mutex guards short-lived critical sections
//! - The QUIC endpoint is shared via Arc
//!
//! # Event Batching
//!
//! Input events are batched to reduce network overhead:
//! 1. Events accumulate in `pending_input` until a trigger condition
//! 2. Triggers: batch window elapsed OR max events reached
//! 3. Batched events are sent as a single QUIC datagram

use crate::core::peer_state::PeerRuntimeStateSnapshot;
use crate::core::platform::{DisplayBounds, ModifierMapping, PlatformFamily};
use crate::core::platform::engine::SessionEvent;
use anyhow::{Context, Result, bail};
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tracing::{info, warn};

use crate::core::events::PROTOCOL_VERSION;
use crate::core::platform::ThroughputMetrics;
use crate::core::platform::traits::NetworkState as NetworkStateTrait;
use crate::events::{Event, LayoutUpdateEvent, Packet, PeerState};
use crate::platform::TransferMode;

use super::super::metrics::MetricsTracker;
use super::super::network::batching::BatchManager;
use super::super::network::registry_impl::PeerRegistry;
use super::super::quic::{PeerConnection, QuicEndpoint};
pub(crate) use super::transport::is_idempotent_control_event;
use super::transport::{EventClass, classify, is_input_event};

/// Interval between heartbeat messages to peers.
pub(crate) const HEARTBEAT_INTERVAL: Duration = Duration::from_millis(100);
/// Time without receiving a packet before a peer is considered timed out.
pub(crate) const PEER_TIMEOUT: Duration = Duration::from_millis(2000);
/// Interval between ping messages for latency measurement.
pub(crate) const PING_INTERVAL: Duration = Duration::from_secs(1);

pub(crate) struct QueuedControlMessage {
    pub(crate) connection: Arc<PeerConnection>,
    pub(crate) data: Vec<u8>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct OutboundInputSession {
    pub(crate) target: SocketAddr,
    pub(crate) motion_session_id: u64,
}

/// Configuration for the network runtime.
#[derive(Clone)]
pub(crate) struct RuntimeConfig {
    /// Shared QUIC endpoint for all peer communication.
    pub(crate) endpoint: Arc<QuicEndpoint>,
    /// Microseconds to wait before flushing a batch (0 = immediate).
    pub(crate) batch_window_us: u64,
    /// Maximum events per packet before forcing a flush.
    pub(crate) max_events_per_packet: usize,
    /// Microseconds between forced movement-batch flushes (0 = disabled).
    pub(crate) movement_cadence_us: u64,
    pub(crate) clipboard_poll_interval_ms: u64,
    pub(crate) clipboard_ack_timeout_ms: u64,
}

/// Core network state shared across runtime threads.
///
/// Manages the QUIC endpoint, peer connections, event batching, and metrics.
/// All fields are either atomic or protected by mutexes for thread safety.
pub(crate) struct NetworkState {
    /// QUIC endpoint for communication.
    endpoint: Arc<QuicEndpoint>,
    /// Unique identifier for this peer (used in packet headers).
    sender_id: u64,
    /// Sequence number counter for outgoing packets.
    seq: AtomicU32,
    /// Sequence number counter for ownership handoff motion sessions.
    motion_session_seq: AtomicU64,
    /// Motion session currently used for locally captured remote input.
    outbound_input: Mutex<Option<OutboundInputSession>>,
    /// Start time for monotonic timestamp generation.
    start: Instant,
    /// Pending input batching configuration and queue.
    batching: BatchManager,
    /// Timestamp of last packet send.
    last_send_us: AtomicU64,
    /// Timestamp of last input event capture.
    last_input_us: AtomicU64,
    /// Throughput counters and rolling metrics window.
    metrics: MetricsTracker,
    /// Peer registry and connection state.
    peers: PeerRegistry,
    /// Channel for queuing control messages from sync code to async send.
    control_tx: crossbeam_channel::Sender<QueuedControlMessage>,
    /// Receiver for control messages (used by async receive loop).
    control_rx: crossbeam_channel::Receiver<QueuedControlMessage>,
    /// Maps QUIC connection remote addresses to peer IDs.
    ///
    /// With QUIC, each peer pair may have two connections (one initiated by
    /// each side), and the remote address on the accepted connection is an
    /// ephemeral port—not the peer's listen port. This mapping lets us
    /// identify which peer owns a given connection address so that
    /// `mark_packet_received` / `mark_peer_reachable` succeed regardless
    /// of which connection a datagram arrives on.
    current_layout_update: Mutex<Option<LayoutUpdateEvent>>,
    session_event_tx: Mutex<Option<crossbeam_channel::Sender<SessionEvent>>>,
}

impl NetworkState {
    pub(crate) fn new(
        config: &RuntimeConfig,
        sender_id: u64,
    ) -> Result<(Self, crossbeam_channel::Receiver<SessionEvent>)> {
        if config.max_events_per_packet == 0 {
            bail!("max_events_per_packet must be greater than zero");
        }

        let (control_tx, control_rx) = crossbeam_channel::unbounded();
        let (session_event_tx, session_event_rx) = crossbeam_channel::bounded(64);

        Ok((
            Self {
                endpoint: Arc::clone(&config.endpoint),
                sender_id,
                seq: AtomicU32::new(0),
                motion_session_seq: AtomicU64::new(0),
                outbound_input: Mutex::new(None),
                start: Instant::now(),
                batching: BatchManager::new(config.batch_window_us, config.max_events_per_packet),
                last_send_us: AtomicU64::new(0),
                last_input_us: AtomicU64::new(0),
                metrics: MetricsTracker::new(),
                peers: PeerRegistry::new(),
                control_tx,
                control_rx,
                current_layout_update: Mutex::new(None),
                session_event_tx: Mutex::new(Some(session_event_tx)),
            },
            session_event_rx,
        ))
    }

    pub(crate) fn endpoint(&self) -> &Arc<QuicEndpoint> {
        &self.endpoint
    }

    pub(crate) fn shutdown_endpoint(&self) {
        if let Ok(mut guard) = self.session_event_tx.lock() {
            guard.take();
        }
        self.endpoint.close();
    }

    pub(crate) fn set_current_layout_update(&self, update: Option<LayoutUpdateEvent>) {
        *self
            .current_layout_update
            .lock()
            .expect("current_layout_update lock poisoned") = update;
    }

    pub(crate) fn control_rx(&self) -> &crossbeam_channel::Receiver<QueuedControlMessage> {
        &self.control_rx
    }

    pub(crate) fn active_target(&self) -> Option<SocketAddr> {
        self.outbound_input_session().map(|session| session.target)
    }

    pub(crate) fn outbound_input_session(&self) -> Option<OutboundInputSession> {
        *self
            .outbound_input
            .lock()
            .expect("outbound_input lock poisoned")
    }

    pub(crate) fn target_platform(&self, target: SocketAddr) -> PlatformFamily {
        self.peers.platform_for_addr(target)
    }

    pub(crate) fn start_outbound_input(&self, target: SocketAddr, motion_session_id: u64) {
        debug_assert_ne!(motion_session_id, 0);
        self.batching.clear();
        *self
            .outbound_input
            .lock()
            .expect("outbound_input lock poisoned") = Some(OutboundInputSession {
            target,
            motion_session_id,
        });
    }

    pub(crate) fn stop_outbound_input(&self) {
        self.batching.clear();
        *self
            .outbound_input
            .lock()
            .expect("outbound_input lock poisoned") = None;
    }

    pub(crate) fn set_swap_alt_cmd(&self, swap: bool) {
        self.peers.set_swap_alt_cmd(swap);
    }

    pub(crate) fn swap_alt_cmd(&self) -> bool {
        self.peers.swap_alt_cmd()
    }

    pub(crate) fn set_modifier_mapping(&self, mapping: ModifierMapping) {
        self.peers.set_modifier_mapping(mapping);
    }

    pub(crate) fn modifier_mapping(&self) -> ModifierMapping {
        self.peers.modifier_mapping()
    }

    pub(crate) fn set_keep_mouse_alive(&self, keep: bool) {
        self.peers.set_keep_mouse_alive(keep);
    }

    pub(crate) fn keep_mouse_alive(&self) -> bool {
        self.peers.keep_mouse_alive()
    }

    pub(crate) fn set_release_tap_key(&self, key: crate::core::config::ReleaseTapKey) {
        self.peers.set_release_tap_key(key);
    }

    pub(crate) fn release_tap_keycode(&self) -> u32 {
        self.peers.release_tap_keycode()
    }

    pub(crate) fn add_peer(
        &self,
        peer_id: &str,
        name: &str,
        addr: SocketAddr,
        display_bounds: Option<DisplayBounds>,
        platform: Option<&str>,
    ) {
        let active_peer_id = self
            .active_target()
            .and_then(|target| self.peers.peer_id_for_addr(target));
        self.peers.clear_conn_addr_for_peer(peer_id);
        self.peers.add_peer(peer_id, name, addr, platform);
        if active_peer_id.as_deref() == Some(peer_id) {
            if let Some(session) = self
                .outbound_input
                .lock()
                .expect("outbound_input lock poisoned")
                .as_mut()
            {
                session.target = addr;
            }
        }
        if let Some(bounds) = display_bounds {
            self.peers.update_peer_display_bounds(peer_id, Some(bounds));
        }
        self.peers.initialize_peer_timeout(addr, self.mono_us());
    }

    pub(crate) fn remove_peer(&self, peer_id: &str) {
        let removing_active_peer = self
            .active_target()
            .and_then(|target| self.peers.peer_id_for_addr(target))
            .is_some_and(|active_peer_id| active_peer_id == peer_id);
        self.peers.remove_peer(peer_id);
        if removing_active_peer {
            self.stop_outbound_input();
        }
    }

    pub(crate) fn peer_addrs(&self) -> Vec<SocketAddr> {
        self.peers.peer_addrs()
    }

    pub(crate) fn peer_id_for_addr(&self, addr: SocketAddr) -> Option<String> {
        self.peers.peer_id_for_addr(addr)
    }

    pub(crate) fn peer_display_bounds(&self, peer_id: &str) -> Option<DisplayBounds> {
        self.peers.peer_display_bounds(peer_id)
    }

    pub(crate) fn connection_for_addr(&self, addr: SocketAddr) -> Option<Arc<PeerConnection>> {
        if let Some(conn) = self.endpoint.get_connection_by_addr(addr)
            && !conn.is_closed()
        {
            return Some(conn);
        }

        let peer_id = self.peer_id_for_addr(addr)?;
        self.endpoint
            .get_connection(&peer_id)
            .filter(|conn| !conn.is_closed())
    }

    pub(crate) fn mono_us(&self) -> u64 {
        self.start.elapsed().as_micros() as u64
    }

    pub(crate) fn mark_input(&self) {
        self.last_input_us.store(self.mono_us(), Ordering::Relaxed);
    }

    pub(crate) fn last_input_us(&self) -> u64 {
        self.last_input_us.load(Ordering::Relaxed)
    }

    pub(crate) fn last_send_us(&self) -> u64 {
        self.last_send_us.load(Ordering::Relaxed)
    }

    pub(crate) fn next_ping_nonce(&self) -> u64 {
        self.seq.fetch_add(1, Ordering::Relaxed) as u64
    }

    pub(crate) fn next_motion_session_id(&self) -> u64 {
        self.motion_session_seq
            .fetch_add(1, Ordering::Relaxed)
            .wrapping_add(1)
    }

    pub(crate) fn mark_packet_received(&self, addr: SocketAddr) {
        self.peers.mark_packet_received(addr, self.mono_us());
    }

    pub(crate) fn is_local_sender(&self, sender_id: u64) -> bool {
        sender_id == self.sender_id
    }

    pub(crate) fn mark_peer_reachable(&self, addr: SocketAddr) -> bool {
        self.peers.mark_peer_reachable(addr)
    }

    /// Register a QUIC connection remote address → peer_id mapping.
    ///
    /// Called when we know which peer a connection address belongs to
    pub(crate) fn register_conn_addr(&self, addr: SocketAddr, peer_id: &str) {
        self.peers.register_conn_addr(addr, peer_id);
    }

    pub(crate) fn peer_id_for_conn_addr(&self, addr: SocketAddr) -> Option<String> {
        self.peers.peer_id_for_addr(addr)
    }

    pub(crate) fn clear_conn_addr_for_peer(&self, peer_id: &str) {
        self.peers.clear_conn_addr_for_peer(peer_id);
    }

    pub(crate) fn remove_conn_addr(&self, addr: SocketAddr) -> Option<String> {
        self.peers.remove_conn_addr(addr)
    }

    fn send_session_event(&self, event: SessionEvent) {
        if let Ok(guard) = self.session_event_tx.lock()
            && let Some(tx) = guard.as_ref()
        {
            let _ = tx.try_send(event);
        }
    }

    pub(crate) fn emit_session_lost(&self, peer_id: &str) {
        self.send_session_event(SessionEvent::SessionLost {
            peer_id: peer_id.to_string(),
        });
    }

    pub(crate) fn emit_session_disconnected(&self, peer_id: &str) {
        self.send_session_event(SessionEvent::SessionDisconnected {
            peer_id: peer_id.to_string(),
        });
    }

    pub(crate) fn emit_session_unpaired(&self, peer_id: &str) {
        self.send_session_event(SessionEvent::SessionUnpaired {
            peer_id: peer_id.to_string(),
        });
    }

    pub(crate) fn emit_session_pairing_pending(&self, peer_id: &str) {
        self.send_session_event(SessionEvent::SessionPairingPending {
            peer_id: peer_id.to_string(),
        });
    }

    pub(crate) fn emit_session_pairing_approved(&self, peer_id: &str) {
        self.send_session_event(SessionEvent::SessionPairingApproved {
            peer_id: peer_id.to_string(),
        });
    }

    /// Emits an `InboundIdentified` session event so the controller can perform
    /// simultaneous-dial tie-breaking after an inbound connection is fully identified.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn emit_inbound_identified(
        &self,
        peer_id: String,
        name: String,
        addr: std::net::SocketAddr,
        conn: Arc<crate::quic::PeerConnection>,
        display_bounds: Option<crate::core::platform::DisplayBounds>,
        display_rects: Vec<crate::core::platform::DisplayBounds>,
        platform: Option<String>,
        is_auto_connect: bool,
        listen_port: Option<u16>,
        certificate: Vec<u8>,
    ) {
        self.send_session_event(SessionEvent::InboundIdentified {
            peer_id,
            name,
            addr,
            conn,
            display_bounds,
            display_rects,
            platform,
            is_auto_connect,
            listen_port,
            certificate,
        });
    }

    /// Mark a packet received from a peer identified by peer_id.
    pub(crate) fn mark_packet_received_for_peer(&self, peer_id: &str) {
        self.peers
            .mark_packet_received_by_peer_id(peer_id, self.mono_us());
    }

    /// Mark a peer as transport-reachable, identified by peer_id.
    pub(crate) fn mark_peer_reachable_for_peer(&self, peer_id: &str) -> bool {
        self.peers.mark_peer_reachable_by_peer_id(peer_id)
    }

    pub(crate) fn mark_peer_timed_out(&self, addr: SocketAddr) -> bool {
        self.peers.mark_peer_timed_out(addr)
    }

    pub(crate) fn is_peer_timed_out(&self, addr: SocketAddr) -> bool {
        self.peers.is_peer_timed_out(addr)
    }

    pub(crate) fn peer_timeout_reference_us(&self, addr: SocketAddr) -> u64 {
        self.peers.peer_timeout_reference_us(addr)
    }

    /// Sets the peer's last_recv timestamp directly. Used by tests to simulate
    /// time passage for timeout detection.
    pub(crate) fn set_peer_last_recv(&self, addr: SocketAddr, last_recv_us: u64) {
        self.peers.mark_packet_received(addr, last_recv_us);
    }

    pub(crate) fn mark_outgoing_pending(&self, peer_id: &str) {
        self.peers.mark_outgoing_pending(peer_id, self.mono_us());
    }

    pub(crate) fn mark_connected(&self, peer_id: &str) {
        self.peers.mark_connected(peer_id, self.mono_us());
    }

    pub(crate) fn mark_disconnected(&self, peer_id: &str, peer_name: &str, addr: SocketAddr) {
        self.peers
            .mark_disconnected(peer_id, peer_name, addr, self.mono_us());
        let disconnected_active_peer = self
            .active_target()
            .is_some_and(|target| {
                target == addr || self.peer_id_for_addr(target).as_deref() == Some(peer_id)
            });
        if disconnected_active_peer {
            self.stop_outbound_input();
        }
    }

    pub(crate) fn get_peer_runtime_state(&self) -> Vec<PeerRuntimeStateSnapshot> {
        self.peers.get_peer_runtime_state()
    }

    pub(crate) fn get_throughput_metrics(&self) -> ThroughputMetrics {
        self.metrics.get_throughput_metrics(self.mono_us())
    }

    pub(crate) fn record_outbound_input_events(&self, count: u64) {
        self.metrics
            .record_outbound_input_events(self.mono_us(), count);
    }

    pub(crate) fn record_inbound_input_event(&self) {
        self.metrics.record_inbound_input_event(self.mono_us());
    }

    pub(crate) fn record_smoother_tick(&self, injected: bool) {
        self.metrics.record_smoother_tick(injected);
    }

    pub(crate) fn record_cadence_miss(&self) {
        self.metrics.record_cadence_miss();
    }

    pub(crate) fn send_packet_on_connection(
        &self,
        conn: &Arc<PeerConnection>,
        events: Vec<Event>,
    ) -> Result<()> {
        log_outbound_events(Some(conn.addr()), &events);

        let outbound_input_count =
            events.iter().filter(|event| is_input_event(event)).count() as u64;
        if outbound_input_count > 0 {
            self.record_outbound_input_events(outbound_input_count);
        }

        let packet = self.build_packet(events);
        let encoded = self.serialize_packet(&packet)?;
        let has_reliable = packet.events.iter().any(|event| {
            matches!(
                classify(event),
                EventClass::InputReliable | EventClass::ReliableControl
            )
        });
        let has_non_idempotent = packet.events.iter().any(|event| {
            matches!(
                classify(event),
                EventClass::InputReliable | EventClass::ReliableControl
            ) && !is_idempotent_control_event(event)
        });

        if has_non_idempotent {
            self.queue_control(conn, encoded)?;
        } else if has_reliable {
            if encoded.len() <= super::super::quic::max_datagram_size() {
                let _ = conn.send_datagram(encoded.clone());
            }
            self.queue_control(conn, encoded)?;
        } else if encoded.len() > super::super::quic::max_datagram_size() {
            self.send_split_datagrams(conn, encoded)?;
        } else {
            conn.send_datagram(encoded)?;
        }

        self.last_send_us.store(packet.t_mono_us, Ordering::Relaxed);
        Ok(())
    }

    pub(crate) fn send_packet_on_connection_blocking(
        &self,
        conn: &Arc<PeerConnection>,
        events: Vec<Event>,
    ) -> Result<()> {
        log_outbound_events(Some(conn.addr()), &events);

        let packet = self.build_packet(events);
        let encoded = self.serialize_packet(&packet)?;
        if encoded.len() <= super::super::quic::max_datagram_size() {
            let _ = conn.send_datagram(encoded.clone());
        }
        conn.send_control_blocking(encoded)
            .context("failed to send control message")?;
        self.last_send_us.store(packet.t_mono_us, Ordering::Relaxed);
        Ok(())
    }

    pub(crate) fn send_packet_to(&self, addr: SocketAddr, events: Vec<Event>) -> Result<()> {
        self.send_packet_to_with_seq(addr, events).map(|_| ())
    }

    pub(crate) fn send_packet_to_with_seq(
        &self,
        addr: SocketAddr,
        events: Vec<Event>,
    ) -> Result<u32> {
        log_outbound_events(Some(addr), &events);

        let outbound_input_count =
            events.iter().filter(|event| is_input_event(event)).count() as u64;
        if outbound_input_count > 0 {
            self.record_outbound_input_events(outbound_input_count);
        }

        let packet = self.build_packet(events);
        let encoded = self.serialize_packet(&packet)?;

        let has_reliable = packet.events.iter().any(|event| {
            matches!(
                classify(event),
                EventClass::InputReliable | EventClass::ReliableControl
            )
        });
        let has_non_idempotent = packet.events.iter().any(|event| {
            matches!(
                classify(event),
                EventClass::InputReliable | EventClass::ReliableControl
            ) && !is_idempotent_control_event(event)
        });
        let connection = self.connection_for_addr(addr);

        if has_non_idempotent {
            // Key/button events: stream-only to prevent double-processing.
            // These are NOT idempotent - receiving twice would cause double-typing.
            let connection =
                connection.ok_or_else(|| anyhow::anyhow!("no QUIC connection to {}", addr))?;
            self.queue_control(&connection, encoded)?;
        } else if has_reliable {
            // Idempotent control events: dual-path for low latency + reliability.
            // Datagram arrives first, stream provides guaranteed backup.
            let connection =
                connection.ok_or_else(|| anyhow::anyhow!("no QUIC connection to {}", addr))?;
            if encoded.len() <= super::super::quic::max_datagram_size() {
                let _ = connection.send_datagram(encoded.clone());
            }
            self.queue_control(&connection, encoded)?;
        } else {
            let conn =
                connection.ok_or_else(|| anyhow::anyhow!("no QUIC connection to {}", addr))?;

            if encoded.len() > super::super::quic::max_datagram_size() {
                self.send_split_datagrams(&conn, encoded)?;
            } else {
                conn.send_datagram(encoded)?;
            }
        }

        self.last_send_us.store(packet.t_mono_us, Ordering::Relaxed);
        Ok(packet.seq)
    }

    fn queue_control(&self, conn: &Arc<PeerConnection>, data: Vec<u8>) -> Result<()> {
        self.control_tx
            .send(QueuedControlMessage {
                connection: Arc::clone(conn),
                data,
            })
            .context("control channel disconnected")
    }

    fn build_packet(&self, events: Vec<Event>) -> Packet {
        let seq = self.seq.fetch_add(1, Ordering::Relaxed);
        Packet {
            version: PROTOCOL_VERSION,
            sender_id: self.sender_id,
            seq,
            t_mono_us: self.mono_us(),
            events,
        }
    }

    fn serialize_packet(&self, packet: &Packet) -> Result<Vec<u8>> {
        bincode::serialize(packet).context("failed to serialize packet")
    }

    fn send_split_datagrams(&self, conn: &Arc<PeerConnection>, data: Vec<u8>) -> Result<()> {
        let fragment_id = self.seq.fetch_add(1, Ordering::Relaxed) as u16;
        let datagrams = split_into_datagrams(
            &data,
            fragment_id,
            super::super::quic::max_datagram_size(),
        )?;
        for datagram in datagrams {
            conn.send_datagram(datagram)?;
        }

        Ok(())
    }

    pub(crate) fn broadcast_packet(&self, events: Vec<Event>) -> Result<()> {
        let peer_addrs = self.peers.peer_addrs();
        let mut delivered = 0usize;
        let mut failures = Vec::new();

        for addr in peer_addrs {
            match self.send_packet_to(addr, events.clone()) {
                Ok(()) => {
                    delivered += 1;
                }
                Err(error) => {
                    failures.push((addr, error.to_string()));
                }
            }
        }

        if delivered == 0 && !failures.is_empty() {
            let failure_summary = failures
                .into_iter()
                .map(|(addr, error)| format!("{addr}: {error}"))
                .collect::<Vec<_>>()
                .join("; ");
            bail!("broadcast failed for all peers: {failure_summary}");
        }

        if !failures.is_empty() {
            warn!(
                delivered,
                failed = failures.len(),
                "broadcast_packet delivered to a subset of peers"
            );
        }
        Ok(())
    }

    /// Queues an input event for batched transmission.
    ///
    /// Events are accumulated until either:
    /// - The batch window elapses since the first event
    /// - The maximum events per packet is reached
    ///
    /// When a trigger fires, events are immediately flushed to the active target.
    pub(crate) fn queue_input_event(&self, event: Event, capture_us: u64) -> Result<()> {
        let to_send = self.batching.queue_event(event, capture_us);
        if let Some((events, coalesced)) = to_send {
            if coalesced > 0 {
                self.metrics.record_moves_coalesced(coalesced);
            }
            self.flush_input_events(events)?;
        }

        Ok(())
    }

    /// Flushes pending input events if the batch window has elapsed.
    ///
    /// Called periodically to ensure events don't wait indefinitely
    /// when input rate is low.
    pub(crate) fn flush_input_batch_if_due(&self) -> Result<()> {
        let to_send = self.batching.flush_if_due(self.mono_us());
        if let Some((events, coalesced)) = to_send {
            if coalesced > 0 {
                self.metrics.record_moves_coalesced(coalesced);
            }
            self.flush_input_events(events)?;
        }

        Ok(())
    }

    pub(crate) fn flush_pending_input_now(&self) -> Result<()> {
        let to_send = self.batching.flush_now();
        if let Some((events, coalesced)) = to_send {
            if coalesced > 0 {
                self.metrics.record_moves_coalesced(coalesced);
            }
            self.flush_input_events(events)?;
        }

        Ok(())
    }

    fn flush_input_events(&self, events: Vec<Event>) -> Result<()> {
        if events.is_empty() {
            return Ok(());
        }

        let target = self
            .active_target()
            .context("cannot flush input without an outbound input session")?;

        let mut start = 0usize;
        while start < events.len() {
            let end = (start + self.batching.max_events_per_packet()).min(events.len());
            self.send_packet_to(target, events[start..end].to_vec())?;
            start = end;
        }

        Ok(())
    }
}

impl NetworkStateTrait for NetworkState {
    fn broadcast_packet(&self, events: Vec<Event>) -> anyhow::Result<()> {
        self.broadcast_packet(events)
    }

    fn active_target(&self) -> Option<SocketAddr> {
        self.active_target()
    }

    fn has_any_peer(&self) -> bool {
        !self.peers.get_peer_runtime_state().is_empty()
    }
}

pub(crate) fn input_activity_state(mode: TransferMode, _now: u64, _last_input: u64) -> PeerState {
    if mode.is_remote_control() {
        PeerState::RemoteControl
    } else {
        PeerState::LocalControl
    }
}

fn log_outbound_events(addr: Option<SocketAddr>, events: &[Event]) {
    for event in events {
        match event {
            Event::TakeControl {
                edge,
                receiver_position_bp,
                motion_session_id,
                ..
            } => {
                info!(
                    domain = "runtime.transition",
                    transition_key = "handoff_request_sent",
                    handoff_id = *motion_session_id,
                    peer = ?addr,
                    edge = ?edge,
                    receiver_position_bp = ?receiver_position_bp,
                    "sending TakeControl"
                );
            }
            Event::ReleaseControl => {
                info!(
                    domain = "runtime.transition",
                    peer = ?addr,
                    "sending ReleaseControl"
                );
            }
            Event::TakeControlAck { .. } => {}
            Event::ConnectionRequest {
                peer_id, peer_name, ..
            } => {
                info!(
                    domain = "runtime.connection",
                    peer = ?addr,
                    peer_id,
                    peer_name,
                    "sending ConnectionRequest"
                );
            }
            Event::ConnectionAccept {
                peer_id, peer_name, ..
            } => {
                info!(
                    domain = "runtime.connection",
                    peer = ?addr,
                    peer_id,
                    peer_name,
                    "sending ConnectionAccept"
                );
            }
            Event::ConnectionReject {
                peer_id,
                peer_name,
                reason,
            } => {
                info!(
                    domain = "runtime.connection",
                    peer = ?addr,
                    peer_id,
                    peer_name,
                    reason,
                    "sending ConnectionReject"
                );
            }
            Event::DisconnectNotify { peer_id, peer_name } => {
                info!(
                    domain = "runtime.connection",
                    peer = ?addr,
                    peer_id,
                    peer_name,
                    "sending DisconnectNotify"
                );
            }
            Event::UnpairNotify { peer_id, peer_name } => {
                info!(
                    domain = "runtime.connection",
                    peer = ?addr,
                    peer_id,
                    peer_name,
                    "sending UnpairNotify"
                );
            }
            Event::PairingPending { peer_id } => {
                info!(
                    domain = "runtime.connection",
                    peer = ?addr,
                    peer_id,
                    "sending PairingPending"
                );
            }
            Event::PairingApproved { peer_id } => {
                info!(
                    domain = "runtime.connection",
                    peer = ?addr,
                    peer_id,
                    "sending PairingApproved"
                );
            }
            Event::Heartbeat { .. }
            | Event::Ping { .. }
            | Event::Pong { .. }
            | Event::LayoutUpdate(_)
            | Event::ClipboardText { .. }
            | Event::ClipboardAck { .. }
            | Event::ClipboardSyncRequest { .. }
            | Event::FileOffer { .. }
            | Event::FileAccept { .. }
            | Event::FileReject { .. }
            | Event::FileChunk { .. }
            | Event::FileAck { .. }
            | Event::FileCancel { .. }
            | Event::MouseMove { .. }
            | Event::MouseButton { .. }
            | Event::MouseWheel { .. }
            | Event::KeyPress { .. }
            | Event::KeyRelease { .. } => {}
        }
    }
}

/// Splits an oversized packet into datagrams that each fit inside the QUIC MTU.
///
/// Every fragment carries a [`FragmentHeader`] prefix, so the header must be
/// counted against the datagram budget. Sizing payload chunks at the full MTU
/// would produce datagrams that [`PeerConnection::send_datagram`] rejects
/// outright, making the whole packet unsendable.
fn split_into_datagrams(
    data: &[u8],
    fragment_id: u16,
    max_datagram_size: usize,
) -> Result<Vec<Vec<u8>>> {
    let header_size = bincode::serialized_size(&FragmentHeader {
        fragment_id: 0,
        fragment_index: 0,
        total_fragments: 0,
    })
    .context("failed to size fragment header")? as usize;

    let payload_capacity = max_datagram_size
        .checked_sub(header_size)
        .filter(|capacity| *capacity > 0)
        .context("datagram MTU is smaller than a fragment header")?;

    let total_fragments = u16::try_from(data.len().div_ceil(payload_capacity))
        .context("packet needs more fragments than the protocol can address")?;

    let mut datagrams = Vec::with_capacity(usize::from(total_fragments));
    for (index, chunk) in data.chunks(payload_capacity).enumerate() {
        let header = FragmentHeader {
            fragment_id,
            fragment_index: index as u16,
            total_fragments,
        };
        let mut datagram =
            bincode::serialize(&header).context("failed to serialize fragment header")?;
        datagram.extend_from_slice(chunk);
        datagrams.push(datagram);
    }

    Ok(datagrams)
}

/// Header for fragmented datagrams when packets exceed MTU.
#[derive(serde::Serialize, serde::Deserialize)]
pub(crate) struct FragmentHeader {
    pub(crate) fragment_id: u16,
    pub(crate) fragment_index: u16,
    pub(crate) total_fragments: u16,
}

#[cfg(test)]
mod tests {
    use super::{
        HEARTBEAT_INTERVAL, NetworkState, OutboundInputSession, RuntimeConfig,
        input_activity_state, is_input_event,
    };
    use crate::core::platform::DisplayBounds;
    use crate::events::{Event, PeerState};
    use crate::platform::shared::quic::{CertificateStore, QuicEndpoint};
    use crate::platform::shared::test_support::{unique_test_cert_path, with_tokio_runtime};
    use crate::platform::{PeerEdge, TransferMode};
    use std::sync::Arc;

    #[test]
    fn is_input_event_includes_keyboard_variants() {
        assert!(is_input_event(&Event::MouseMove {
            dx: 0,
            dy: 0,
            total_dx: 0,
            total_dy: 0,
            motion_session_id: 1,
        }));
        assert!(is_input_event(&Event::KeyPress { keycode: 0x04 }));
        assert!(is_input_event(&Event::KeyRelease { keycode: 0x04 }));
        assert!(!is_input_event(&Event::Heartbeat {
            state: PeerState::LocalControl,
        }));
    }

    #[test]
    fn input_activity_state_reflects_recent_input() {
        assert!(matches!(
            input_activity_state(TransferMode::LocalControl, 1_000_000, 1_000_000),
            PeerState::LocalControl
        ));
        assert!(matches!(
            input_activity_state(
                TransferMode::LocalControl,
                1_000_000 + HEARTBEAT_INTERVAL.as_micros() as u64,
                1_000_000
            ),
            PeerState::LocalControl
        ));
        assert!(matches!(
            input_activity_state(TransferMode::RemoteControl, 0, 0),
            PeerState::RemoteControl
        ));
        assert!(matches!(
            input_activity_state(TransferMode::LocalControl, 0, 0),
            PeerState::LocalControl
        ));
    }

    #[test]
    fn outbound_input_session_stays_consistent() {
        with_tokio_runtime(|| {
            let cert_path = unique_test_cert_path("mouse-thing-outbound-input-test-certs");
            let cert_store = CertificateStore::load_or_create(&cert_path).unwrap();
            let endpoint = Arc::new(QuicEndpoint::new(0, "local-peer", cert_store).unwrap());
            let config = RuntimeConfig {
                endpoint: Arc::clone(&endpoint),
                batch_window_us: 0,
                max_events_per_packet: 32,
                movement_cadence_us: 0,
                clipboard_poll_interval_ms: 50,
                clipboard_ack_timeout_ms: 200,
            };
            let (network, _) = NetworkState::new(&config, 1).unwrap();
            let first_addr = "127.0.0.1:54321".parse().unwrap();
            let updated_addr = "127.0.0.1:54322".parse().unwrap();

            network.add_peer("peer", "Peer", first_addr, None, None);
            network.start_outbound_input(first_addr, 42);
            assert_eq!(
                network.outbound_input_session(),
                Some(OutboundInputSession {
                    target: first_addr,
                    motion_session_id: 42,
                })
            );

            network.add_peer("peer", "Peer", updated_addr, None, None);
            assert_eq!(
                network.outbound_input_session(),
                Some(OutboundInputSession {
                    target: updated_addr,
                    motion_session_id: 42,
                })
            );

            network.remove_peer("peer");
            assert_eq!(network.outbound_input_session(), None);
            let error = network
                .queue_input_event(Event::KeyPress { keycode: 0x04 }, 1)
                .expect_err("input without a session must fail");
            assert!(error.to_string().contains("without an outbound input session"));
            network.shutdown_endpoint();
        });
    }

    #[test]
    fn needs_reliable_transport_includes_key_and_button_events() {
        use super::super::transport::needs_reliable_transport;

        assert!(needs_reliable_transport(&Event::KeyPress { keycode: 0x04 }));
        assert!(needs_reliable_transport(&Event::KeyRelease {
            keycode: 0x04
        }));
        assert!(needs_reliable_transport(&Event::MouseButton {
            button: 0,
            pressed: true
        }));
        assert!(needs_reliable_transport(&Event::MouseButton {
            button: 0,
            pressed: false
        }));

        assert!(!needs_reliable_transport(&Event::MouseMove {
            dx: 0,
            dy: 0,
            total_dx: 0,
            total_dy: 0,
            motion_session_id: 1,
        }));
        assert!(!needs_reliable_transport(&Event::MouseWheel {
            delta_y: 1,
            delta_x: 0,
        }));
    }

    #[test]
    fn key_and_button_events_are_not_idempotent() {
        use super::super::transport::{is_idempotent_control_event, needs_reliable_transport};

        // Key/button events need reliable transport but are NOT idempotent
        // (receiving twice would cause double-typing or double-click)
        let key_press = Event::KeyPress { keycode: 0x04 };
        let key_release = Event::KeyRelease { keycode: 0x04 };
        let mouse_down = Event::MouseButton {
            button: 0,
            pressed: true,
        };
        let mouse_up = Event::MouseButton {
            button: 0,
            pressed: false,
        };

        assert!(needs_reliable_transport(&key_press));
        assert!(needs_reliable_transport(&key_release));
        assert!(needs_reliable_transport(&mouse_down));
        assert!(needs_reliable_transport(&mouse_up));

        assert!(!is_idempotent_control_event(&key_press));
        assert!(!is_idempotent_control_event(&key_release));
        assert!(!is_idempotent_control_event(&mouse_down));
        assert!(!is_idempotent_control_event(&mouse_up));

        // ReleaseControl is idempotent (safe to receive multiple times)
        assert!(is_idempotent_control_event(&Event::ReleaseControl));
    }

    #[test]
    fn add_peer_preserves_display_bounds_in_runtime_snapshots() {
        with_tokio_runtime(|| {
            let cert_path = unique_test_cert_path("mouse-thing-network-test-certs");
            let cert_store = CertificateStore::load_or_create(&cert_path).unwrap();
            let endpoint = Arc::new(QuicEndpoint::new(0, "local-peer", cert_store).unwrap());
            let config = RuntimeConfig {
                endpoint: Arc::clone(&endpoint),
                batch_window_us: 0,
                max_events_per_packet: 32,
                movement_cadence_us: 0,
                clipboard_poll_interval_ms: 50,
                clipboard_ack_timeout_ms: 200,
            };
            let (network, _) = NetworkState::new(&config, 1).unwrap();
            let bounds = DisplayBounds {
                x: -1920,
                y: 0,
                width: 2560,
                height: 1440,
            };

            network.add_peer(
                "outbound-id",
                "outbound",
                "127.0.0.1:54321".parse().unwrap(),
                Some(bounds),
                None,
            );

            let runtime = network.get_peer_runtime_state();
            assert_eq!(runtime.len(), 1);
            assert_eq!(runtime[0].virtual_bounds, Some(bounds));
            network.shutdown_endpoint();
        });
    }

    #[test]
    fn split_datagrams_respect_the_mtu_and_reassemble() {
        use super::super::fragment::reassemble_fragment;
        use super::split_into_datagrams;
        use crate::platform::shared::quic::max_datagram_size;
        use std::collections::HashMap;

        let mtu = max_datagram_size();
        let data: Vec<u8> = (0..(mtu * 3 + 17)).map(|i| i as u8).collect();
        let datagrams =
            split_into_datagrams(&data, 9, mtu).expect("splitting a large packet should succeed");

        assert!(datagrams.len() > 1, "payload should require fragmentation");
        for datagram in &datagrams {
            assert!(
                datagram.len() <= mtu,
                "fragment of {} bytes exceeds the {} byte MTU",
                datagram.len(),
                mtu
            );
        }

        let mut reassembly = HashMap::new();
        let mut complete = None;
        for datagram in datagrams {
            complete = reassemble_fragment(&mut reassembly, datagram);
        }
        assert_eq!(
            complete.expect("fragments should reassemble into the original packet"),
            data
        );
    }

    #[test]
    fn take_control_requires_an_established_connection() {
        with_tokio_runtime(|| {
            let cert_path = unique_test_cert_path("mouse-thing-take-control-test-certs");
            let cert_store = CertificateStore::load_or_create(&cert_path).unwrap();
            let endpoint = Arc::new(QuicEndpoint::new(0, "local-peer", cert_store).unwrap());
            let config = RuntimeConfig {
                endpoint: Arc::clone(&endpoint),
                batch_window_us: 0,
                max_events_per_packet: 32,
                movement_cadence_us: 0,
                clipboard_poll_interval_ms: 50,
                clipboard_ack_timeout_ms: 200,
            };
            let (network, _) = NetworkState::new(&config, 1).unwrap();
            let addr = "127.0.0.1:54321".parse().unwrap();
            network.add_peer("peer-b", "Peer B", addr, None, None);

            let result = network.send_packet_to_with_seq(
                addr,
                vec![Event::TakeControl {
                    edge: PeerEdge::Right,
                    initial_x: 0,
                    initial_y: 0,
                    receiver_position_bp: Some(5_000),
                    motion_session_id: 1,
                    return_motion: None,
                    caps_lock: None,
                }],
            );

            assert!(result.is_err());
            network.shutdown_endpoint();
        });
    }
}
