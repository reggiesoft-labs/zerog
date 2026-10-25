//! Application configuration loaded from TOML files.
//!
//! Configuration is stored in `~/.zerog/config.toml` and defines:
//! - Layout: which peers are positioned at which screen edges
//! - Performance: tuning parameters for network behavior
//!
//! # Example Configuration
//!
//! ```toml
//! [layout]
//! left = "peer-left"
//! right = "peer-right"
//!
//! [[layout.zones]]
//! edge = "left"
//! peer = "peer-top-left"
//! start_pct = 0.0
//! end_pct = 0.5
//!
//! [performance]
//! batch_window_us = 250
//! max_events_per_packet = 32
//! ```

use crate::core::peer_state::canonical_peer_id;
use crate::core::platform::{ModifierMapping, PeerEdge};
use crate::core::secure_fs::write_private_file;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;
use uuid::Uuid;

const CONFIG_DIR_NAME: &str = ".zerog";
const LEGACY_CONFIG_DIR_NAME: &str = ".mouse-thing";
const CONFIG_FILE_NAME: &str = "config.toml";

/// Root configuration structure.
///
/// Contains all configuration sections with sensible defaults.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AppConfig {
    /// Installation-scoped local identity.
    #[serde(default)]
    pub identity: IdentityConfig,
    /// Layout configuration for peer positioning.
    #[serde(default)]
    pub layout: LayoutConfig,
    /// Performance tuning parameters.
    #[serde(default)]
    pub performance: PerformanceConfig,
    /// Keyboard mapping parameters.
    #[serde(default)]
    pub keyboard: KeyboardConfig,
    /// Mouse configuration parameters.
    #[serde(default)]
    pub mouse: MouseConfig,
    /// Clipboard configuration parameters.
    #[serde(default)]
    pub clipboard: ClipboardConfig,
    /// Connection configuration parameters.
    #[serde(default)]
    pub connection: ConnectionConfig,
    #[serde(default)]
    pub license_key: Option<String>,
}

/// Connection configuration options.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ConnectionConfig {
    /// Whether to automatically connect to discovered peers.
    #[serde(default = "default_auto_connect", alias = "auto_connect_peers")]
    pub auto_connect: bool,
    /// Explicitly paired peer IDs.
    #[serde(default)]
    pub paired_peers: Vec<String>,
    /// Paired peers that were manually disconnected and should not auto-reconnect.
    #[serde(default)]
    pub manual_disconnect_peers: Vec<String>,
}

fn default_auto_connect() -> bool {
    true
}

impl Default for ConnectionConfig {
    fn default() -> Self {
        Self {
            auto_connect: true,
            paired_peers: Vec::new(),
            manual_disconnect_peers: Vec::new(),
        }
    }
}

impl ConnectionConfig {
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
}

/// Clipboard configuration options.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ClipboardConfig {
    /// Whether to synchronize clipboard across peers.
    #[serde(default = "default_share_clipboard", alias = "enabled", alias = "share")]
    pub share_clipboard: bool,
}

fn default_share_clipboard() -> bool {
    true
}

impl Default for ClipboardConfig {
    fn default() -> Self {
        Self {
            share_clipboard: true,
        }
    }
}

/// Mouse configuration options.
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub struct MouseConfig {
    /// Whether to keep the mouse alive by moving it 1 pixel every 5 seconds.
    #[serde(default)]
    pub keep_alive: bool,
}

const fn default_swap_alt_cmd() -> bool {
    cfg!(any(target_os = "windows", target_os = "macos"))
}

/// Key that returns the cursor to this computer when pressed 3 times quickly.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, Default, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ReleaseTapKey {
    #[default]
    Escape,
    Pause,
    Insert,
    Home,
    End,
    PageUp,
    PageDown,
    F1,
    F2,
    F3,
    F4,
    F5,
    F6,
    F7,
    F8,
    F9,
    F10,
    F11,
    F12,
    Grave,
    Off,
}

