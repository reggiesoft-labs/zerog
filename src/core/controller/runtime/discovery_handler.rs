use super::{RunningController, RuntimeContext};
use crate::core::controller::events::PeerUpdate;
use crate::core::controller::peers::{ConnectedPeerDirection, DialState};
use crate::core::discovery::DiscoveryEvent;
use std::time::{Duration, Instant};

impl RunningController {
    pub(super) fn handle_discovery_event(
        ctx: &mut RuntimeContext<'_>,
        event: DiscoveryEvent,
        auto_connect: bool,
    ) {
        match event {
            DiscoveryEvent::Discovered(peer) => {
                if peer.peer_id == ctx.local_peer_id {
                    return;
                }
                let peer_id = peer.peer_id.clone();
                let is_direct = peer_id.starts_with("direct-");
                ctx.peers.remember_discovered(peer.clone());
                let snapshots = ctx.peers.snapshots(ctx.local_peer_id);
                let _ = ctx.updates.send(PeerUpdate::Discovered(snapshots));
                if !auto_connect && !is_direct {
                    return;
                }
                if ctx.peers.is_connected(&peer_id)
                    || ctx.peers.dial_state(&peer_id).is_dial_in_flight()
                {
                    return;
                }
                if is_direct || matches!(ctx.peers.dial_state(&peer_id), DialState::Idle) {
                    let prefers_outbound = is_direct
                        || Self::preferred_connection_direction(ctx.local_peer_id, &peer_id)
                            == ConnectedPeerDirection::Outbound;

                    if prefers_outbound {
                        let attempt = ctx.peers.set_connecting(&peer_id);
                        Self::spawn_connection(ctx, peer, !is_direct && auto_connect, attempt);
                    } else {
                        let defer_duration = Duration::from_millis(500);
                        let retry_at = Instant::now() + defer_duration;
                        ctx.peers.set_backoff(&peer_id, retry_at);
                        Self::schedule_retry(
                            ctx.controller_tx.clone(),
                            peer_id,
                            retry_at,
                            ctx.background_tasks,
                        );
                    }
                    let _ = ctx
                        .updates
                        .send(PeerUpdate::Discovered(ctx.peers.snapshots(ctx.local_peer_id)));
                }
            }
            DiscoveryEvent::Removed { peer_id, .. } => {
                ctx.peers.expire_discovered(&peer_id);
                if !ctx.peers.is_connected(&peer_id)
                    && !ctx.peers.dial_state(&peer_id).is_pairing()
                {
                    ctx.peers.set_idle(&peer_id);
                    let _ = ctx.updates.send(PeerUpdate::Removed(peer_id));
                }
            }
        }
    }
}
