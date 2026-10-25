//! Thread lifecycle management for runtime worker threads.
//!
//! This module provides the `Runtime` struct which encapsulates all worker thread
//! spawning and joining. The `GenericPlatformEngine` delegates to `Runtime` for
//! thread management while retaining ownership of the threads.

pub(crate) mod cursor_runtime;
pub(crate) mod edge_transitions;
pub(crate) mod fragment;
pub(crate) mod loops;
pub(crate) mod motion;
pub(crate) mod network;
pub(crate) mod packet_handler;
pub(crate) mod smoother;
pub(crate) mod transport;

pub(crate) use loops::process_network_event_internal;
pub(crate) use network::{
    FragmentHeader, HEARTBEAT_INTERVAL, NetworkState, PEER_TIMEOUT, PING_INTERVAL, RuntimeConfig,
    input_activity_state,
};
pub(crate) use transport::is_input_event;

use crate::core::api::PeerRefSnapshot;
use crate::core::platform::traits::{DisplayManager, EventInjection};
use anyhow::{Context, Result};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread::JoinHandle;
use tokio::sync::mpsc;
use tracing::{debug, info};

use super::cursor_state::CursorState;
use super::edge_detector::EdgeDetector;
use super::layout_state::LayoutState;
use super::transition::TransitionState;
use crate::events::LayoutUpdateEvent;
use crate::logging::{LogRateLimiter, RateLimitDecision, RateLimitPolicy, RollupLevel};
use crate::platform::CapturedInputEvent;
use crate::platform::shared::clipboard_sync::ClipboardSync;

pub(crate) const WARN_RATE_LIMIT_POLICY: RateLimitPolicy =
    RateLimitPolicy::new(5_000, 3, RollupLevel::Debug);

pub(crate) struct RuntimeDependencies {
    pub(crate) network: Arc<NetworkState>,
    pub(crate) cursor: Arc<CursorState>,
    pub(crate) transition: Arc<TransitionState>,
    pub(crate) layout: Arc<LayoutState>,
    pub(crate) injection: Arc<dyn EventInjection>,
    pub(crate) local_identity: Option<PeerRefSnapshot>,
    pub(crate) layout_updates: mpsc::Sender<LayoutUpdateEvent>,
    pub(crate) clipboard_sync: Option<Arc<ClipboardSync>>,
    pub(crate) transfer_sync: Option<Arc<crate::core::transfer::TransferSync>>,
    pub(crate) edge_detector: Mutex<EdgeDetector>,
}

pub(crate) struct ThreadContext {
    pub(crate) dependencies: RuntimeDependencies,
    pub(crate) display: Box<dyn DisplayManager + Send>,
}

impl std::ops::Deref for ThreadContext {
    type Target = RuntimeDependencies;

    fn deref(&self) -> &Self::Target {
        &self.dependencies
    }
}

pub(crate) fn runtime_warning_limiter() -> &'static LogRateLimiter {
    static LIMITER: OnceLock<LogRateLimiter> = OnceLock::new();
    LIMITER.get_or_init(LogRateLimiter::default)
}

pub(crate) fn emit_rate_limit_rollup(
    domain: &'static str,
    warning_key: &'static str,
    decision: RateLimitDecision,
) {
    let Some(rollup) = decision.rollup else {
        return;
    };

    match rollup.level {
        RollupLevel::Info => info!(
            domain,
            warning_key,
            suppressed = rollup.suppressed,
            interval_ms = WARN_RATE_LIMIT_POLICY.interval_ms,
            "suppressed recurring warnings"
        ),
        RollupLevel::Debug => debug!(
            domain,
            warning_key,
            suppressed = rollup.suppressed,
            interval_ms = WARN_RATE_LIMIT_POLICY.interval_ms,
            "suppressed recurring warnings"
        ),
    }
}

pub(crate) struct Runtime {
    thread_handles: Mutex<Vec<JoinHandle<()>>>,
}