impl ReleaseTapKey {
    /// USB HID usage code of the key, or 0 when disabled.
    pub fn hid_code(self) -> u32 {
        match self {
            Self::Escape => 0x29,
            Self::Pause => 0x48,
            Self::Insert => 0x49,
            Self::Home => 0x4A,
            Self::End => 0x4D,
            Self::PageUp => 0x4B,
            Self::PageDown => 0x4E,
            Self::F1 => 0x3A,
            Self::F2 => 0x3B,
            Self::F3 => 0x3C,
            Self::F4 => 0x3D,
            Self::F5 => 0x3E,
            Self::F6 => 0x3F,
            Self::F7 => 0x40,
            Self::F8 => 0x41,
            Self::F9 => 0x42,
            Self::F10 => 0x43,
            Self::F11 => 0x44,
            Self::F12 => 0x45,
            Self::Grave => 0x35,
            Self::Off => 0,
        }
    }
}

/// Keyboard configuration options.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct KeyboardConfig {
    /// Whether to swap Alt/Option and Command/Win when redirecting across macOS and PC.
    #[serde(default = "default_swap_alt_cmd")]
    pub swap_alt_cmd: bool,
    /// Granular modifier mapping overrides when redirecting input across macOS and PC.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub modifier_mapping: Option<ModifierMapping>,
    /// Key that returns the cursor to this computer when pressed 3 times quickly.
    #[serde(default)]
    pub release_tap_key: ReleaseTapKey,
}

impl Default for KeyboardConfig {
    fn default() -> Self {
        Self {
            swap_alt_cmd: default_swap_alt_cmd(),
            modifier_mapping: None,
            release_tap_key: ReleaseTapKey::default(),
        }
    }
}

/// Persisted installation identity.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct IdentityConfig {
    /// Stable UUID for this installation.
    #[serde(default)]
    pub peer_id: Option<String>,
}

/// Layout configuration defining peer positions at screen edges.
///
/// Peers can be assigned to full edges (left/right/top/bottom) or to
/// specific zones along an edge for more complex topologies.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct LayoutConfig {
    /// Peer name for the full left edge.
    #[serde(default)]
    pub left: Option<String>,
    /// Peer name for the full right edge.
    #[serde(default)]
    pub right: Option<String>,
    /// Peer name for the full top edge.
    #[serde(default)]
    pub top: Option<String>,
    /// Peer name for the full bottom edge.
    #[serde(default)]
    pub bottom: Option<String>,
    /// Zone-based edge assignments for partial edge coverage.
    #[serde(default)]
    pub zones: Vec<LayoutZoneConfig>,
    /// Visual layout placements for multi-peer topology.
    #[serde(default)]
    pub placements: Vec<crate::core::events::LayoutPlacement>,
}

/// A zone along a screen edge assigned to a specific peer.
///
/// Zones allow splitting an edge between multiple peers. For example,
/// the left edge could have one peer for the top half and another for
/// the bottom half.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LayoutZoneConfig {
    /// Which screen edge this zone covers.
    pub edge: PeerEdge,
    /// Stable peer identity for this zone.
    #[serde(alias = "peer")]
    pub peer_id: String,
    /// Last-known display label for this peer.
    #[serde(default)]
    pub peer_label: String,
    /// Start position as percentage along the edge (0.0 = start, 1.0 = end).
    pub start_pct: f64,
    /// End position as percentage along the edge (0.0 = start, 1.0 = end).
    pub end_pct: f64,
}

/// A peer's position in the visual layout grid.
///
/// Re-exported from [`crate::core::events::LayoutPlacement`] for use in config contexts.
pub use crate::core::events::LayoutPlacement;

