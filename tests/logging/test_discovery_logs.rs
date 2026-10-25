use mouse_thing::logging::{RuntimeDiscoveryLoggingConfig, subscriber_for_config_with_writer};
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

fn output_as_string(writer: &SharedBufferWriter) -> String {
    String::from_utf8(
        writer
            .data
            .lock()
            .expect("shared buffer lock poisoned")
            .clone(),
    )
    .expect("log buffer should contain utf-8")
}

fn emit_discovery_refresh_scenario() {
    tracing::debug!(
        target: "mouse_thing::core::services::discovery",
        domain = "discovery.refresh",
        wait_ms = 250,
        "starting discovery refresh"
    );
    tracing::info!(
        target: "mouse_thing::core::services::discovery",
        domain = "discovery.health",
        from = "online",
        to = "degraded",
        "discovery health transition"
    );
    tracing::info!(
        target: "mouse_thing::core::services::discovery",
        domain = "discovery.peer",
        change = "added",
        peer_id = "peer-a",
        peer_name = "Alpha",
        addr = "192.168.1.20:54321",
        "peer-set update: added peer"
    );
    tracing::info!(
        target: "mouse_thing::core::services::discovery",
        domain = "discovery.peer",
        change = "stale",
        peer_id = "peer-a",
        peer_name = "Alpha",
        addr = "192.168.1.20:54321",
        "peer-set update: stale peer"
    );
    tracing::info!(
        target: "mouse_thing::core::services::discovery",
        domain = "discovery.peer",
        change = "recovered",
        peer_id = "peer-a",
        peer_name = "Alpha",
        addr = "192.168.1.30:54321",
        "peer-set update: recovered peer"
    );
    tracing::info!(
        target: "mouse_thing::core::services::discovery",
        domain = "discovery.peer",
        change = "removed",
        peer_id = "peer-b",
        peer_name = "Beta",
        addr = "192.168.1.21:54321",
        "peer-set update: removed peer"
    );
    tracing::debug!(
        target: "mouse_thing::core::services::discovery",
        domain = "discovery.peer",
        reason_code = "peer_id_match",
        cooldown_decision = "skip_due_to_cooldown",
        "classified discovered peer locality"
    );
    tracing::debug!(
        target: "mouse_thing::core::services::discovery",
        domain = "discovery.peer",
        peers = "[peer-a, peer-b]",
        "full peer list dump"
    );
    tracing::warn!(
        target: "mouse_thing::core::services::discovery",
        domain = "discovery.fallback",
        attempted_strategy = "reinitialize_runtime",
        error_type = "runtime_factory",
        retry_decision = "retry_next_refresh",
        next_action = "serve_cached_peers",
        "discovery runtime reinitialization failed"
    );
}

#[test]
fn default_discovery_logs_show_health_and_peer_diffs_without_legacy_prefixes() {
    let writer = SharedBufferWriter::default();
    let config =
        RuntimeDiscoveryLoggingConfig::from_parts(Some("normal"), None, false, false, true);
    let subscriber = subscriber_for_config_with_writer(&config, writer.clone());

    tracing::subscriber::with_default(subscriber, emit_discovery_refresh_scenario);

    let output = output_as_string(&writer);
    assert!(
        output.contains("discovery health transition"),
        "output={output}"
    );
    assert!(output.contains("change=\"added\""), "output={output}");
    assert!(output.contains("change=\"stale\""), "output={output}");
    assert!(output.contains("change=\"recovered\""), "output={output}");
    assert!(output.contains("change=\"removed\""), "output={output}");
    assert!(!output.contains("[DIAG]"), "output={output}");
    assert!(!output.contains("[OBS]"), "output={output}");
    assert!(!output.contains("full peer list dump"), "output={output}");
}

#[test]
fn debug_discovery_logs_include_classification_and_cooldown_rationale() {
    let writer = SharedBufferWriter::default();
    let config = RuntimeDiscoveryLoggingConfig::from_parts(Some("debug"), None, false, false, true);
    let subscriber = subscriber_for_config_with_writer(&config, writer.clone());

    tracing::subscriber::with_default(subscriber, emit_discovery_refresh_scenario);

    let output = output_as_string(&writer);
    assert!(
        output.contains("classified discovered peer locality"),
        "output={output}"
    );
    assert!(
        output.contains("reason_code=\"peer_id_match\""),
        "output={output}"
    );
    assert!(
        output.contains("cooldown_decision=\"skip_due_to_cooldown\""),
        "output={output}"
    );
}

#[test]
fn fallback_logs_include_strategy_error_and_next_action_context_fields() {
    let writer = SharedBufferWriter::default();
    let config =
        RuntimeDiscoveryLoggingConfig::from_parts(Some("normal"), None, false, false, true);
    let subscriber = subscriber_for_config_with_writer(&config, writer.clone());

    tracing::subscriber::with_default(subscriber, emit_discovery_refresh_scenario);

    let output = output_as_string(&writer);
    assert!(
        output.contains("attempted_strategy=\"reinitialize_runtime\""),
        "output={output}"
    );
    assert!(
        output.contains("error_type=\"runtime_factory\""),
        "output={output}"
    );
    assert!(
        output.contains("retry_decision=\"retry_next_refresh\""),
        "output={output}"
    );
    assert!(
        output.contains("next_action=\"serve_cached_peers\""),
        "output={output}"
    );
}
