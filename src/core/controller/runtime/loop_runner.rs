use super::{ControllerEvent, ControllerTaskEvent, RunningController, RuntimeContext};
use crate::core::discovery::DiscoveredPeer;
use crate::core::controller::config_state::ConfigStore;
use crate::core::controller::events::PeerUpdate;
use crate::core::controller::peer_connector::SharedPeerConnector;
use crate::core::controller::peers::{DialState, PeerStore};
use crate::core::discovery::DiscoveryEvent;
use crate::core::handshake::HandshakePeer;
use crate::core::platform::engine::SessionEvent;
use crate::events::LayoutUpdateEvent;
use crate::quic::{QuicEndpoint, cert_fingerprint};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::mpsc;
use tokio::sync::{broadcast, oneshot};
use tokio::task::JoinSet;

pub(super) struct PendingPairing {
    pub(super) name: String,
    pub(super) addr: std::net::SocketAddr,
    pub(super) conn: Arc<crate::quic::PeerConnection>,
    pub(super) display_bounds: Option<crate::core::platform::DisplayBounds>,
    pub(super) display_rects: Vec<crate::core::platform::DisplayBounds>,
    pub(super) platform: Option<String>,
    pub(super) listen_port: Option<u16>,
    pub(super) certificate: Vec<u8>,
}

pub(super) struct ControllerLoop {
    pub(super) config_store: ConfigStore,
    pub(super) local_peer_id: String,
    pub(super) peers: Arc<PeerStore>,
    pub(super) updates: Arc<broadcast::Sender<PeerUpdate>>,
    pub(super) connector: SharedPeerConnector,
    pub(super) session: Arc<dyn crate::core::platform::SessionEngine>,
    pub(super) controller_tx: mpsc::Sender<ControllerEvent>,
    pub(super) background_tasks: JoinSet<ControllerTaskEvent>,
    pub(super) endpoint: Arc<QuicEndpoint>,
    pub(super) pending_pairings: HashMap<String, PendingPairing>,
    pub(super) pending_outbound_pairings: HashMap<
        String,
        (
            DiscoveredPeer,
            crate::core::controller::peers::ConnectedPeer,
            Vec<u8>,
        ),
    >,
}

pub(super) struct ControllerLoopInput {
    pub(super) discovery_rx: mpsc::Receiver<DiscoveryEvent>,
    pub(super) layout_update_rx: mpsc::Receiver<LayoutUpdateEvent>,
    pub(super) session_event_rx: crossbeam_channel::Receiver<SessionEvent>,
    pub(super) controller_rx: mpsc::Receiver<ControllerEvent>,
    pub(super) shutdown_rx: oneshot::Receiver<()>,
    pub(super) shutdown_ack_tx: oneshot::Sender<()>,
    pub(super) scan_endpoint: Arc<QuicEndpoint>,
    pub(super) scan_local_peer: HandshakePeer,
    pub(super) scan_port: u16,
}

impl ControllerLoop {
    pub(super) async fn run(mut self, input: ControllerLoopInput) {
        let ControllerLoopInput {
            mut discovery_rx,
            mut layout_update_rx,
            session_event_rx,
            mut controller_rx,
            mut shutdown_rx,
            shutdown_ack_tx,
            scan_endpoint,
            scan_local_peer,
            scan_port,
        } = input;
        let mut session_events_rx = self.forward_session_events(session_event_rx);
        self.start_subnet_scan(scan_endpoint, scan_local_peer, scan_port);
        let mut discovery_active = true;

        loop {
            tokio::select! {
                _ = &mut shutdown_rx => {
                    tracing::debug!("controller shutdown signal received");
                    break;
                }
                result = discovery_rx.recv(), if discovery_active => {
                    if let Some(event) = result {
                        self.handle_discovery_event(event);
                    } else {
                        tracing::warn!("discovery channel closed; discovery inactive");
                        discovery_active = false;
                    }
                }
                result = RunningController::recv_layout_update(&mut layout_update_rx) => {
                    let Some(update) = result else {
                        tracing::debug!("layout update channel closed");
                        break;
                    };
                    RunningController::handle_layout_update(
                        &self.config_store,
                        &self.session,
                        &self.updates,
                        update,
                        &mut layout_update_rx,
                    );
                }
                result = session_events_rx.recv() => {
                    let Some(event) = result else {
                        tracing::debug!("session event channel closed");
                        break;
                    };
                    self.handle_session_event(event);
                }
                result = controller_rx.recv() => {
                    let Some(event) = result else {
                        tracing::debug!("controller event channel closed");
                        break;
                    };
                    self.handle_controller_event(event);
                }
                result = self.background_tasks.join_next(), if !self.background_tasks.is_empty() => {
                    self.handle_background_result(result);
                }
            }
        }

        self.background_tasks.abort_all();
        let _ = shutdown_ack_tx.send(());
        while self.background_tasks.join_next().await.is_some() {}
    }