/// Performance tuning parameters for network behavior.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PerformanceConfig {
    /// Microseconds to wait for batching multiple events into one packet.
    /// Default 2000us (2ms) balances latency with WiFi reliability.
    /// Set to 0 for immediate delivery (lowest latency, higher bandwidth).
    #[serde(default)]
    pub batch_window_us: u64,
    /// Maximum events to include in a single packet before forcing transmission.
    #[serde(default = "default_max_events_per_packet")]
    pub max_events_per_packet: usize,
    /// Microseconds between forced movement-batch flushes (~120Hz = 8333).
    /// Set to 0 to disable cadence control.
    #[serde(default = "default_movement_cadence_us")]
    pub movement_cadence_us: u64,
    /// Optional path to write throughput metrics for debugging.
    #[serde(default)]
    pub metrics_log_path: Option<String>,
}

impl Default for PerformanceConfig {
    fn default() -> Self {
        Self {
            batch_window_us: 0,
            max_events_per_packet: default_max_events_per_packet(),
            movement_cadence_us: default_movement_cadence_us(),
            metrics_log_path: None,
        }
    }
}

pub(crate) fn home_dir() -> Result<PathBuf> {
    std::env::var_os("HOME")
        .filter(|h| !h.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("USERPROFILE").filter(|p| !p.is_empty()).map(PathBuf::from))
        .or_else(dirs::home_dir)
        .context("Cannot resolve home directory; cannot resolve config path")
}

/// Returns the path to the configuration file (`~/.zerog/config.toml`).
///
/// # Errors
///
/// Returns an error if the `HOME` environment variable is not set.
pub fn config_path() -> Result<PathBuf> {
    Ok(home_dir()?.join(CONFIG_DIR_NAME).join(CONFIG_FILE_NAME))
}

fn legacy_config_path() -> Result<Option<PathBuf>> {
    let path = home_dir()?.join(LEGACY_CONFIG_DIR_NAME).join(CONFIG_FILE_NAME);
    Ok(path.exists().then_some(path))
}

static CONFIG_MUTEX: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Loads configuration from disk or returns defaults if no file exists.
///
/// # Errors
///
/// Returns an error if the file exists but cannot be read or parsed.
pub fn load_or_default() -> Result<AppConfig> {
    let _guard = CONFIG_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
    let path = config_path()?;
    let load_path = if path.exists() {
        path
    } else if let Some(legacy) = legacy_config_path()? {
        legacy
    } else {
        return Ok(AppConfig::default());
    };

    let raw = fs::read_to_string(&load_path)
        .with_context(|| format!("Failed to read config file at {}", load_path.display()))?;
    toml::from_str::<AppConfig>(&raw)
        .with_context(|| format!("Failed to parse config file at {}", load_path.display()))
}

/// Saves configuration to disk, creating the directory if needed.
///
/// # Returns
///
/// The path where the configuration was saved.
///
/// # Errors
///
/// Returns an error if the directory cannot be created or the file cannot be written.
pub fn save(config: &AppConfig) -> Result<PathBuf> {
    let _guard = CONFIG_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
    let path = config_path()?;
    let parent = path
        .parent()
        .context("Invalid config path: missing parent directory")?;
    fs::create_dir_all(parent)
        .with_context(|| format!("Failed to create config directory {}", parent.display()))?;

    let mut to_save = config.clone();
    if path.exists() {
        if let Ok(raw) = fs::read_to_string(&path) {
            if let Ok(existing) = toml::from_str::<AppConfig>(&raw) {
                if to_save.identity.peer_id.is_none() && existing.identity.peer_id.is_some() {
                    to_save.identity.peer_id = existing.identity.peer_id;
                }
                if to_save.license_key.is_none() && existing.license_key.is_some() {
                    to_save.license_key = existing.license_key;
                }
            }
        }
    }

    let encoded = toml::to_string_pretty(&to_save).context("Failed to serialize config as TOML")?;
    write_private_file(&path, encoded.as_bytes())
        .with_context(|| format!("Failed to write config file at {}", path.display()))?;
    Ok(path)
}

pub fn load_license_status() -> crate::core::license::LicenseStatus {
    load_or_default()
        .map(|config| config.license_status())
        .unwrap_or(crate::core::license::LicenseStatus::Free)
}

