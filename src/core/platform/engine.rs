//! Runtime facade trait used to compose platform subsystems.
//!
//! # Threading Model
//!
//! All `PlatformEngine` implementations must be `Send + Sync`.
//!
//! **Concurrent reads (safe from any thread):**
//! - `get_display_manager()`, `get_permission_checker()`:
//!   Return shared references to subsystems. Subsystems must support concurrent access.
//!
//! **Mutating methods (must be called sequentially or externally synchronized):**
//! - `PlatformEngine::start()`, `PlatformEngine::stop()`, `SessionEngine::configure_runtime()`, `SessionEngine::set_local_identity()`, `SessionEngine::add_peer()`, `SessionEngine::remove_peer()`, `SessionEngine::reconcile_*()`:
//!   Implementations use internal `Mutex`/`RwLock` to protect mutable state.

use anyhow::Result;
use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::sync::mpsc;

use crate::core::api::{ConfigSnapshot, PeerRefSnapshot};
use crate::core::events::{Event, LayoutUpdateEvent};
use crate::core::layout::{EdgeSpanBp, bootstrap_edge_for_placement};
use crate::core::peer_state::PeerRuntimeStateSnapshot;
use crate::core::platform::traits::{
    DisplayManager, EventCapture, EventInjection, PermissionChecker, ThroughputMetrics,
};
use crate::core::platform::types::{CapturedInputEvent, PeerEdge, TransferMode};
use crate::quic::{PeerConnection, QuicEndpoint};

/// Canonical peer-to-edge assignments derived from config.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EdgeAssignments {
    by_peer: BTreeMap<String, PeerEdge>,
}

impl EdgeAssignments {
    /// Builds edge assignments from the persisted config snapshot.
    pub fn from_config(config: &ConfigSnapshot) -> Self {
        let mut by_peer = BTreeMap::new();

        for (peer_id, edge) in [
            (normalized_peer_id(config.left.as_deref()), PeerEdge::Left),
            (normalized_peer_id(config.right.as_deref()), PeerEdge::Right),
            (normalized_peer_id(config.top.as_deref()), PeerEdge::Top),
            (
                normalized_peer_id(config.bottom.as_deref()),
                PeerEdge::Bottom,
            ),
        ] {
            if let Some(peer_id) = peer_id {
                by_peer.insert(peer_id.to_string(), edge);
            }
        }

        for placement in &config.placements {
            let peer_id = placement.peer_id.trim();
            if peer_id.is_empty() || by_peer.contains_key(peer_id) {
                continue;
            }
            by_peer.insert(
                peer_id.to_string(),
                bootstrap_edge_for_placement(placement, None),
            );
        }

        Self { by_peer }
    }

    /// Builds assignments from explicit `(peer_id, edge)` pairs.
    pub fn from_pairs<I>(pairs: I) -> Self
    where
        I: IntoIterator<Item = (String, PeerEdge)>,
    {
        let mut by_peer = BTreeMap::new();
        for (peer_id, edge) in pairs {
            let peer_id = peer_id.trim();
            if peer_id.is_empty() {
                continue;
            }
            by_peer.insert(peer_id.to_string(), edge);
        }

        Self { by_peer }
    }

    /// Returns the configured edge for a peer, if any.
    pub fn edge_for_peer(&self, peer_id: &str) -> Option<PeerEdge> {
        self.by_peer.get(peer_id).copied()
    }

    /// Iterates through configured edge assignments.
    pub fn iter(&self) -> impl Iterator<Item = (&str, PeerEdge)> + '_ {
        self.by_peer
            .iter()
            .map(|(peer_id, edge)| (peer_id.as_str(), *edge))
    }
}

fn normalized_peer_id(value: Option<&str>) -> Option<&str> {
    let trimmed = value?.trim();
    (!trimmed.is_empty()).then_some(trimmed)
}

/// Runtime-originated session lifecycle event forwarded to the controller.
#[derive(Debug, Clone)]
pub enum SessionEvent {
    SessionLost {
        peer_id: String,
    },
    SessionDisconnected {
        peer_id: String,
    },
    SessionUnpaired {
        peer_id: String,
    },
    SessionPairingPending {
        peer_id: String,
    },
    SessionPairingApproved {
        peer_id: String,
    },
    /// An inbound connection was fully identified by the stream hello handshake.
    /// The controller uses this for simultaneous-dial tie-breaking.
    InboundIdentified {
        peer_id: String,
        name: String,
        addr: SocketAddr,
        conn: Arc<PeerConnection>,
        display_bounds: Option<crate::core::platform::types::DisplayBounds>,
        display_rects: Vec<crate::core::platform::types::DisplayBounds>,
        platform: Option<String>,
        is_auto_connect: bool,
        listen_port: Option<u16>,
        /// Certificate presented by the remote during the QUIC/TLS handshake.
        certificate: Vec<u8>,
    },
}