    fn forward_session_events(
        &self,
        session_event_rx: crossbeam_channel::Receiver<SessionEvent>,
    ) -> mpsc::Receiver<SessionEvent> {
        let (session_events_tx, session_events_rx) = mpsc::channel(32);
        tokio::task::spawn_blocking(move || {
            while let Ok(event) = session_event_rx.recv() {
                if session_events_tx.blocking_send(event).is_err() {
                    break;
                }
            }
        });
        session_events_rx
    }

    fn start_subnet_scan(
        &mut self,
        endpoint: Arc<QuicEndpoint>,
        local_peer: HandshakePeer,
        port: u16,
    ) {
        let peers = Arc::clone(&self.peers);
        let controller_tx = self.controller_tx.clone();
        self.background_tasks.spawn(async move {
            if port == 0 {
                return ControllerTaskEvent::None;
            }
            tokio::time::sleep(crate::core::policies::SUBNET_SCAN_TRIGGER_GRACE).await;
            loop {
                if !peers.is_empty() {
                    tokio::time::sleep(crate::core::policies::SUBNET_SCAN_POLL_INTERVAL).await;
                    continue;
                }

                tracing::info!(
                    "no peers discovered via mDNS; triggering subnet scan fallback"
                );
                let discovered =
                    crate::core::discovery::subnet_scanner::scan_subnets(
                        Arc::clone(&endpoint),
                        local_peer.clone(),
                        port,
                    )
                    .await;
                for peer in discovered {
                    let _ = controller_tx
                        .send(ControllerEvent::Discovery(DiscoveryEvent::Discovered(peer)))
                        .await;
                }
                tokio::time::sleep(crate::core::policies::SUBNET_SCAN_POLL_INTERVAL).await;
            }
        });
    }