pub fn save_license_key(key: Option<String>) -> Result<crate::core::license::LicenseStatus> {
    let mut config = load_or_default()?;
    config.license_key = key;
    save(&config)?;
    Ok(config.license_status())
}

/// Loads or creates the installation-scoped peer ID and persists it if needed.
pub fn load_or_create_peer_id() -> Result<String> {
    let mut config = load_or_default()?;
    if let Some(existing) = config
        .identity
        .peer_id
        .as_deref()
        .map(|id| canonical_peer_id(id))
        .filter(|value| !value.is_empty())
    {
        return Ok(existing);
    }

    let peer_id = Uuid::new_v4().to_string();
    config.identity.peer_id = Some(peer_id.clone());
    save(&config)?;
    Ok(peer_id)
}

impl AppConfig {
    pub fn license_status(&self) -> crate::core::license::LicenseStatus {
        match &self.license_key {
            Some(key) => crate::core::license::verify_license_key(key, &crate::core::license::DEFAULT_LICENSE_PUBKEY),
            None => crate::core::license::LicenseStatus::Free,
        }
    }

    /// Returns the peer name assigned to the given edge, if any.
    pub fn peer_for_edge(&self, edge: PeerEdge) -> Option<&str> {
        self.layout.peer_for_edge(edge)
    }

    /// Assigns a peer name to the given edge.
    pub fn set_peer_for_edge(&mut self, edge: PeerEdge, peer_name: String) {
        self.layout.set_peer_for_edge(edge, peer_name);
    }

    /// Validates the configuration and returns warnings for any issues.
    ///
    /// Warnings include empty peer names, invalid zone ranges, and overlapping zones.
    pub fn validation_warnings(&self) -> Vec<String> {
        self.layout.validation_warnings()
    }

    /// Returns the peer for an edge at a specific position, checking zones first.
    ///
    /// If no zone matches the position, falls back to the edge's default peer.
    ///
    /// # Arguments
    ///
    /// * `edge` - The screen edge to check.
    /// * `position_pct` - Position along the edge (0.0 = start, 1.0 = end).
    pub fn peer_for_edge_at_position(&self, edge: PeerEdge, position_pct: f64) -> Option<&str> {
        self.layout
            .peer_for_edge_at_position(edge, position_pct)
            .or_else(|| self.layout.peer_for_edge(edge))
    }

    /// Returns the first configured edge with a non-empty peer name.
    ///
    /// Checks edges in order: Left, Right, Top, Bottom.
    pub fn first_configured_edge(&self) -> Option<PeerEdge> {
        self.layout.first_configured_edge()
    }
}

impl LayoutConfig {
    fn peer_for_edge(&self, edge: PeerEdge) -> Option<&str> {
        match edge {
            PeerEdge::Left => self.left.as_deref(),
            PeerEdge::Right => self.right.as_deref(),
            PeerEdge::Top => self.top.as_deref(),
            PeerEdge::Bottom => self.bottom.as_deref(),
        }
    }

    fn set_peer_for_edge(&mut self, edge: PeerEdge, peer_name: String) {
        match edge {
            PeerEdge::Left => self.left = Some(peer_name),
            PeerEdge::Right => self.right = Some(peer_name),
            PeerEdge::Top => self.top = Some(peer_name),
            PeerEdge::Bottom => self.bottom = Some(peer_name),
        }
    }

    /// Looks up a peer for a specific position along an edge using zones.
    fn peer_for_edge_at_position(&self, edge: PeerEdge, position_pct: f64) -> Option<&str> {
        self.zones
            .iter()
            .find(|zone| {
                zone.edge == edge
                    && position_pct >= zone.start_pct
                    && position_pct <= zone.end_pct
                    && !zone.peer_id.trim().is_empty()
            })
            .map(|zone| zone.peer_id.as_str())
    }

