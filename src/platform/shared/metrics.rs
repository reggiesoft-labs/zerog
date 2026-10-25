use std::sync::atomic::{AtomicU64, Ordering};

use crate::core::platform::ThroughputMetrics;

const METRICS_WINDOW_US: u64 = 1_000_000;

pub(crate) struct MetricsTracker {
    events_sent_total: AtomicU64,
    events_received_total: AtomicU64,
    events_sent_window: AtomicU64,
    events_received_window: AtomicU64,
    window_start_us: AtomicU64,
    moves_coalesced_total: AtomicU64,
    moves_coalesced_window: AtomicU64,
    smoother_ticks_total: AtomicU64,
    smoother_injections_total: AtomicU64,
    cadence_misses_total: AtomicU64,
}

impl MetricsTracker {
    pub(crate) fn new() -> Self {
        Self {
            events_sent_total: AtomicU64::new(0),
            events_received_total: AtomicU64::new(0),
            events_sent_window: AtomicU64::new(0),
            events_received_window: AtomicU64::new(0),
            window_start_us: AtomicU64::new(0),
            moves_coalesced_total: AtomicU64::new(0),
            moves_coalesced_window: AtomicU64::new(0),
            smoother_ticks_total: AtomicU64::new(0),
            smoother_injections_total: AtomicU64::new(0),
            cadence_misses_total: AtomicU64::new(0),
        }
    }

    pub(crate) fn get_throughput_metrics(&self, now_us: u64) -> ThroughputMetrics {
        let window_start = self.window_start_us.load(Ordering::Relaxed);

        let (events_sent, events_received, coalesced_window, elapsed_us) =
            if window_start == 0 || now_us.saturating_sub(window_start) >= METRICS_WINDOW_US {
                let sent = self.events_sent_window.swap(0, Ordering::Relaxed);
                let received = self.events_received_window.swap(0, Ordering::Relaxed);
                let coalesced = self.moves_coalesced_window.swap(0, Ordering::Relaxed);
                self.window_start_us.store(now_us, Ordering::Relaxed);
                (sent, received, coalesced, METRICS_WINDOW_US)
            } else {
                (
                    self.events_sent_window.load(Ordering::Relaxed),
                    self.events_received_window.load(Ordering::Relaxed),
                    self.moves_coalesced_window.load(Ordering::Relaxed),
                    now_us.saturating_sub(window_start),
                )
            };

        let elapsed_sec = (elapsed_us.max(1) as f64) / 1_000_000.0;

        let smoother_ticks = self.smoother_ticks_total.load(Ordering::Relaxed);
        let smoother_injections = self.smoother_injections_total.load(Ordering::Relaxed);

        ThroughputMetrics {
            out_events_per_sec: (events_sent as f64 / elapsed_sec) as u64,
            in_events_per_sec: (events_received as f64 / elapsed_sec) as u64,
            packets_sent: self.events_sent_total.load(Ordering::Relaxed),
            packets_received: self.events_received_total.load(Ordering::Relaxed),
            moves_coalesced_per_sec: (coalesced_window as f64 / elapsed_sec) as u64,
            smoother_inject_pct: if smoother_ticks > 0 {
                (smoother_injections as f64 / smoother_ticks as f64) * 100.0
            } else {
                0.0
            },
        }
    }

    pub(crate) fn record_outbound_input_events(&self, now_us: u64, count: u64) {
        if count == 0 {
            return;
        }

        self.roll_metrics_window_if_elapsed(now_us);
        self.events_sent_total.fetch_add(count, Ordering::Relaxed);
        self.events_sent_window.fetch_add(count, Ordering::Relaxed);
    }

    pub(crate) fn record_inbound_input_event(&self, now_us: u64) {
        self.roll_metrics_window_if_elapsed(now_us);
        self.events_received_total.fetch_add(1, Ordering::Relaxed);
        self.events_received_window.fetch_add(1, Ordering::Relaxed);
    }

    pub(crate) fn record_moves_coalesced(&self, count: u64) {
        self.moves_coalesced_total
            .fetch_add(count, Ordering::Relaxed);
        self.moves_coalesced_window
            .fetch_add(count, Ordering::Relaxed);
    }

