use super::{ControllerEvent, ControllerTaskEvent, RECONNECT_BACKOFF, RunningController, RuntimeContext};
use crate::core::api::{
    DiscoveryStatusSnapshot, DisplayRectSnapshot, PeerRefSnapshot, PeerSnapshot,
    VirtualBoundsSnapshot,
};
use crate::core::controller::config_state::ConfigStore;
use crate::core::controller::events::PeerUpdate;
use crate::core::controller::peers::{ConnectedPeer, ConnectedPeerDirection, DialState, PeerStore};
use crate::core::controller::{CoreError, CoreResult};
use crate::core::discovery::DiscoveredPeer;
use crate::events::LayoutUpdateEvent;
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::{broadcast, mpsc};
use tokio::task::JoinSet;
use tracing::warn;

impl RunningController {
    pub(super) fn handle_controller_event(
        ctx: &mut RuntimeContext<'_>,
        event: ControllerEvent,
        auto_connect: bool,
    ) {
        match event {
            ControllerEvent::ConnectPeer { peer_id } => {
                if ctx.peers.is_connected(&peer_id)
                    || ctx.peers.dial_state(&peer_id).is_dial_in_flight()
                {
                    return;
                }

                let Some(peer) = ctx.peers.discovered_peer(&peer_id) else {
                    ctx.peers.set_idle(&peer_id);
                    return;
                };

                let attempt = ctx.peers.set_connecting(&peer.peer_id);
                Self::spawn_connection(ctx, peer.into(), false, attempt);
            }
            ControllerEvent::RetryPeer { peer_id, retry_at } => {
                if !matches!(ctx.peers.dial_state(&peer_id), DialState::Backoff(until) if until == retry_at)
                {
                    return;
                }

                if !auto_connect {
                    ctx.peers.set_idle(&peer_id);
                    return;
                }

                // Single-dialer tie-break: the side with the smaller peer_id
                // dials while the larger side waits for inbound. Simultaneous
                // fixed-port redials from both sides collide and livelock, so
                // the larger side must never auto-dial. Manual connects and
                // direct addresses bypass this and always dial.
                if !peer_id.starts_with("direct-")
                    && Self::preferred_connection_direction(ctx.local_peer_id, &peer_id)
                        == ConnectedPeerDirection::Inbound
                {
                    tracing::info!(
                        peer_id = %peer_id,
                        "deferring to inbound connection from smaller peer_id"
                    );
                    ctx.peers.set_idle(&peer_id);
                    return;
                }

                if ctx.peers.is_connected(&peer_id)
                    || ctx.peers.dial_state(&peer_id).is_dial_in_flight()
                {
                    return;
                }

                let Some(peer) = ctx.peers.discovered_peer(&peer_id) else {
                    ctx.peers.set_idle(&peer_id);
                    return;
                };

                if Instant::now() < retry_at {
                    Self::schedule_retry(
                        ctx.controller_tx.clone(),
                        peer_id,
                        retry_at,
                        ctx.background_tasks,
                    );
                    return;
                }

                let attempt = ctx.peers.set_connecting(&peer.peer_id);
                Self::spawn_connection(ctx, peer.into(), true, attempt);
            }
            ControllerEvent::ConnectFailed { peer_id, attempt } => {
                if ctx.peers.dial_state(&peer_id) != DialState::Connecting(attempt) {
                    return;
                }
                // Peer connected via inbound while our outbound was racing — ignore.
                if ctx.peers.is_connected(&peer_id) {
                    return;
                }

                // A stale failure from an earlier attempt must not disturb a
                // handshake that has since succeeded and is awaiting pairing
                // approval on the remote.
                if ctx.peers.dial_state(&peer_id).is_pairing() {
                    return;
                }

                // A failure arriving after Cancel (idle) must not resurrect
                // backoff/retry: the user already asked to stop.
                if matches!(ctx.peers.dial_state(&peer_id), DialState::Idle) {
                    return;
                }

                if peer_id.starts_with("direct-") {
                    ctx.peers.remove_peer(&peer_id);
                    let _ = ctx.updates.send(PeerUpdate::Removed(peer_id));
                    return;
                }

                if !ctx.peers.is_discovered(&peer_id) || !auto_connect {
                    ctx.peers.set_idle(&peer_id);
                    return;
                }

                let retry_at = Instant::now() + RECONNECT_BACKOFF;
                ctx.peers.set_backoff(&peer_id, retry_at);
                Self::schedule_retry(
                    ctx.controller_tx.clone(),
                    peer_id,
                    retry_at,
                    ctx.background_tasks,
                );
            }
            ControllerEvent::PeerUnpaired { .. } => {}
            ControllerEvent::CancelConnect { .. } => {}
            ControllerEvent::PairingDecision { .. } => {}
            ControllerEvent::OutboundPairingPending { .. } => {}
            ControllerEvent::Discovery(_) => {}
        }
    }

