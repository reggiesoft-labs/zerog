//! Archived CLI entrypoint helpers — not on the production GUI path.
//!
//! This module is intentionally excluded from `crate::platform::shared`'s public
//! module tree (see `src/platform/shared.rs`). The production runtime is
//! `Controller`/`RunningController` + `RuntimeEngine` via the Tauri GUI.
//! `minimal-mdns-quic` is a separate archived prototype (`src/bin/minimal-mdns-quic.rs`).

use crate::core::api::PeerRefSnapshot;
use anyhow::{Context, Result};
use std::path::PathBuf;
use std::time::{Duration, Instant};
use tokio::sync::mpsc;
use tracing::{info, warn};

use crate::core::config;
use crate::core::discovery::{DiscoveryClient, DiscoveryState, PeerInfo};
use crate::platform::{PeerEdge, RuntimeEngine};

use super::discovery_helpers;

/// Starts a platform engine with mDNS discovery and runs until Ctrl+C.
///
/// `screen_resolution` should come from the platform's display module.
/// `ensure_permission` is called before anything else (no-op on platforms that
/// don't need a pre-check).
#[allow(clippy::too_many_arguments)]
pub(crate) fn run_phase4_start(
    listen_port: u16,
    peer_edge: PeerEdge,
    preferred_peer_name: Option<String>,
    batch_window_us: u64,
    max_events_per_packet: usize,
    movement_cadence_us: u64,
    metrics_log_path: Option<PathBuf>,
    ensure_permission: impl FnOnce() -> Result<()>,
    engine: &dyn RuntimeEngine,
) -> Result<()> {
    ensure_permission()?;
    let local_peer_id = config::load_or_create_peer_id()?;

    // Get local hostname for filtering
    let local_hostname = hostname::get()
        .ok()
        .and_then(|h| h.into_string().ok())
        .filter(|h| !h.trim().is_empty())
        .unwrap_or_else(|| format!("zerog-{}", std::process::id()));

    // Use discovery_helpers::wait_for_peer which handles the new API internally
    let peer = discovery_helpers::wait_for_peer(
        listen_port,
        &local_hostname,
        preferred_peer_name.as_deref(),
    )?;

    info!(peer = %peer.addr, name = %peer.name, "Peer selected via mDNS");
    println!(
        "selected peer: {} ({}) addr={}",
        peer.name, peer.hostname, peer.addr,
    );

    if metrics_log_path.is_some() {
        warn!("metrics logging path is currently ignored by trait-based runtime");
    }

    let cert_base = dirs::config_dir().unwrap_or_else(|| PathBuf::from("."));
    let cert_path = {
        let new_path = cert_base.join("zerog").join("certs");
        if !new_path.exists() && cert_base.join("mouse-thing").join("certs").exists() {
            cert_base.join("mouse-thing").join("certs")
        } else {
            new_path
        }
    };

    engine.configure_runtime(
        listen_port,
        batch_window_us,
        max_events_per_packet,
        movement_cadence_us,
        local_peer_id.clone(),
        cert_path,
        500,
        2000,
    )?;

    // Note: peer.peer_id may be empty if we only have discovery info
    // The actual connection will learn the peer_id via the stream hello handshake
    engine.add_peer(
        PeerRefSnapshot {
            peer_id: peer.peer_id.clone(),
            name: peer.name.clone(),
        },
        peer.addr,
        peer_edge,
        peer.display_bounds,
    )?;

    engine.start()?;

    println!(
        "status: connected_peer={} edge={} batch_window_us={} max_events_per_packet={} (Ctrl+C to stop)",
        peer.addr,
        peer_edge.as_str(),
        batch_window_us,
        max_events_per_packet,
    );

    let shutdown_result = wait_for_ctrl_c();
    let stop_result = engine.stop();
    shutdown_result?;
    stop_result
}

/// Lists peers discovered on the local network via mDNS.
pub(crate) fn run_phase4_list(listen_port: u16) -> Result<()> {
    let max_wait = Duration::from_secs(5);
    let deadline = Instant::now() + max_wait;

    // Create and start discovery client
    let mut discovery = DiscoveryClient::new()?;
    let mut rx = discovery.start(listen_port, None, None, None)?;

    // Create local state to track peers
    let mut state = DiscoveryState::default();

    info!(port = listen_port, "mDNS discovery started");
    info!("Listening for peers for {} seconds", max_wait.as_secs());

    // Collect peers for the duration
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            break;
        }

        match rx.try_recv() {
            Ok(event) => {
                state.apply(event, Instant::now());
            }
            Err(mpsc::error::TryRecvError::Empty) => {
                std::thread::sleep(Duration::from_millis(100));
            }
            Err(mpsc::error::TryRecvError::Disconnected) => {
                break;
            }
        }
    }

    // Stop discovery
    discovery.stop();

    // Get peers from state - now filtering by hostname instead of peer_id
    let peers: Vec<PeerInfo> = state.snapshot(Instant::now()).into_iter().collect();

    discovery_helpers::print_peer_list(peers);
    Ok(())
}

/// Blocks until the user presses Ctrl+C.
pub(crate) fn wait_for_ctrl_c() -> Result<()> {
    let (signal_tx, signal_rx) = mpsc::channel();
    ctrlc::set_handler(move || {
        let _ = signal_tx.send(());
    })
    .context("failed to install Ctrl+C handler")?;
    signal_rx
        .recv()
        .context("failed while waiting for Ctrl+C signal")
}