    pub(crate) fn record_smoother_tick(&self, injected: bool) {
        self.smoother_ticks_total.fetch_add(1, Ordering::Relaxed);
        if injected {
            self.smoother_injections_total
                .fetch_add(1, Ordering::Relaxed);
        }
    }

    pub(crate) fn record_cadence_miss(&self) {
        self.cadence_misses_total.fetch_add(1, Ordering::Relaxed);
    }

    fn roll_metrics_window_if_elapsed(&self, now_us: u64) {
        let window_start = self.window_start_us.load(Ordering::Relaxed);
        if window_start == 0 {
            self.window_start_us.store(now_us, Ordering::Relaxed);
            return;
        }

        if now_us.saturating_sub(window_start) >= METRICS_WINDOW_US {
            self.events_sent_window.store(0, Ordering::Relaxed);
            self.events_received_window.store(0, Ordering::Relaxed);
            self.moves_coalesced_window.store(0, Ordering::Relaxed);
            self.window_start_us.store(now_us, Ordering::Relaxed);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::MetricsTracker;

    #[test]
    fn throughput_metrics_scale_with_current_window() {
        let tracker = MetricsTracker::new();

        tracker.record_outbound_input_events(100_000, 2);
        tracker.record_inbound_input_event(100_000);

        let metrics = tracker.get_throughput_metrics(600_000);
        assert_eq!(metrics.out_events_per_sec, 4);
        assert_eq!(metrics.in_events_per_sec, 2);
        assert_eq!(metrics.packets_sent, 2);
        assert_eq!(metrics.packets_received, 1);
    }

    #[test]
    fn throughput_metrics_roll_window_forward() {
        let tracker = MetricsTracker::new();

        tracker.record_outbound_input_events(100_000, 3);
        tracker.record_inbound_input_event(100_000);

        let rolled = tracker.get_throughput_metrics(1_200_000);
        assert_eq!(rolled.out_events_per_sec, 3);
        assert_eq!(rolled.in_events_per_sec, 1);
        assert_eq!(rolled.packets_sent, 3);
        assert_eq!(rolled.packets_received, 1);

        let next_window = tracker.get_throughput_metrics(1_300_000);
        assert_eq!(next_window.out_events_per_sec, 0);
        assert_eq!(next_window.in_events_per_sec, 0);
        assert_eq!(next_window.packets_sent, 3);
        assert_eq!(next_window.packets_received, 1);
    }

    #[test]
    fn coalescing_metrics_accumulate_and_roll() {
        let tracker = MetricsTracker::new();

        // First window: record 10 coalesced moves
        tracker.record_outbound_input_events(100_000, 1); // initialize window
        tracker.record_moves_coalesced(10);

        // Get metrics at 1.2s (window rolls because >= 1s)
        let metrics = tracker.get_throughput_metrics(1_200_000);
        assert_eq!(
            metrics.moves_coalesced_per_sec, 10,
            "should report 10 coalesced/sec"
        );

        // After roll, window counter should be reset
        // Record 5 more in the new window
        tracker.record_moves_coalesced(5);
        let metrics2 = tracker.get_throughput_metrics(1_500_000);
        // 5 coalesced in 0.3s = ~16/sec
        assert!(
            metrics2.moves_coalesced_per_sec > 0,
            "should have coalesced events in new window"
        );

        // Roll again, verify reset
        let metrics3 = tracker.get_throughput_metrics(2_600_000);
        // Window was rolled at 1.5s with 5 events, now at 2.6s it rolls again
        // The 5 events from previous window are reported
        assert_eq!(metrics3.moves_coalesced_per_sec, 5);
    }

    #[test]
    fn smoother_metrics_track_injection_rate() {
        let tracker = MetricsTracker::new();

        // Record 10 ticks, 6 with injection
        for i in 0..10 {
            tracker.record_smoother_tick(i < 6);
        }

        let metrics = tracker.get_throughput_metrics(1_000_000);
        assert!(
            (metrics.smoother_inject_pct - 60.0).abs() < 0.01,
            "expected 60%, got {}",
            metrics.smoother_inject_pct
        );
    }
}
