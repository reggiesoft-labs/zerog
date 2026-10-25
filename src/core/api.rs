//! Core API surface consumed by CLI and GUI frontends.
//!
//! This module contains the essential types for the mouse-sharing application.

use crate::core::config;
use serde::{Deserialize, Serialize};

/// Peer identifier type.
pub type PeerId = String;

/// Runtime status returned to UI callers.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct StatusSnapshot {
    /// True when the platform engine is currently running.
    pub engine_running: bool,
    /// Connected peers when linked to remote hosts.
    #[serde(default)]
    pub connected_peers: Vec<PeerRefSnapshot>,
    /// Outbound events per second reported by runtime metrics.
    pub out_events_per_sec: u64,
    /// Inbound events per second reported by runtime metrics.
    pub in_events_per_sec: u64,
    /// Peers that have sent connection requests to us (waiting for our acceptance).
    #[serde(default)]
    pub incoming_connections: Vec<PeerRefSnapshot>,
}

/// Discovery reachability state exposed to frontends.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum DiscoveryStatusSnapshot {
    #[default]
    Online,
    Stale,
}

/// Stable peer identity and display label.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct PeerRefSnapshot {
    pub peer_id: String,
    pub name: String,
    #[serde(default)]
    pub platform: Option<String>,
}

/// Discovered peer metadata returned by network discovery.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VirtualBoundsSnapshot {
    /// Virtual display origin X in global desktop coordinates.
    pub x: i32,
    /// Virtual display origin Y in global desktop coordinates.
    pub y: i32,
    /// Virtual display width in pixels.
    pub width: u32,
    /// Virtual display height in pixels.
    pub height: u32,
}

/// Display rectangle in the peer's virtual desktop coordinate space.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DisplayRectSnapshot {
    /// Display origin X in the peer's virtual desktop coordinates.
    pub x: i32,
    /// Display origin Y in the peer's virtual desktop coordinates.
    pub y: i32,
    /// Display width in pixels.
    pub width: u32,
    /// Display height in pixels.
    pub height: u32,
}

/// Discovered peer metadata returned by network discovery.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PeerSnapshot {
    /// Stable identity for this peer.
    pub peer_id: String,
    /// User-visible peer name.
    pub name: String,
    /// Peer hostname as advertised on the network.
    pub hostname: String,
    /// Socket address of the peer.
    pub addr: String,
    #[serde(default)]
    /// Optional peer platform identifier (for example: macos, linux, windows).
    pub platform: Option<String>,
    /// Optional peer screen label.
    pub screen: Option<String>,
    /// Optional peer virtual bounds.
    pub virtual_bounds: Option<VirtualBoundsSnapshot>,
    /// Individual display rectangles that make up the peer's desktop shape.
    #[serde(default)]
    pub display_rects: Vec<DisplayRectSnapshot>,
    /// Milliseconds since peer was last observed.
    pub last_seen_ms: u64,
    /// Whether discovery currently considers the peer online or stale.
    #[serde(default)]
    pub discovery_status: DiscoveryStatusSnapshot,
    /// True when the discovered peer maps to the local machine.
    pub is_local: bool,
    /// Whether the peer is currently connected.
    #[serde(default)]
    pub connected: bool,
    /// Whether the peer is currently paired.
    #[serde(default)]
    pub paired: bool,
    /// Detailed connection status (e.g., "connected", "incoming", "connecting", "disconnected", "backoff").
    #[serde(default)]
    pub connection_status: Option<String>,
}

/// Persisted layout and performance settings exposed to frontends.
///
/// Re-exported from [`crate::core::events::LayoutPlacement`] for use in API contexts.
pub use crate::core::events::LayoutPlacement;

/// Legacy config entries that could not be resolved to a unique peer identity.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LegacyConfigIssueSnapshot {
    pub location: String,
    pub peer_name: String,
    pub reason: String,
}