    /// Validates zone configuration and returns warnings for any issues.
    ///
    /// Checks for:
    /// - Empty peer names
    /// - Zone ranges outside [0.0, 1.0]
    /// - Zones where start >= end
    /// - Overlapping zones on the same edge
    pub fn validation_warnings(&self) -> Vec<String> {
        let mut warnings = Vec::new();

        for (idx, zone) in self.zones.iter().enumerate() {
            let label = format!(
                "layout.zones[{idx}] {} {}..{}",
                zone.edge.as_str(),
                zone.start_pct,
                zone.end_pct
            );

            if zone.peer_id.trim().is_empty() {
                warnings.push(format!("{label}: peer cannot be empty"));
            }
            if !(0.0..=1.0).contains(&zone.start_pct) || !(0.0..=1.0).contains(&zone.end_pct) {
                warnings.push(format!("{label}: range must remain inside [0.0, 1.0]"));
            }
            if zone.start_pct >= zone.end_pct {
                warnings.push(format!("{label}: start_pct must be less than end_pct"));
            }
        }

        for edge in [
            PeerEdge::Left,
            PeerEdge::Right,
            PeerEdge::Top,
            PeerEdge::Bottom,
        ] {
            let mut ranges = self
                .zones
                .iter()
                .enumerate()
                .filter(|(_, zone)| zone.edge == edge)
                .collect::<Vec<_>>();
            ranges.sort_by(|a, b| a.1.start_pct.total_cmp(&b.1.start_pct));

            let mut previous: Option<(usize, &LayoutZoneConfig)> = None;
            for (idx, zone) in ranges {
                if let Some((prev_idx, prev_zone)) = previous
                    && zone.start_pct < prev_zone.end_pct
                {
                    warnings.push(format!(
                        "layout.zones[{prev_idx}] overlaps layout.zones[{idx}] on {} edge",
                        edge.as_str()
                    ));
                }
                previous = Some((idx, zone));
            }
        }

        warnings
    }

    fn first_configured_edge(&self) -> Option<PeerEdge> {
        for edge in [
            PeerEdge::Left,
            PeerEdge::Right,
            PeerEdge::Top,
            PeerEdge::Bottom,
        ] {
            if let Some(name) = self.peer_for_edge(edge)
                && !name.trim().is_empty()
            {
                return Some(edge);
            }
        }
        None
    }
}

use crate::core::api::ConfigSnapshot;

fn default_max_events_per_packet() -> usize {
    32
}

fn default_movement_cadence_us() -> u64 {
    8_333
}

impl From<AppConfig> for ConfigSnapshot {
    fn from(cfg: AppConfig) -> Self {
        ConfigSnapshot {
            left: cfg.layout.left,
            right: cfg.layout.right,
            top: cfg.layout.top,
            bottom: cfg.layout.bottom,
            zones: cfg.layout.zones,
            placements: cfg.layout.placements,
            legacy_issues: Vec::new(),
            batch_window_us: cfg.performance.batch_window_us,
            max_events_per_packet: cfg.performance.max_events_per_packet,
            movement_cadence_us: cfg.performance.movement_cadence_us,
            metrics_log_path: cfg.performance.metrics_log_path,
            remote_layout_updated: false,
            swap_alt_cmd: cfg.keyboard.swap_alt_cmd,
            modifier_mapping: cfg.keyboard.modifier_mapping,
            release_tap_key: cfg.keyboard.release_tap_key,
            keep_mouse_alive: cfg.mouse.keep_alive,
            share_clipboard: cfg.clipboard.share_clipboard,
            auto_connect_peers: cfg.connection.auto_connect,
            paired_peers: cfg.connection.paired_peers,
            manual_disconnect_peers: cfg.connection.manual_disconnect_peers,
        }
    }
}

impl From<&ConfigSnapshot> for LayoutConfig {
    fn from(snapshot: &ConfigSnapshot) -> Self {
        Self {
            left: snapshot.left.clone(),
            right: snapshot.right.clone(),
            top: snapshot.top.clone(),
            bottom: snapshot.bottom.clone(),
            zones: snapshot.zones.clone(),
            placements: snapshot.placements.clone(),
        }
    }
}

