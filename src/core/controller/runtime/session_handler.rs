use super::{RECONNECT_BACKOFF, RunningController, RuntimeContext};
use crate::core::api::PeerRefSnapshot;
use crate::core::controller::events::PeerUpdate;
use crate::core::controller::peers::{ConnectedPeer, ConnectedPeerDirection};
use crate::core::platform::engine::SessionEvent;
use std::sync::Arc;
use std::time::Instant;

impl RunningController {
    pub(super) fn handle_session_event(
        ctx: &mut RuntimeContext<'_>,
        event: SessionEvent,
        auto_connect: bool,
    ) {
        match event {
            SessionEvent::SessionLost { peer_id } => {
                let _ = ctx.session.remove_peer(&peer_id);
                let removed = ctx.peers.remove_connected(&peer_id);
                if removed.is_some() {
                    let _ = ctx.updates.send(PeerUpdate::ConnectionLost {
                        peer_id: peer_id.clone(),
                    });
                }

                if auto_connect && ctx.peers.is_discovered(&peer_id) {
                    let retry_at = Instant::now() + RECONNECT_BACKOFF;
                    ctx.peers.set_backoff(&peer_id, retry_at);
                    Self::schedule_retry(
                        ctx.controller_tx.clone(),
                        peer_id,
                        retry_at,
                        ctx.background_tasks,
                    );
                } else {
                    ctx.peers.set_idle(&peer_id);
                }
            }
            SessionEvent::SessionDisconnected { peer_id } => {
                let _ = ctx.session.remove_peer(&peer_id);
                let removed = ctx.peers.remove_connected(&peer_id);
                if removed.is_some() {
                    let _ = ctx.updates.send(PeerUpdate::ConnectionLost {
                        peer_id: peer_id.clone(),
                    });
                }
                ctx.peers.set_idle(&peer_id);
            }
            SessionEvent::SessionUnpaired { peer_id } => {
                let _ = ctx.session.unpair_peer(&peer_id);
                let removed = ctx.peers.remove_connected(&peer_id);
                if removed.is_some() {
                    let _ = ctx.updates.send(PeerUpdate::ConnectionLost {
                        peer_id: peer_id.clone(),
                    });
                }
                ctx.peers.set_idle(&peer_id);
            }
            SessionEvent::InboundIdentified {
                peer_id,
                name,
                addr,
                conn,
                display_bounds,
                display_rects,
                platform,
                is_auto_connect: _,
                listen_port,
                certificate: _,
            } => {
                if !auto_connect {
                    ctx.session.close_duplicate_connection(
                        Arc::clone(&conn),
                        b"peer unpaired",
                    );
                    tracing::info!(
                        peer_id = %peer_id,
                        "rejected inbound connection from unpaired peer"
                    );
                    return;
                }

                let discovered = ctx.peers.discovered_peer(&peer_id);
                let port = listen_port
                    .or_else(|| discovered.map(|peer| peer.primary_addr.port()))
                    .unwrap_or(crate::core::policies::DEFAULT_PORT);
                let primary_addr = std::net::SocketAddr::new(addr.ip(), port);

                if let Some(existing) = ctx.peers.connected(&peer_id) {
                    if Self::should_keep_new_connection(
                        ctx.local_peer_id,
                        &peer_id,
                        existing.direction,
                        ConnectedPeerDirection::Inbound,
                    ) {
                        let _ = ctx.session.promote_connection(&peer_id, Arc::clone(&conn));
                        let edge = ctx.peers.configured_edge(&peer_id);
                        let peer_ref = PeerRefSnapshot {
                            peer_id: peer_id.clone(),
                            name: name.clone(),
                            platform: platform.clone(),
                        };
                        let _ = ctx.session.add_peer(peer_ref, primary_addr, edge, display_bounds, true);
                        ctx.session.mark_connection_established(&peer_id);
                        let connected = ConnectedPeer {
                            peer_id: peer_id.clone(),
                            name,
                            addr: primary_addr,
                            conn_addr: Some(addr),
                            virtual_bounds: display_bounds,
                            display_rects,
                            direction: ConnectedPeerDirection::Inbound,
                            platform,
                        };
                        ctx.peers.remember_connected(connected.clone());
                        let hostname = ctx
                            .peers
                            .discovered_peer(&connected.peer_id)
                            .map(|peer| peer.hostname)
                            .unwrap_or_default();
                        Self::publish_connected_peer(
                            &ctx.updates,
                            &connected,
                            &hostname,
                            ctx.peers.is_paired(&connected.peer_id),
                        );
                        tracing::info!(
                            peer_id = %peer_id,
                            "duplicate session resolved: keeping inbound"
                        );
                    } else {
                        ctx.session.close_duplicate_connection(
                            Arc::clone(&conn),
                            b"duplicate: existing session preferred",
                        );
                        tracing::info!(
                            peer_id = %peer_id,
                            "duplicate session resolved: keeping existing session"
                        );
                    }
                } else {
                    let _ = ctx.session.promote_connection(&peer_id, Arc::clone(&conn));
                    ctx.peers.set_idle(&peer_id);
                    let edge = ctx.peers.configured_edge(&peer_id);
                    let peer_ref = PeerRefSnapshot {
                        peer_id: peer_id.clone(),
                        name: name.clone(),
                        platform: platform.clone(),
                    };
                    let _ = ctx.session.add_peer(peer_ref, primary_addr, edge, display_bounds, true);
                    ctx.session.mark_connection_established(&peer_id);
                    let connected = ConnectedPeer {
                        peer_id,
                        name,
                        addr: primary_addr,
                        conn_addr: Some(addr),
                        virtual_bounds: display_bounds,
                        display_rects,
                        direction: ConnectedPeerDirection::Inbound,
                        platform,
                    };
                    ctx.peers.remember_connected(connected.clone());
                    let hostname = ctx
                        .peers
                        .discovered_peer(&connected.peer_id)
                        .map(|peer| peer.hostname)
                        .unwrap_or_default();
                    Self::publish_connected_peer(
                        &ctx.updates,
                        &connected,
                        &hostname,
                        ctx.peers.is_paired(&connected.peer_id),
                    );
                }
            }
            SessionEvent::SessionPairingPending { .. }
            | SessionEvent::SessionPairingApproved { .. } => {}
        }
    }
}
