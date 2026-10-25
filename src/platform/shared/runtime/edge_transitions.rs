//! Mouse edge transition handlers for local movement events.

use crate::core::layout::map_position_bp;
use crate::events::MotionCheckpoint;
use crate::platform::PeerEdge;
use tracing::{debug, info, trace, warn};

use super::super::layout_state::OutboundTarget;
use super::super::topology::{
    DisplayTopology, clamp_to_display_union, edge_position_bp_from_bounds, is_within_edge_buffer,
};
use super::ThreadContext;
use super::cursor_runtime::HandoffRequest;
use crate::platform::shared::transition::HandoffContext;

pub(crate) const TRANSITION_COOLDOWN_US: u64 = 100_000;

pub(crate) fn handle_local_mouse_move(ctx: &ThreadContext, x: i32, y: i32, dx: i32, dy: i32) {
    let sample_us = ctx.network.mono_us();
    if let Err(err) = ctx
        .transition
        .refresh_topology_if_due(&*ctx.display, sample_us)
    {
        debug!(error = %err, "failed to refresh display topology");
    }
    handle_owner_cursor_position(ctx, sample_us, x, y, dx, dy, None);
}

pub(crate) fn handle_owner_cursor_position(
    ctx: &ThreadContext,
    sample_us: u64,
    x: i32,
    y: i32,
    dx: i32,
    dy: i32,
    return_motion: Option<MotionCheckpoint>,
) {
    let topology = match ctx.transition.topology_snapshot() {
        Ok(topology) => topology,
        Err(err) => {
            debug!(error = %err, "failed to read display topology");
            return;
        }
    };
    let mode = ctx.cursor.mode();

    trace!(?mode, x, y, dx, dy, "mouse move");

    if mode.owns_cursor() {
        handle_owner_mouse_move(ctx, &topology, sample_us, x, y, dx, dy, return_motion);
    }
}

fn handle_owner_mouse_move(
    ctx: &ThreadContext,
    topology: &DisplayTopology,
    sample_us: u64,
    local_x: i32,
    local_y: i32,
    dx: i32,
    dy: i32,
    return_motion: Option<MotionCheckpoint>,
) {
    let last_transition = ctx.cursor.last_transition_us();
    if last_transition > 0 && sample_us.saturating_sub(last_transition) < TRANSITION_COOLDOWN_US {
        return;
    }
    let (local_x, local_y) = clamp_to_display_union(local_x, local_y, topology);
    let source_position = (local_x, local_y);
    if ctx.cursor.has_pressed_buttons() {
        return;
    }
    let Some(target) = confirm_outbound_target(ctx, topology, source_position, (dx, dy), sample_us)
    else {
        return;
    };

    let addr = target.contact.addr;
    if !ctx.cursor.can_begin_outbound() {
        debug!(peer = %addr, "ignored outbound handoff because cursor runtime is busy");
        return;
    }

    let Some(handoff) = begin_handoff(
        ctx,
        topology,
        target,
        source_position,
        sample_us,
        return_motion,
    ) else {
        return;
    };
    enter_remote_control(ctx, &handoff);
}

fn confirm_outbound_target(
    ctx: &ThreadContext,
    topology: &DisplayTopology,
    position: (i32, i32),
    movement: (i32, i32),
    sample_us: u64,
) -> Option<OutboundTarget> {
    let (detected, outward_push_px) = detect_outbound_target(ctx, topology, position, movement);
    let mut detector = ctx
        .edge_detector
        .lock()
        .expect("edge_detector lock poisoned");
    let target = detector.check_outbound_target(detected, sample_us, outward_push_px);
    if target.is_some() {
        detector.reset();
    }
    target
}