impl From<&ConfigSnapshot> for PerformanceConfig {
    fn from(snapshot: &ConfigSnapshot) -> Self {
        Self {
            batch_window_us: snapshot.batch_window_us,
            max_events_per_packet: snapshot.max_events_per_packet,
            movement_cadence_us: snapshot.movement_cadence_us,
            metrics_log_path: snapshot.metrics_log_path.clone(),
        }
    }
}

impl From<&ConfigSnapshot> for KeyboardConfig {
    fn from(snapshot: &ConfigSnapshot) -> Self {
        Self {
            swap_alt_cmd: snapshot.swap_alt_cmd,
            modifier_mapping: snapshot.modifier_mapping,
            release_tap_key: snapshot.release_tap_key,
        }
    }
}

impl From<&ConfigSnapshot> for MouseConfig {
    fn from(snapshot: &ConfigSnapshot) -> Self {
        Self {
            keep_alive: snapshot.keep_mouse_alive,
        }
    }
}

impl From<&ConfigSnapshot> for ClipboardConfig {
    fn from(snapshot: &ConfigSnapshot) -> Self {
        Self {
            share_clipboard: snapshot.share_clipboard,
        }
    }
}

impl From<&ConfigSnapshot> for ConnectionConfig {
    fn from(snapshot: &ConfigSnapshot) -> Self {
        Self {
            auto_connect: snapshot.auto_connect_peers,
            paired_peers: snapshot.paired_peers.clone(),
            manual_disconnect_peers: snapshot.manual_disconnect_peers.clone(),
        }
    }
}

