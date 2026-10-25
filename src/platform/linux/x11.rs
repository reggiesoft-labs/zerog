//! X11 backend components.

use anyhow::{Context, Result, anyhow};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use tracing::info;

use crate::platform::EventCapture;

pub(crate) mod capture;
pub(crate) mod clipboard;
pub(crate) mod display;
pub(crate) mod injection;
pub(crate) mod marker;
pub(crate) mod permission;

pub(crate) fn run_loopback() -> Result<()> {
    permission::ensure_accessibility_trusted("loopback")?;

    let capture = capture::X11EventCapture::new();
    let injection = injection::X11EventInjection::new();
    capture.install_tap()?;
    let mut event_rx = capture
        .take_event_receiver()
        .ok_or_else(|| anyhow!("capture installed without event receiver"))?;

    let shutdown = Arc::new(AtomicBool::new(false));
    let signal_shutdown = Arc::clone(&shutdown);
    ctrlc::set_handler(move || {
        signal_shutdown.store(true, Ordering::SeqCst);
    })
    .context("failed to install Ctrl+C handler")?;

    info!("Starting Linux X11 loopback");
    info!("Capturing mouse input and re-injecting locally (Ctrl+C to stop)");

    crate::platform::shared::loopback::relay_events_until_shutdown(
        &mut event_rx, &injection, &shutdown,
    );

    capture.uninstall_tap()?;
    Ok(())
}
