//! Telemetry and logging for the GUI command layer.
//!
//! Provides standardized formatting for logging command errors, timing,
//! performance metrics, and engine lifecycle events to stderr.
//!
//! # Log Format
//!
//! Logs use special prefixes to help filter messages in terminal or logs:
//! - `[gui-command]` : Command results and errors
//! - `[gui-perf]`    : Timing and connection metrics
//! - `[engine-toggle]`: Engine start/stop lifecycle
//! - `[gui-error-banner]`: Critical errors reported from the frontend

use crate::CommandError;
use std::time::Instant;

/// Logs a command error with detailed context.
pub(crate) fn log_command_error(command: &str, error: &CommandError) {
    match &error.details {
        Some(details) => eprintln!(
            "[gui-command] {command} failed: {} ({}) [{}]",
            error.code, error.message, details
        ),
        None => eprintln!(
            "[gui-command] {command} failed: {} ({})",
            error.code, error.message
        ),
    }
}

pub(crate) fn log_command_result<T>(
    command: &str,
    result: Result<T, CommandError>,
) -> Result<T, CommandError> {
    if let Err(error) = &result {
        log_command_error(command, error);
    }
    result
}

pub(crate) fn log_command_timing(command: &str, started_at: Instant) {
    let elapsed_ms = started_at.elapsed().as_secs_f64() * 1000.0;
    if elapsed_ms >= 100.0 {
        eprintln!("[gui-perf] {command} completed in {elapsed_ms:.1}ms");
    }
}

pub(crate) fn log_engine_toggle_received(command: &str) {
    eprintln!("[engine-toggle] {command} command received");
}

