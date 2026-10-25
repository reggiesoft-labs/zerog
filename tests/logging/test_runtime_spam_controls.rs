use mouse_thing::logging::{
    LogPreset, LogRateLimiter, RateLimitPolicy, RollupLevel, RuntimeDiscoveryLoggingConfig,
    subscriber_for_config_with_writer,
};
use std::io;
use std::sync::{Arc, Mutex};

#[derive(Clone, Default)]
struct SharedBufferWriter {
    data: Arc<Mutex<Vec<u8>>>,
}

struct SharedBufferGuard {
    data: Arc<Mutex<Vec<u8>>>,
}

impl io::Write for SharedBufferGuard {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.data
            .lock()
            .expect("shared buffer lock poisoned")
            .extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl<'a> tracing_subscriber::fmt::writer::MakeWriter<'a> for SharedBufferWriter {
    type Writer = SharedBufferGuard;

    fn make_writer(&'a self) -> Self::Writer {
        SharedBufferGuard {
            data: Arc::clone(&self.data),
        }
    }
}

fn buffer_to_string(writer: &SharedBufferWriter) -> String {
    String::from_utf8(
        writer
            .data
            .lock()
            .expect("shared buffer lock poisoned")
            .clone(),
    )
    .expect("buffer should be valid utf-8")
}

#[test]
fn normal_preset_suppresses_high_frequency_trace_ticks() {
    let writer = SharedBufferWriter::default();
    let config =
        RuntimeDiscoveryLoggingConfig::from_parts(Some("normal"), None, false, false, true);
    let subscriber = subscriber_for_config_with_writer(&config, writer.clone());

    tracing::subscriber::with_default(subscriber, || {
        for tick in 0..100 {
            tracing::trace!(
                target: "mouse_thing::platform::shared::runtime_threads",
                domain = "runtime.input",
                tick,
                "runtime input tick"
            );
        }
    });

    let output = buffer_to_string(&writer);
    assert!(!output.contains("runtime input tick"));
}

#[test]
fn transition_sequence_logs_once_per_state_change_at_default_visibility() {
    let writer = SharedBufferWriter::default();
    let config =
        RuntimeDiscoveryLoggingConfig::from_parts(Some("normal"), None, false, false, true);
    let subscriber = subscriber_for_config_with_writer(&config, writer.clone());

    tracing::subscriber::with_default(subscriber, || {
        tracing::info!(
            target: "mouse_thing::platform::shared::runtime_threads",
            domain = "runtime.transition",
            transition_key = "state_change",
            from = "Idle",
            to = "Connected",
            "runtime state transition"
        );
        tracing::info!(
            target: "mouse_thing::platform::shared::runtime_threads",
            domain = "runtime.transition",
            transition_key = "state_change",
            from = "Connected",
            to = "Active",
            "runtime state transition"
        );
        tracing::info!(
            target: "mouse_thing::platform::shared::runtime_threads",
            domain = "runtime.transition",
            transition_key = "state_change",
            from = "Active",
            to = "Idle",
            "runtime state transition"
        );
    });

    let output = buffer_to_string(&writer);
    let transition_count = output
        .lines()
        .filter(|line| line.contains("runtime state transition"))
        .count();
    assert_eq!(transition_count, 3);
}

#[test]
fn repeated_failures_are_rate_limited_and_report_suppressed_counts() {
    let writer = SharedBufferWriter::default();
    let config = RuntimeDiscoveryLoggingConfig::from_parts(Some("debug"), None, false, false, true);
    let subscriber = subscriber_for_config_with_writer(&config, writer.clone());
    let limiter = LogRateLimiter::default();
    let policy = RateLimitPolicy::new(1_000, 3, RollupLevel::Debug);

    tracing::subscriber::with_default(subscriber, || {
        let start_ms = 10_000;
        for attempt in 0..20 {
            let decision = limiter.evaluate_at("recoverable_warning", policy, start_ms);
            if decision.emit_now {
                tracing::warn!(
                    target: "mouse_thing::platform::shared::runtime_threads",
                    domain = "runtime.health",
                    warning_key = "recoverable_warning",
                    suppressed_since_last = decision.suppressed_since_last_emit,
                    attempt,
                    "recoverable runtime warning"
                );
            }
        }

        let rollover = limiter.evaluate_at("recoverable_warning", policy, start_ms + 1_001);
        if let Some(rollup) = rollover.rollup {
            match rollup.level {
                RollupLevel::Info => tracing::info!(
                    target: "mouse_thing::platform::shared::runtime_threads",
                    domain = "runtime.health",
                    warning_key = "recoverable_warning",
                    suppressed = rollup.suppressed,
                    "suppressed recurring warnings"
                ),
                RollupLevel::Debug => tracing::debug!(
                    target: "mouse_thing::platform::shared::runtime_threads",
                    domain = "runtime.health",
                    warning_key = "recoverable_warning",
                    suppressed = rollup.suppressed,
                    "suppressed recurring warnings"
                ),
            }
        }
    });

    let output = buffer_to_string(&writer);
    let warning_count = output
        .lines()
        .filter(|line| line.contains("recoverable runtime warning"))
        .count();
    assert!(
        warning_count <= 3,
        "warning_count={warning_count}, output={output}"
    );
    assert!(output.contains("suppressed recurring warnings"));
    assert!(output.contains("suppressed=17"), "output={output}");
}

#[test]
fn trace_preset_keeps_runtime_trace_visibility_opt_in() {
    assert!(
        LogPreset::Trace
            .filter_directive()
            .contains("runtime_threads=trace")
    );
}
