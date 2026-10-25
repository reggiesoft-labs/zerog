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

fn emit_start_command_flow() {
    tracing::info!(
        target: "mouse_thing::platform::shared::runtime::network",
        domain = "runtime.connection",
        peer = "Some(192.168.1.20:54321)",
        peer_id = "peer-a",
        peer_name = "Alpha",
        "sending ConnectionRequest"
    );
    tracing::info!(
        target: "mouse_thing::platform::shared::entrypoints",
        domain = "discovery.peer",
        peer = "192.168.1.20:54321",
        name = "Alpha",
        "Peer selected via mDNS"
    );
    tracing::info!(
        target: "mouse_thing::platform::shared::runtime_threads",
        domain = "runtime.connection",
        peer_id = "peer-a",
        "connection accepted by peer"
    );
    tracing::info!(
        target: "mouse_thing::platform::shared::runtime_threads",
        domain = "runtime.transition",
        transition_key = "state_change",
        from = "LocalControl",
        to = "RemoteControl",
        "runtime state transition"
    );
    tracing::warn!(
        target: "mouse_thing::platform::shared::runtime_threads",
        domain = "runtime.health",
        warning_key = "peer_timeout",
        "peer heartbeat timeout"
    );
    tracing::debug!(
        target: "mouse_thing::core::services::discovery",
        domain = "discovery.peer",
        reason_code = "peer_id_match",
        cooldown_decision = "skip_due_to_cooldown",
        "classified discovered peer locality"
    );
    tracing::trace!(
        target: "mouse_thing::platform::shared::runtime_threads",
        domain = "runtime.input",
        tick = 42,
        "runtime input tick"
    );
}

fn emit_list_command_flow() {
    tracing::info!(
        target: "mouse_thing::platform::shared::entrypoints",
        domain = "discovery.refresh",
        port = 54321,
        "mDNS discovery started"
    );
    tracing::info!(
        target: "mouse_thing::platform::shared::entrypoints",
        domain = "discovery.refresh",
        seconds = 1,
        "Listening for peers for 1 seconds"
    );
    tracing::info!(
        target: "mouse_thing::core::services::discovery",
        domain = "discovery.health",
        from = "online",
        to = "degraded",
        "discovery health transition"
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
    tracing::debug!(
        target: "mouse_thing::core::services::discovery",
        domain = "discovery.refresh",
        wait_ms = 500,
        "starting discovery refresh"
    );
    tracing::debug!(
        target: "mouse_thing::core::services::discovery",
        domain = "discovery.peer",
        peers = "[peer-a, peer-b]",
        "full peer list dump"
    );
}

#[test]
fn start_flow_normal_preset_keeps_operator_events_and_suppresses_debug_trace() {
    let writer = SharedBufferWriter::default();
    let config =
        RuntimeDiscoveryLoggingConfig::from_parts(Some("normal"), None, false, false, true);
    let subscriber = subscriber_for_config_with_writer(&config, writer.clone());

    tracing::subscriber::with_default(subscriber, emit_start_command_flow);

    let output = output_as_string(&writer);
    assert!(
        output.contains("discovery.peer Peer selected via mDNS"),
        "output={output}"
    );
    assert!(
        output.contains("runtime.connection connection accepted by peer"),
        "output={output}"
    );
    assert!(
        output.contains("runtime.connection sending ConnectionRequest"),
        "output={output}"
    );
    assert!(
        output.contains("runtime.transition runtime state transition"),
        "output={output}"
    );
    assert!(
        !output.contains("peer heartbeat timeout"),
        "output={output}"
    );
    assert!(
        !output.contains("classified discovered peer locality"),
        "output={output}"
    );
    assert!(!output.contains("runtime input tick"), "output={output}");
}

#[test]
fn start_flow_debug_preset_includes_debug_and_still_suppresses_trace_noise() {
    let writer = SharedBufferWriter::default();
    let config = RuntimeDiscoveryLoggingConfig::from_parts(Some("debug"), None, false, false, true);
    let subscriber = subscriber_for_config_with_writer(&config, writer.clone());

    tracing::subscriber::with_default(subscriber, emit_start_command_flow);

    let output = output_as_string(&writer);
    assert!(output.contains("Peer selected via mDNS"), "output={output}");
    assert!(
        output.contains("classified discovered peer locality"),
        "output={output}"
    );
    assert!(
        output.contains("reason_code=\"peer_id_match\""),
        "output={output}"
    );
    assert!(!output.contains("runtime input tick"), "output={output}");
}

#[test]
fn list_flow_normal_preset_keeps_refresh_and_fallback_events_without_debug_dump() {
    let writer = SharedBufferWriter::default();
    let config =
        RuntimeDiscoveryLoggingConfig::from_parts(Some("normal"), None, false, false, true);
    let subscriber = subscriber_for_config_with_writer(&config, writer.clone());

    tracing::subscriber::with_default(subscriber, emit_list_command_flow);

    let output = output_as_string(&writer);
    assert!(
        output.contains("discovery.refresh mDNS discovery started"),
        "output={output}"
    );
    assert!(
        output.contains("discovery.health discovery health transition"),
        "output={output}"
    );
    assert!(
        output.contains("discovery.fallback discovery runtime reinitialization failed"),
        "output={output}"
    );
    assert!(
        !output.contains("starting discovery refresh"),
        "output={output}"
    );
    assert!(!output.contains("full peer list dump"), "output={output}");
}

#[test]
fn list_flow_debug_preset_includes_diagnostics_and_all_discovery_domains() {
    let writer = SharedBufferWriter::default();
    let config = RuntimeDiscoveryLoggingConfig::from_parts(Some("debug"), None, false, false, true);
    let subscriber = subscriber_for_config_with_writer(&config, writer.clone());

    tracing::subscriber::with_default(subscriber, || {
        emit_start_command_flow();
        emit_list_command_flow();
    });

    let output = output_as_string(&writer);
    for required_domain in [
        "runtime.connection",
        "runtime.transition",
        "discovery.refresh",
        "discovery.health",
        "discovery.peer",
        "discovery.fallback",
    ] {
        assert!(
            output.contains(required_domain),
            "missing domain {required_domain} in output={output}"
        );
    }
    assert!(
        output.contains("starting discovery refresh"),
        "output={output}"
    );
    assert!(output.contains("full peer list dump"), "output={output}");
    assert!(!output.contains("runtime input tick"), "output={output}");
}
