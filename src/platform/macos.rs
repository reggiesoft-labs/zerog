//! macOS platform module that composes extracted subsystems and runtime entrypoints.

use anyhow::{Context, Result, anyhow};
use std::path::PathBuf;
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicU8, Ordering},
};
use tracing::info;

use crate::platform::{EventCapture, PeerEdge, PlatformEngine, SessionEngine};

pub(crate) mod capture;
pub(crate) mod clipboard;
mod click_state;
pub(crate) mod display;
pub(crate) mod ffi;
pub(crate) mod injection;
pub(crate) mod permission;

pub fn create_engine_pair(
    peer_edge: PeerEdge,
) -> Result<(Arc<dyn PlatformEngine>, Arc<dyn SessionEngine>)> {
    crate::sys::os::create_engine_pair(peer_edge)
}

pub fn run_phase1_loopback() -> Result<()> {
    permission::ensure_accessibility_trusted("loopback")?;

    let capture_policy = Arc::new(AtomicU8::new(
        crate::platform::CapturePolicy::DeliverLocally as u8,
    ));
    let capture = capture::MacosEventCapture::new(capture_policy);
    let injection = injection::MacosEventInjection::new();
    capture.install_tap()?;
    let mut event_rx = capture
        .take_event_receiver()
        .ok_or_else(|| anyhow!("capture installed without event receiver"))?;

    info!("Starting phase 1 loopback");
    info!("Capturing mouse input and re-injecting locally (Ctrl+C to stop)");

    let shutdown = Arc::new(AtomicBool::new(false));
    let signal_shutdown = Arc::clone(&shutdown);
    ctrlc::set_handler(move || {
        signal_shutdown.store(true, Ordering::SeqCst);
    })
    .context("failed to install Ctrl+C handler")?;

    super::shared::loopback::relay_events_until_shutdown(&mut event_rx, &injection, &shutdown);

    capture.uninstall_tap()?;
    Ok(())
}

pub fn run_doctor() -> Result<()> {
    permission::run_doctor()
}

pub fn check_accessibility_permission() -> Result<()> {
    permission::check_accessibility_permission()
}

pub fn run_phase3_manual(_peer: String, _listen_port: u16, _peer_edge: PeerEdge) -> Result<()> {
    anyhow::bail!("Manual peer mode is removed; use `mouse-thing start` with mDNS discovery")
}

pub fn run_phase4_start(
    _listen_port: u16,
    _peer_edge: PeerEdge,
    _preferred_peer_name: Option<String>,
    _batch_window_us: u64,
    _max_events_per_packet: usize,
    _movement_cadence_us: u64,
    _metrics_log_path: Option<PathBuf>,
) -> Result<()> {
    anyhow::bail!("Phase 4 runtime is deprecated; use the GUI instead")
}

pub fn run_phase4_list(_listen_port: u16) -> Result<()> {
    anyhow::bail!("Phase 4 peer listing is deprecated; use the GUI instead")
}