/// Platform runtime facade used by higher layers.
///
/// `PlatformEngine` is intentionally narrow: only capture/display/permission
/// lifecycle. Session/transport concerns live on `SessionEngine`; most
/// runtime code needs `RuntimeEngine` (both).
pub trait PlatformEngine: Send + Sync {
    /// Starts platform runtime resources.
    fn start(&self) -> Result<mpsc::Receiver<CapturedInputEvent>>;

    /// Stops platform runtime resources.
    fn stop(&self) -> Result<()>;

    /// Returns display subsystem.
    fn get_display_manager(&self) -> &dyn DisplayManager;

    /// Returns permission subsystem.
    fn get_permission_checker(&self) -> &dyn PermissionChecker;
}

/// Session/transport facade. Together with `PlatformEngine` this forms the
/// full runtime. New code that only needs display/capture should bound on
/// `PlatformEngine`; code that manages peers should bound on `SessionEngine`
/// or `RuntimeEngine`.
pub trait SessionEngine: Send + Sync {
    fn start(&self, event_rx: mpsc::Receiver<CapturedInputEvent>) -> Result<()>;

    /// Sets the local peer identity used in connection management messages.
    fn set_local_identity(&self, peer: PeerRefSnapshot) -> Result<()>;

    /// Applies runtime network and batching configuration before start.
    #[allow(clippy::too_many_arguments)]
    fn configure_runtime(
        &self,
        endpoint: Arc<QuicEndpoint>,
        batch_window_us: u64,
        max_events_per_packet: usize,
        movement_cadence_us: u64,
        peer_id: String,
        clipboard_poll_interval_ms: u64,
        clipboard_ack_timeout_ms: u64,
    ) -> Result<()>;

    /// Emergency unlock: immediately restores local control, unhides/unlocks cursor,
    /// releases held keys, and clears active remote state.
    fn emergency_unlock(&self) -> Result<()> {
        Ok(())
    }

    /// Stops session network resources (used for startup-guard cleanup).
    fn shutdown(&self) -> Result<()> {
        Ok(())
    }

    /// Adds a new peer connection.
    fn add_peer(
        &self,
        _peer: PeerRefSnapshot,
        _addr: SocketAddr,
        _edge: crate::core::platform::types::PeerEdge,
        _display_bounds: Option<crate::core::platform::types::DisplayBounds>,
        _is_inbound: bool,
    ) -> Result<()> {
        Ok(())
    }

    fn mark_connection_established(&self, _peer_id: &str) {}

    /// Removes a peer connection.
    fn remove_peer(&self, _peer_id: &str) -> Result<()> {
        Ok(())
    }

    /// Unpairs and removes a peer connection, notifying the remote peer.
    fn unpair_peer(&self, peer_id: &str) -> Result<()> {
        self.remove_peer(peer_id)
    }

    fn send_pairing_pending(&self, _addr: SocketAddr, _peer_id: &str) -> Result<()> {
        Ok(())
    }

    fn send_pairing_approved(&self, _addr: SocketAddr, _peer_id: &str) -> Result<()> {
        Ok(())
    }

    /// Notifies the remote at `addr` that our outbound pairing attempt ended
    /// (cancelled) so it can drop its inbound waiter. Best effort.
    fn notify_pairing_cancelled(&self, _addr: SocketAddr) -> Result<()> {
        Ok(())
    }

    /// Sends a layout update event to the connected peer.
    fn send_layout_update(&self, _update: LayoutUpdateEvent) -> Result<()> {
        Ok(())
    }

    /// Takes the receiver used for remote layout updates.
    fn take_layout_update_rx(&self) -> Result<Option<mpsc::Receiver<LayoutUpdateEvent>>> {
        Ok(None)
    }

    fn transfer_sync(&self) -> Option<Arc<crate::core::transfer::TransferSync>> {
        None
    }

    /// Takes the receiver used for runtime session lifecycle events.
    fn take_session_event_rx(&self) -> Result<Option<crossbeam_channel::Receiver<SessionEvent>>> {
        Ok(None)
    }

    /// Reconciles live peer-edge assignments with the latest saved config.
    fn reconcile_peer_edges(&self, _assignments: &EdgeAssignments) -> Result<()> {
        Ok(())
    }

    /// Reconciles runtime contact layout derived from the latest saved config.
    fn reconcile_layout(&self, _config: &ConfigSnapshot) -> Result<()> {
        Ok(())
    }

    /// Returns current runtime throughput metrics.
    fn get_throughput_metrics(&self) -> ThroughputMetrics {
        ThroughputMetrics::default()
    }

