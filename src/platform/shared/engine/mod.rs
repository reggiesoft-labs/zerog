pub(crate) mod test_ext;

use anyhow::{Result, anyhow};
use std::sync::atomic::AtomicU8;
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc;
use tracing::info;

use crate::core::api::PeerRefSnapshot;
use crate::core::platform::EdgeAssignments;
use crate::core::platform::engine::SessionEvent;
use crate::core::platform::traits::ClipboardManager;
use crate::events::LayoutUpdateEvent;
use crate::platform::{
    CapturedInputEvent, DisplayBounds, DisplayManager, EventCapture, EventInjection, PeerEdge,
    PermissionChecker, PlatformEngine, SessionEngine,
};
use crate::quic::PeerConnection;
use std::net::SocketAddr;

use super::session_manager::SessionManager;

pub(crate) use super::session_manager::RuntimeConfig;

pub(crate) trait CaptureExt: EventCapture + Send + Sync {
    fn take_event_receiver(&self) -> Option<mpsc::Receiver<CapturedInputEvent>>;
}

pub(crate) type ThreadDisplayFactory =
    Arc<dyn Fn() -> Box<dyn DisplayManager + Send> + Send + Sync>;
pub(crate) type LifecycleHook = Arc<dyn Fn() + Send + Sync>;

pub(crate) struct EngineBindings {
    pub(crate) capture: Box<dyn CaptureExt>,
    pub(crate) injection: Arc<dyn EventInjection>,
    pub(crate) display: Box<dyn DisplayManager + Send + Sync>,
    pub(crate) permission: Box<dyn PermissionChecker + Send>,
    pub(crate) capture_policy: Arc<AtomicU8>,
    pub(crate) source_marker: u64,
    pub(crate) platform_name: &'static str,
    pub(crate) create_thread_display: ThreadDisplayFactory,
    pub(crate) on_capture_installed: LifecycleHook,
    pub(crate) on_capture_uninstalling: LifecycleHook,
    pub(crate) clipboard: Option<Arc<dyn ClipboardManager>>,
}

pub(crate) struct PlatformEngineImpl {
    capture: Box<dyn CaptureExt>,
    display: Arc<dyn DisplayManager + Send + Sync>,
    permission: Box<dyn PermissionChecker + Send>,
    on_capture_installed: LifecycleHook,
    on_capture_uninstalling: LifecycleHook,
    lifecycle: Mutex<()>,
}

impl PlatformEngineImpl {
    fn new(
        capture: Box<dyn CaptureExt>,
        display: Arc<dyn DisplayManager + Send + Sync>,
        permission: Box<dyn PermissionChecker + Send>,
        on_capture_installed: LifecycleHook,
        on_capture_uninstalling: LifecycleHook,
    ) -> Self {
        Self {
            capture,
            display,
            permission,
            on_capture_installed,
            on_capture_uninstalling,
            lifecycle: Mutex::new(()),
        }
    }

    fn install_capture(&self) -> Result<mpsc::Receiver<CapturedInputEvent>> {
        self.capture.install_tap()?;
        let event_rx = match self.capture.take_event_receiver() {
            Some(event_rx) => event_rx,
            None => {
                let _ = self.capture.uninstall_tap();
                return Err(anyhow!("capture installed without event receiver"));
            }
        };
        (self.on_capture_installed)();
        Ok(event_rx)
    }

    fn uninstall_capture(&self) -> Result<()> {
        (self.on_capture_uninstalling)();
        self.capture.uninstall_tap()
    }
}

impl PlatformEngine for PlatformEngineImpl {
    fn start(&self) -> Result<mpsc::Receiver<CapturedInputEvent>> {
        let _guard = self.lifecycle.lock().unwrap_or_else(|e| e.into_inner());
        if self.capture.is_installed() {
            anyhow::bail!("platform capture is already running");
        }
        if !self.permission.check_accessibility_permission() {
            anyhow::bail!("accessibility permission is required before starting engine runtime");
        }
        let event_rx = self.install_capture()?;
        info!("{} platform engine started (capture only)", "platform");
        Ok(event_rx)
    }