fn begin_handoff(
    ctx: &ThreadContext,
    topology: &DisplayTopology,
    target: OutboundTarget,
    source_position: (i32, i32),
    sample_us: u64,
    return_motion: Option<MotionCheckpoint>,
) -> Option<HandoffContext> {
    let contact = target.contact;
    let edge = contact.local.edge;
    let edge_bp = edge_position_bp_from_bounds(
        edge,
        topology.virtual_bounds,
        source_position.0,
        source_position.1,
    );
    let receiver_position_bp = map_position_bp(edge_bp, contact.local, contact.remote);
    let motion_session_id = ctx.network.next_motion_session_id();
    ctx.network
        .start_outbound_input(contact.addr, motion_session_id);

    let request = HandoffRequest::new(edge, receiver_position_bp, motion_session_id, return_motion)
        .expect("motion session id is non-zero")
        .with_caps_lock(ctx.injection.caps_lock_active());
    let request_seq = match ctx
        .network
        .send_packet_to_with_seq(contact.addr, vec![request.into_wire_event()])
    {
        Ok(request_seq) => request_seq,
        Err(err) => {
            warn!(error = %err, "failed to send TakeControl");
            clear_outbound(ctx);
            return None;
        }
    };
    let handoff = HandoffContext {
        peer_id: contact.peer_id.clone(),
        addr: contact.addr,
        request_seq,
        motion_session_id,
        edge,
        receiver_position_bp,
        source_position,
        started_us: sample_us,
    };
    if !ctx.cursor.begin_outbound(handoff.clone()) {
        warn!(peer = %contact.addr, request_seq, "ignored outbound handoff because cursor runtime is busy");
        clear_outbound(ctx);
        return None;
    }

    debug!(
        ownership = ?ctx.cursor.mode(),
        phase = ctx.cursor.phase_name(),
        peer = %contact.addr,
        request_seq,
        "cursor runtime started outbound handoff"
    );
    info!(
        domain = "runtime.transition",
        transition_key = "handoff_started",
        handoff_id = motion_session_id,
        phase = "AwaitingAck",
        peer_id = %handoff.peer_id,
        peer = %contact.addr,
        request_seq,
        edge = ?edge,
        source_x = source_position.0,
        source_y = source_position.1,
        receiver_position_bp,
        contact_local_start_bp = contact.local.start_bp,
        contact_local_end_bp = contact.local.end_bp,
        contact_remote_start_bp = contact.remote.start_bp,
        contact_remote_end_bp = contact.remote.end_bp,
        topology_signature = topology.signature,
        topology_x = topology.virtual_bounds.x,
        topology_y = topology.virtual_bounds.y,
        topology_width = topology.virtual_bounds.width,
        topology_height = topology.virtual_bounds.height,
        "started cursor handoff"
    );
    Some(handoff)
}

fn enter_remote_control(ctx: &ThreadContext, handoff: &HandoffContext) {
    let transition_result = ctx.cursor.transition_to_remote_control(&*ctx.injection);
    if transition_result.is_ok() {
        ctx.cursor.record_transition(handoff.started_us);
        info!(
            domain = "runtime.transition",
            transition_key = "mode_change",
            handoff_id = handoff.motion_session_id,
            from_mode = "LocalControl",
            to_mode = "RemoteControl",
            phase = "AwaitingAck",
            edge = ?handoff.edge,
            "transfer mode transitioned"
        );
        return;
    }

    warn!(
        domain = "runtime.transition",
        transition_key = "handoff_failed",
        handoff_id = handoff.motion_session_id,
        phase = "Idle",
        peer_id = %handoff.peer_id,
        peer = %handoff.addr,
        request_seq = handoff.request_seq,
        edge = ?handoff.edge,
        "failed to enter remote control for handoff"
    );
    ctx.cursor.clear_handoff();
    clear_outbound(ctx);
}

fn clear_outbound(ctx: &ThreadContext) {
    ctx.network.stop_outbound_input();
}

fn edge_normal_and_parallel_velocity(edge: PeerEdge, dx: i32, dy: i32) -> (i32, i32) {
    match edge {
        PeerEdge::Right => (dx, dy.abs()),
        PeerEdge::Left => (-dx, dy.abs()),
        PeerEdge::Bottom => (dy, dx.abs()),
        PeerEdge::Top => (-dy, dx.abs()),
    }
}

fn edge_directional_weight(edge: PeerEdge, dx: i32, dy: i32) -> i32 {
    edge_normal_and_parallel_velocity(edge, dx, dy).0
}

fn outward_push_distance(edge: PeerEdge, dx: i32, dy: i32) -> u32 {
    let (normal, parallel) = edge_normal_and_parallel_velocity(edge, dx, dy);
    if normal <= 0 || normal < parallel {
        return 0;
    }
    normal as u32
}