impl Runtime {
    pub(crate) fn new() -> Self {
        Self {
            thread_handles: Mutex::new(Vec::new()),
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn spawn(
        &self,
        platform: &str,
        create_thread_display: &(dyn Fn() -> Box<dyn DisplayManager + Send> + Send + Sync),
        injection: Arc<dyn EventInjection>,
        event_rx: mpsc::Receiver<CapturedInputEvent>,
        local_identity: Option<PeerRefSnapshot>,
        running: Arc<AtomicBool>,
        network: Arc<NetworkState>,
        cursor: Arc<CursorState>,
        transition: Arc<TransitionState>,
        layout: Arc<LayoutState>,
        layout_updates: mpsc::Sender<LayoutUpdateEvent>,
        cadence_us: u64,
        clipboard_sync: Option<Arc<ClipboardSync>>,
        transfer_sync: Option<Arc<crate::core::transfer::TransferSync>>,
    ) -> Result<()> {
        let mut handles = Vec::with_capacity(1);

        {
            let dependencies = RuntimeDependencies {
                network: Arc::clone(&network),
                cursor: Arc::clone(&cursor),
                transition: Arc::clone(&transition),
                layout: Arc::clone(&layout),
                injection: Arc::clone(&injection),
                local_identity: local_identity.clone(),
                layout_updates: layout_updates.clone(),
                clipboard_sync,
                transfer_sync,
                edge_detector: Mutex::new(EdgeDetector::new()),
            };
            let ctx = ThreadContext {
                dependencies,
                display: create_thread_display(),
            };
            let thread_running = Arc::clone(&running);
            let thread_endpoint = Arc::clone(network.endpoint());
            let handle = match std::thread::Builder::new()
                .name(format!("{platform}-engine-input"))
                .spawn(move || {
                    super::priority::set_realtime_thread_priority();
                    loops::input_loop(event_rx, thread_endpoint, thread_running, ctx, cadence_us)
                }) {
                Ok(handle) => handle,
                Err(error) => {
                    running.store(false, Ordering::SeqCst);
                    let _ = join_runtime_threads(handles);
                    return Err(error).context("failed to spawn input loop thread");
                }
            };
            handles.push(handle);
        }

        *self
            .thread_handles
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = handles;
        Ok(())
    }

    pub(crate) fn into_handles(self) -> Vec<JoinHandle<()>> {
        self.thread_handles
            .into_inner()
            .unwrap_or_else(|e| e.into_inner())
    }
}

impl Default for Runtime {
    fn default() -> Self {
        Self::new()
    }
}

/// Spawn runtime worker threads.
///
/// This is a convenience wrapper around `Runtime::spawn`. The returned handles
/// can be joined using `join_runtime_threads`.
#[allow(clippy::too_many_arguments)]
pub(crate) fn spawn_runtime_threads(
    platform: &str,
    create_thread_display: &(dyn Fn() -> Box<dyn DisplayManager + Send> + Send + Sync),
    injection: Arc<dyn EventInjection>,
    event_rx: mpsc::Receiver<CapturedInputEvent>,
    local_identity: Option<PeerRefSnapshot>,
    running: Arc<AtomicBool>,
    network: Arc<NetworkState>,
    cursor: Arc<CursorState>,
    transition: Arc<TransitionState>,
    layout: Arc<LayoutState>,
    layout_updates: mpsc::Sender<LayoutUpdateEvent>,
    cadence_us: u64,
    clipboard_sync: Option<Arc<ClipboardSync>>,
    transfer_sync: Option<Arc<crate::core::transfer::TransferSync>>,
) -> Result<Vec<JoinHandle<()>>> {
    let runtime = Runtime::new();
    runtime.spawn(
        platform,
        create_thread_display,
        injection,
        event_rx,
        local_identity,
        running,
        network,
        cursor,
        transition,
        layout,
        layout_updates,
        cadence_us,
        clipboard_sync,
        transfer_sync,
    )?;
    Ok(runtime.into_handles())
}

/// Join runtime worker threads.
pub(crate) fn join_runtime_threads(handles: Vec<JoinHandle<()>>) -> Result<()> {
    for handle in handles {
        handle
            .join()
            .map_err(|_| anyhow::anyhow!("engine worker thread panicked"))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::Runtime;

    #[test]
    fn runtime_default_construction() {
        let runtime = Runtime::default();
        let handles = runtime.thread_handles.lock().unwrap();
        assert!(handles.is_empty());
    }
}