impl From<&ConfigSnapshot> for AppConfig {
    fn from(snapshot: &ConfigSnapshot) -> Self {
        AppConfig {
            identity: IdentityConfig::default(),
            layout: LayoutConfig::from(snapshot),
            performance: PerformanceConfig::from(snapshot),
            keyboard: KeyboardConfig::from(snapshot),
            mouse: MouseConfig::from(snapshot),
            clipboard: ClipboardConfig::from(snapshot),
            connection: ConnectionConfig::from(snapshot),
            license_key: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn app_config_deserializes_without_layout_placements() {
        let raw = r#"
[layout]
left = "peer-left"

[performance]
batch_window_us = 250
max_events_per_packet = 32
"#;

        let config = toml::from_str::<AppConfig>(raw).unwrap();
        assert_eq!(config.layout.left.as_deref(), Some("peer-left"));
        assert!(config.layout.placements.is_empty());
    }

    #[test]
    fn app_config_round_trips_layout_placements() {
        let config = AppConfig {
            layout: LayoutConfig {
                placements: vec![LayoutPlacement {
                    peer_id: "peer-a".to_string(),
                    peer_label: "peer-a".to_string(),
                    x: -320,
                    y: 120,
                }],
                ..LayoutConfig::default()
            },
            ..AppConfig::default()
        };

        let encoded = toml::to_string_pretty(&config).unwrap();
        let decoded = toml::from_str::<AppConfig>(&encoded).unwrap();
        assert_eq!(decoded.layout.placements, config.layout.placements);
    }

    #[test]
    fn first_configured_edge_returns_left_when_left_has_peer() {
        let config = AppConfig {
            layout: LayoutConfig {
                left: Some("peer-left".to_string()),
                ..LayoutConfig::default()
            },
            ..AppConfig::default()
        };

        assert_eq!(config.first_configured_edge(), Some(PeerEdge::Left));
    }

    #[test]
    fn first_configured_edge_returns_right_when_only_right_has_peer() {
        let config = AppConfig {
            layout: LayoutConfig {
                right: Some("peer-right".to_string()),
                ..LayoutConfig::default()
            },
            ..AppConfig::default()
        };

        assert_eq!(config.first_configured_edge(), Some(PeerEdge::Right));
    }

    #[test]
    fn first_configured_edge_returns_none_when_no_peers_configured() {
        let config = AppConfig::default();
        assert_eq!(config.first_configured_edge(), None);
    }

    #[test]
    fn first_configured_edge_uses_deterministic_order() {
        let config = AppConfig {
            layout: LayoutConfig {
                right: Some("peer-right".to_string()),
                top: Some("peer-top".to_string()),
                bottom: Some("peer-bottom".to_string()),
                ..LayoutConfig::default()
            },
            ..AppConfig::default()
        };

        assert_eq!(config.first_configured_edge(), Some(PeerEdge::Right));
    }

    #[test]
    fn first_configured_edge_ignores_whitespace_only_peer_values() {
        let config = AppConfig {
            layout: LayoutConfig {
                left: Some("   ".to_string()),
                top: Some("peer-top".to_string()),
                ..LayoutConfig::default()
            },
            ..AppConfig::default()
        };

        assert_eq!(config.first_configured_edge(), Some(PeerEdge::Top));
    }

    use crate::platform::shared::test_support::with_temp_home;

    #[test]
    #[serial_test::serial]
    fn save_preserves_existing_identity_when_omitted_in_update() {
        with_temp_home(|| {
            let initial_config = AppConfig {
                identity: IdentityConfig {
                    peer_id: Some("stable-peer-uuid-123".to_string()),
                },
                ..AppConfig::default()
            };
            save(&initial_config).unwrap();

            let update_without_identity = AppConfig {
                layout: LayoutConfig {
                    placements: vec![LayoutPlacement {
                        peer_id: "peer-b".to_string(),
                        peer_label: "Peer B".to_string(),
                        x: 100,
                        y: 200,
                    }],
                    ..LayoutConfig::default()
                },
                ..AppConfig::default()
            };
            save(&update_without_identity).unwrap();

            let loaded = load_or_default().unwrap();
            assert_eq!(
                loaded.identity.peer_id.as_deref(),
                Some("stable-peer-uuid-123")
            );
            assert_eq!(loaded.layout.placements.len(), 1);
            assert_eq!(loaded.layout.placements[0].peer_id, "peer-b");
        });
    }

    #[test]
    fn parses_and_converts_keyboard_config() {
        let raw = r#"
            [keyboard]
            swap_alt_cmd = true
        "#;
        let config: AppConfig = toml::from_str(raw).unwrap();
        assert!(config.keyboard.swap_alt_cmd);

        let snapshot = ConfigSnapshot::from(config);
        assert!(snapshot.swap_alt_cmd);

        let back = AppConfig::from(&snapshot);
        assert!(back.keyboard.swap_alt_cmd);
    }

    #[test]
    fn default_keyboard_config_matches_platform() {
        let default_config = AppConfig::default();
        if cfg!(any(target_os = "windows", target_os = "macos")) {
            assert!(default_config.keyboard.swap_alt_cmd);
        } else {
            assert!(!default_config.keyboard.swap_alt_cmd);
        }

        let empty_keyboard_raw = r#"
            [keyboard]
        "#;
        let config: AppConfig = toml::from_str(empty_keyboard_raw).unwrap();
        if cfg!(any(target_os = "windows", target_os = "macos")) {
            assert!(config.keyboard.swap_alt_cmd);
        } else {
            assert!(!config.keyboard.swap_alt_cmd);
        }

        let explicit_false_raw = r#"
            [keyboard]
            swap_alt_cmd = false
        "#;
        let config_false: AppConfig = toml::from_str(explicit_false_raw).unwrap();
        assert!(!config_false.keyboard.swap_alt_cmd);
    }

    #[test]
    fn release_tap_key_defaults_to_escape_and_round_trips() {
        let config: AppConfig = toml::from_str("[keyboard]\n").unwrap();
        assert_eq!(config.keyboard.release_tap_key, ReleaseTapKey::Escape);

        let raw = r#"
            [keyboard]
            release_tap_key = "f12"
        "#;
        let config: AppConfig = toml::from_str(raw).unwrap();
        assert_eq!(config.keyboard.release_tap_key, ReleaseTapKey::F12);

        let snapshot = ConfigSnapshot::from(config);
        assert_eq!(snapshot.release_tap_key, ReleaseTapKey::F12);

        let back = AppConfig::from(&snapshot);
        assert_eq!(back.keyboard.release_tap_key, ReleaseTapKey::F12);
    }

    #[test]
    fn parses_and_converts_mouse_config() {
        let raw = r#"
            [mouse]
            keep_alive = true
        "#;
        let config: AppConfig = toml::from_str(raw).unwrap();
        assert!(config.mouse.keep_alive);

        let snapshot = ConfigSnapshot::from(config);
        assert!(snapshot.keep_mouse_alive);

        let back = AppConfig::from(&snapshot);
        assert!(back.mouse.keep_alive);
    }

    #[test]
    fn parses_and_converts_clipboard_config() {
        let raw = r#"
            [clipboard]
            share_clipboard = false
        "#;
        let config: AppConfig = toml::from_str(raw).unwrap();
        assert!(!config.clipboard.share_clipboard);

        let snapshot = ConfigSnapshot::from(config);
        assert!(!snapshot.share_clipboard);

        let back = AppConfig::from(&snapshot);
        assert!(!back.clipboard.share_clipboard);
    }

    #[test]
    fn parses_and_converts_connection_config() {
        let raw = r#"
            [connection]
            auto_connect = false
        "#;
        let config: AppConfig = toml::from_str(raw).unwrap();
        assert!(!config.connection.auto_connect);

        let snapshot = ConfigSnapshot::from(config);
        assert!(!snapshot.auto_connect_peers);

        let back = AppConfig::from(&snapshot);
        assert!(!back.connection.auto_connect);
    }

    #[test]
    fn parses_and_converts_paired_peers() {
        let raw = r#"
            [connection]
            auto_connect = true
            paired_peers = ["peer-1", "peer-2"]
        "#;
        let mut config: AppConfig = toml::from_str(raw).unwrap();
        assert!(config.connection.is_peer_paired("peer-1"));
        assert!(config.connection.is_peer_paired("peer-2"));
        assert!(!config.connection.is_peer_paired("peer-3"));

        config.connection.pair_peer("peer-3");
        assert!(config.connection.is_peer_paired("peer-3"));

        config.connection.unpair_peer("peer-1");
        assert!(!config.connection.is_peer_paired("peer-1"));

        let snapshot = ConfigSnapshot::from(config);
        assert!(snapshot.is_peer_paired("peer-2"));
        assert!(snapshot.is_peer_paired("peer-3"));
        assert!(!snapshot.is_peer_paired("peer-1"));

        let back = AppConfig::from(&snapshot);
        assert!(back.connection.is_peer_paired("peer-2"));
        assert!(back.connection.is_peer_paired("peer-3"));
    }

    #[test]
    fn parses_and_converts_manual_disconnect_peers() {
        let raw = r#"
            [connection]
            auto_connect = true
            paired_peers = ["peer-1", "peer-2"]
            manual_disconnect_peers = ["peer-1"]
        "#;
        let mut config: AppConfig = toml::from_str(raw).unwrap();
        assert!(config.connection.is_peer_paused("peer-1"));
        assert!(!config.connection.is_peer_paused("peer-2"));

        config.connection.pause_peer("peer-2");
        assert!(config.connection.is_peer_paused("peer-2"));

        config.connection.resume_peer("peer-1");
        assert!(!config.connection.is_peer_paused("peer-1"));

        let mut snapshot = ConfigSnapshot::from(config);
        assert!(snapshot.is_peer_paused("peer-2"));
        assert!(!snapshot.is_peer_paused("peer-1"));

        snapshot.unpair_peer("peer-2");
        assert!(!snapshot.is_peer_paused("peer-2"));
        assert!(!snapshot.is_peer_paired("peer-2"));

        let back = AppConfig::from(&snapshot);
        assert!(!back.connection.is_peer_paused("peer-2"));
    }
}