    fn stop(&self) -> Result<()> {
        let _guard = self.lifecycle.lock().unwrap_or_else(|e| e.into_inner());
        if !self.capture.is_installed() {
            return Ok(());
        }
        self.uninstall_capture()?;
        Ok(())
    }

    fn get_display_manager(&self) -> &dyn DisplayManager {
        &*self.display
    }

    fn get_permission_checker(&self) -> &dyn PermissionChecker {
        &*self.permission
    }
}

pub(crate) struct RuntimeEngineImpl {
    platform: PlatformEngineImpl,
    session: Arc<SessionManager>,
    lifecycle: Mutex<()>,
}

impl RuntimeEngineImpl {
    pub(crate) fn from_bindings(_peer_edge: PeerEdge, bindings: EngineBindings) -> Self {
        let shared_display: Arc<dyn DisplayManager + Send + Sync> = bindings.display.into();
        let session = Arc::new(SessionManager::new(
            Arc::clone(&shared_display),
            bindings.injection,
            Arc::clone(&bindings.capture_policy),
            bindings.source_marker,
            bindings.platform_name,
            Arc::clone(&bindings.create_thread_display),
            bindings.clipboard.clone(),
        ));
        let platform = PlatformEngineImpl::new(
            bindings.capture,
            shared_display,
            bindings.permission,
            bindings.on_capture_installed,
            bindings.on_capture_uninstalling,
        );
        Self {
            platform,
            session,
            lifecycle: Mutex::new(()),
        }
    }

    #[allow(dead_code)]
    pub(crate) fn set_runtime_config(&self, config: RuntimeConfig) -> Result<()> {
        self.session.set_runtime_config(config)
    }

    #[allow(dead_code)]
    pub(crate) fn session_handle(&self) -> Arc<SessionManager> {
        Arc::clone(&self.session)
    }

    #[allow(dead_code)]
    pub(crate) fn platform_handle(&self) -> &PlatformEngineImpl {
        &self.platform
    }
}

impl PlatformEngine for RuntimeEngineImpl {
    fn start(&self) -> Result<mpsc::Receiver<CapturedInputEvent>> {
        let _guard = self.lifecycle.lock().unwrap_or_else(|e| e.into_inner());
        if self.session.is_running() {
            anyhow::bail!("session runtime is already running");
        }
        self.platform.start()
    }

    fn stop(&self) -> Result<()> {
        let _guard = self.lifecycle.lock().unwrap_or_else(|e| e.into_inner());
        let is_running = self.session.is_running();
        if !is_running {
            if self.platform.capture.is_installed() {
                (self.platform.on_capture_uninstalling)();
                let _ = self.platform.capture.uninstall_tap();
            }
            return Ok(());
        }
        (self.platform.on_capture_uninstalling)();
        let mut stop_error: Option<anyhow::Error> = None;
        if let Err(err) = self.platform.capture.uninstall_tap() {
            stop_error = Some(anyhow!(
                "failed to uninstall capture tap during stop: {err}"
            ));
        }
        if let Err(err) = self.session.stop_network()
            && stop_error.is_none()
        {
            stop_error = Some(err);
        }
        if let Some(err) = stop_error {
            return Err(err);
        }
        Ok(())
    }

    fn get_display_manager(&self) -> &dyn DisplayManager {
        self.platform.get_display_manager()
    }

    fn get_permission_checker(&self) -> &dyn PermissionChecker {
        self.platform.get_permission_checker()
    }
}

impl SessionEngine for RuntimeEngineImpl {
    fn start(&self, event_rx: mpsc::Receiver<CapturedInputEvent>) -> Result<()> {
        self.session.start_network(event_rx)
    }

    fn set_local_identity(&self, peer: PeerRefSnapshot) -> Result<()> {
        self.session.set_local_identity(peer)
    }

    fn configure_runtime(
        &self,
        endpoint: Arc<crate::platform::shared::quic::QuicEndpoint>,
        batch_window_us: u64,
        max_events_per_packet: usize,
        movement_cadence_us: u64,
        peer_id: String,
        clipboard_poll_interval_ms: u64,
        clipboard_ack_timeout_ms: u64,
    ) -> Result<()> {
        self.session.configure_runtime(
            endpoint,
            batch_window_us,
            max_events_per_packet,
            movement_cadence_us,
            peer_id,
            clipboard_poll_interval_ms,
            clipboard_ack_timeout_ms,
        )
    }