    pub(super) fn spawn_connection(
        ctx: &mut RuntimeContext<'_>,
        peer: DiscoveredPeer,
        is_auto_connect: bool,
        mut attempt: Instant,
    ) {
        let local_peer_id = ctx.local_peer_id.to_string();
        let connector = Arc::clone(&ctx.connector);
        let engine = Arc::clone(&ctx.session);
        let peers = Arc::clone(&ctx.peers);
        let updates = Arc::clone(&ctx.updates);
        let controller_tx = ctx.controller_tx.clone();

        ctx.background_tasks.spawn(async move {
            match connector.connect(peer.clone(), is_auto_connect).await {
                Ok(connected) => {
                    if peer.peer_id != connected.peer.peer_id {
                        if peers.dial_state(&peer.peer_id) != DialState::Connecting(attempt) {
                            engine.close_duplicate_connection(
                                connected.connection,
                                b"connection cancelled",
                            );
                            return ControllerTaskEvent::None;
                        }
                        peers.set_idle(&peer.peer_id);
                        attempt = peers.set_connecting(&connected.peer.peer_id);
                    }

                    // The user may have cancelled while we were dialing. If
                    // we are no longer in Connecting, drop the fresh
                    // connection instead of resurrecting pairing state.
                    if peers.dial_state(&connected.peer.peer_id) != DialState::Connecting(attempt) {
                        engine.close_duplicate_connection(
                            connected.connection,
                            b"connection cancelled",
                        );
                        return ControllerTaskEvent::None;
                    }
                    let connection = connected.connection;
                    let connected_peer = connected.peer;
                    let certificate = connected.certificate;
                    let certificate_trusted = connected.certificate_trusted;
                    if let Some(existing) = peers.connected(&connected_peer.peer_id) {
                        if !Self::should_keep_new_connection(
                            &local_peer_id,
                            &connected_peer.peer_id,
                            existing.direction,
                            ConnectedPeerDirection::Outbound,
                        ) {
                            engine.close_duplicate_connection(
                                Arc::clone(&connection),
                                b"duplicate: existing session preferred",
                            );
                            tracing::info!(
                                peer_id = %connected_peer.peer_id,
                                "duplicate session resolved: keeping existing inbound"
                            );
                            return ControllerTaskEvent::None;
                        }
                        tracing::info!(
                            peer_id = %connected_peer.peer_id,
                            "duplicate session resolved: keeping outbound"
                        );
                    }

                    let is_paired = peers.is_paired(&connected_peer.peer_id);
                    if !is_paired || !certificate_trusted {
                        // Re-check after the (possibly slow) handshake: a Cancel
                        // during dial must win over a fresh pairing waiter.
                        if peers.dial_state(&connected_peer.peer_id)
                            != DialState::Connecting(attempt)
                        {
                            engine.close_duplicate_connection(connection, b"connection cancelled");
                            return ControllerTaskEvent::None;
                        }
                        if controller_tx
                            .send(ControllerEvent::OutboundPairingPending {
                                discovered: peer.clone(),
                                connected: connected_peer,
                                certificate,
                                connection: Arc::clone(&connection),
                                attempt,
                            })
                            .await
                            .is_err()
                        {
                            engine.close_duplicate_connection(connection, b"controller stopped");
                        }
                        return ControllerTaskEvent::None;
                    }

                    let connected_id = connected_peer.peer_id.clone();
                    if let Err(error) =
                        engine.promote_connection(&connected_peer.peer_id, connection.clone())
                    {
                        tracing::warn!(
                            peer_id = %connected_peer.peer_id,
                            error = %error,
                            "failed to promote outbound connection"
                        );
                        engine.close_duplicate_connection(
                            connection,
                            b"failed to promote outbound connection",
                        );
                        let _ = controller_tx
                            .send(ControllerEvent::ConnectFailed {
                                peer_id: connected_id,
                                attempt,
                            })
                            .await;
                        return ControllerTaskEvent::None;
                    }

                    if let Err(error) =
                        Self::commit_outbound_peer(&peer, connected_peer, &engine, &peers, &updates)
                    {
                        engine.close_duplicate_connection(
                            connection,
                            b"failed to register connected peer",
                        );
                        tracing::warn!(
                            peer_id = %peer.peer_id,
                            error = %error,
                            "failed to register connected peer"
                        );
                        let _ = controller_tx
                            .send(ControllerEvent::ConnectFailed {
                                peer_id: connected_id,
                                attempt,
                            })
                            .await;
                    }
                }
                Err(error) => {
                    // A dial failure after Cancel must stay idle: do not emit
                    // ConnectFailed (which would schedule backoff/retry).
                    if peers.dial_state(&peer.peer_id) != DialState::Connecting(attempt) {
                        return ControllerTaskEvent::None;
                    }
                    tracing::warn!(
                        peer_id = %peer.peer_id,
                        primary_addr = %peer.primary_addr,
                        error = %error,
                        "peer connector failed"
                    );
                    if error.to_string().contains("unpaired") {
                        let _ = controller_tx
                            .send(ControllerEvent::PeerUnpaired {
                                peer_id: peer.peer_id,
                                attempt: Some(attempt),
                            })
                            .await;
                    } else {
                        let _ = controller_tx
                            .send(ControllerEvent::ConnectFailed {
                                peer_id: peer.peer_id,
                                attempt,
                            })
                            .await;
                    }
                }
            }
            ControllerTaskEvent::None
        });
    }

