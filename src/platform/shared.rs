pub(crate) mod capture_queue;
pub(crate) mod clipboard;
pub mod clipboard_sync;
// `entrypoints.rs` and `discovery_helpers.rs` are archived CLI helpers, not on the
// production GUI path, and are intentionally not re-exported here.
// `src/bin/minimal-mdns-quic.rs` is an archived handshake/session prototype
// isolated from `RuntimeEngine` (see `findings/production-path.md`).
pub(crate) mod cursor_state;
pub(crate) mod edge_detector;
pub mod edge_probe;
pub(crate) mod engine;
pub(crate) mod layout_state;
pub(crate) mod loopback;
pub(crate) mod metrics;
pub(crate) mod network;
pub(crate) mod priority;
pub(crate) mod quic;
pub(crate) mod runtime;
pub(crate) mod session_manager;
#[doc(hidden)]
pub mod test_support;
pub mod tokio_runtime;
pub(crate) mod topology;
pub(crate) mod transition;
