//! Incoming packet and event handling for runtime receive threads.
//!
//! Processes decoded packets, routes control and connection events, and
//! updates transfer/transition state in response to peer messages.

use std::net::SocketAddr;
use std::sync::Arc;

use crate::core::layout::rebase_layout_update;
use tracing::{debug, info, trace, warn};

use crate::events::{Event, LayoutUpdateEvent, MotionCheckpoint, PROTOCOL_VERSION, Packet};
use crate::platform::PeerEdge;

use super::super::quic::PeerConnection;
use super::super::topology::{
    DisplayTopology, clamp_to_display_union, edge_point_from_contact_span_bp,
};
use super::ThreadContext;
use super::cursor_runtime::{HandoffAck, HandoffRequest};
use super::edge_transitions::handle_owner_cursor_position;
use super::motion::OutboundMotion;
use super::process_network_event_internal;
use super::smoother::MovementSmoother;
use super::transport::is_input_event;

struct IncomingHandoff {
    request: HandoffRequest,
    request_seq: u32,
    sender_id: u64,
    sender_peer_id: String,
    receiver_edge: PeerEdge,
    position: (i32, i32),
    topology: DisplayTopology,
}

#[derive(Clone, Copy)]
enum HandoffReply {
    Accepted,
    Rejected(&'static str),
}

pub(crate) fn handle_received_packet(
    ctx: &ThreadContext,
    conn: &Arc<PeerConnection>,
    packet: Packet,
    smoother: &mut MovementSmoother,
    outbound_motion: &mut OutboundMotion,
) {
    let addr = conn.addr();

    if packet.version != PROTOCOL_VERSION {
        return;
    }

    if ctx.network.is_local_sender(packet.sender_id) {
        return;
    }

    // Try addr-based reachability first (works when conn.addr() matches
    // the peer's registered address in the registry). Falls back to the
    // conn_addr -> peer_id mapping for the common case where the datagram
    // arrives on a different QUIC connection than the one that last
    // updated the peer's address.
    ctx.network.mark_packet_received(addr);
    let reachable_via_addr = ctx.network.mark_peer_reachable(addr);
    if reachable_via_addr {
        info!(peer = %addr, "peer is reachable");
    } else if ctx.network.peer_id_for_addr(addr).is_none()
        && let Some(peer_id) = ctx.network.peer_id_for_conn_addr(addr)
    {
        ctx.network.mark_packet_received_for_peer(&peer_id);
        if ctx.network.mark_peer_reachable_for_peer(&peer_id) {
            info!(peer_id = %peer_id, addr = %addr, "peer is reachable (via conn mapping)");
        }
    }

    let request_seq = packet.seq;
    let sender_id = packet.sender_id;
    for event in packet.events {
        handle_received_event(
            ctx,
            conn,
            event,
            request_seq,
            sender_id,
            smoother,
            outbound_motion,
        );
    }
}

fn handle_received_event(
    ctx: &ThreadContext,
    conn: &Arc<PeerConnection>,
    event: Event,
    request_seq: u32,
    sender_id: u64,
    smoother: &mut MovementSmoother,
    outbound_motion: &mut OutboundMotion,
) {
    let addr = conn.addr();
    if is_input_event(&event) {
        ctx.network.record_inbound_input_event();
    }

    match event {
        Event::TakeControl {
            edge,
            receiver_position_bp,
            motion_session_id,
            return_motion,
            caps_lock,
            ..
        } => {
            let Some(request) = HandoffRequest::from_wire(
                edge,
                receiver_position_bp,
                motion_session_id,
                return_motion,
                caps_lock,
            ) else {
                warn!(
                    domain = "runtime.transition",
                    transition_key = "handoff_rejected",
                    handoff_id = motion_session_id,
                    phase = ctx.cursor.phase_name(),
                    peer = %addr,
                    request_seq,
                    rejection_reason = "malformed_request",
                    "ignored malformed TakeControl request"
                );
                send_take_control_ack(
                    ctx,
                    conn,
                    request_seq,
                    motion_session_id,
                    HandoffReply::Rejected("malformed_request"),
                );
                return;
            };
            handle_take_control_request(
                ctx,
                conn,
                request_seq,
                sender_id,
                request,
                smoother,
                outbound_motion,
            );
        }
        Event::TakeControlAck {
            request_seq,
            accepted,
        } => handle_take_control_ack(ctx, conn, request_seq, accepted, smoother),
        Event::ReleaseControl => handle_release_control(ctx, smoother),
        Event::Ping {
            nonce,
            sent_mono_us,
        } => {
            let _ = ctx.network.send_packet_to(
                addr,
                vec![Event::Pong {
                    nonce,
                    echo_mono_us: sent_mono_us,
                }],
            );
        }
        Event::LayoutUpdate(update) => handle_layout_update(ctx, addr, update),
        Event::ConnectionRequest { .. }
        | Event::ConnectionAccept { .. }
        | Event::ConnectionReject { .. } => {
            debug!(addr = %addr, "ignored legacy packet handshake event");
        }
        Event::DisconnectNotify { peer_id, peer_name } => {
            handle_disconnect_notification(ctx, conn, peer_id, peer_name, smoother);
        }
        Event::UnpairNotify { peer_id, peer_name } => {
            handle_unpair_notification(ctx, conn, peer_id, peer_name, smoother);
        }
        Event::PairingPending { peer_id } => {
            ctx.network.emit_session_pairing_pending(&peer_id);
        }
        Event::PairingApproved { peer_id } => {
            ctx.network.emit_session_pairing_approved(&peer_id);
        }
        Event::Heartbeat { .. } | Event::Pong { .. } => {}
        Event::ClipboardText {
            content,
            sequence,
            content_hash,
            timestamp,
        } => {
            if let Some(sync) = ctx.clipboard_sync.as_ref() {
                sync.handle_received_clipboard_text(content, sequence, content_hash, timestamp);
            }
        }
        Event::ClipboardAck {
            sequence,
            received_hash: _,
        } => {
            if let Some(sync) = ctx.clipboard_sync.as_ref() {
                sync.handle_received_ack(sequence);
            }
        }
        Event::ClipboardSyncRequest { request_id } => {
            let _ = request_id;
        }
        Event::FileOffer { manifest } => {
            let is_for_us = match manifest.target_peer_id.as_deref() {
                Some(target) => {
                    ctx.local_identity
                        .as_ref()
                        .map(|id| id.peer_id.as_str())
                        == Some(target)
                }
                None => {
                    ctx.local_identity
                        .as_ref()
                        .map(|id| id.peer_id.as_str())
                        != Some(&manifest.sender_peer_id)
                }
            };
            if is_for_us {
                if let Some(sync) = ctx.transfer_sync.as_ref() {
                    sync.handle_received_offer(manifest);
                }
            }
        }
        Event::FileAccept { transfer_id } => {
            if let Some(sync) = ctx.transfer_sync.as_ref() {
                sync.handle_received_accept(transfer_id);
            }
        }
        Event::FileReject {
            transfer_id,
            reason,
        } => {
            if let Some(sync) = ctx.transfer_sync.as_ref() {
                sync.handle_received_reject(&transfer_id, &reason);
            }
        }
        Event::FileChunk { chunk } => {
            if let Some(sync) = ctx.transfer_sync.as_ref() {
                sync.handle_received_chunk(chunk);
            }
        }
        Event::FileAck { .. } => {}
        Event::FileCancel { transfer_id } => {
            if let Some(sync) = ctx.transfer_sync.as_ref() {
                sync.handle_received_cancel(&transfer_id);
            }
        }
        other => handle_owned_event(ctx, other, request_seq, sender_id, smoother),
    }
}

fn handle_layout_update(ctx: &ThreadContext, addr: SocketAddr, update: LayoutUpdateEvent) {
    let sender_peer_id = ctx
        .network
        .peer_id_for_addr(addr)
        .or_else(|| ctx.network.peer_id_for_conn_addr(addr));
    let receiver_peer_id = ctx
        .local_identity
        .as_ref()
        .map(|peer| peer.peer_id.as_str());
    let (Some(sender_peer_id), Some(receiver_peer_id)) =
        (sender_peer_id.as_deref(), receiver_peer_id)
    else {
        debug!(
            sender_peer_id = ?sender_peer_id,
            receiver_peer_id = ?receiver_peer_id,
            "ignored layout update from unidentified peer"
        );
        return;
    };
    let Some(update) = rebase_layout_update(update, sender_peer_id, receiver_peer_id) else {
        debug!(
            sender_peer_id,
            receiver_peer_id, "ignored layout update without receiver placement"
        );
        return;
    };
    if let Err(error) = ctx.layout_updates.try_send(update) {
        debug!(error = %error, "failed to forward layout update to controller");
    }
}

fn handle_owned_event(
    ctx: &ThreadContext,
    event: Event,
    request_seq: u32,
    sender_id: u64,
    smoother: &mut MovementSmoother,
) {
    if !ctx.cursor.mode().owns_cursor() {
        return;
    }

    let Event::MouseMove {
        dx,
        dy,
        total_dx,
        total_dy,
        motion_session_id,
    } = event
    else {
        let _ =
            process_network_event_internal(&*ctx.injection, Some(Arc::clone(&ctx.cursor)), event);
        return;
    };
    let Some((x, y)) = smoother.apply_remote_motion(
        sender_id,
        request_seq,
        motion_session_id,
        total_dx,
        total_dy,
    ) else {
        return;
    };
    trace!(
        sender_id,
        packet_seq = request_seq,
        motion_session_id,
        total_dx,
        total_dy,
        target_x = x,
        target_y = y,
        "accepted remote motion"
    );
    if dx == 0 && dy == 0 {
        return;
    }
    let return_motion = Some(MotionCheckpoint {
        session_id: motion_session_id,
        total_dx,
        total_dy,
    });
    handle_owner_cursor_position(
        ctx,
        ctx.network.mono_us(),
        x.round() as i32,
        y.round() as i32,
        dx,
        dy,
        return_motion,
    );
    if !ctx.cursor.mode().owns_cursor() {
        smoother.deactivate();
    }
}

fn handle_take_control_request(
    ctx: &ThreadContext,
    conn: &Arc<PeerConnection>,
    request_seq: u32,
    sender_id: u64,
    request: HandoffRequest,
    smoother: &mut MovementSmoother,
    outbound_motion: &mut OutboundMotion,
) {
    let addr = conn.addr();
    let motion_session_id = request.motion_session_id;
    if smoother.matches_motion_session(sender_id, motion_session_id) {
        send_take_control_ack(
            ctx,
            conn,
            request_seq,
            motion_session_id,
            HandoffReply::Accepted,
        );
        return;
    }
    let cur_mode = ctx.cursor.mode();
    info!(
        domain = "runtime.transition",
        transition_key = "take_control_received",
        handoff_id = motion_session_id,
        from_mode = ?cur_mode,
        phase = ctx.cursor.phase_name(),
        peer = %addr,
        sender_id,
        request_seq,
        requested_edge = ?request.edge,
        receiver_position_bp = request.receiver_position_bp,
        "received TakeControl"
    );

    let Some(handoff) = prepare_handoff(ctx, conn, request_seq, sender_id, request) else {
        return;
    };
    if !ctx
        .cursor
        .begin_inbound(&handoff.sender_peer_id, addr, motion_session_id)
    {
        debug!(
            domain = "runtime.transition",
            transition_key = "handoff_rejected",
            handoff_id = motion_session_id,
            peer = %addr,
            request_seq,
            rejection_reason = "runtime_busy",
            "ignored TakeControl because cursor runtime is busy"
        );
        send_take_control_ack(
            ctx,
            conn,
            request_seq,
            motion_session_id,
            HandoffReply::Rejected("runtime_busy"),
        );
        return;
    }
    debug!(
        ownership = ?ctx.cursor.mode(),
        phase = ctx.cursor.phase_name(),
        peer = %addr,
        request_seq,
        "cursor runtime started inbound handoff"
    );
    debug!(
        receiver_edge = ?handoff.receiver_edge,
        x = handoff.position.0,
        y = handoff.position.1,
        "TakeControl warp target"
    );
    if ctx
        .cursor
        .transition_to_local_control(&*ctx.injection, Some(handoff.position))
        .is_err()
    {
        warn!(
            domain = "runtime.transition",
            transition_key = "handoff_rejected",
            handoff_id = motion_session_id,
            peer = %addr,
            request_seq,
            rejection_reason = "transition_failed",
            "failed to enter local control for TakeControl"
        );
        ctx.cursor.recover();
        send_take_control_ack(
            ctx,
            conn,
            request_seq,
            motion_session_id,
            HandoffReply::Rejected("transition_failed"),
        );
        return;
    }
    let restored = ctx
        .cursor
        .begin_inbound(&handoff.sender_peer_id, addr, motion_session_id);
    debug_assert!(restored);

    complete_handoff(ctx, conn, &handoff, smoother, outbound_motion);
    if let Some(active) = handoff.request.caps_lock
        && let Err(error) = ctx.injection.set_caps_lock(active)
    {
        debug!(
            domain = "runtime.transition",
            transition_key = "caps_lock_sync_failed",
            handoff_id = motion_session_id,
            peer = %addr,
            active,
            error = %error,
            "failed to sync Caps Lock state on handoff"
        );
    }
    send_take_control_ack(
        ctx,
        conn,
        request_seq,
        motion_session_id,
        HandoffReply::Accepted,
    );
}

fn prepare_handoff(
    ctx: &ThreadContext,
    conn: &Arc<PeerConnection>,
    request_seq: u32,
    sender_id: u64,
    request: HandoffRequest,
) -> Option<IncomingHandoff> {
    let addr = conn.addr();
    let receiver_edge = match request.edge {
        PeerEdge::Left => PeerEdge::Right,
        PeerEdge::Right => PeerEdge::Left,
        PeerEdge::Top => PeerEdge::Bottom,
        PeerEdge::Bottom => PeerEdge::Top,
    };
    let sender_peer_id = ctx
        .network
        .peer_id_for_addr(addr)
        .or_else(|| ctx.network.peer_id_for_conn_addr(addr));
    let contact = sender_peer_id.as_deref().and_then(|peer_id| {
        ctx.layout
            .contact_for_peer_local_position(peer_id, receiver_edge, request.receiver_position_bp)
            .map(|contact| (peer_id.to_owned(), contact))
    });
    let Some((sender_peer_id, contact)) = contact else {
        warn!(
            domain = "runtime.transition",
            transition_key = "handoff_rejected",
            handoff_id = request.motion_session_id,
            ?receiver_edge,
            position_bp = request.receiver_position_bp,
            sender_peer_id = ?sender_peer_id,
            peer = %addr,
            request_seq,
            rejection_reason = "missing_contact",
            "ignored TakeControl without matching runtime contact"
        );
        send_take_control_ack(
            ctx,
            conn,
            request_seq,
            request.motion_session_id,
            HandoffReply::Rejected("missing_contact"),
        );
        return None;
    };
    let topology = match ctx.transition.topology_snapshot() {
        Ok(topology) => topology,
        Err(error) => {
            warn!(
                domain = "runtime.transition",
                transition_key = "handoff_rejected",
                handoff_id = request.motion_session_id,
                peer = %addr,
                request_seq,
                rejection_reason = "missing_topology",
                error = %error,
                "ignored TakeControl without display topology"
            );
            send_take_control_ack(
                ctx,
                conn,
                request_seq,
                request.motion_session_id,
                HandoffReply::Rejected("missing_topology"),
            );
            return None;
        }
    };
    let position =
        edge_point_from_contact_span_bp(contact.local, &topology, request.receiver_position_bp);
    Some(IncomingHandoff {
        request,
        request_seq,
        sender_id,
        sender_peer_id,
        receiver_edge,
        position,
        topology,
    })
}

fn complete_handoff(
    ctx: &ThreadContext,
    conn: &Arc<PeerConnection>,
    handoff: &IncomingHandoff,
    smoother: &mut MovementSmoother,
    outbound_motion: &mut OutboundMotion,
) {
    let addr = conn.addr();
    let motion_session_id = handoff.request.motion_session_id;
    let position = handoff.position;
    ctx.cursor.record_transition(ctx.network.mono_us());
    info!(
        domain = "runtime.transition",
        transition_key = "mode_change",
        handoff_id = motion_session_id,
        from_mode = "RemoteControl",
        to_mode = "LocalControl",
        phase = "ReceivingSession",
        peer = %addr,
        request_seq = handoff.request_seq,
        entry_x = position.0,
        entry_y = position.1,
        "transfer mode transitioned"
    );
    ctx.network.stop_outbound_input();

    smoother.activate();
    let sender_bounds = ctx.network.peer_display_bounds(&handoff.sender_peer_id);
    smoother.set_displays(
        handoff.topology.displays.clone(),
        Some(handoff.topology.virtual_bounds),
        sender_bounds,
    );
    smoother.snap_to(f64::from(position.0), f64::from(position.1));
    let _ = smoother.start_motion(handoff.sender_id, handoff.request_seq, motion_session_id);

    let (tail_dx_i32, tail_dy_i32) = outbound_motion.take_tail(handoff.request.return_motion);
    let target_pos = if tail_dx_i32 != 0 || tail_dy_i32 != 0 {
        let replayed_x = position.0.saturating_add(tail_dx_i32);
        let replayed_y = position.1.saturating_add(tail_dy_i32);
        let replayed = clamp_to_display_union(replayed_x, replayed_y, &handoff.topology);
        smoother.snap_to(f64::from(replayed.0), f64::from(replayed.1));
        replayed
    } else {
        position
    };
    if ctx
        .injection
        .inject_mouse_move_subpixel(f64::from(target_pos.0), f64::from(target_pos.1))
        .is_ok()
    {
        smoother.commit_injection((f64::from(target_pos.0), f64::from(target_pos.1)));
    }
}

fn handle_release_control(ctx: &ThreadContext, smoother: &mut MovementSmoother) {
    info!(
        domain = "runtime.transition",
        transition_key = "release_control_received",
        from_mode = ?ctx.cursor.mode(),
        "received ReleaseControl"
    );
    let _ = ctx.injection.release_all_keys();
    if ctx.cursor.mode().is_remote_control() {
        ctx.network.stop_outbound_input();
    }
    smoother.deactivate();
    ctx.cursor.recover();
}

fn send_take_control_ack(
    ctx: &ThreadContext,
    conn: &Arc<PeerConnection>,
    request_seq: u32,
    handoff_id: u64,
    reply: HandoffReply,
) {
    let (accepted, rejection_reason) = match reply {
        HandoffReply::Accepted => (true, ""),
        HandoffReply::Rejected(reason) => (false, reason),
    };
    if let Err(error) = ctx.network.send_packet_on_connection(
        conn,
        vec![Event::TakeControlAck {
            request_seq,
            accepted,
        }],
    ) {
        warn!(
            error = %error,
            request_seq,
            accepted,
            handoff_id,
            rejection_reason,
            "failed to send TakeControl acknowledgement"
        );
    } else {
        debug!(
            domain = "runtime.transition",
            transition_key = "handoff_ack_sent",
            handoff_id,
            request_seq,
            accepted,
            rejection_reason = (!rejection_reason.is_empty()).then_some(rejection_reason),
            "sent TakeControl acknowledgement"
        );
    }
}

fn handle_take_control_ack(
    ctx: &ThreadContext,
    conn: &Arc<PeerConnection>,
    request_seq: u32,
    accepted: bool,
    smoother: &mut MovementSmoother,
) {
    let addr = conn.addr();
    let peer_id = ctx
        .network
        .peer_id_for_addr(addr)
        .or_else(|| ctx.network.peer_id_for_conn_addr(addr));

    let (runtime_result, handoff_context) =
        ctx.cursor
            .resolve_outbound(peer_id.as_deref(), addr, request_seq, accepted);

    let Some(context) = handoff_context else {
        debug!(
            domain = "runtime.transition",
            transition_key = "handoff_ack_stale",
            addr = %addr,
            request_seq,
            accepted,
            "ignored stale TakeControl acknowledgement"
        );
        return;
    };

    if runtime_result == HandoffAck::Accepted {
        info!(
            domain = "runtime.transition",
            transition_key = "take_control_acknowledged",
            handoff_id = context.motion_session_id,
            phase = "RemoteSession",
            peer_id = %context.peer_id,
            peer = %addr,
            request_seq,
            edge = ?context.edge,
            source_x = context.source_position.0,
            source_y = context.source_position.1,
            receiver_position_bp = context.receiver_position_bp,
            elapsed_us = ctx
                .network
                .mono_us()
                .saturating_sub(context.started_us),
            "TakeControl acknowledged"
        );
        return;
    }

    warn!(
        domain = "runtime.transition",
        transition_key = "take_control_rejected",
        handoff_id = context.motion_session_id,
        phase = "Idle",
        peer_id = %context.peer_id,
        peer = %addr,
        request_seq,
        edge = ?context.edge,
        source_x = context.source_position.0,
        source_y = context.source_position.1,
        receiver_position_bp = context.receiver_position_bp,
        rejection_reason = "receiver_rejected",
        elapsed_us = ctx
            .network
            .mono_us()
            .saturating_sub(context.started_us),
        "TakeControl rejected"
    );
    restore_local_control_after_failed_handoff(ctx, smoother, &context);
}

fn restore_local_control_after_failed_handoff(
    ctx: &ThreadContext,
    smoother: &mut MovementSmoother,
    handoff: &super::super::transition::HandoffContext,
) {
    if !ctx.cursor.mode().is_remote_control() {
        return;
    }

    ctx.network.stop_outbound_input();
    let recovery = Some(handoff.source_position);
    info!(
        domain = "runtime.transition",
        transition_key = "handoff_recovery",
        handoff_id = handoff.motion_session_id,
        phase = "Idle",
        recovery_x = handoff.source_position.0,
        recovery_y = handoff.source_position.1,
        recovery_source = "handoff_source",
        "restoring local cursor after rejected handoff"
    );
    let _ = ctx
        .cursor
        .transition_to_local_control(&*ctx.injection, recovery);
    ctx.cursor.record_transition(ctx.network.mono_us());
    smoother.deactivate();
}

fn handle_disconnect_notification(
    ctx: &ThreadContext,
    conn: &Arc<PeerConnection>,
    peer_id: String,
    peer_name: String,
    smoother: &mut MovementSmoother,
) {
    let addr = conn.addr();
    ctx.network.register_conn_addr(addr, &peer_id);
    super::loops::recover_from_peer_loss(ctx, Some(&peer_id), addr, "disconnect_notification");
    smoother.deactivate();
    ctx.network.mark_disconnected(&peer_id, &peer_name, addr);
    if let Some(sync) = ctx.clipboard_sync.as_ref() {
        sync.notify_peer_disconnected();
    }
    if let Some(sync) = ctx.transfer_sync.as_ref() {
        sync.notify_peer_disconnected(Some(&peer_id));
    }
    conn.close(b"peer disconnected");
    info!(peer = %peer_name, "peer disconnected");
    ctx.network.emit_session_disconnected(&peer_id);
}

fn handle_unpair_notification(
    ctx: &ThreadContext,
    conn: &Arc<PeerConnection>,
    peer_id: String,
    peer_name: String,
    smoother: &mut MovementSmoother,
) {
    let addr = conn.addr();
    ctx.network.register_conn_addr(addr, &peer_id);
    super::loops::recover_from_peer_loss(ctx, Some(&peer_id), addr, "unpair_notification");
    smoother.deactivate();
    ctx.network.mark_disconnected(&peer_id, &peer_name, addr);
    if let Some(sync) = ctx.clipboard_sync.as_ref() {
        sync.notify_peer_disconnected();
    }
    if let Some(sync) = ctx.transfer_sync.as_ref() {
        sync.notify_peer_disconnected(Some(&peer_id));
    }
    conn.close(b"peer unpaired");
    info!(peer = %peer_name, "peer unpaired");
    ctx.network.emit_session_unpaired(&peer_id);
}

#[cfg(test)]
mod tests {
    use anyhow::Result;

