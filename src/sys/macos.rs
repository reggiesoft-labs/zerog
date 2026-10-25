use anyhow::{Result, anyhow};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use tokio::sync::mpsc;

use crate::platform::macos::capture::MacosEventCapture;
use crate::platform::macos::clipboard::MacOSClipboardManager;
use crate::platform::macos::display::MacosDisplayManager;
use crate::platform::macos::injection::MacosEventInjection;
use crate::platform::macos::permission::MacosPermissionChecker;
use crate::platform::shared::engine::{
    CaptureExt, EngineBindings, create_engine_pair as create_pair_inner,
};
use crate::platform::{CapturePolicy, CapturedInputEvent, PeerEdge, PlatformEngine, SessionEngine};

impl CaptureExt for MacosEventCapture {
    fn take_event_receiver(&self) -> Option<mpsc::Receiver<CapturedInputEvent>> {
        self.take_event_receiver()
    }
}

pub(crate) fn create_engine_pair(
    peer_edge: PeerEdge,
) -> Result<(Arc<dyn PlatformEngine>, Arc<dyn SessionEngine>)> {
    let capture_policy = Arc::new(AtomicU8::new(CapturePolicy::DeliverLocally as u8));
    let capture = MacosEventCapture::new(Arc::clone(&capture_policy));
    let source_marker = capture.source_marker();
    let cf_runloop_active = Arc::new(AtomicBool::new(false));
    let bindings = EngineBindings {
        capture: Box::new(capture),
        injection: Arc::new(MacosEventInjection::new()),
        display: Box::new(MacosDisplayManager::new()),
        permission: Box::new(MacosPermissionChecker::new()),
        capture_policy,
        source_marker,
        platform_name: "macos",
        create_thread_display: Arc::new(|| Box::new(MacosDisplayManager::new())),
        on_capture_installed: {
            let cf_runloop_active = Arc::clone(&cf_runloop_active);
            Arc::new(move || cf_runloop_active.store(true, Ordering::SeqCst))
        },
        on_capture_uninstalling: {
            let cf_runloop_active = Arc::clone(&cf_runloop_active);
            Arc::new(move || cf_runloop_active.store(false, Ordering::SeqCst))
        },
        clipboard: MacOSClipboardManager::new()
            .map_err(|e| anyhow!("Failed to initialize clipboard: {}", e))
            .ok()
            .map(|manager| manager as Arc<dyn crate::core::platform::traits::ClipboardManager>),
    };
    Ok(create_pair_inner(peer_edge, bindings))
}
