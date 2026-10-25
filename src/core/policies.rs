//! Timing constants and policy decisions for the application.
//!
//! This module centralizes all timing-related constants and policy functions
//! to ensure consistent behavior across the codebase and make tuning easier.

use std::time::Duration;

/// Canonical DNS-SD service type used for discovery and registration.
pub const DISCOVERY_SERVICE_TYPE: &str = "_zerog._udp.local.";

/// Default UDP listen port for QUIC transport.
pub const DEFAULT_PORT: u16 = 54321;

/// Interval between mDNS service refresh operations.
pub const DISCOVERY_REFRESH_INTERVAL: Duration = Duration::from_millis(500);

pub const DISCOVERY_BROWSE_REFRESH_INTERVAL: Duration = Duration::from_secs(15);

pub const DISCOVERY_ANNOUNCE_HEARTBEAT_INTERVAL: Duration = Duration::from_secs(30);

pub const SUBNET_SCAN_POLL_INTERVAL: Duration = Duration::from_secs(60);

/// Time after which a peer becomes stale if no new discovery event arrives.
pub const DISCOVERY_STALE_AFTER: Duration = Duration::from_secs(10);

/// Time after which a stale peer is removed from the cache.
pub const DISCOVERY_REMOVE_AFTER: Duration = Duration::from_secs(60);

/// Default cooldown between mDNS discovery client re-initialization attempts.
pub const DISCOVERY_REINIT_COOLDOWN: Duration = Duration::from_secs(5);

/// Minimum non-zero cooldown accepted from runtime configuration.
pub const DISCOVERY_REINIT_COOLDOWN_MIN_SECS: u64 = 1;

/// Maximum cooldown accepted from runtime configuration.
pub const DISCOVERY_REINIT_COOLDOWN_MAX_SECS: u64 = 30;

/// Step interval for peer discovery polling in the engine.
pub const ENGINE_PEER_DISCOVERY_STEP: Duration = Duration::from_millis(500);

/// Maximum time to wait for peer discovery in the engine.
pub const ENGINE_PEER_DISCOVERY_TIMEOUT: Duration = Duration::from_secs(30);

/// Initial wait time for discovery results in the GUI before showing results.
pub const GUI_DISCOVERY_WAIT: Duration = Duration::from_millis(750);

/// How long to cache discovery results in the GUI before refreshing.
pub const GUI_DISCOVERY_CACHE_TTL: Duration = Duration::from_millis(5000);

/// Duplicate suppression window for GUI discovery-loop errors.
pub const GUI_ERROR_SUPPRESS_WINDOW: Duration = Duration::from_secs(10);

/// Interval between GUI discovery-loop reminder logs for persistent failures.
pub const GUI_ERROR_REMINDER_INTERVAL: Duration = Duration::from_secs(30);

/// Maximum number of tracked GUI discovery error keys before oldest eviction.
pub const GUI_ERROR_KEY_CAP: usize = 64;

/// Short discovery wait for fallback when connecting to a specific peer.
pub const GUI_CONNECT_FALLBACK_DISCOVERY_WAIT: Duration = Duration::from_millis(120);

/// Time slice for polling the discovery cache during connect flow.
pub const GUI_CONNECT_CACHE_POLL_SLICE: Duration = Duration::from_millis(25);

/// Backoff between GUI auto-connect retries for the same peer.
pub const GUI_AUTO_CONNECT_RETRY_BACKOFF: Duration = Duration::from_secs(5);

/// Maximum number of peers tracked by GUI auto-connect retry bookkeeping.
pub const GUI_AUTO_CONNECT_MAX_TRACKED_PEERS: usize = 256;

/// Spaced initial announcement burst delays to overcome transient packet drops on startup.
pub const DISCOVERY_ANNOUNCE_BURST_DELAYS: [Duration; 3] = [
    Duration::from_millis(100),
    Duration::from_millis(500),
    Duration::from_millis(2000),
];

/// Timeout for an individual candidate address connect attempt before fallback.
pub const PEER_CANDIDATE_CONNECT_TIMEOUT: Duration = Duration::from_millis(2500);

/// Maximum concurrent probes during staged /24 subnet scanning.
pub const SUBNET_SCAN_CONCURRENCY: usize = 32;

/// Probe timeout for a single candidate host during subnet scanning.
pub const SUBNET_SCAN_PROBE_TIMEOUT: Duration = Duration::from_millis(350);

/// Grace period after startup before triggering background subnet scanning if no peers are found.
pub const SUBNET_SCAN_TRIGGER_GRACE: Duration = Duration::from_millis(1500);

/// Placeholder screen name advertised before display discovery completes.
pub const TEMPORARY_ADVERTISED_SCREEN: &str = "unknown";

/// Determines whether to run a fallback discovery during connect.
///
/// Returns true when the peer list is empty AND the cache is stale.
/// This triggers a quick discovery refresh to find the target peer.
pub fn should_run_connect_discovery_fallback(
    peers_empty: bool,
    cache_stale: bool,
    target_found_in_cache: bool,
) -> bool {
    cache_stale && (peers_empty || !target_found_in_cache)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn connect_fallback_runs_when_cache_empty_and_stale() {
        assert!(should_run_connect_discovery_fallback(true, true, false));
    }

    #[test]
    fn connect_fallback_runs_when_target_is_missing_from_stale_cache() {
        assert!(should_run_connect_discovery_fallback(false, true, false));
    }

    #[test]
    fn connect_fallback_skips_when_cache_is_fresh() {
        assert!(!should_run_connect_discovery_fallback(true, false, false));
    }

    #[test]
    fn connect_fallback_skips_when_target_is_already_in_stale_cache() {
        assert!(!should_run_connect_discovery_fallback(false, true, true));
    }

    #[test]
    fn discovery_resilience_policy_contract_matches_phase_targets() {
        assert_eq!(DISCOVERY_SERVICE_TYPE, "_zerog._udp.local.");
        assert_eq!(DISCOVERY_REINIT_COOLDOWN, Duration::from_secs(5));
        assert_eq!(DISCOVERY_REINIT_COOLDOWN_MIN_SECS, 1);
        assert_eq!(DISCOVERY_REINIT_COOLDOWN_MAX_SECS, 30);

        assert_eq!(GUI_ERROR_SUPPRESS_WINDOW, Duration::from_secs(10));
        assert_eq!(GUI_ERROR_REMINDER_INTERVAL, Duration::from_secs(30));
        assert_eq!(GUI_ERROR_KEY_CAP, 64);
        assert!(GUI_ERROR_KEY_CAP <= 64);

        assert_eq!(
            DISCOVERY_ANNOUNCE_BURST_DELAYS,
            [
                Duration::from_millis(100),
                Duration::from_millis(500),
                Duration::from_millis(2000),
            ]
        );
        assert_eq!(
            PEER_CANDIDATE_CONNECT_TIMEOUT,
            Duration::from_millis(2500)
        );
        assert_eq!(SUBNET_SCAN_CONCURRENCY, 32);
        assert_eq!(
            SUBNET_SCAN_PROBE_TIMEOUT,
            Duration::from_millis(350)
        );
        assert_eq!(
            SUBNET_SCAN_TRIGGER_GRACE,
            Duration::from_millis(1500)
        );
        assert_eq!(
            DISCOVERY_BROWSE_REFRESH_INTERVAL,
            Duration::from_secs(15)
        );
        assert_eq!(
            DISCOVERY_ANNOUNCE_HEARTBEAT_INTERVAL,
            Duration::from_secs(30)
        );
        assert_eq!(
            SUBNET_SCAN_POLL_INTERVAL,
            Duration::from_secs(60)
        );
    }
}
