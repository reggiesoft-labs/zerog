//! Portal session management for native Wayland input via xdg-desktop-portal.
//!
//! This module manages two portal sessions:
//! - **RemoteDesktop** session for input injection (emulation mode)
//! - **InputCapture** session for capturing input events
//!
//! Both sessions provide EIS file descriptors for use with the `reis` crate.
//! Session restore tokens are persisted to disk so users don't need to
//! re-authorize on every launch.

#[cfg(feature = "linux-wayland")]
mod inner {
    use anyhow::{Context, Result};
    use ashpd::desktop::PersistMode;
    use ashpd::desktop::{Response, ResponseError};
    use ashpd::desktop::input_capture::{
        Activated, Barrier, BarrierID, Capabilities, SetPointerBarriersResponse, Zones,
    };
    use ashpd::desktop::remote_desktop::{DeviceType, RemoteDesktop};
    use ashpd::enumflags2::BitFlags;
    use ashpd::zbus::zvariant::{
        DynamicType, OwnedObjectPath, OwnedValue, Type, Value,
    };
    use ashpd::zbus::{Connection, Proxy};
    use std::collections::HashMap;
    use std::os::fd::OwnedFd;
    use std::path::{Path, PathBuf};
    use std::sync::{Arc, Mutex, OnceLock};
    use tracing::{debug, info, warn};

    use super::super::runtime::{wayland_block_on, wayland_runtime_handle};
    use crate::platform::PeerEdge;