    pub(super) fn schedule_retry(
        controller_tx: mpsc::Sender<ControllerEvent>,
        peer_id: String,
        retry_at: Instant,
        background_tasks: &mut JoinSet<ControllerTaskEvent>,
    ) {
        background_tasks.spawn(async move {
            tokio::time::sleep_until(tokio::time::Instant::from_std(retry_at)).await;
            let _ = controller_tx
                .send(ControllerEvent::RetryPeer { peer_id, retry_at })
                .await;
            ControllerTaskEvent::None
        });
    }

    pub(super) fn commit_outbound_peer(
        discovered: &DiscoveredPeer,
        connected: ConnectedPeer,
        engine: &Arc<dyn crate::core::platform::SessionEngine>,
        peers: &PeerStore,
        updates: &broadcast::Sender<PeerUpdate>,
    ) -> CoreResult<()> {
        if discovered.peer_id.starts_with("direct-") && discovered.peer_id != connected.peer_id {
            peers.remove_peer(&discovered.peer_id);
            let _ = updates.send(PeerUpdate::Removed(discovered.peer_id.clone()));
        }

        let peer_ref = PeerRefSnapshot {
            peer_id: connected.peer_id.clone(),
            name: connected.name.clone(),
            platform: connected.platform.clone(),
        };
        let edge = peers.configured_edge(&connected.peer_id);
        engine
            .add_peer(
                peer_ref,
                connected.addr,
                edge,
                connected.virtual_bounds,
                false,
            )
            .map_err(|error| CoreError::Connection(error.to_string()))?;
        engine.mark_connection_established(&connected.peer_id);
        peers.remember_connected(connected.clone());

        let hostname = if discovered.peer_id.starts_with("direct-") {
            connected.name.clone()
        } else {
            peers
                .discovered_peer(&connected.peer_id)
                .map(|peer| peer.hostname)
                .unwrap_or_default()
        };
        Self::publish_connected_peer(
            updates,
            &connected,
            &hostname,
            peers.is_paired(&connected.peer_id),
        );
        Ok(())
    }

