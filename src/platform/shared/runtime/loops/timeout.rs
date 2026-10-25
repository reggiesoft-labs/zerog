//! Timeout checking utilities for runtime worker threads.

use std::sync::Arc;
use std::time::Duration;

use tracing::warn;

use super::super::super::topology::recovery_point;
use super::{MovementSmoother, QuicEndpoint, ThreadContext};

pub(crate) const PEER_TIMEOUT_CHECK_INTERVAL: Duration = Duration::from_millis(100);
const HANDOFF_ACK_WARN_US: u64 = 500_000;
const HANDOFF_TIMEOUT_US: u64 = 1_500_000;

pub(crate) fn run_timeout_check(
    ctx: &ThreadContext,
    endpoint: &QuicEndpoint,
    smoother: &mut MovementSmoother,
) {
    let now_us = ctx.network.mono_us();

    let delayed_handoff = ctx
        .cursor
        .report_delayed_handoff(now_us, HANDOFF_ACK_WARN_US);

    if let Some(addr) = delayed_handoff {
        warn!(
            domain = "runtime.transition",
            transition_key = "handoff_ack_delayed",
            handoff_id = addr.motion_session_id,
            phase = "AwaitingAck",
            peer_id = %addr.peer_id,
            peer = %addr.addr,
            request_seq = addr.request_seq,
            edge = ?addr.edge,
            source_x = addr.source_position.0,
            source_y = addr.source_position.1,
            receiver_position_bp = addr.receiver_position_bp,
            elapsed_us = now_us.saturating_sub(addr.started_us),
            "TakeControl acknowledgement delayed; retaining handoff until acknowledgement or connection failure"
        );
    }

    if let Some(pending) = ctx.cursor.pending_handoff() {
        if now_us.saturating_sub(pending.started_us) >= HANDOFF_TIMEOUT_US {
            warn!(
                domain = "runtime.transition",
                transition_key = "handoff_timeout",
                handoff_id = pending.motion_session_id,
                peer_id = %pending.peer_id,
                peer = %pending.addr,
                "handoff timed out waiting for ack; restoring local control"
            );
            super::input_loop::perform_emergency_unlock(ctx);
            smoother.deactivate();
        }
    }

    for conn in endpoint.all_connections() {
        if conn.is_closed() {
            let addr = conn.addr();
            let peer_id = conn.peer_id();
            let resolved_peer_id = if !peer_id.is_empty() {
                Some(peer_id.clone())
            } else {
                ctx.network.peer_id_for_conn_addr(addr)
            };
            let canonical = resolved_peer_id
                .as_deref()
                .and_then(|id| endpoint.get_connection(id))
                .or_else(|| endpoint.get_connection_by_addr(addr));
            let is_canonical = canonical
                .as_ref()
                .is_some_and(|registered| Arc::ptr_eq(registered, &conn));
            let canonical_is_live = canonical
                .as_ref()
                .is_some_and(|registered| !registered.is_closed());

            if !is_canonical && canonical_is_live {
                endpoint.close_duplicate_connection(&conn, b"duplicate connection closed");
                continue;
            }

            if !is_canonical && canonical.is_none() && resolved_peer_id.is_none() {
                endpoint.close_duplicate_connection(&conn, b"unregistered connection closed");
                continue;
            }

            recover_from_peer_loss(ctx, resolved_peer_id.as_deref(), addr, "connection_closed");
            smoother.deactivate();
            if let Some(sync) = ctx.clipboard_sync.as_ref() {
                sync.notify_peer_disconnected();
            }
            if let Some(sync) = ctx.transfer_sync.as_ref() {
                sync.notify_peer_disconnected(resolved_peer_id.as_deref());
            }
            endpoint.close_duplicate_connection(&conn, b"connection closed");
            if let Some(id) = resolved_peer_id {
                ctx.network.emit_session_lost(&id);
            }
        }
    }

    if ctx.cursor.mode().is_remote_control() {
        let active_target = ctx.network.active_target();
        let target_live = active_target.is_some_and(|addr| {
            let ref_us = ctx.network.peer_timeout_reference_us(addr);
            let conn_live = endpoint
                .get_connection_by_addr(addr)
                .is_some_and(|conn| !conn.is_closed());
            conn_live
                && ref_us > 0
                && now_us.saturating_sub(ref_us) <= super::PEER_TIMEOUT.as_micros() as u64
        });
        if !target_live {
            warn!("remote control active without live target; restoring local control");
            super::input_loop::perform_emergency_unlock(ctx);
            smoother.deactivate();
        }
    }
}

pub(crate) fn recover_from_peer_loss(
    ctx: &ThreadContext,
    peer_id: Option<&str>,
    addr: std::net::SocketAddr,
    reason: &'static str,
) {
    if let Some(sync) = ctx.transfer_sync.as_ref() {
        sync.notify_peer_disconnected(peer_id);
    }

    let active_target = ctx.network.active_target();
    let active_peer_id = active_target.and_then(|target| ctx.network.peer_id_for_addr(target));
    let was_active =
        active_target == Some(addr) || peer_id.is_some() && active_peer_id.as_deref() == peer_id;
    let had_session = ctx.cursor.is_session_peer(peer_id, addr);
    let was_remote = ctx.cursor.mode().is_remote_control();
    warn!(
        domain = "runtime.connection",
        transition_key = "peer_lost",
        peer = %addr,
        peer_id,
        reason,
        "peer connection lost"
    );

    ctx.network.remove_conn_addr(addr);
    if !was_active && !had_session && !was_remote {
        return;
    }

    let (pending_handoff, runtime_recovery) =
        ctx.cursor.take_pending_handoff_for_peer(peer_id, addr);
    ctx.network.stop_outbound_input();
    ctx.cursor.clear_pressed_buttons();
    ctx.edge_detector
        .lock()
        .expect("edge_detector lock poisoned")
        .reset();
    let (recovery, recovery_source) = if was_remote {
        pending_handoff
            .as_ref()
            .map(|handoff| handoff.source_position)
            .map(|position| (Some(position), "handoff_source"))
            .or_else(|| runtime_recovery.map(|position| (Some(position), "runtime_source")))
            .or_else(|| {
                ctx.cursor
                    .last_edge_position()
                    .map(|position| (Some(position), "last_edge"))
            })
            .or_else(|| match ctx.transition.topology_snapshot() {
                Ok(topology) => Some((Some(recovery_point(&topology)), "topology_center")),
                Err(err) => {
                    tracing::debug!(error = %err, "failed to read topology for recovery warp");
                    Some((None, "none"))
                }
            })
            .unwrap_or((None, "none"))
    } else {
        (None, "already_local")
    };
    tracing::info!(
        domain = "runtime.transition",
        transition_key = "handoff_recovery",
        handoff_id = pending_handoff
            .as_ref()
            .map(|value| value.motion_session_id),
        recovery_x = recovery.map(|value| value.0),
        recovery_y = recovery.map(|value| value.1),
        recovery_source,
        "restoring local cursor after peer loss"
    );
    let _ = ctx
        .cursor
        .transition_to_local_control(&*ctx.injection, recovery);
    ctx.cursor.record_transition(ctx.network.mono_us());
}