    fn emergency_unlock(&self) -> Result<()> {
        self.session.emergency_unlock()
    }

    fn shutdown(&self) -> Result<()> {
        self.session.shutdown()
    }

    fn send_layout_update(&self, update: LayoutUpdateEvent) -> Result<()> {
        self.session.send_layout_update(update)
    }

    fn take_layout_update_rx(&self) -> Result<Option<mpsc::Receiver<LayoutUpdateEvent>>> {
        self.session.take_layout_update_rx()
    }

    fn take_session_event_rx(&self) -> Result<Option<crossbeam_channel::Receiver<SessionEvent>>> {
        self.session.take_session_event_rx()
    }

    fn transfer_sync(&self) -> Option<Arc<crate::core::transfer::TransferSync>> {
        self.session.transfer_sync()
    }

    fn reconcile_peer_edges(&self, assignments: &EdgeAssignments) -> Result<()> {
        self.session.reconcile_peer_edges(assignments)
    }

    fn reconcile_layout(&self, config: &crate::core::api::ConfigSnapshot) -> Result<()> {
        self.session.reconcile_layout(config)
    }

    fn get_throughput_metrics(&self) -> crate::core::platform::ThroughputMetrics {
        self.session.get_throughput_metrics()
    }

    fn get_peer_runtime_state(&self) -> Vec<crate::core::peer_state::PeerRuntimeStateSnapshot> {
        self.session.get_peer_runtime_state()
    }

    fn close_duplicate_connection(&self, conn: Arc<PeerConnection>, reason: &[u8]) {
        self.session.close_duplicate_connection(conn, reason)
    }

    fn promote_connection(&self, peer_id: &str, conn: Arc<PeerConnection>) -> Result<()> {
        self.session.promote_connection(peer_id, conn)
    }

    fn add_peer(
        &self,
        peer: PeerRefSnapshot,
        addr: SocketAddr,
        edge: PeerEdge,
        display_bounds: Option<DisplayBounds>,
        is_inbound: bool,
    ) -> Result<()> {
        self.session
            .add_peer(peer, addr, edge, display_bounds, is_inbound)
    }

    fn mark_connection_established(&self, peer_id: &str) {
        self.session.mark_connection_established(peer_id)
    }

    fn remove_peer(&self, peer_id: &str) -> Result<()> {
        self.session.remove_peer(peer_id)
    }

    fn unpair_peer(&self, peer_id: &str) -> Result<()> {
        self.session.unpair_peer(peer_id)
    }

    fn send_pairing_pending(&self, addr: SocketAddr, peer_id: &str) -> Result<()> {
        self.session.send_pairing_pending(addr, peer_id)
    }

    fn send_pairing_approved(&self, addr: SocketAddr, peer_id: &str) -> Result<()> {
        self.session.send_pairing_approved(addr, peer_id)
    }

    fn notify_pairing_cancelled(&self, addr: SocketAddr) -> Result<()> {
        self.session.notify_pairing_cancelled(addr)
    }
}

pub(crate) fn create_engine_pair(
    _peer_edge: PeerEdge,
    bindings: EngineBindings,
) -> (Arc<dyn PlatformEngine>, Arc<dyn SessionEngine>) {
    let shared_display: Arc<dyn DisplayManager + Send + Sync> = bindings.display.into();
    let session: Arc<dyn SessionEngine> = Arc::new(SessionManager::new(
        Arc::clone(&shared_display),
        bindings.injection,
        Arc::clone(&bindings.capture_policy),
        bindings.source_marker,
        bindings.platform_name,
        Arc::clone(&bindings.create_thread_display),
        bindings.clipboard.clone(),
    )) as Arc<dyn SessionEngine>;
    let platform: Arc<dyn PlatformEngine> = Arc::new(PlatformEngineImpl::new(
        bindings.capture,
        shared_display,
        bindings.permission,
        bindings.on_capture_installed,
        bindings.on_capture_uninstalling,
    )) as Arc<dyn PlatformEngine>;
    (platform, session)
}