    pub(super) fn publish_connected_peer(
        updates: &broadcast::Sender<PeerUpdate>,
        connected: &ConnectedPeer,
        hostname: &str,
        is_paired: bool,
    ) {
        let _ = updates.send(PeerUpdate::Discovered(vec![PeerSnapshot {
            peer_id: connected.peer_id.clone(),
            name: connected.name.clone(),
            hostname: hostname.to_string(),
            addr: connected.addr.to_string(),
            platform: connected.platform.clone(),
            screen: None,
            virtual_bounds: connected
                .virtual_bounds
                .map(|bounds| VirtualBoundsSnapshot {
                    x: bounds.x,
                    y: bounds.y,
                    width: bounds.width,
                    height: bounds.height,
                }),
            display_rects: connected
                .display_rects
                .iter()
                .copied()
                .map(|bounds| DisplayRectSnapshot {
                    x: bounds.x,
                    y: bounds.y,
                    width: bounds.width,
                    height: bounds.height,
                })
                .collect(),
            last_seen_ms: 0,
            discovery_status: DiscoveryStatusSnapshot::Online,
            is_local: false,
            connected: true,
            paired: is_paired,
            connection_status: Some("connected".to_string()),
        }]));
    }

    pub(super) fn should_keep_new_connection(
        local_peer_id: &str,
        remote_peer_id: &str,
        existing_direction: ConnectedPeerDirection,
        new_direction: ConnectedPeerDirection,
    ) -> bool {
        if existing_direction == new_direction {
            return true;
        }

        new_direction == Self::preferred_connection_direction(local_peer_id, remote_peer_id)
    }

    pub(super) fn preferred_connection_direction(
        local_peer_id: &str,
        remote_peer_id: &str,
    ) -> ConnectedPeerDirection {
        if local_peer_id < remote_peer_id {
            ConnectedPeerDirection::Outbound
        } else {
            ConnectedPeerDirection::Inbound
        }
    }

    pub(super) async fn recv_layout_update(
        layout_update_rx: &mut mpsc::Receiver<LayoutUpdateEvent>,
    ) -> Option<LayoutUpdateEvent> {
        layout_update_rx.recv().await
    }

    pub(super) fn handle_layout_update(
        config_store: &ConfigStore,
        engine: &Arc<dyn crate::core::platform::SessionEngine>,
        updates: &broadcast::Sender<PeerUpdate>,
        mut latest: LayoutUpdateEvent,
        layout_update_rx: &mut mpsc::Receiver<LayoutUpdateEvent>,
    ) {
        loop {
            match layout_update_rx.try_recv() {
                Ok(next) if next.timestamp >= latest.timestamp => latest = next,
                Ok(_) => {}
                Err(tokio::sync::mpsc::error::TryRecvError::Empty) => break,
                Err(tokio::sync::mpsc::error::TryRecvError::Disconnected) => break,
            }
        }

        match config_store.apply_layout_update(latest) {
            Ok(Some(snapshot)) => {
                if let Err(error) = engine.reconcile_layout(&snapshot) {
                    warn!(error = %error, "failed to reconcile remote layout update");
                }
                let _ = updates.send(PeerUpdate::ConfigChanged(snapshot));
            }
            Ok(None) => {}
            Err(error) => warn!(error = %error, "failed to apply remote layout update"),
        }
    }
}
