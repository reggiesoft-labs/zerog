use anyhow::{Result, anyhow};
use std::sync::Arc;
use std::sync::atomic::AtomicU8;
use tokio::sync::mpsc;

use crate::platform::linux::wayland::capture::WaylandEventCapture;
use crate::platform::linux::wayland::clipboard::WaylandClipboardManager;
use crate::platform::linux::wayland::display::WaylandDisplayManager;
use crate::platform::linux::wayland::injection::WaylandEventInjection;
use crate::platform::linux::wayland::permission::WaylandPermissionChecker;
use crate::platform::linux::x11::capture::X11EventCapture;
use crate::platform::linux::x11::clipboard::X11ClipboardManager;
use crate::platform::linux::x11::display::X11DisplayManager;
use crate::platform::linux::x11::injection::X11EventInjection;
use crate::platform::linux::x11::permission::X11PermissionChecker;
use crate::platform::shared::engine::{
    CaptureExt, EngineBindings, create_engine_pair as create_pair_inner,
};
use crate::platform::{CapturePolicy, CapturedInputEvent, PeerEdge, PlatformEngine, SessionEngine};

impl CaptureExt for X11EventCapture {
    fn take_event_receiver(&self) -> Option<mpsc::Receiver<CapturedInputEvent>> {
        self.take_event_receiver()
    }
}

impl CaptureExt for WaylandEventCapture {
    fn take_event_receiver(&self) -> Option<mpsc::Receiver<CapturedInputEvent>> {
        self.take_event_receiver()
    }
}

pub(crate) fn create_x11_engine_pair(
    peer_edge: PeerEdge,
) -> (Arc<dyn PlatformEngine>, Arc<dyn SessionEngine>) {
    let capture_policy = Arc::new(AtomicU8::new(CapturePolicy::DeliverLocally as u8));
    let bindings = EngineBindings {
        capture: Box::new(X11EventCapture::with_capture_policy(Arc::clone(
            &capture_policy,
        ))),
        injection: Arc::new(X11EventInjection::new()),
        display: Box::new(X11DisplayManager::new()),
        permission: Box::new(X11PermissionChecker::new()),
        capture_policy,
        source_marker: crate::platform::linux::x11::marker::source_marker(),
        platform_name: "linux-x11",
        create_thread_display: Arc::new(|| Box::new(X11DisplayManager::new())),
        on_capture_installed: Arc::new(|| {}),
        on_capture_uninstalling: Arc::new(|| {}),
        clipboard: X11ClipboardManager::new()
            .map_err(|e| anyhow!("Failed to initialize clipboard: {}", e))
            .ok()
            .map(|manager| manager as Arc<dyn crate::core::platform::traits::ClipboardManager>),
    };
    create_pair_inner(peer_edge, bindings)
}

pub(crate) fn create_wayland_engine_pair(
    peer_edge: PeerEdge,
) -> Result<(Arc<dyn PlatformEngine>, Arc<dyn SessionEngine>)> {
    let capture_policy = Arc::new(AtomicU8::new(CapturePolicy::DeliverLocally as u8));
    #[cfg(feature = "linux-wayland")]
    let portal = crate::platform::linux::wayland::portal::get_or_create_portal_session()?;
    let bindings = EngineBindings {
        capture: Box::new(WaylandEventCapture::with_peer_edge(
            peer_edge,
            Arc::clone(&capture_policy),
        )),
        injection: Arc::new(WaylandEventInjection::new()),
        display: Box::new(WaylandDisplayManager::new()),
        permission: {
            #[cfg(feature = "linux-wayland")]
            {
                Box::new(WaylandPermissionChecker::with_portal(Arc::clone(&portal)))
                    as Box<dyn crate::core::platform::traits::PermissionChecker + Send>
            }
            #[cfg(not(feature = "linux-wayland"))]
            {
                Box::new(WaylandPermissionChecker::new())
                    as Box<dyn crate::core::platform::traits::PermissionChecker + Send>
            }
        },
        capture_policy,
        source_marker: crate::platform::linux::x11::marker::source_marker(),
        platform_name: "linux-wayland",
        create_thread_display: Arc::new(|| Box::new(WaylandDisplayManager::new())),
        on_capture_installed: Arc::new(|| {}),
        on_capture_uninstalling: Arc::new(|| {}),
        clipboard: WaylandClipboardManager::new()
            .inspect_err(|e| tracing::warn!("Clipboard sharing unavailable: {e}"))
            .ok()
            .map(|manager| manager as Arc<dyn crate::core::platform::traits::ClipboardManager>),
    };
    Ok(create_pair_inner(peer_edge, bindings))
}