    fn shutdown_signal() -> &'static tokio::sync::watch::Sender<bool> {
        static SHUTDOWN: OnceLock<tokio::sync::watch::Sender<bool>> = OnceLock::new();
        SHUTDOWN.get_or_init(|| tokio::sync::watch::channel(false).0)
    }

    pub(crate) fn begin_shutdown() {
        shutdown_signal().send_replace(true);
    }

    async fn run_portal_setup<T>(
        mut shutdown: tokio::sync::watch::Receiver<bool>,
        setup: impl std::future::Future<Output = Result<T>>,
    ) -> Result<T> {
        tokio::select! {
            biased;
            _ = shutdown.wait_for(|closing| *closing) => {
                anyhow::bail!("portal setup cancelled during application shutdown")
            }
            result = setup => result,
        }
    }

    async fn call_portal_request<R, B>(
        conn: &Connection,
        proxy: &Proxy<'_>,
        method: &str,
        handle_token: &str,
        body: &B,
    ) -> Result<R>
    where
        R: for<'de> serde::Deserialize<'de> + Type,
        B: serde::Serialize + DynamicType,
    {
        use futures::StreamExt;
        let unique_name = conn.unique_name().context("missing unique name on session bus")?;
        let unique_id = unique_name.trim_start_matches(':').replace('.', "_");
        let request_path = format!("/org/freedesktop/portal/desktop/request/{unique_id}/{handle_token}");
        let request_proxy = Proxy::new(
            conn,
            "org.freedesktop.portal.Desktop",
            request_path.as_str(),
            "org.freedesktop.portal.Request",
        )
        .await
        .context("failed to create request proxy")?;

        let mut stream = request_proxy
            .receive_signal("Response")
            .await
            .context("failed to subscribe to Response signal")?;

        let _: OwnedObjectPath = proxy
            .call(method, body)
            .await
            .with_context(|| format!("portal method {method} failed"))?;

        let msg = stream
            .next()
            .await
            .context("portal request stream closed unexpectedly")?;
        let response: Response<R> = msg
            .body()
            .deserialize()
            .context("failed to deserialize Response body")?;

        match response {
            Response::Ok(r) => Ok(r),
            Response::Err(ResponseError::Cancelled) => {
                anyhow::bail!("portal request was cancelled by user")
            }
            Response::Err(ResponseError::Other) => anyhow::bail!("portal request failed"),
        }
    }

    /// State for the RemoteDesktop portal session (used for injection).
    #[derive(Default)]
    struct PortalRdState {
        /// The EIS fd obtained from connect_to_eis, consumable once.
        eis_fd: Option<OwnedFd>,
        /// Whether the RD session is currently active.
        active: bool,
        /// Device types granted by the user.
        granted_devices: Option<BitFlags<DeviceType>>,
    }

    #[derive(Clone, Copy, Debug)]
    pub(crate) struct ActivatedCoords {
        pub(crate) position: Option<(f64, f64)>,
    }

    pub(crate) enum ReleaseRequest {
        Release {
            cursor_position: Option<(f64, f64)>,
        },
        Close,
    }

    /// State for the InputCapture portal session (used for capture).
    #[derive(Default)]
    struct PortalIcState {
        /// The EIS fd obtained from connect_to_eis, consumable once.
        eis_fd: Option<OwnedFd>,
        /// Whether the IC session is currently active.
        active: bool,
        /// Granted capabilities.
        granted_capabilities: Option<BitFlags<Capabilities>>,
        activation_rx: Option<tokio::sync::mpsc::UnboundedReceiver<ActivatedCoords>>,
        release_tx: Option<tokio::sync::mpsc::UnboundedSender<ReleaseRequest>>,
    }

    /// Manages xdg-desktop-portal sessions for native Wayland input.
    ///
    /// Wraps two session types:
    /// - RemoteDesktop for input injection (pointer/keyboard emulation)
    /// - InputCapture for capturing pointer/keyboard events
    ///
    /// Thread safety: All fields are behind Mutex, making this Send + Sync.
    pub(crate) struct PortalSession {
        rd_state: Mutex<PortalRdState>,
        ic_state: Mutex<PortalIcState>,
        restore_token: Mutex<Option<String>>,
        token_path: PathBuf,
        ic_restore_token: Mutex<Option<String>>,
        ic_token_path: PathBuf,
    }

    // Verify Send + Sync at compile time.
    const _: () = {
        fn assert_send_sync<T: Send + Sync>() {}
        let _ = assert_send_sync::<PortalSession>;
    };

    impl PortalSession {
        /// Create a new PortalSession, loading any persisted restore token.
        pub(crate) fn new() -> Result<Self> {
            let token_path = Self::token_path();
            let restore_token = Self::load_token_from(&token_path);
            if restore_token.is_some() {
                debug!("Loaded portal restore token from {}", token_path.display());
            }

            let ic_token_path = Self::ic_token_path();
            let ic_restore_token = Self::load_token_from(&ic_token_path);
            if ic_restore_token.is_some() {
                debug!("Loaded InputCapture restore token from {}", ic_token_path.display());
            }

            Ok(Self {
                rd_state: Mutex::new(PortalRdState::default()),
                ic_state: Mutex::new(PortalIcState::default()),
                restore_token: Mutex::new(restore_token),
                token_path,
                ic_restore_token: Mutex::new(ic_restore_token),
                ic_token_path,
            })
        }

        #[cfg(test)]
        pub(crate) fn with_consumed_sessions() -> Self {
            let portal = Self::new().expect("test portal");
            portal.rd_state.lock().unwrap().active = true;
            portal.ic_state.lock().unwrap().active = true;
            portal
        }

        /// Create a RemoteDesktop session via the portal.
        ///
        /// This requests pointer + keyboard device access, starts the session,
        /// and obtains an EIS fd for libei connections. The EIS fd can then be
        /// consumed via `take_rd_eis_fd()`.
        pub(crate) fn create_rd_session(&self) -> Result<()> {
            wayland_block_on(run_portal_setup(shutdown_signal().subscribe(), async {
                let rd = RemoteDesktop::new()
                    .await
                    .context("failed to create RemoteDesktop proxy")?;

                let session = rd
                    .create_session()
                    .await
                    .context("failed to create RemoteDesktop session")?;

                // Load restore token for session resumption.
                let token = self.restore_token.lock().unwrap().clone();

                rd.select_devices(
                    &session,
                    DeviceType::Pointer | DeviceType::Keyboard,
                    token.as_deref(),
                    PersistMode::ExplicitlyRevoked,
                )
                .await
                .context("failed to select devices")?
                .response()
                .context("device selection was denied")?;

                let started = rd
                    .start(&session, None)
                    .await
                    .context("failed to start RemoteDesktop session")?;

                let response = started
                    .response()
                    .context("RemoteDesktop session was denied by user")?;

                let granted = response.devices();
                info!(
                    "RemoteDesktop session started, granted devices: {:?}",
                    granted
                );

                // Persist the restore token if provided.
                if let Some(new_token) = response.restore_token() {
                    if let Err(e) = self.persist_token(new_token) {
                        warn!("Failed to persist portal restore token: {e}");
                    }
                    *self.restore_token.lock().unwrap() = Some(new_token.to_string());
                }

                // Get the EIS fd for libei.
                let eis_fd = rd
                    .connect_to_eis(&session)
                    .await
                    .context("failed to connect to EIS (RemoteDesktop v2+ required)")?;

                let mut state = self.rd_state.lock().unwrap();
                state.eis_fd = Some(eis_fd);
                state.active = true;
                state.granted_devices = Some(granted);

                Ok(())
            }))
        }

        /// Create an InputCapture session via the portal.
        ///
        /// This requests pointer + keyboard capture capabilities and obtains
        /// an EIS fd. The session is used for detecting cursor hitting screen
        /// edges (pointer barriers).
        #[allow(dead_code)]
        pub(crate) fn create_ic_session(&self) -> Result<()> {
            self.create_ic_session_with_barriers(PeerEdge::Right)
        }

        /// Consume the EIS fd from the RemoteDesktop session.
        ///
        /// Returns `None` if no RD session is active or the fd was already taken.
        pub(crate) fn take_rd_eis_fd(&self) -> Option<OwnedFd> {
            self.rd_state.lock().unwrap().eis_fd.take()
        }

        /// Consume the EIS fd from the InputCapture session.
        ///
        /// Returns `None` if no IC session is active or the fd was already taken.
        pub(crate) fn take_ic_eis_fd(&self) -> Option<OwnedFd> {
            self.ic_state.lock().unwrap().eis_fd.take()
        }

        pub(crate) fn take_ic_activation_rx(
            &self,
        ) -> Option<tokio::sync::mpsc::UnboundedReceiver<ActivatedCoords>> {
            self.ic_state.lock().unwrap().activation_rx.take()
        }

        pub(crate) fn release_ic(&self, cursor_position: Option<(f64, f64)>) {
            let state = self.ic_state.lock().unwrap();
            if let Some(ref tx) = state.release_tx {
                let _ = tx.send(ReleaseRequest::Release { cursor_position });
            }
        }

        /// Check whether the RemoteDesktop session is active.
        pub(crate) fn rd_active(&self) -> bool {
            self.rd_state.lock().unwrap().active
        }

        /// Check whether the InputCapture session is active.
        pub(crate) fn ic_active(&self) -> bool {
            self.ic_state.lock().unwrap().active
        }

        /// Mark the RemoteDesktop session as inactive.
        #[allow(dead_code)]
        pub(crate) fn close_rd_session(&self) {
            let mut state = self.rd_state.lock().unwrap();
            state.active = false;
            state.eis_fd = None;
            state.granted_devices = None;
        }

        /// Mark the InputCapture session as inactive.
        #[allow(dead_code)]
        pub(crate) fn close_ic_session(&self) {
            let mut state = self.ic_state.lock().unwrap();
            state.active = false;
            state.eis_fd = None;
            state.granted_capabilities = None;
            state.activation_rx = None;
            if let Some(tx) = state.release_tx.take() {
                let _ = tx.send(ReleaseRequest::Close);
            }
        }

        /// Recreate the RemoteDesktop session (GNOME barrier refresh workaround).
        ///
        /// Destroys the current session and creates a new one. The restore token
        /// is preserved so the user doesn't need to re-authorize.
        #[allow(dead_code)]
        pub(crate) fn recreate_rd_session(&self) -> Result<()> {
            self.close_rd_session();
            self.create_rd_session()
        }

        /// Create an InputCapture session with pointer barriers at the given edge.
        ///
        /// This creates an IC session, queries zones from the compositor, builds
        /// barriers along `peer_edge`, sets them, enables input capture, and
        /// obtains the EIS fd. The EIS fd can be consumed via `take_ic_eis_fd()`.
        pub(crate) fn create_ic_session_with_barriers(&self, peer_edge: PeerEdge) -> Result<()> {
            wayland_block_on(run_portal_setup(shutdown_signal().subscribe(), async {
                let conn = Connection::session()
                    .await
                    .context("failed to connect to session bus")?;

                let ic_proxy = Proxy::new_owned(
                    conn.clone(),
                    "org.freedesktop.portal.Desktop",
                    "/org/freedesktop/portal/desktop",
                    "org.freedesktop.portal.InputCapture",
                )
                .await
                .context("failed to create InputCapture proxy")?;

                let (session_handle, granted) = match self.create_ic_session_v2(&conn, &ic_proxy).await {
                    Ok(res) => res,
                    Err(e) => {
                        debug!("InputCapture v2 failed ({e}), falling back to v1");
                        self.create_ic_session_v1(&conn, &ic_proxy).await?
                    }
                };

                info!(
                    "InputCapture session started, granted capabilities: {:?}",
                    granted
                );

                let eis_options: HashMap<&str, Value<'_>> = HashMap::new();
                let eis_fd_raw: ashpd::zbus::zvariant::OwnedFd = ic_proxy
                    .call("ConnectToEIS", &(session_handle.as_ref(), &eis_options))
                    .await
                    .context("failed to connect to EIS for InputCapture")?;
                let eis_fd: OwnedFd = eis_fd_raw.into();

                let zones_handle_token = format!("mt_zones_{}", uuid::Uuid::new_v4().simple());
                let mut zones_options = HashMap::new();
                zones_options.insert("handle_token", Value::from(zones_handle_token.as_str()));
                let zones_response: Zones = call_portal_request(
                    &conn,
                    &ic_proxy,
                    "GetZones",
                    &zones_handle_token,
                    &(session_handle.as_ref(), &zones_options),
                )
                .await
                .context("failed to request zones")?;

                let regions = zones_response.regions();
                let zone_set = zones_response.zone_set();
                debug!(
                    "InputCapture zones: {} regions, zone_set={}",
                    regions.len(),
                    zone_set
                );

                let barriers = build_barriers(regions, peer_edge);
                debug!(
                    "Setting {} pointer barriers for edge {:?}",
                    barriers.len(),
                    peer_edge
                );

                if !barriers.is_empty() {
                    let barrier_handle_token = format!("mt_barriers_{}", uuid::Uuid::new_v4().simple());
                    let mut barrier_options = HashMap::new();
                    barrier_options.insert("handle_token", Value::from(barrier_handle_token.as_str()));
                    let barrier_response: SetPointerBarriersResponse = call_portal_request(
                        &conn,
                        &ic_proxy,
                        "SetPointerBarriers",
                        &barrier_handle_token,
                        &(session_handle.as_ref(), &barrier_options, &barriers, zone_set),
                    )
                    .await
                    .context("failed to set pointer barriers")?;

                    let failed = barrier_response.failed_barriers();
                    if !failed.is_empty() {
                        warn!(
                            "Some pointer barriers were denied by compositor: {:?}",
                            failed
                        );
                    }
                }

                let enable_options: HashMap<&str, Value<'_>> = HashMap::new();
                let _: () = ic_proxy
                    .call("Enable", &(session_handle.as_ref(), &enable_options))
                    .await
                    .context("failed to enable InputCapture")?;
                debug!("InputCapture enabled");

                use futures::StreamExt;
                let mut activate_stream = ic_proxy
                    .receive_signal("Activated")
                    .await
                    .context("failed to subscribe to InputCapture Activated signal")?;

                let (act_tx, act_rx) =
                    tokio::sync::mpsc::unbounded_channel::<ActivatedCoords>();
                let (rel_tx, mut rel_rx) =
                    tokio::sync::mpsc::unbounded_channel::<ReleaseRequest>();

                let sess_handle = session_handle.clone();
                let proxy = ic_proxy.clone();
                let session_proxy = Proxy::new_owned(
                    conn.clone(),
                    "org.freedesktop.portal.Desktop",
                    session_handle,
                    "org.freedesktop.portal.Session",
                )
                .await
                .context("failed to create Session proxy")?;

                {
                    let mut state = self.ic_state.lock().unwrap();
                    state.eis_fd = Some(eis_fd);
                    state.active = true;
                    state.granted_capabilities = Some(granted);
                    state.activation_rx = Some(act_rx);
                    state.release_tx = Some(rel_tx);
                }

                wayland_runtime_handle().spawn(async move {
                    let mut last_activation_id = None;
                    loop {
                        tokio::select! {
                            Some(msg) = activate_stream.next() => {
                                if let Ok(activated) = msg.body().deserialize::<Activated>() {
                                    if activated.session_handle() == sess_handle.as_ref() {
                                        let activation_id = activated.activation_id();
                                        last_activation_id = activation_id;
                                        let position = activated
                                            .cursor_position()
                                            .map(|(x, y)| (x as f64, y as f64));
                                        let _ = act_tx.send(ActivatedCoords {
                                            position,
                                        });
                                    }
                                }
                            }
                            Some(req) = rel_rx.recv() => {
                                match req {
                                    ReleaseRequest::Release { cursor_position } => {
                                        let act_id = last_activation_id.take();
                                        let mut options: HashMap<&str, Value<'_>> = HashMap::new();
                                        if let Some(id) = act_id {
                                            options.insert("activation_id", Value::from(id));
                                        }
                                        if let Some((x, y)) = cursor_position {
                                            options.insert("cursor_position", Value::from((x, y)));
                                        }
                                        let res: std::result::Result<(), _> = proxy
                                            .call("Release", &(sess_handle.as_ref(), &options))
                                            .await;
                                        if let Err(e) = res {
                                            warn!("Failed to release InputCapture: {e}");
                                        }
                                    }
                                    ReleaseRequest::Close => {
                                        let _ = session_proxy.call::<&str, (), ()>("Close", &()).await;
                                        break;
                                    }
                                }
                            }
                            else => {
                                break;
                            }
                        }
                    }
                });

                Ok(())
            }))
        }

        async fn create_ic_session_v2(
            &self,
            conn: &Connection,
            ic_proxy: &Proxy<'_>,
        ) -> Result<(OwnedObjectPath, BitFlags<Capabilities>)> {
            let session_token = format!("mt_sess_{}", uuid::Uuid::new_v4().simple());
            let mut create_options = HashMap::new();
            create_options.insert("session_handle_token", Value::from(session_token.as_str()));

            let res: HashMap<String, OwnedValue> = ic_proxy
                .call("CreateSession2", &(create_options,))
                .await
                .context("CreateSession2 failed")?;

            let session_handle_val = res
                .get("session_handle")
                .context("missing session_handle in CreateSession2 response")?;
            let session_handle = OwnedObjectPath::try_from(session_handle_val.clone())
                .context("invalid session_handle object path")?;

            let saved_token = self.ic_restore_token.lock().unwrap().clone();
            let start_res = self
                .start_ic_session(conn, ic_proxy, &session_handle, saved_token.as_deref())
                .await;

            let results = match start_res {
                Ok(r) => r,
                Err(e) if saved_token.is_some() => {
                    warn!("Start with cached InputCapture restore token failed ({e}), retrying without token");
                    self.remove_ic_token();
                    *self.ic_restore_token.lock().unwrap() = None;
                    self.start_ic_session(conn, ic_proxy, &session_handle, None)
                        .await?
                }
                Err(e) => return Err(e),
            };

            let new_token = results
                .get("restore_token")
                .or_else(|| results.get("restore_data"))
                .and_then(|v| <&str>::try_from(v).ok())
                .map(|s| s.to_string());

            if let Some(token) = new_token {
                if let Err(e) = self.persist_ic_token(&token) {
                    warn!("Failed to persist InputCapture restore token: {e}");
                }
                *self.ic_restore_token.lock().unwrap() = Some(token);
            }

            let caps_raw = results
                .get("capabilities")
                .and_then(|v| u32::try_from(v).ok())
                .unwrap_or_else(|| (Capabilities::Keyboard | Capabilities::Pointer).bits());
            let granted_caps = BitFlags::<Capabilities>::from_bits_truncate(caps_raw);

            Ok((session_handle, granted_caps))
        }

        async fn start_ic_session(
            &self,
            conn: &Connection,
            ic_proxy: &Proxy<'_>,
            session_handle: &OwnedObjectPath,
            restore_token: Option<&str>,
        ) -> Result<HashMap<String, OwnedValue>> {
            let handle_token = format!("mt_start_{}", uuid::Uuid::new_v4().simple());
            let mut options = HashMap::new();
            options.insert("handle_token", Value::from(handle_token.as_str()));
            let caps = (Capabilities::Keyboard | Capabilities::Pointer).bits();
            options.insert("capabilities", Value::from(caps));
            options.insert("persist_mode", Value::from(2u32));
            if let Some(token) = restore_token {
                options.insert("restore_token", Value::from(token));
            }

            call_portal_request(
                conn,
                ic_proxy,
                "Start",
                &handle_token,
                &(session_handle, "", options),
            )
            .await
        }

        async fn create_ic_session_v1(
            &self,
            conn: &Connection,
            ic_proxy: &Proxy<'_>,
        ) -> Result<(OwnedObjectPath, BitFlags<Capabilities>)> {
            let handle_token = format!("mt_v1_{}", uuid::Uuid::new_v4().simple());
            let session_token = format!("mt_sess_{}", uuid::Uuid::new_v4().simple());
            let mut options = HashMap::new();
            options.insert("handle_token", Value::from(handle_token.as_str()));
            options.insert("session_handle_token", Value::from(session_token.as_str()));
            let caps = (Capabilities::Keyboard | Capabilities::Pointer).bits();
            options.insert("capabilities", Value::from(caps));

            let res: HashMap<String, OwnedValue> = call_portal_request(
                conn,
                ic_proxy,
                "CreateSession",
                &handle_token,
                &("", options),
            )
            .await?;

            let session_handle_val = res
                .get("session_handle")
                .context("missing session_handle in CreateSession response")?;
            let session_handle = OwnedObjectPath::try_from(session_handle_val.clone())
                .context("invalid session_handle object path")?;

            let caps_raw = res
                .get("capabilities")
                .and_then(|v| u32::try_from(v).ok())
                .unwrap_or(caps);
            let granted_caps = BitFlags::<Capabilities>::from_bits_truncate(caps_raw);
            Ok((session_handle, granted_caps))
        }

        /// Recreate the InputCapture session (GNOME workaround).
        ///
        /// GNOME has a bug where pointer barriers cannot be updated on an
        /// existing session. The workaround is to destroy and recreate the
        /// entire IC session when barrier configuration changes (e.g., display
        /// layout changed or peer edge changed).
        #[allow(dead_code)]
        pub(crate) fn recreate_ic_session(&self, peer_edge: PeerEdge) -> Result<()> {
            self.close_ic_session();
            self.create_ic_session_with_barriers(peer_edge)
        }

        // -- Token persistence --

        /// Compute the path for storing the portal restore token.
        fn token_path() -> PathBuf {
            let config_dir = std::env::var("XDG_CONFIG_HOME")
                .map(PathBuf::from)
                .unwrap_or_else(|_| {
                    let home = std::env::var("HOME").expect("HOME not set");
                    PathBuf::from(home).join(".config")
                });
            config_dir.join("mouse-thing").join("portal-token")
        }

        /// Persist the restore token to disk.
        fn persist_token(&self, token: &str) -> std::io::Result<()> {
            if let Some(parent) = self.token_path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(&self.token_path, token)?;
            // Restrict permissions: owner read/write only.
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&self.token_path, std::fs::Permissions::from_mode(0o600))?;
            }
            Ok(())
        }

        /// Load a previously persisted restore token from disk.
        fn load_token_from(path: &Path) -> Option<String> {
            std::fs::read_to_string(path)
                .ok()
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
        }

        fn ic_token_path() -> PathBuf {
            let config_dir = std::env::var("XDG_CONFIG_HOME")
                .map(PathBuf::from)
                .unwrap_or_else(|_| {
                    let home = std::env::var("HOME").expect("HOME not set");
                    PathBuf::from(home).join(".config")
                });
            config_dir.join("mouse-thing").join("portal-ic-token")
        }

        fn persist_ic_token(&self, token: &str) -> std::io::Result<()> {
            if let Some(parent) = self.ic_token_path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(&self.ic_token_path, token)?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&self.ic_token_path, std::fs::Permissions::from_mode(0o600))?;
            }
            Ok(())
        }

        fn remove_ic_token(&self) {
            let _ = std::fs::remove_file(&self.ic_token_path);
        }
    }

    /// Build pointer barriers at the specified screen edge from compositor zones.
    ///
    /// Each zone region gets a barrier at the requested edge. Barrier positions
    /// must be at the exact edge of the zone region to be accepted by the
    /// compositor. Positions are inclusive lines (x1,y1)-(x2,y2).
    fn build_barriers(
        regions: &[ashpd::desktop::input_capture::Region],
        peer_edge: PeerEdge,
    ) -> Vec<Barrier> {
        regions
            .iter()
            .enumerate()
            .filter(|(_, r)| is_outer_edge(r, regions, peer_edge))
            .filter_map(|(n, r)| {
                let id = BarrierID::new((n as u32) + 1)?;
                let (x, y) = (r.x_offset(), r.y_offset());
                let (width, height) = (r.width() as i32, r.height() as i32);
                let pos = match peer_edge {
                    PeerEdge::Left => (x, y, x, y + height - 1),
                    PeerEdge::Right => (x + width, y, x + width, y + height - 1),
                    PeerEdge::Top => (x, y, x + width - 1, y),
                    PeerEdge::Bottom => (x, y + height, x + width - 1, y + height),
                };
                Some(Barrier::new(id, pos))
            })
            .collect()
    }

    fn is_outer_edge(
        r: &ashpd::desktop::input_capture::Region,
        all: &[ashpd::desktop::input_capture::Region],
        edge: PeerEdge,
    ) -> bool {
        let rx1 = r.x_offset();
        let rx2 = rx1 + r.width() as i32;
        let ry1 = r.y_offset();
        let ry2 = ry1 + r.height() as i32;
        !all.iter().any(|other| {
            if std::ptr::eq(r, other) {
                return false;
            }
            let ox1 = other.x_offset();
            let ox2 = ox1 + other.width() as i32;
            let oy1 = other.y_offset();
            let oy2 = oy1 + other.height() as i32;
            let y_overlap = ry1 < oy2 && ry2 > oy1;
            let x_overlap = rx1 < ox2 && rx2 > ox1;
            match edge {
                PeerEdge::Right => y_overlap && ox1 >= rx2,
                PeerEdge::Left => y_overlap && ox2 <= rx1,
                PeerEdge::Top => x_overlap && oy2 <= ry1,
                PeerEdge::Bottom => x_overlap && oy1 >= ry2,
            }
        })
    }

    /// Process-global singleton for the portal session.
    static PORTAL_SESSION: OnceLock<Arc<PortalSession>> = OnceLock::new();

    /// Get or create the global PortalSession instance.
    pub(crate) fn get_or_create_portal_session() -> Result<Arc<PortalSession>> {
        if let Some(existing) = PORTAL_SESSION.get() {
            return Ok(Arc::clone(existing));
        }

        let session = Arc::new(PortalSession::new()?);
        // If another thread raced us, use theirs.
        let _ = PORTAL_SESSION.set(Arc::clone(&session));
        Ok(PORTAL_SESSION.get().map(Arc::clone).unwrap_or(session))
    }

    /// Check whether the portal RemoteDesktop service is reachable using ashpd.
    ///
    /// This attempts to create a RemoteDesktop proxy without starting a session.
    pub(crate) fn ashpd_portal_available() -> bool {
        wayland_block_on(run_portal_setup(shutdown_signal().subscribe(), async {
            RemoteDesktop::new()
                .await
                .context("failed to create RemoteDesktop proxy")
        }))
        .is_ok()
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use std::path::PathBuf;

        #[tokio::test]
        async fn portal_setup_completes_before_shutdown() {
            let (shutdown, receiver) = tokio::sync::watch::channel(false);

            assert_eq!(
                run_portal_setup(receiver, async { Ok(42) }).await.unwrap(),
                42
            );
            shutdown.send_replace(true);
            assert!(
                run_portal_setup(shutdown.subscribe(), async { Ok(43) })
                    .await
                    .is_err()
            );
        }

        #[tokio::test]
        async fn shutdown_cancels_pending_portal_setup() {
            let (shutdown, receiver) = tokio::sync::watch::channel(false);
            let (started_tx, started_rx) = tokio::sync::oneshot::channel();
            let (request_tx, request_rx) = tokio::sync::oneshot::channel::<()>();
            let task = tokio::spawn(run_portal_setup(receiver, async move {
                let _request = request_tx;
                started_tx.send(()).unwrap();
                std::future::pending::<Result<()>>().await
            }));

            started_rx.await.unwrap();
            shutdown.send_replace(true);
            let result = tokio::time::timeout(std::time::Duration::from_secs(1), task)
                .await
                .expect("shutdown must interrupt a pending permission response")
                .unwrap();

            assert!(result.is_err());
            assert!(request_rx.await.is_err());
        }

        #[tokio::test]
        async fn shutdown_prevents_late_portal_setup() {
            let (shutdown, receiver) = tokio::sync::watch::channel(false);
            drop(receiver);
            shutdown.send_replace(true);
            shutdown.send_replace(true);

            let result: Result<()> = run_portal_setup(shutdown.subscribe(), async {
                panic!("portal setup must not be polled after shutdown");
            })
            .await;

            assert!(result.is_err());
        }

        /// PortalSession::new() should start with both RD and IC sessions idle.
        ///
        /// Note: new() only loads a restore token from disk if one exists; it does
        /// not connect to any compositor or portal service.
        #[test]
        fn portal_session_new_starts_idle() {
            // PortalSession::new() does not connect to any service — it only reads
            // a token file from disk.  On a CI/macOS box the token file won't
            // exist, so new() just constructs the struct with no token.
            let session = PortalSession::new().expect("PortalSession::new");
            assert!(
                !session.rd_active(),
                "RemoteDesktop session should start idle"
            );
            assert!(
                !session.ic_active(),
                "InputCapture session should start idle"
            );
        }

        /// Persisting a token and loading it back should return the same value.
        #[test]
        fn token_persistence_roundtrip() {
            let dir = tempfile::tempdir().expect("tempdir");
            let path = dir.path().join("portal-token");

            // Write a token file (simulating what persist_token does internally).
            std::fs::write(&path, "test-restore-token\n").expect("write token");

            let loaded = PortalSession::load_token_from(&path);
            assert_eq!(
                loaded.as_deref(),
                Some("test-restore-token"),
                "loaded token should match persisted value (trimmed)"
            );
        }

        /// Loading a token from a non-existent path should return None.
        #[test]
        fn token_load_returns_none_for_missing() {
            let path = PathBuf::from("/tmp/mouse-thing-test-nonexistent-portal-token-xyz-12345678");
            let loaded = PortalSession::load_token_from(&path);
            assert!(
                loaded.is_none(),
                "load_token_from a missing path should return None"
            );
        }

        #[test]
        fn ic_token_persistence_roundtrip() {
            let dir = tempfile::tempdir().expect("tempdir");
            let path = dir.path().join("portal-ic-token");

            std::fs::write(&path, "kde-restore-token-xyz").expect("write token");
            let loaded = PortalSession::load_token_from(&path);
            assert_eq!(
                loaded.as_deref(),
                Some("kde-restore-token-xyz"),
                "loaded InputCapture token should match persisted value"
            );
        }

        #[test]
        fn ic_token_load_returns_none_for_missing() {
            let path = PathBuf::from("/tmp/mouse-thing-test-nonexistent-ic-token-xyz-12345678");
            let loaded = PortalSession::load_token_from(&path);
            assert!(
                loaded.is_none(),
                "load_token_from a missing path should return None"
            );
        }
    }
}

#[cfg(feature = "linux-wayland")]
pub(crate) use inner::*;