fn detect_outbound_target(
    ctx: &ThreadContext,
    topology: &DisplayTopology,
    (local_x, local_y): (i32, i32),
    (dx, dy): (i32, i32),
) -> (Option<OutboundTarget>, u32) {
    let (x, y) = (local_x as f64, local_y as f64);
    let mut edges = [
        PeerEdge::Right,
        PeerEdge::Left,
        PeerEdge::Top,
        PeerEdge::Bottom,
    ];
    if dx != 0 || dy != 0 {
        edges.sort_by_key(|&edge| std::cmp::Reverse(edge_directional_weight(edge, dx, dy)));
    }

    for edge in edges {
        if !is_within_edge_buffer(x, y, edge, topology) {
            continue;
        }

        let (normal, _) = edge_normal_and_parallel_velocity(edge, dx, dy);
        if normal <= 0 {
            continue;
        }

        let position_bp =
            edge_position_bp_from_bounds(edge, topology.virtual_bounds, local_x, local_y);
        let Some(contact) = ctx.layout.contact_for_position(edge, position_bp) else {
            continue;
        };
        if ctx.network.is_peer_timed_out(contact.addr) {
            continue;
        }

        let endpoint = ctx.network.endpoint();
        let has_active_connection = endpoint
            .get_connection_by_addr(contact.addr)
            .filter(|conn| !conn.is_closed())
            .or_else(|| {
                endpoint
                    .get_connection(&contact.peer_id)
                    .filter(|conn| !conn.is_closed())
            })
            .is_some();

        if !has_active_connection {
            continue;
        }

        let outward_push_px = outward_push_distance(edge, dx, dy);
        return (Some(OutboundTarget { contact }), outward_push_px);
    }

    (None, 0)
}

#[cfg(test)]
mod tests {
    use super::{edge_directional_weight, outward_push_distance};
    use crate::platform::shared::edge_detector::OUTBOUND_PUSH_DISTANCE_PX;
    use crate::platform::PeerEdge;

    #[test]
    fn edge_directional_weight_prioritizes_dominant_axis() {
        // Moving strongly rightward with slight upward drift
        let dx = 10;
        let dy = -2;
        let mut edges = [
            PeerEdge::Right,
            PeerEdge::Left,
            PeerEdge::Top,
            PeerEdge::Bottom,
        ];
        edges.sort_by_key(|&edge| std::cmp::Reverse(edge_directional_weight(edge, dx, dy)));
        assert_eq!(edges[0], PeerEdge::Right);
        assert_eq!(edges[1], PeerEdge::Top);

        // Moving strongly upward into top-right corner
        let dx = 2;
        let dy = -10;
        edges.sort_by_key(|&edge| std::cmp::Reverse(edge_directional_weight(edge, dx, dy)));
        assert_eq!(edges[0], PeerEdge::Top);
        assert_eq!(edges[1], PeerEdge::Right);
    }

    #[test]
    fn outward_push_distance_follows_the_edge_normal() {
        assert_eq!(outward_push_distance(PeerEdge::Right, 6, 1), 6);
        assert_eq!(outward_push_distance(PeerEdge::Right, 4, 4), 4);
        assert_eq!(outward_push_distance(PeerEdge::Right, 3, 0), 3);
        assert_eq!(outward_push_distance(PeerEdge::Left, -6, 2), 6);
        assert_eq!(outward_push_distance(PeerEdge::Bottom, 1, 8), 8);
        assert_eq!(outward_push_distance(PeerEdge::Top, -2, -5), 5);
    }

    #[test]
    fn outward_push_distance_ignores_parallel_scrollbar_motion() {
        // Scrolling vertically along right edge
        assert_eq!(outward_push_distance(PeerEdge::Right, 2, 10), 0);
        assert_eq!(outward_push_distance(PeerEdge::Right, 0, 15), 0);
        // Moving away from right edge
        assert_eq!(outward_push_distance(PeerEdge::Right, -5, 0), 0);
        // Scrolling horizontally along top edge
        assert_eq!(outward_push_distance(PeerEdge::Top, 12, -1), 0);
    }

    #[test]
    fn transition_cooldown_is_at_least_100ms() {
        assert!(super::TRANSITION_COOLDOWN_US >= 100_000);
    }

    #[test]
    fn outbound_push_distance_exceeds_threshold_for_self_healing_boundary_crossing() {
        assert!(outward_push_distance(PeerEdge::Right, 5, 0) >= OUTBOUND_PUSH_DISTANCE_PX);
        assert!(outward_push_distance(PeerEdge::Right, 2, 0) < OUTBOUND_PUSH_DISTANCE_PX);
    }
}
