use anyhow::{Result, anyhow};
use std::sync::Arc;
use std::sync::atomic::AtomicU8;
use tokio::sync::mpsc;

use crate::platform::shared::engine::{
    CaptureExt, EngineBindings, create_engine_pair as create_pair_inner,
};
use crate::platform::windows::capture::WindowsEventCapture;
use crate::platform::windows::clipboard::WindowsClipboardManager;
use crate::platform::windows::display::WindowsDisplayManager;
use crate::platform::windows::injection::WindowsEventInjection;
use crate::platform::windows::marker::local_mouse_marker;
use crate::platform::windows::service::client::ServiceBackedInjection;
use crate::platform::windows::permission::WindowsPermissionChecker;
use crate::platform::{CapturePolicy, CapturedInputEvent, PeerEdge, PlatformEngine, SessionEngine};

impl CaptureExt for WindowsEventCapture {
    fn take_event_receiver(&self) -> Option<mpsc::Receiver<CapturedInputEvent>> {
        self.take_event_receiver()
    }
}

pub(crate) fn create_engine_pair(
    peer_edge: PeerEdge,
) -> Result<(Arc<dyn PlatformEngine>, Arc<dyn SessionEngine>)> {
    let capture_policy = Arc::new(AtomicU8::new(CapturePolicy::DeliverLocally as u8));
    let bindings = EngineBindings {
        capture: Box::new(WindowsEventCapture::new(Arc::clone(&capture_policy))),
        injection: Arc::new(ServiceBackedInjection::new(
            Arc::new(WindowsEventInjection::new()),
            local_mouse_marker() as u64,
        )),
        display: Box::new(WindowsDisplayManager::new()),
        permission: Box::new(WindowsPermissionChecker::new()),
        capture_policy,
        source_marker: local_mouse_marker() as u64,
        platform_name: "windows",
        create_thread_display: Arc::new(|| Box::new(WindowsDisplayManager::new())),
        on_capture_installed: Arc::new(|| {}),
        on_capture_uninstalling: Arc::new(|| {}),
        clipboard: WindowsClipboardManager::new()
            .map_err(|e| anyhow!("Failed to initialize clipboard: {}", e))
            .ok()
            .map(|manager| manager as Arc<dyn crate::core::platform::traits::ClipboardManager>),
    };
    Ok(create_pair_inner(peer_edge, bindings))
}