pub(crate) fn log_engine_toggle_result<T>(command: &str, result: &Result<T, CommandError>) {
    match result {
        Ok(_) => eprintln!("[engine-toggle] {command} command completed"),
        Err(error) => eprintln!(
            "[engine-toggle] {command} command failed: {} ({})",
            error.code, error.message
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::{HashMap, VecDeque};
    use std::time::Duration;

    #[derive(Debug, Default)]
    struct DiscoveryErrorKeyState {
        last_logged_at: Option<Instant>,
        suppressed_since_last_log: u64,
        suppressed_total: u64,
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum DiscoveryLogDecision {
        First,
        Reminder { suppressed_count: u64 },
        Suppressed,
    }

    #[derive(Debug)]
    struct DiscoveryErrorSuppressionState {
        suppress_window: Duration,
        reminder_interval: Duration,
        key_cap: usize,
        active_error_key: Option<String>,
        keys_by_age: VecDeque<String>,
        keys: HashMap<String, DiscoveryErrorKeyState>,
    }

    impl DiscoveryErrorSuppressionState {
        fn new(suppress_window: Duration, reminder_interval: Duration, key_cap: usize) -> Self {
            Self {
                suppress_window,
                reminder_interval,
                key_cap,
                active_error_key: None,
                keys_by_age: VecDeque::new(),
                keys: HashMap::new(),
            }
        }

        fn handle_error(&mut self, error_key: String, now: Instant) -> DiscoveryLogDecision {
            if self
                .active_error_key
                .as_ref()
                .map_or(true, |active| active != &error_key)
            {
                self.active_error_key = Some(error_key.clone());
                self.reset_key(error_key, now);
                return DiscoveryLogDecision::First;
            }

            let state = self
                .keys
                .entry(error_key.clone())
                .or_insert_with(DiscoveryErrorKeyState::default);

            if self.suppress_window.is_zero() {
                state.last_logged_at = Some(now);
                state.suppressed_since_last_log = 0;
                return DiscoveryLogDecision::First;
            }

            let elapsed_since_last_log = state
                .last_logged_at
                .map(|last| now.saturating_duration_since(last))
                .unwrap_or(Duration::ZERO);

            if elapsed_since_last_log >= self.reminder_interval {
                let suppressed_count = state.suppressed_since_last_log;
                state.last_logged_at = Some(now);
                state.suppressed_since_last_log = 0;
                return DiscoveryLogDecision::Reminder { suppressed_count };
            }

            if elapsed_since_last_log < self.suppress_window {
                state.suppressed_since_last_log += 1;
                state.suppressed_total += 1;
                return DiscoveryLogDecision::Suppressed;
            }

            if elapsed_since_last_log < self.reminder_interval {
                state.suppressed_since_last_log += 1;
                state.suppressed_total += 1;
                return DiscoveryLogDecision::Suppressed;
            }

            state.last_logged_at = Some(now);
            state.suppressed_since_last_log = 0;
            DiscoveryLogDecision::First
        }

        fn handle_success(&mut self) -> bool {
            if let Some(active_key) = self.active_error_key.take() {
                self.keys.remove(&active_key);
                self.keys_by_age.retain(|key| key != &active_key);
                return true;
            }
            false
        }

        fn active_keys(&self) -> usize {
            self.keys.len()
        }

        fn reset_key(&mut self, error_key: String, now: Instant) {
            if !self.keys.contains_key(&error_key) {
                self.keys_by_age.push_back(error_key.clone());
                self.keys.insert(
                    error_key.clone(),
                    DiscoveryErrorKeyState {
                        last_logged_at: Some(now),
                        suppressed_since_last_log: 0,
                        suppressed_total: 0,
                    },
                );
            } else if let Some(state) = self.keys.get_mut(&error_key) {
                state.last_logged_at = Some(now);
                state.suppressed_since_last_log = 0;
                state.suppressed_total = 0;
            }

            while self.keys.len() > self.key_cap {
                if let Some(oldest_key) = self.keys_by_age.pop_front() {
                    self.keys.remove(&oldest_key);
                    if self.active_error_key.as_ref() == Some(&oldest_key) {
                        self.active_error_key = None;
                    }
                } else {
                    break;
                }
            }
        }
    }

    /// Constructs a unique key for an error to enable deduplication across retries.
    fn discovery_error_key(command: &str, error: &CommandError) -> String {
        let details = error.details.as_deref().unwrap_or(error.message.as_str());
        format!("{command}|{}|{details}", error.code)
    }

    fn test_error(details: &str) -> CommandError {
        CommandError {
            code: "discovery_error",
            message: "Peer discovery failed".to_string(),
            details: Some(details.to_string()),
        }
    }

    #[test]
    fn persistent_identical_error_for_sixty_seconds_is_bounded_to_first_and_reminders() {
        let mut state = DiscoveryErrorSuppressionState::new(
            Duration::from_secs(10),
            Duration::from_secs(30),
            64,
        );
        let start = Instant::now();
        let key = discovery_error_key("peer_discovery_loop", &test_error("mdns timeout"));

        let mut visible_logs = 0;
        for sec in (0..=60).step_by(5) {
            let now = start + Duration::from_secs(sec);
            match state.handle_error(key.clone(), now) {
                DiscoveryLogDecision::First | DiscoveryLogDecision::Reminder { .. } => {
                    visible_logs += 1;
                }
                DiscoveryLogDecision::Suppressed => {}
            }
        }

        assert_eq!(visible_logs, 3);
    }

    #[test]
    fn distinct_errors_are_tracked_independently() {
        let mut state = DiscoveryErrorSuppressionState::new(
            Duration::from_secs(10),
            Duration::from_secs(30),
            64,
        );
        let start = Instant::now();

        let first_key = discovery_error_key("peer_discovery_loop", &test_error("mdns timeout"));
        let second_key =
            discovery_error_key("peer_discovery_loop", &test_error("socket unavailable"));

        assert_eq!(
            state.handle_error(first_key, start),
            DiscoveryLogDecision::First
        );
        assert_eq!(
            state.handle_error(second_key, start + Duration::from_secs(1)),
            DiscoveryLogDecision::First
        );
    }

    #[test]
    fn recovery_resets_suppression_for_next_failure() {
        let mut state = DiscoveryErrorSuppressionState::new(
            Duration::from_secs(10),
            Duration::from_secs(30),
            64,
        );
        let start = Instant::now();
        let key = discovery_error_key("peer_discovery_loop", &test_error("mdns timeout"));

        assert_eq!(
            state.handle_error(key.clone(), start),
            DiscoveryLogDecision::First
        );
        assert_eq!(
            state.handle_error(key.clone(), start + Duration::from_secs(5)),
            DiscoveryLogDecision::Suppressed
        );
        assert!(state.handle_success());
        assert_eq!(
            state.handle_error(key, start + Duration::from_secs(6)),
            DiscoveryLogDecision::First
        );
    }

    #[test]
    fn loop_restart_starts_with_clean_state() {
        let start = Instant::now();
        let key = discovery_error_key("peer_discovery_loop", &test_error("mdns timeout"));

        let mut first = DiscoveryErrorSuppressionState::new(
            Duration::from_secs(10),
            Duration::from_secs(30),
            64,
        );
        assert_eq!(
            first.handle_error(key.clone(), start),
            DiscoveryLogDecision::First
        );

        let mut restarted = DiscoveryErrorSuppressionState::new(
            Duration::from_secs(10),
            Duration::from_secs(30),
            64,
        );
        assert_eq!(
            restarted.handle_error(key, start + Duration::from_secs(1)),
            DiscoveryLogDecision::First
        );
    }

    #[test]
    fn suppression_key_cap_evicts_oldest_and_never_exceeds_sixty_four() {
        let mut state = DiscoveryErrorSuppressionState::new(
            Duration::from_secs(10),
            Duration::from_secs(30),
            64,
        );
        let start = Instant::now();

        for idx in 0..100 {
            let key = format!("peer_discovery_loop|discovery_error|root-cause-{idx}");
            let _ = state.handle_error(key, start + Duration::from_secs(idx));
            assert!(state.active_keys() <= 64);
        }

        assert_eq!(state.active_keys(), 64);
        assert!(!state
            .keys
            .contains_key("peer_discovery_loop|discovery_error|root-cause-0"));
        assert!(state
            .keys
            .contains_key("peer_discovery_loop|discovery_error|root-cause-99"));
    }

    #[test]
    fn suppress_zero_disables_deduplication_immediately() {
        let mut state =
            DiscoveryErrorSuppressionState::new(Duration::ZERO, Duration::from_secs(30), 64);
        let start = Instant::now();
        let key = discovery_error_key("peer_discovery_loop", &test_error("mdns timeout"));

        assert_eq!(
            state.handle_error(key.clone(), start),
            DiscoveryLogDecision::First
        );
        assert_eq!(
            state.handle_error(key, start + Duration::from_secs(1)),
            DiscoveryLogDecision::First
        );
    }

    #[test]
    fn reminder_reports_suppressed_count_since_last_visible_log() {
        let mut state = DiscoveryErrorSuppressionState::new(
            Duration::from_secs(10),
            Duration::from_secs(30),
            64,
        );
        let start = Instant::now();
        let key = discovery_error_key("peer_discovery_loop", &test_error("mdns timeout"));

        assert_eq!(
            state.handle_error(key.clone(), start),
            DiscoveryLogDecision::First
        );
        assert_eq!(
            state.handle_error(key.clone(), start + Duration::from_secs(5)),
            DiscoveryLogDecision::Suppressed
        );
        assert_eq!(
            state.handle_error(key.clone(), start + Duration::from_secs(10)),
            DiscoveryLogDecision::Suppressed
        );

        assert_eq!(
            state.handle_error(key.clone(), start + Duration::from_secs(30)),
            DiscoveryLogDecision::Reminder {
                suppressed_count: 2
            }
        );

        assert_eq!(
            state.handle_error(key, start + Duration::from_secs(35)),
            DiscoveryLogDecision::Suppressed
        );
    }
}