    fn context(&mut self) -> RuntimeContext<'_> {
        RuntimeContext {
            local_peer_id: &self.local_peer_id,
            peers: Arc::clone(&self.peers),
            updates: Arc::clone(&self.updates),
            connector: Arc::clone(&self.connector),
            session: Arc::clone(&self.session),
            controller_tx: self.controller_tx.clone(),
            background_tasks: &mut self.background_tasks,
        }
    }

    fn handle_discovery_event(&mut self, event: DiscoveryEvent) {
        let config = self.config_store.snapshot();
        let auto_connect = match &event {
            DiscoveryEvent::Discovered(peer) => config.should_auto_connect_peer(&peer.peer_id),
            DiscoveryEvent::Removed { .. } => false,
        };
        let mut ctx = self.context();
        RunningController::handle_discovery_event(&mut ctx, event, auto_connect);
    }

    pub(super) fn handle_session_event(&mut self, event: SessionEvent) {
        if matches!(event, SessionEvent::InboundIdentified { .. }) {
            self.handle_inbound_identified(event);
            return;
        }

        match &event {
            SessionEvent::SessionPairingPending { peer_id } => {
                if !self.pending_outbound_pairings.contains_key(peer_id) {
                    return;
                }
                self.peers.set_pairing(peer_id);
                let _ = self.updates.send(PeerUpdate::Discovered(self.peers.snapshots(&self.local_peer_id)));
                return;
            }
            SessionEvent::SessionPairingApproved { peer_id } => {
                // Only an outstanding outbound waiter may complete pairing.
                // A stray approval (or one arriving after Cancel) must not
                // flip pairing state or rebroadcast config.
                let Some((discovered, connected, certificate)) =
                    self.pending_outbound_pairings.remove(peer_id)
                else {
                    return;
                };
                // PairingApproved can arrive without config already paired
                // (the remote auto-accepted because it still trusts us). Only
                // broadcast a config change when pairing state actually flips.
                let was_paired = self.config_store.snapshot().is_peer_paired(peer_id);
                if let Ok(updated) = self.config_store.pair_peer(peer_id) {
                    if !was_paired {
                        let _ = self.updates.send(PeerUpdate::ConfigChanged(updated));
                    }
                }
                let _ = self
                    .endpoint
                    .cert_store()
                    .lock()
                    .unwrap()
                    .trust_peer(peer_id, &certificate);
                if let Err(error) = RunningController::commit_outbound_peer(
                    &discovered,
                    connected,
                    &self.session,
                    &self.peers,
                    &self.updates,
                ) {
                    tracing::warn!(error = %error, peer_id = %peer_id, "failed to commit outbound peer after pairing approval");
                }
                let _ = self.updates.send(PeerUpdate::PairingResolved {
                    peer_id: peer_id.clone(),
                    approved: true,
                });
                return;
            }
            SessionEvent::SessionLost { peer_id }
            | SessionEvent::SessionDisconnected { peer_id } => {
                if self.pending_outbound_pairings.remove(peer_id).is_some() {
                    self.peers.set_idle(peer_id);
                    let _ = self.updates.send(PeerUpdate::Discovered(self.peers.snapshots(&self.local_peer_id)));
                    let _ = self.updates.send(PeerUpdate::ConnectionLost {
                        peer_id: peer_id.clone(),
                    });
                    let _ = self.updates.send(PeerUpdate::PairingResolved {
                        peer_id: peer_id.clone(),
                        approved: false,
                    });
                }
                // The other side hung up while we were deciding (e.g. it
                // cancelled its request). Drop our inbound waiter and hide
                // the dialog instead of leaving it pending forever.
                if let Some(stale) = self.pending_pairings.remove(peer_id) {
                    self.session.close_duplicate_connection(
                        stale.conn,
                        b"pairing peer disconnected",
                    );
                    let _ = self.updates.send(PeerUpdate::Discovered(self.peers.snapshots(&self.local_peer_id)));
                    let _ = self.updates.send(PeerUpdate::PairingResolved {
                        peer_id: peer_id.clone(),
                        approved: false,
                    });
                }
            }
            _ => {}
        }

        let config = self.config_store.snapshot();
        let allowed_or_auto_connect = match &event {
            SessionEvent::SessionLost { peer_id }
            | SessionEvent::SessionDisconnected { peer_id } => {
                config.should_auto_connect_peer(peer_id)
            }
            SessionEvent::SessionUnpaired { peer_id } => {
                if let Ok(updated) = self.config_store.unpair_peer(peer_id) {
                    let _ = self.updates.send(PeerUpdate::ConfigChanged(updated));
                }
                // Forget means forget: drop the pinned certificate and any
                // pairing waiters so a later Connect starts a fresh pairing.
                self.remove_trusted_peer(peer_id);
                self.clear_pairing_waiters(peer_id);
                false
            }
            SessionEvent::InboundIdentified { .. }
            | SessionEvent::SessionPairingPending { .. }
            | SessionEvent::SessionPairingApproved { .. } => unreachable!(),
        };
        let mut ctx = self.context();
        RunningController::handle_session_event(&mut ctx, event, allowed_or_auto_connect);
    }

    fn handle_inbound_identified(&mut self, event: SessionEvent) {
        let SessionEvent::InboundIdentified {
            ref peer_id,
            ref name,
            ref addr,
            ref conn,
            ref display_bounds,
            ref display_rects,
            ref platform,
            is_auto_connect,
            listen_port,
            ref certificate,
        } = event
        else {
            return;
        };

        let config = self.config_store.snapshot();
        let is_paired = config.is_peer_paired(peer_id);

        // Bind the peer id to the certificate presented during the QUIC/TLS
        // handshake. A paired peer whose certificate is not pinned (for
        // example after an upgrade) must be re-approved before use.
        let certificate_trusted = {
            let cert_store = self.endpoint.cert_store();
            let store = cert_store.lock().unwrap();
            if store.is_known_peer(peer_id) {
                if !store.verify_fingerprint(peer_id, certificate) {
                    tracing::warn!(
                        peer_id = %peer_id,
                        "inbound connection rejected: certificate fingerprint mismatch"
                    );
                    let _ = self.session.close_duplicate_connection(
                        Arc::clone(conn),
                        b"certificate verification failed",
                    );
                    return;
                }
                true
            } else {
                false
            }
        };

        if is_paired && certificate_trusted {
            if config.is_peer_paused(peer_id) {
                if let Ok(updated) = self.config_store.resume_peer(peer_id) {
                    let _ = self.updates.send(PeerUpdate::ConfigChanged(updated));
                }
            }
            let peer_id_owned = peer_id.clone();
            let addr_owned = *addr;
            let mut ctx = self.context();
            RunningController::handle_session_event(&mut ctx, event, true);
            // The remote may have forgotten us locally while we still trust
            // it. It then waits for a pairing approval that would never come.
            // Signal approval so its outbound waiter can complete.
            let _ = self
                .session
                .send_pairing_approved(addr_owned, &peer_id_owned);
            return;
        }

        // Unpaired peer, or a paired peer without a pinned certificate:
        // require an explicit user pairing decision that pins the certificate.
        if !is_paired && is_auto_connect {
            let _ = self
                .session
                .close_duplicate_connection(Arc::clone(conn), b"peer unpaired");
            return;
        }
        // A fresh handshake while a pairing waiter lingers means the remote
        // is retrying (e.g. it cancelled and dialed again, so its Cancel
        // never cleared our inbound waiter). Replace the stale waiter so the
        // retry starts fresh instead of being rejected as a duplicate.
        if let Some(stale) = self.pending_pairings.remove(peer_id) {
            self.session.close_duplicate_connection(
                stale.conn,
                b"pairing superseded by new attempt",
            );
        }
        if let Err(error) = self.session.promote_connection(peer_id, Arc::clone(conn)) {
            tracing::warn!(peer_id = %peer_id, error = %error, "failed to promote inbound pairing connection");
            self.session.close_duplicate_connection(
                Arc::clone(conn),
                b"failed to promote pairing connection",
            );
            return;
        }
        let _ = self.session.send_pairing_pending(*addr, peer_id);
        self.pending_pairings.insert(
            peer_id.clone(),
            PendingPairing {
                name: name.clone(),
                addr: *addr,
                conn: Arc::clone(conn),
                display_bounds: *display_bounds,
                display_rects: display_rects.clone(),
                platform: platform.clone(),
                listen_port,
                certificate: certificate.clone(),
            },
        );
        let _ = self.updates.send(PeerUpdate::PairingRequested {
            peer_id: peer_id.clone(),
            name: name.clone(),
            platform: platform.clone(),
            code: self.pairing_code_for(certificate),
            can_respond: true,
        });
    }

    fn remove_trusted_peer(&self, peer_id: &str) {
        self.endpoint
            .cert_store()
            .lock()
            .unwrap()
            .remove_peer(peer_id);
    }

    fn clear_pairing_waiters(&mut self, peer_id: &str) {
        self.pending_pairings.remove(peer_id);
        if let Some((_, connected, _)) = self.pending_outbound_pairings.remove(peer_id) {
            // Tell the remote our attempt ended so it drops its inbound
            // waiter (hiding its dialog). Otherwise it could Accept after
            // our Cancel and pair a connection we already closed.
            let _ = self.session.notify_pairing_cancelled(connected.addr);
            // Drop the promoted pending connection so the remote stops
            // waiting and no late approval can complete after Cancel.
            // Outbound pairing has no session record yet, so `remove_peer`
            // must also close endpoint connections without a record (see
            // `remove_peer_internal`). Best effort.
            let _ = self.session.remove_peer(peer_id);
            self.peers.set_idle(peer_id);
            let _ = self.updates.send(PeerUpdate::Discovered(self.peers.snapshots(&self.local_peer_id)));
            let _ = self.updates.send(PeerUpdate::ConnectionLost {
                peer_id: peer_id.to_string(),
            });
            let _ = self.updates.send(PeerUpdate::PairingResolved {
                peer_id: peer_id.to_string(),
                approved: false,
            });
        }
    }

    /// Derives the short authentication string shown for a pending pairing from
    /// the local certificate and the certificate the peer presented during the
    /// TLS handshake. Returns `None` until the local certificate exists.
    fn pairing_code_for(&self, peer_certificate: &[u8]) -> Option<String> {
        let cert_store = self.endpoint.cert_store();
        let store = cert_store.lock().unwrap();
        let self_fingerprint = store.get_self_fingerprint()?;
        Some(crate::core::pairing_code::pairing_code(
            &self_fingerprint,
            &cert_fingerprint(peer_certificate),
        ))
    }

    fn handle_pairing_decision(&mut self, peer_id: &str, approved: bool) {
        let Some(pending) = self.pending_pairings.remove(peer_id) else {
            return;
        };

        if pending.conn.is_closed() {
            // The remote went away after we showed the dialog (e.g. it
            // cancelled while our Allow click was in flight). Drop the
            // decision instead of pairing a dead peer.
            tracing::info!(
                peer_id = %peer_id,
                "ignoring pairing decision for closed connection"
            );
            let _ = self.updates.send(PeerUpdate::PairingResolved {
                peer_id: peer_id.to_string(),
                approved: false,
            });
            return;
        }

        if !approved {
            let _ = self.session.close_duplicate_connection(
                pending.conn,
                b"pairing rejected",
            );
            let _ = self.updates.send(PeerUpdate::PairingResolved {
                peer_id: peer_id.to_string(),
                approved: false,
            });
            return;
        }

        let _ = self.session.send_pairing_approved(pending.addr, peer_id);

        if let Ok(updated) = self.config_store.pair_peer(peer_id) {
            let _ = self.updates.send(PeerUpdate::ConfigChanged(updated));
        }

        let _ = self
            .endpoint
            .cert_store()
            .lock()
            .unwrap()
            .trust_peer(peer_id, &pending.certificate);

        let discovered = self.peers.discovered_peer(peer_id);
        let port = pending
            .listen_port
            .or_else(|| discovered.map(|peer| peer.primary_addr.port()))
            .unwrap_or(crate::core::policies::DEFAULT_PORT);
        let primary_addr = std::net::SocketAddr::new(pending.addr.ip(), port);

        let _ = self.session.promote_connection(peer_id, Arc::clone(&pending.conn));
        let edge = self.peers.configured_edge(peer_id);
        let peer_ref = crate::core::api::PeerRefSnapshot {
            peer_id: peer_id.to_string(),
            name: pending.name.clone(),
            platform: pending.platform.clone(),
        };
        let _ = self.session.add_peer(peer_ref, primary_addr, edge, pending.display_bounds, true);
        self.session.mark_connection_established(peer_id);
        let connected = crate::core::controller::peers::ConnectedPeer {
            peer_id: peer_id.to_string(),
            name: pending.name,
            addr: primary_addr,
            conn_addr: Some(pending.addr),
            virtual_bounds: pending.display_bounds,
            display_rects: pending.display_rects,
            direction: crate::core::controller::peers::ConnectedPeerDirection::Inbound,
            platform: pending.platform,
        };
        self.peers.remember_connected(connected.clone());
        let hostname = self
            .peers
            .discovered_peer(&connected.peer_id)
            .map(|peer| peer.hostname)
            .unwrap_or_default();
        RunningController::publish_connected_peer(
            &self.updates,
            &connected,
            &hostname,
            true,
        );
        let _ = self.updates.send(PeerUpdate::PairingResolved {
            peer_id: peer_id.to_string(),
            approved: true,
        });
    }

    pub(super) fn handle_controller_event(&mut self, event: ControllerEvent) {
        if let ControllerEvent::Discovery(event) = event {
            self.handle_discovery_event(event);
            return;
        }

        if let ControllerEvent::PeerUnpaired { peer_id, attempt } = &event {
            if let Some(attempt) = attempt
                && self.peers.dial_state(peer_id) != DialState::Connecting(*attempt)
            {
                return;
            }
            if let Ok(updated) = self.config_store.unpair_peer(peer_id) {
                let _ = self.updates.send(PeerUpdate::ConfigChanged(updated));
            }
            // Local forget (or a rejected dial): drop the pinned certificate
            // and clear pairing waiters so reconnect starts fresh.
            self.remove_trusted_peer(peer_id);
            self.clear_pairing_waiters(peer_id);
            self.peers.set_idle(peer_id);
            return;
        }

        if let ControllerEvent::PairingDecision { peer_id, approved } = &event {
            self.handle_pairing_decision(peer_id, *approved);
            return;
        }

        if let ControllerEvent::CancelConnect { peer_id } = &event {
            // Cancel must not touch pairing config or pinned trust: it only
            // drops waiters and returns the peer to idle so the UI can
            // leave connecting/pairing/backoff state.
            self.clear_pairing_waiters(peer_id);
            self.peers.set_idle(peer_id);
            let _ = self
                .updates
                .send(PeerUpdate::Discovered(self.peers.snapshots(&self.local_peer_id)));
            return;
        }

        if let ControllerEvent::OutboundPairingPending {
            discovered,
            connected,
            certificate,
            connection,
            attempt,
        } = event
        {
            let peer_id = connected.peer_id.clone();
            if self.peers.dial_state(&peer_id) != DialState::Connecting(attempt) {
                self.session
                    .close_duplicate_connection(connection, b"connection cancelled");
                return;
            }
            let name = connected.name.clone();
            let platform = connected.platform.clone();
            let code = self.pairing_code_for(&certificate);
            self.pending_outbound_pairings
                .insert(peer_id.clone(), (discovered, connected, certificate));
            self.peers.set_pairing(&peer_id);
            if let Err(error) = self
                .session
                .promote_connection(&peer_id, Arc::clone(&connection))
            {
                tracing::warn!(peer_id = %peer_id, error = %error, "failed to promote outbound pairing connection");
                self.pending_outbound_pairings.remove(&peer_id);
                self.session.close_duplicate_connection(
                    connection,
                    b"failed to promote pairing connection",
                );
                let attempt = self.peers.set_connecting(&peer_id);
                self.handle_controller_event(ControllerEvent::ConnectFailed { peer_id, attempt });
                return;
            }
            // The peer that dialed out cannot approve, but it still shows the
            // code so both users have something to compare out of band.
            let _ = self.updates.send(PeerUpdate::PairingRequested {
                peer_id: peer_id.clone(),
                name,
                platform,
                code,
                can_respond: false,
            });
            let _ = self
                .updates
                .send(PeerUpdate::Discovered(self.peers.snapshots(&self.local_peer_id)));
            return;
        }

        let config = self.config_store.snapshot();
        let auto_connect = match &event {
            ControllerEvent::ConnectPeer { peer_id }
            | ControllerEvent::RetryPeer { peer_id, .. }
            | ControllerEvent::ConnectFailed { peer_id, .. } => {
                config.should_auto_connect_peer(peer_id)
            }
            _ => config.auto_connect_peers,
        };
        let mut ctx = self.context();
        RunningController::handle_controller_event(&mut ctx, event, auto_connect);
        let _ = self
            .updates
            .send(PeerUpdate::Discovered(self.peers.snapshots(&self.local_peer_id)));
    }

    fn handle_background_result(
        &mut self,
        result: Option<Result<ControllerTaskEvent, tokio::task::JoinError>>,
    ) {
        match result {
            Some(Ok(ControllerTaskEvent::None)) => {}
            Some(Err(error)) if !error.is_cancelled() => {
                tracing::warn!(error = %error, "controller background task failed");
            }
            Some(Err(_)) | None => {}
        }
    }
}