    use crate::core::api::ConfigSnapshot;
    use crate::events::LayoutPlacement;
    use crate::platform::PeerEdge;
    use crate::platform::shared::test_support::{TestPeerPair, with_tokio_runtime};

    #[test]
    fn partial_overlap_layout_handoff_warps_within_contact_span() -> Result<()> {
        with_tokio_runtime(|| {
            let pair = TestPeerPair::new(PeerEdge::Right, PeerEdge::Left)?;
            let layout_a = ConfigSnapshot {
                placements: vec![LayoutPlacement {
                    peer_id: pair.b.peer_id.clone(),
                    peer_label: pair.b.peer_name.clone(),
                    x: 166,
                    y: 40,
                }],
                ..Default::default()
            };
            let layout_b = ConfigSnapshot {
                placements: vec![LayoutPlacement {
                    peer_id: pair.a.peer_id.clone(),
                    peer_label: pair.a.peer_name.clone(),
                    x: -166,
                    y: -40,
                }],
                ..Default::default()
            };

            pair.a.reconcile_layout(&layout_a)?;
            pair.b.reconcile_layout(&layout_b)?;

            let a_contact = pair
                .a
                .runtime_contacts()
                .into_iter()
                .find(|contact| contact.peer_id == pair.b.peer_id)
                .expect("peer A should resolve a runtime contact for peer B");
            let b_contact = pair
                .b
                .runtime_contacts()
                .into_iter()
                .find(|contact| contact.peer_id == pair.a.peer_id)
                .expect("peer B should resolve a runtime contact for peer A");

            assert!(a_contact.local.start_bp > 0 || a_contact.local.end_bp < 10_000);
            assert!(b_contact.local.start_bp > 0 || b_contact.local.end_bp < 10_000);

            Ok(())
        })
    }

    #[test]
    fn disconnect_notification_cleans_up_and_notifies() -> Result<()> {
        with_tokio_runtime(|| {
            let pair = TestPeerPair::new(PeerEdge::Right, PeerEdge::Left)?;
            pair.a.remove_peer(&pair.b.peer_id)?;

            crate::platform::shared::test_support::wait_until(
                "disconnect notification",
                std::time::Duration::from_secs(2),
                std::time::Duration::from_millis(20),
                || {
                    pair.b.runtime_state().iter().any(|state| {
                        state.peer_id == pair.a.peer_id
                            && state.connection_status
                                == crate::core::peer_state::ConnectionStatus::Disconnected
                    })
                },
                || pair.b.debug_snapshot(),
            )?;

            Ok(())
        })
    }
}