    /// Returns runtime state snapshots keyed by peer ID.
    fn get_peer_runtime_state(&self) -> Vec<PeerRuntimeStateSnapshot> {
        Vec::new()
    }

    /// Closes a duplicate connection to a peer gracefully during simultaneous-dial
    /// tie-breaking. Removes the connection from all registries so the timeout
    /// checker never sees it.
    fn close_duplicate_connection(&self, _conn: Arc<PeerConnection>, _reason: &[u8]) {}

    /// Promotes an identified connection to the canonical connection for the peer.
    fn promote_connection(&self, _peer_id: &str, _conn: Arc<PeerConnection>) -> Result<()> {
        Ok(())
    }
}

pub trait RuntimeEngine: PlatformEngine + SessionEngine {}

impl<T: PlatformEngine + SessionEngine> RuntimeEngine for T {}

#[cfg(test)]
mod tests {
    use super::{EdgeAssignments, PeerEdge};
    use crate::core::api::ConfigSnapshot;
    use crate::core::events::LayoutPlacement;

    #[test]
    fn edge_assignments_from_config_ignores_blank_values() {
        let config = ConfigSnapshot {
            left: Some(" peer-left ".to_string()),
            right: Some("".to_string()),
            top: Some("peer-top".to_string()),
            bottom: Some("   ".to_string()),
            ..Default::default()
        };

        let assignments = EdgeAssignments::from_config(&config);

        assert_eq!(assignments.edge_for_peer("peer-left"), Some(PeerEdge::Left));
        assert_eq!(assignments.edge_for_peer("peer-top"), Some(PeerEdge::Top));
        assert_eq!(assignments.edge_for_peer(""), None);
    }

    #[test]
    fn edge_assignments_fall_back_to_layout_placements() {
        let config = ConfigSnapshot {
            placements: vec![
                LayoutPlacement {
                    peer_id: "peer-right".to_string(),
                    peer_label: String::new(),
                    x: 166,
                    y: 20,
                },
                LayoutPlacement {
                    peer_id: "peer-top".to_string(),
                    peer_label: String::new(),
                    x: 0,
                    y: -106,
                },
            ],
            ..Default::default()
        };

        let assignments = EdgeAssignments::from_config(&config);

        assert_eq!(
            assignments.edge_for_peer("peer-right"),
            Some(PeerEdge::Right)
        );
        assert_eq!(assignments.edge_for_peer("peer-top"), Some(PeerEdge::Top));
    }
}

/// Test-only trait for operations needed by integration tests.
///
/// Separated from the main `PlatformEngine` trait so production code
/// never depends on these methods.
/// Test-only extension trait. Methods have no effect in production builds.
pub trait PlatformEngineTestExt {
    /// Returns capture subsystem.
    fn get_capture(&self) -> &dyn EventCapture {
        unimplemented!("capture not supported in this test engine")
    }

    /// Returns injection subsystem.
    fn get_injection(&self) -> &dyn EventInjection {
        unimplemented!("injection not supported in this test engine")
    }

    /// Returns current transfer mode.
    fn get_transfer_mode(&self) -> TransferMode {
        TransferMode::LocalControl
    }

    /// Updates transfer mode.
    fn set_transfer_mode(&self, _mode: TransferMode) -> Result<()> {
        Ok(())
    }

    /// Processes one network event received from peer.
    fn process_network_event(&self, _event: Event) -> Result<()> {
        Ok(())
    }

    /// Sends locally produced events to peer.
    fn send_events(&self, _events: Vec<Event>) -> Result<()> {
        Ok(())
    }

    /// Returns the current runtime-resolved contacts.
    fn get_runtime_contacts(&self) -> Vec<RuntimeContactSnapshot> {
        Vec::new()
    }

    /// Forces the runtime transition state to refresh its cached display topology.
    fn refresh_display_topology(&self) -> Result<()> {
        Ok(())
    }

    /// Closes the QUIC connection to a specific peer (simulates network partition).
    fn close_connection_to(&self, _peer_id: &str) {}

    /// Sets a peer's last_recv timestamp to (now - elapsed_us) for testing timeouts.
    fn advance_peer_last_recv(&self, _addr: std::net::SocketAddr, _elapsed_us: u64) {}

    /// Marks a peer as connected for test harness setup.
    fn mark_peer_connected(&self, _peer_id: &str) {}

    /// Returns the number of active (non-closed) connections to a peer.
    fn get_connection_count(&self, _peer_id: &str) -> usize {
        0
    }
}

pub trait TestPlatformEngine: RuntimeEngine + PlatformEngineTestExt {}

impl<T: RuntimeEngine + PlatformEngineTestExt> TestPlatformEngine for T {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeContactSnapshot {
    pub peer_id: String,
    pub addr: SocketAddr,
    pub local: EdgeSpanBp,
    pub remote: EdgeSpanBp,
}