/// Persisted layout and performance settings exposed to frontends.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConfigSnapshot {
    #[serde(default)]
    /// Peer ID assigned to the left edge.
    pub left: Option<String>,
    #[serde(default)]
    /// Peer ID assigned to the right edge.
    pub right: Option<String>,
    #[serde(default)]
    /// Peer ID assigned to the top edge.
    pub top: Option<String>,
    #[serde(default)]
    /// Peer ID assigned to the bottom edge.
    pub bottom: Option<String>,
    #[serde(default)]
    /// Zone-based edge mappings.
    pub zones: Vec<config::LayoutZoneConfig>,
    #[serde(default)]
    /// Free-form visual placements keyed by peer identity.
    pub placements: Vec<LayoutPlacement>,
    #[serde(default)]
    /// Legacy config entries awaiting explicit reselection.
    pub legacy_issues: Vec<LegacyConfigIssueSnapshot>,
    /// Event batch aggregation window in microseconds.
    pub batch_window_us: u64,
    /// Maximum events to send in a single packet.
    pub max_events_per_packet: usize,
    /// Movement send cadence in microseconds (~120Hz = 8333). Set to 0 to disable.
    #[serde(default = "default_movement_cadence_us")]
    pub movement_cadence_us: u64,
    #[serde(default)]
    /// Optional metrics log output path.
    pub metrics_log_path: Option<String>,
    #[serde(default)]
    /// Reserved for frontend compatibility; remote layout updates now apply immediately.
    pub remote_layout_updated: bool,
    #[serde(default = "default_swap_alt_cmd")]
    /// Whether to swap Alt/Option and Command/Win when redirecting across macOS and PC.
    pub swap_alt_cmd: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    /// Granular modifier key mapping overrides when redirecting input.
    pub modifier_mapping: Option<crate::core::platform::ModifierMapping>,
    #[serde(default)]
    /// Key that returns the cursor to this computer when pressed 3 times quickly.
    pub release_tap_key: config::ReleaseTapKey,
    #[serde(default)]
    /// Whether to keep the mouse alive by moving it 1 pixel every 5 seconds.
    pub keep_mouse_alive: bool,
    #[serde(default = "default_share_clipboard")]
    /// Whether to synchronize clipboard across peers.
    pub share_clipboard: bool,
    #[serde(default = "default_auto_connect_peers", alias = "auto_connect")]
    /// Whether to automatically connect to discovered peers.
    pub auto_connect_peers: bool,
    #[serde(default)]
    /// Explicitly paired peer IDs.
    pub paired_peers: Vec<String>,
    #[serde(default)]
    /// Paired peers that were manually disconnected and should not auto-reconnect.
    pub manual_disconnect_peers: Vec<String>,
}

impl ConfigSnapshot {
    pub fn is_peer_paired(&self, peer_id: &str) -> bool {
        let canonical = crate::core::peer_state::canonical_peer_id(peer_id);
        self.paired_peers
            .iter()
            .any(|id| crate::core::peer_state::canonical_peer_id(id) == canonical)
    }

    pub fn pair_peer(&mut self, peer_id: &str) {
        let canonical = crate::core::peer_state::canonical_peer_id(peer_id);
        if !canonical.is_empty() && !self.is_peer_paired(&canonical) {
            self.paired_peers.push(canonical);
        }
    }

    pub fn unpair_peer(&mut self, peer_id: &str) {
        let canonical = crate::core::peer_state::canonical_peer_id(peer_id);
        self.paired_peers
            .retain(|id| crate::core::peer_state::canonical_peer_id(id) != canonical);
        self.resume_peer(peer_id);
    }

    pub fn is_peer_paused(&self, peer_id: &str) -> bool {
        let canonical = crate::core::peer_state::canonical_peer_id(peer_id);
        self.manual_disconnect_peers
            .iter()
            .any(|id| crate::core::peer_state::canonical_peer_id(id) == canonical)
    }

    pub fn pause_peer(&mut self, peer_id: &str) {
        let canonical = crate::core::peer_state::canonical_peer_id(peer_id);
        if !canonical.is_empty() && !self.is_peer_paused(&canonical) {
            self.manual_disconnect_peers.push(canonical);
        }
    }

    pub fn resume_peer(&mut self, peer_id: &str) {
        let canonical = crate::core::peer_state::canonical_peer_id(peer_id);
        self.manual_disconnect_peers
            .retain(|id| crate::core::peer_state::canonical_peer_id(id) != canonical);
    }

    pub fn is_peer_allowed_inbound(&self, peer_id: &str) -> bool {
        self.is_peer_paired(peer_id) && !self.is_peer_paused(peer_id)
    }

    pub fn should_auto_connect_peer(&self, peer_id: &str) -> bool {
        self.auto_connect_peers && self.is_peer_allowed_inbound(peer_id)
    }
}

fn default_auto_connect_peers() -> bool {
    true
}

fn default_share_clipboard() -> bool {
    true
}

fn default_movement_cadence_us() -> u64 {
    8_333
}

fn default_swap_alt_cmd() -> bool {
    cfg!(target_os = "macos")
}

impl Default for ConfigSnapshot {
    fn default() -> Self {
        Self::from(config::AppConfig::default())
    }
}
