//! Wayland backend components.

use anyhow::{Context, Result, anyhow};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use tracing::info;

use crate::platform::EventCapture;

pub(crate) mod capture;
pub(crate) mod clipboard;
pub(crate) mod display;
pub(crate) mod injection;
#[cfg(feature = "linux-wayland")]
pub(crate) mod kwin;
#[cfg(feature = "linux-wayland")]
pub(crate) mod libei_capture;
#[cfg(feature = "linux-wayland")]
pub(crate) mod libei_injection;
#[cfg(all(test, feature = "linux-wayland"))]
pub(crate) mod libei_test_support;
pub(crate) mod permission;
#[cfg(feature = "linux-wayland")]
pub(crate) mod portal;
pub(crate) mod runtime;
pub(crate) mod session;

#[allow(unused_imports)]
pub(crate) use runtime::{wayland_block_on, wayland_runtime_handle};

pub(crate) fn run_loopback() -> Result<()> {
    permission::ensure_accessibility_trusted("loopback")?;

    let capture = capture::WaylandEventCapture::new();
    let injection = injection::WaylandEventInjection::new();
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

    info!("Starting Linux Wayland loopback");
    info!("Capturing mouse input and re-injecting locally (Ctrl+C to stop)");

    crate::platform::shared::loopback::relay_events_until_shutdown(
        &mut event_rx, &injection, &shutdown,
    );

    capture.uninstall_tap()?;
    Ok(())
}
