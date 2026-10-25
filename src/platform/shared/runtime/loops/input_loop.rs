//! Combined input loop handling local capture and network receive.
//!
//! This module consolidates the former local_event_loop and receive_loop into
//! a single async loop using tokio::select! for simpler coordination.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crossbeam_channel::TryRecvError;
use tokio::sync::mpsc::Receiver;
use tracing::{debug, info, warn};

use super::super::edge_transitions::handle_local_mouse_move;
use super::super::network::NetworkState;
use super::super::network::QueuedControlMessage;
use super::{
    emit_rate_limit_rollup, input_activity_state, recover_from_peer_loss, run_timeout_check,
    runtime_warning_limiter, Event, MovementSmoother, OutboundMotion, PendingMessage, QuicEndpoint,
    ThreadContext, HEARTBEAT_INTERVAL, PEER_TIMEOUT, PEER_TIMEOUT_CHECK_INTERVAL,
    WARN_RATE_LIMIT_POLICY,
};
use crate::core::api::PeerRefSnapshot;
use crate::core::handshake::{HandshakePeer, HandshakeResponder};
use crate::core::platform::{scale_wheel_for_target, translate_hid_for_target_platform, PlatformFamily};
use crate::platform::shared::quic::PeerConnection;
use crate::platform::shared::transition::TransitionState;
use crate::platform::{CapturedInputEvent, TransferMode};

const HOUSEKEEPING_INTERVAL: Duration = HEARTBEAT_INTERVAL;
const POLL_INTERVAL: Duration = Duration::from_millis(1);
const TIMEOUT_CHECK_INTERVAL: Duration = PEER_TIMEOUT_CHECK_INTERVAL;
const KEEP_ALIVE_INTERVAL: Duration = Duration::from_secs(5);
const MAX_LOCAL_EVENTS_PER_TURN: usize = 32;
const MAX_INBOUND_MESSAGES_PER_TURN: usize = 32;
const MAX_CONTROL_MESSAGES_PER_TICK: usize = 32;
const ESCAPE_TAP_WINDOW_US: u64 = 1_500_000;
const ESCAPE_REPEAT_DEBOUNCE_US: u64 = 30_000;
const ESCAPE_KEYCODE: u32 = 0x29;

static INJECTION_FAILURES: AtomicU64 = AtomicU64::new(0);

pub(crate) fn input_loop(
    event_rx: Receiver<CapturedInputEvent>,
    endpoint: Arc<QuicEndpoint>,
    running: Arc<AtomicBool>,
    ctx: ThreadContext,
    cadence_us: u64,
) {
    let state = InputLoopState::new();
    let inbound_rx = endpoint
        .take_inbound_receiver()
        .expect("inbound receiver already taken");

    #[cfg(windows)]
    let _timer_guard = crate::platform::windows::timer::HighResolutionTimerGuard::new(1);

    spawn_accept_loop(&endpoint, &running, &ctx);

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .thread_name("input-loop")
        .build()
        .expect("failed to create tokio runtime");

    rt.block_on(run_input_loop(
        event_rx, inbound_rx, endpoint, running, ctx, cadence_us, state,
    ));
}

fn spawn_accept_loop(endpoint: &Arc<QuicEndpoint>, running: &Arc<AtomicBool>, ctx: &ThreadContext) {
    let endpoint = Arc::clone(endpoint);
    let running = Arc::clone(running);
    let network = Arc::clone(&ctx.network);
    let transition = Arc::clone(&ctx.transition);
    let local_identity = ctx.local_identity.clone();

    crate::platform::shared::tokio_runtime::global_runtime_handle().spawn(run_accept_loop(
        endpoint,
        running,
        network,
        transition,
        local_identity,
    ));
}

async fn run_accept_loop(
    endpoint: Arc<QuicEndpoint>,
    running: Arc<AtomicBool>,
    network: Arc<NetworkState>,
    transition: Arc<TransitionState>,
    local_identity: Option<PeerRefSnapshot>,
) {
    while running.load(Ordering::Relaxed) && !endpoint.is_shutting_down() {
        match endpoint.accept_incoming().await {
            Ok(incoming) => {
                // Each attempt completes in its own task so a slow or
                // abandoned handshake never delays any other inbound attempt.
                let endpoint = Arc::clone(&endpoint);
                let network = Arc::clone(&network);
                let transition = Arc::clone(&transition);
                let local_identity = local_identity.clone();
                tokio::spawn(async move {
                    let addr = incoming.remote_address();
                    match endpoint.complete_incoming(incoming).await {
                        Ok(conn) => {
                            info!(addr = %conn.addr(), "QUIC connection accepted");
                            handle_inbound_connection(
                                endpoint,
                                network,
                                transition,
                                local_identity,
                                conn,
                            )
                            .await;
                        }
                        Err(error) => {
                            debug!(addr = %addr, error = %error, "inbound handshake attempt failed");
                        }
                    }
                });
            }
            Err(error) => {
                if !endpoint.is_shutting_down() {
                    let decision = runtime_warning_limiter()
                        .evaluate("quic_accept_failed", WARN_RATE_LIMIT_POLICY);
                    emit_rate_limit_rollup("runtime.connection", "quic_accept_failed", decision);
                    if decision.emit_now {
                        warn!(
                            domain = "runtime.connection",
                            warning_key = "quic_accept_failed",
                            error = %error,
                            "failed to accept QUIC connection"
                        );
                    }
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }
            }
        }
    }
}

async fn handle_inbound_connection(
    endpoint: Arc<QuicEndpoint>,
    network: Arc<NetworkState>,
    transition: Arc<TransitionState>,
    local_identity: Option<PeerRefSnapshot>,
    conn: Arc<PeerConnection>,
) {
    let local_peer = local_handshake_peer(&endpoint, &transition, local_identity.as_ref());
    let responder = HandshakeResponder::new(local_peer);
    let handshake_result = tokio::time::timeout(
        crate::core::policies::PEER_CANDIDATE_CONNECT_TIMEOUT,
        responder.accept(&endpoint, &conn),
    )
    .await
    .unwrap_or_else(|_| {
        Err(crate::core::error::CoreError::Connection(format!(
            "handshake timed out after {:?}",
            crate::core::policies::PEER_CANDIDATE_CONNECT_TIMEOUT
        )))
    });

    match handshake_result {
        Ok(request) => {
            let Some(certificate) = request.certificate.clone() else {
                warn!(
                    domain = "runtime.connection",
                    warning_key = "inbound_missing_certificate",
                    addr = %conn.addr(),
                    "inbound handshake completed without a TLS certificate"
                );
                endpoint.abort_connection_attempt(&conn, b"missing peer certificate");
                return;
            };
            let addr = conn.addr();
            network.register_conn_addr(addr, &request.peer_id);
            network.mark_connected(&request.peer_id);
            network.emit_inbound_identified(
                request.peer_id.clone(),
                request.name.clone(),
                addr,
                Arc::clone(&conn),
                request.display_bounds,
                request.display_rects,
                request.platform.clone(),
                request.is_auto_connect,
                request.listen_port,
                certificate,
            );
        }
        Err(error) => {
            let decision = runtime_warning_limiter()
                .evaluate("inbound_handshake_failed", WARN_RATE_LIMIT_POLICY);
            emit_rate_limit_rollup("runtime.health", "inbound_handshake_failed", decision);
            if decision.emit_now {
                warn!(
                    domain = "runtime.health",
                    warning_key = "inbound_handshake_failed",
                    error = %error,
                    addr = %conn.addr(),
                    "failed to complete inbound handshake"
                );
            }
            endpoint.abort_connection_attempt(&conn, b"inbound handshake failed");
        }
    }
}

fn local_handshake_peer(
    endpoint: &QuicEndpoint,
    transition: &TransitionState,
    local_identity: Option<&PeerRefSnapshot>,
) -> Option<HandshakePeer> {
    let listen_port = endpoint.local_addr().ok().map(|addr| addr.port());
    let mut peer = local_identity
        .map(|local_peer| {
            let local_topology = transition.topology_snapshot().ok();
            let local_bounds = local_topology
                .as_ref()
                .map(|topology| topology.virtual_bounds);
            let local_display_rects = local_topology
                .map(|topology| topology.displays)
                .unwrap_or_default();
            HandshakePeer::local(
                local_peer.peer_id.clone(),
                local_peer.name.clone(),
                local_bounds,
                local_display_rects,
            )
        })
        .or_else(|| {
            let peer_id = endpoint.self_peer_id().to_string();
            if peer_id.is_empty() {
                None
            } else {
                Some(HandshakePeer::local(
                    peer_id.clone(),
                    peer_id,
                    None,
                    Vec::new(),
                ))
            }
        })?;
    peer.listen_port = listen_port;
    peer.cert_der = Some(endpoint.self_cert_der().to_vec());
    Some(peer)
}

struct InputLoopState {
    last_cadence_flush: Instant,
    smoother: MovementSmoother,
    fragment_reassembly: HashMap<u16, PendingMessage>,
    last_ping: Instant,
    last_housekeeping: Instant,
    last_timeout_check: Instant,
    last_keep_alive: Instant,
    emergency_tracker: EmergencyUnlockTracker,
    outbound_motion: OutboundMotion,
}

impl InputLoopState {
    fn new() -> Self {
        Self {
            last_cadence_flush: Instant::now(),
            smoother: MovementSmoother::new(),
            fragment_reassembly: HashMap::new(),
            last_ping: Instant::now() - super::PING_INTERVAL,
            last_housekeeping: Instant::now(),
            last_timeout_check: Instant::now(),
            last_keep_alive: Instant::now(),
            emergency_tracker: EmergencyUnlockTracker::new(),
            outbound_motion: OutboundMotion::default(),
        }
    }

    fn on_local_event(&mut self, event: &CapturedInputEvent) {
        if matches!(event, CapturedInputEvent::MouseMove { .. }) {
            self.last_keep_alive = Instant::now();
        }
    }

    fn on_inbound_packet(&mut self, packet: &crate::events::Packet) {
        if packet.events.iter().any(|e| matches!(e, Event::MouseMove { .. })) {
            self.last_keep_alive = Instant::now();
        }
    }

    async fn handle_local_events(
        &mut self,
        ctx: &ThreadContext,
        event_rx: &mut Receiver<CapturedInputEvent>,
        event: CapturedInputEvent,
        cadence_us: u64,
    ) {
        self.on_local_event(&event);
        process_local_event(
            ctx,
            event,
            &mut self.emergency_tracker,
            &mut self.outbound_motion,
            &mut self.smoother,
        );

        let mut processed = 1;
        while processed < MAX_LOCAL_EVENTS_PER_TURN {
            let Ok(event) = event_rx.try_recv() else {
                break;
            };
            processed += 1;
            self.on_local_event(&event);
            process_local_event(
                ctx,
                event,
                &mut self.emergency_tracker,
                &mut self.outbound_motion,
                &mut self.smoother,
            );
        }

        if cadence_us > 0 {
            let elapsed = self.last_cadence_flush.elapsed();
            if elapsed >= Duration::from_micros(cadence_us) {
                if elapsed >= Duration::from_micros(cadence_us * 2) {
                    ctx.network.record_cadence_miss();
                }
                flush_due_input(ctx);
                self.last_cadence_flush = Instant::now();
            }
        } else {
            flush_due_input(ctx);
        }
        process_control_messages(ctx).await;
    }

    async fn handle_inbound_message(
        &mut self,
        endpoint: &Arc<QuicEndpoint>,
        ctx: &ThreadContext,
        message: crate::platform::shared::quic::InboundMessage,
    ) {
        match message {
            crate::platform::shared::quic::InboundMessage::Datagram(conn, data) => {
                if conn.peer_id().is_empty()
                    || conn.is_closed()
                    || !endpoint.is_canonical_connection(&conn)
                {
                    return;
                }
                if let Some(complete) = super::reassemble_fragment(&mut self.fragment_reassembly, data) {
                    if let Ok(packet) = bincode::deserialize::<crate::events::Packet>(&complete) {
                        if packet.version == crate::events::PROTOCOL_VERSION
                            && !ctx.network.is_local_sender(packet.sender_id)
                        {
                            self.on_inbound_packet(&packet);
                            super::handle_received_packet(
                                ctx,
                                &conn,
                                packet,
                                &mut self.smoother,
                                &mut self.outbound_motion,
                            );
                        }
                    }
                }
            }
            crate::platform::shared::quic::InboundMessage::Control(conn, data) => {
                if conn.peer_id().is_empty()
                    || conn.is_closed()
                    || !endpoint.is_canonical_connection(&conn)
                {
                    return;
                }
                let Ok(packet) = bincode::deserialize::<crate::events::Packet>(&data) else {
                    return;
                };
                if packet.version == crate::events::PROTOCOL_VERSION
                    && !ctx.network.is_local_sender(packet.sender_id)
                {
                    self.on_inbound_packet(&packet);
                    super::handle_received_packet(
                        ctx,
                        &conn,
                        packet,
                        &mut self.smoother,
                        &mut self.outbound_motion,
                    );
                }
            }
        }
        let injected = tick_smoother(ctx, &*ctx.injection, &mut self.smoother);
        if self.smoother.active {
            ctx.network.record_smoother_tick(injected);
        }
        process_control_messages(ctx).await;
    }

    async fn handle_tick(&mut self, endpoint: &Arc<QuicEndpoint>, ctx: &ThreadContext) {
        process_control_messages(ctx).await;

        let now_us = ctx.network.mono_us();
        if let Err(error) = ctx
            .transition
            .refresh_topology_if_due(&*ctx.display, now_us)
        {
            debug!(error = %error, "failed to refresh display topology");
        }
        check_peer_timeouts(ctx, &mut self.smoother, now_us);

        let injection = &*ctx.injection;
        let injected = tick_smoother(ctx, injection, &mut self.smoother);
        if self.smoother.active {
            ctx.network.record_smoother_tick(injected);
        }

        if self.last_housekeeping.elapsed() >= HOUSEKEEPING_INTERVAL {
            run_housekeeping(ctx, &mut self.last_ping);
            self.last_housekeeping = Instant::now();
        }

        if self.last_timeout_check.elapsed() >= TIMEOUT_CHECK_INTERVAL {
            run_timeout_check(ctx, endpoint, &mut self.smoother);
            endpoint.prune_closed_connections();
            self.last_timeout_check = Instant::now();
        }

        if self.last_keep_alive.elapsed() >= KEEP_ALIVE_INTERVAL {
            if ctx.network.keep_mouse_alive() {
                if ctx.cursor.mode() == TransferMode::LocalControl {
                    let _ = ctx.injection.inject_mouse_move_relative(1, 0);
                    let _ = ctx.injection.inject_mouse_move_relative(-1, 0);
                }
                let _ = ctx.injection.declare_user_activity();
            }
            self.last_keep_alive = Instant::now();
        }
    }
}

async fn run_input_loop(
    mut event_rx: Receiver<CapturedInputEvent>,
    mut inbound_rx: Receiver<crate::platform::shared::quic::InboundMessage>,
    endpoint: Arc<QuicEndpoint>,
    running: Arc<AtomicBool>,
    ctx: ThreadContext,
    cadence_us: u64,
    mut state: InputLoopState,
) {
    let mut event_rx_closed = false;
    let mut tick = tokio::time::interval(POLL_INTERVAL);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    loop {
        if !running.load(Ordering::Relaxed) {
            break;
        }

        tokio::select! {
            biased;
            Some(message) = inbound_rx.recv() => {
                state.handle_inbound_message(&endpoint, &ctx, message).await;
                for _ in 1..MAX_INBOUND_MESSAGES_PER_TURN {
                    let Ok(pending) = inbound_rx.try_recv() else {
                        break;
                    };
                    state.handle_inbound_message(&endpoint, &ctx, pending).await;
                }
            }
            result = event_rx.recv(), if !event_rx_closed => {
                match result {
                    Some(event) => state.handle_local_events(&ctx, &mut event_rx, event, cadence_us).await,
                    None => event_rx_closed = true,
                }
            }
            _ = tick.tick() => {
                state.handle_tick(&endpoint, &ctx).await;
            }
        }
    }
    perform_emergency_unlock(&ctx);
}

fn tick_smoother(
    ctx: &ThreadContext,
    injection: &dyn crate::platform::EventInjection,
    smoother: &mut MovementSmoother,
) -> bool {
    if !ctx.cursor.mode().owns_cursor() {
        smoother.deactivate();
        return false;
    }
    let Some((x, y)) = smoother.tick() else {
        return false;
    };
    match injection.inject_mouse_move_subpixel(x, y) {
        Ok(()) => {
            smoother.commit_injection((x, y));
            true
        }
        Err(error) => {
            let failures = INJECTION_FAILURES.fetch_add(1, Ordering::Relaxed) + 1;
            if failures == 1 || failures.is_power_of_two() {
                warn!(
                    error = %error,
                    x = x.round() as i32,
                    y = y.round() as i32,
                    failures,
                    "failed to inject remote mouse move"
                );
            }
            false
        }
    }
}

fn process_local_event(
    ctx: &ThreadContext,
    event: CapturedInputEvent,
    emergency_tracker: &mut EmergencyUnlockTracker,
    outbound_motion: &mut OutboundMotion,
    smoother: &mut MovementSmoother,
) {
    let now_us = ctx.network.mono_us();
    let was_remote_control = ctx.cursor.mode().is_remote_control();

    let tap_keycode = ctx.network.release_tap_keycode();
    if emergency_tracker.process_event_in_mode(&event, now_us, tap_keycode, was_remote_control) {
        emergency_tracker.reset();
        outbound_motion.reset();
        smoother.deactivate();
        perform_emergency_unlock(ctx);
        return;
    }

    if let CapturedInputEvent::MouseButton { button, pressed } = event {
        ctx.cursor.set_button_state(button, pressed);
        if pressed {
            ctx.edge_detector
                .lock()
                .expect("edge_detector lock poisoned")
                .reset();
        }
    }

    if let CapturedInputEvent::MouseMove {
        x,
        y,
        dx,
        dy,
        locally_delivered,
    } = event.clone()
    {
        if locally_delivered && was_remote_control {
            return;
        }
        if !locally_delivered && !was_remote_control {
            let dx_i32 = dx.round() as i32;
            let dy_i32 = dy.round() as i32;
            if dx_i32 != 0 || dy_i32 != 0 {
                let _ = ctx.injection.inject_mouse_move_relative(dx_i32, dy_i32);
            }
            return;
        }
        handle_local_mouse_move(ctx, x, y, dx.round() as i32, dy.round() as i32);
    }

    let mode = ctx.cursor.mode();
    if !was_remote_control && mode.is_remote_control() {
        emergency_tracker.reset();
        outbound_motion.reset();
        return;
    }

    if !mode.is_remote_control() {
        if was_remote_control {
            emergency_tracker.reset();
            outbound_motion.reset();
            ctx.network.stop_outbound_input();
            let _ = ctx.injection.release_all_keys();
        }
        return;
    }

    let Some(session) = ctx.network.outbound_input_session() else {
        let decision = runtime_warning_limiter()
            .evaluate("remote_input_session_missing", WARN_RATE_LIMIT_POLICY);
        emit_rate_limit_rollup("runtime.input", "remote_input_session_missing", decision);
        if decision.emit_now {
            warn!(
                domain = "runtime.input",
                warning_key = "remote_input_session_missing",
                event = ?event,
                phase = ctx.cursor.phase_name(),
                "remote input rejected because no outbound input session exists"
            );
        }
        return;
    };
    let session_id = session.motion_session_id;

    if outbound_motion.session_id() != Some(session_id) {
        outbound_motion.reset();
        smoother.deactivate();
    }

    let target_platform = ctx.network.target_platform(session.target);
    let outbound = match event {
        CapturedInputEvent::MouseMove { dx, dy, .. } => {
            outbound_motion.record(session_id, dx, dy).into_event()
        }
        CapturedInputEvent::MouseButton { button, pressed } => {
            Event::MouseButton { button, pressed }
        }
        CapturedInputEvent::MouseWheel { delta_x, delta_y } => {
            let delta_y = scale_wheel_for_target(delta_y, PlatformFamily::LOCAL, target_platform);
            let delta_x = scale_wheel_for_target(delta_x, PlatformFamily::LOCAL, target_platform);
            Event::MouseWheel { delta_y, delta_x }
        }
        CapturedInputEvent::KeyPress { keycode } => {
            let keycode = translate_hid_for_target_platform(
                keycode,
                PlatformFamily::LOCAL,
                target_platform,
                ctx.network.swap_alt_cmd(),
                Some(ctx.network.modifier_mapping()),
            );
            Event::KeyPress { keycode }
        }
        CapturedInputEvent::KeyRelease { keycode } => {
            let keycode = translate_hid_for_target_platform(
                keycode,
                PlatformFamily::LOCAL,
                target_platform,
                ctx.network.swap_alt_cmd(),
                Some(ctx.network.modifier_mapping()),
            );
            Event::KeyRelease { keycode }
        }
    };
    ctx.network.mark_input();

    if let Err(err) = ctx.network.queue_input_event(outbound, now_us) {
        let decision = runtime_warning_limiter()
            .evaluate("queue_local_input_failed", WARN_RATE_LIMIT_POLICY);
        emit_rate_limit_rollup("runtime.health", "queue_local_input_failed", decision);
        if decision.emit_now {
            warn!(
                domain = "runtime.health",
                warning_key = "queue_local_input_failed",
                error = %err,
                target = %session.target,
                motion_session_id = session.motion_session_id,
                "failed to queue local input event"
            );
        }
    }
}

fn flush_due_input(ctx: &ThreadContext) {
    if let Err(error) = ctx.network.flush_input_batch_if_due() {
        let decision = runtime_warning_limiter()
            .evaluate("flush_outbound_input_failed", WARN_RATE_LIMIT_POLICY);
        emit_rate_limit_rollup("runtime.health", "flush_outbound_input_failed", decision);
        if decision.emit_now {
            warn!(
                domain = "runtime.health",
                warning_key = "flush_outbound_input_failed",
                error = %error,
                outbound_session = ?ctx.network.outbound_input_session(),
                "failed to flush outbound input"
            );
        }
    }
}

pub(crate) fn perform_emergency_unlock(ctx: &ThreadContext) {
    if ctx.cursor.mode() == TransferMode::LocalControl && ctx.cursor.pending_handoff().is_none() {
        return;
    }
    let (handoff, runtime_recovery) = ctx.cursor.take_pending_handoff_and_recovery();
    tracing::warn!(
        domain = "runtime.emergency",
        transition_key = "handoff_recovery",
        handoff_id = handoff.as_ref().map(|value| value.motion_session_id),
        "emergency unlock triggered: restoring local control"
    );
    ctx.network.stop_outbound_input();
    ctx.cursor.clear_pressed_buttons();
    ctx.edge_detector
        .lock()
        .expect("edge_detector lock poisoned")
        .reset();
    let (recovery, recovery_source) = runtime_recovery
        .map(|position| (Some(position), "runtime_source"))
        .or_else(|| {
            ctx.cursor
                .last_edge_position()
                .map(|pos| (Some(pos), "last_edge"))
        })
        .or_else(|| match ctx.transition.topology_snapshot() {
            Ok(topology) => Some((
                Some(crate::platform::shared::topology::recovery_point(&topology)),
                "topology_center",
            )),
            Err(err) => {
                tracing::debug!(error = %err, "failed to read display topology for recovery warp");
                Some((None, "none"))
            }
        })
        .unwrap_or((None, "none"));
    tracing::info!(
        domain = "runtime.transition",
        transition_key = "handoff_recovery",
        handoff_id = handoff.as_ref().map(|value| value.motion_session_id),
        recovery_x = recovery.map(|value| value.0),
        recovery_y = recovery.map(|value| value.1),
        recovery_source,
        "restoring local cursor after emergency unlock"
    );
    let _ = ctx
        .cursor
        .transition_to_local_control(&*ctx.injection, recovery);
    ctx.cursor.record_transition(ctx.network.mono_us());
}

async fn process_control_messages(ctx: &ThreadContext) {
    let mut processed = 0;
    let mut bytes_sent = 0;
    while processed < MAX_CONTROL_MESSAGES_PER_TICK {
        match ctx.network.control_rx().try_recv() {
            Ok(message) => {
                let msg_len = message.data.len();
                processed += 1;
                bytes_sent += msg_len;
                send_control_message(message).await;
                if bytes_sent >= 65536 {
                    break;
                }
            }
            Err(TryRecvError::Empty) | Err(TryRecvError::Disconnected) => {
                break;
            }
        }
    }
}

async fn send_control_message(message: QueuedControlMessage) {
    let addr = message.connection.addr();
    debug!(
        addr = %addr,
        data_len = message.data.len(),
        "sending queued control message"
    );
    if let Err(error) = message.connection.send_control(message.data).await {
        let decision =
            runtime_warning_limiter().evaluate("control_send_failed", WARN_RATE_LIMIT_POLICY);
        emit_rate_limit_rollup("runtime.connection", "control_send_failed", decision);
        if decision.emit_now {
            warn!(
                domain = "runtime.connection",
                warning_key = "control_send_failed",
                error = %error,
                addr = %addr,
                "failed to send control message"
            );
        }
    }
}

fn check_peer_timeouts(ctx: &ThreadContext, smoother: &mut MovementSmoother, now_us: u64) {
    for addr in ctx.network.peer_addrs() {
        let reference_us = ctx.network.peer_timeout_reference_us(addr);
        if reference_us > 0
            && now_us.saturating_sub(reference_us) > PEER_TIMEOUT.as_micros() as u64
            && ctx.network.mark_peer_timed_out(addr)
        {
            let peer_id = ctx
                .network
                .peer_id_for_addr(addr)
                .or_else(|| ctx.network.peer_id_for_conn_addr(addr));
            warn!(peer = %addr, "peer connection lost");
            recover_from_peer_loss(ctx, peer_id.as_deref(), addr, "heartbeat_timeout");
            smoother.deactivate();
            if let Some(sync) = ctx.clipboard_sync.as_ref() {
                sync.notify_peer_disconnected();
            }
            if let Some(sync) = ctx.transfer_sync.as_ref() {
                sync.notify_peer_disconnected(peer_id.as_deref());
            }
            if let Some(peer_id) = peer_id {
                ctx.network.emit_session_lost(&peer_id);
            }
        }
    }

    if ctx.cursor.mode().is_remote_control() {
        let active_target = ctx.network.active_target();
        let target_live = active_target.is_some_and(|addr| {
            let ref_us = ctx.network.peer_timeout_reference_us(addr);
            ref_us > 0 && now_us.saturating_sub(ref_us) <= PEER_TIMEOUT.as_micros() as u64
        });
        if !target_live {
            warn!("remote control active without live target; restoring local control");
            perform_emergency_unlock(ctx);
            smoother.deactivate();
        }
    }
}

fn run_housekeeping(ctx: &ThreadContext, last_ping: &mut Instant) {
    if let Err(err) = ctx.network.flush_input_batch_if_due() {
        debug!(error = %err, "batch flush failed");
    }

    if last_ping.elapsed() >= super::PING_INTERVAL {
        let nonce = ctx.network.next_ping_nonce();
        let sent_mono_us = ctx.network.mono_us();
        if ctx
            .network
            .broadcast_packet(vec![Event::Ping {
                nonce,
                sent_mono_us,
            }])
            .is_ok()
        {
            *last_ping = Instant::now();
        }
    }

    let now = ctx.network.mono_us();
    let last_send = ctx.network.last_send_us();
    if now.saturating_sub(last_send) >= HEARTBEAT_INTERVAL.as_micros() as u64 {
        let state = input_activity_state(ctx.cursor.mode(), now, ctx.network.last_input_us());
        let _ = ctx
            .network
            .broadcast_packet(vec![Event::Heartbeat { state }]);
    }
}

/// Tracks key press sequences and modifier chords to trigger emergency unlocks.
#[derive(Debug, Default)]
pub(crate) struct EmergencyUnlockTracker {
    ctrl_down: bool,
    alt_down: bool,
    shift_down: bool,
    tap_keycode: u32,
    tap_timestamps_us: Vec<u64>,
}

impl EmergencyUnlockTracker {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    pub(crate) fn reset(&mut self) {
        self.ctrl_down = false;
        self.alt_down = false;
        self.shift_down = false;
        self.tap_timestamps_us.clear();
    }

    /// Evaluates input events for emergency escape triggers.
    ///
    /// Triggers on:
    /// - `Ctrl + Alt + Shift + Escape`
    /// - `Ctrl + Alt + Pause`
    /// - `Ctrl + Alt + Shift + Backspace`
    /// - Rapid triple-tap of the configured release key (3 presses within 1.5 seconds)
    #[cfg(test)]
    pub(crate) fn process_event(&mut self, event: &CapturedInputEvent, now_us: u64) -> bool {
        self.process_event_in_mode(event, now_us, ESCAPE_KEYCODE, true)
    }

    fn process_event_in_mode(
        &mut self,
        event: &CapturedInputEvent,
        now_us: u64,
        tap_keycode: u32,
        enabled: bool,
    ) -> bool {
        if tap_keycode != self.tap_keycode {
            self.tap_keycode = tap_keycode;
            self.tap_timestamps_us.clear();
        }
        let chord = self.process_chord(event);
        let tapped = self.process_tap(event, now_us);
        if !enabled && self.is_tap_key_event(event) {
            self.tap_timestamps_us.clear();
        }
        enabled && (chord || tapped)
    }

    fn is_tap_key_event(&self, event: &CapturedInputEvent) -> bool {
        match event {
            CapturedInputEvent::KeyPress { keycode } | CapturedInputEvent::KeyRelease { keycode } => {
                self.tap_keycode != 0 && *keycode == self.tap_keycode
            }
            _ => false,
        }
    }

    fn process_tap(&mut self, event: &CapturedInputEvent, now_us: u64) -> bool {
        if !matches!(event, CapturedInputEvent::KeyPress { .. }) || !self.is_tap_key_event(event) {
            return false;
        }
        if let Some(&last_us) = self.tap_timestamps_us.last() {
            if now_us.saturating_sub(last_us) < ESCAPE_REPEAT_DEBOUNCE_US {
                return false;
            }
        }
        self.tap_timestamps_us
            .retain(|&t| now_us.saturating_sub(t) <= ESCAPE_TAP_WINDOW_US);
        self.tap_timestamps_us.push(now_us);
        if self.tap_timestamps_us.len() >= 3 {
            self.tap_timestamps_us.clear();
            return true;
        }
        false
    }

    fn process_chord(&mut self, event: &CapturedInputEvent) -> bool {
        match event {
            CapturedInputEvent::KeyPress { keycode } => {
                match *keycode {
                    // Left/Right Control (0xE0, 0xE4)
                    0xE0 | 0xE4 => {
                        self.ctrl_down = true;
                        false
                    }
                    // Left/Right Shift (0xE1, 0xE5)
                    0xE1 | 0xE5 => {
                        self.shift_down = true;
                        false
                    }
                    // Left/Right Alt (0xE2, 0xE6)
                    0xE2 | 0xE6 => {
                        self.alt_down = true;
                        false
                    }
                    // Modifier chord: Ctrl + Alt + Shift + Escape
                    ESCAPE_KEYCODE => self.ctrl_down && self.alt_down && self.shift_down,
                    // Pause/Break (0x48)
                    0x48 => {
                        if self.ctrl_down && self.alt_down {
                            return true;
                        }
                        false
                    }
                    // Backspace (0x2A)
                    0x2A => {
                        if self.ctrl_down && self.alt_down && self.shift_down {
                            return true;
                        }
                        false
                    }
                    _ => false,
                }
            }
            CapturedInputEvent::KeyRelease { keycode } => {
                match *keycode {
                    0xE0 | 0xE4 => self.ctrl_down = false,
                    0xE1 | 0xE5 => self.shift_down = false,
                    0xE2 | 0xE6 => self.alt_down = false,
                    _ => {}
                }
                false
            }
            _ => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::EmergencyUnlockTracker;
    use crate::platform::CapturedInputEvent;

    const F12: u32 = 0x45;

    #[test]
    fn emergency_unlock_on_ctrl_alt_shift_escape() {
        let mut tracker = EmergencyUnlockTracker::new();

        // Press Ctrl (0xE0)
        assert!(!tracker.process_event(&CapturedInputEvent::KeyPress { keycode: 0xE0 }, 100));
        // Press Alt (0xE2)
        assert!(!tracker.process_event(&CapturedInputEvent::KeyPress { keycode: 0xE2 }, 200));
        // Press Shift (0xE1)
        assert!(!tracker.process_event(&CapturedInputEvent::KeyPress { keycode: 0xE1 }, 250));
        // Press Escape (0x29)
        assert!(tracker.process_event(&CapturedInputEvent::KeyPress { keycode: 0x29 }, 300));
    }

    #[test]
    fn ctrl_alt_escape_without_shift_does_not_trigger() {
        let mut tracker = EmergencyUnlockTracker::new();

        // Press Ctrl (0xE0)
        assert!(!tracker.process_event(&CapturedInputEvent::KeyPress { keycode: 0xE0 }, 100));
        // Press Alt (0xE2)
        assert!(!tracker.process_event(&CapturedInputEvent::KeyPress { keycode: 0xE2 }, 200));
        // Press Escape (0x29) without Shift
        assert!(!tracker.process_event(&CapturedInputEvent::KeyPress { keycode: 0x29 }, 300));
    }

    #[test]
    fn emergency_unlock_on_ctrl_alt_pause() {
        let mut tracker = EmergencyUnlockTracker::new();

        assert!(!tracker.process_event(&CapturedInputEvent::KeyPress { keycode: 0xE0 }, 100));
        assert!(!tracker.process_event(&CapturedInputEvent::KeyPress { keycode: 0xE2 }, 150));
        assert!(tracker.process_event(&CapturedInputEvent::KeyPress { keycode: 0x48 }, 200));
    }

    #[test]
    fn ctrl_pause_without_alt_does_not_trigger() {
        let mut tracker = EmergencyUnlockTracker::new();

        assert!(!tracker.process_event(&CapturedInputEvent::KeyPress { keycode: 0xE0 }, 100));
        assert!(!tracker.process_event(&CapturedInputEvent::KeyPress { keycode: 0x48 }, 200));
    }

    #[test]
    fn emergency_unlock_on_ctrl_alt_shift_backspace() {
        let mut tracker = EmergencyUnlockTracker::new();

        assert!(!tracker.process_event(&CapturedInputEvent::KeyPress { keycode: 0xE0 }, 100));
        assert!(!tracker.process_event(&CapturedInputEvent::KeyPress { keycode: 0xE2 }, 200));
        assert!(!tracker.process_event(&CapturedInputEvent::KeyPress { keycode: 0xE1 }, 250));
        assert!(tracker.process_event(&CapturedInputEvent::KeyPress { keycode: 0x2A }, 300));
    }

    #[test]
    fn backspace_without_all_modifiers_does_not_trigger() {
        let mut tracker = EmergencyUnlockTracker::new();

        // Plain backspace
        assert!(!tracker.process_event(&CapturedInputEvent::KeyPress { keycode: 0x2A }, 100));

        // Ctrl + Backspace (word delete in Windows)
        assert!(!tracker.process_event(&CapturedInputEvent::KeyPress { keycode: 0xE0 }, 150));
        assert!(!tracker.process_event(&CapturedInputEvent::KeyPress { keycode: 0x2A }, 200));
        assert!(!tracker.process_event(&CapturedInputEvent::KeyRelease { keycode: 0xE0 }, 250));

        // Alt + Backspace
        assert!(!tracker.process_event(&CapturedInputEvent::KeyPress { keycode: 0xE2 }, 300));
        assert!(!tracker.process_event(&CapturedInputEvent::KeyPress { keycode: 0x2A }, 350));
        assert!(!tracker.process_event(&CapturedInputEvent::KeyRelease { keycode: 0xE2 }, 400));

        // Shift + Backspace
        assert!(!tracker.process_event(&CapturedInputEvent::KeyPress { keycode: 0xE1 }, 450));
        assert!(!tracker.process_event(&CapturedInputEvent::KeyPress { keycode: 0x2A }, 500));
    }

    #[test]
    fn reset_clears_latched_modifiers() {
        let mut tracker = EmergencyUnlockTracker::new();

        // Press Ctrl + Alt + Shift
        assert!(!tracker.process_event(&CapturedInputEvent::KeyPress { keycode: 0xE0 }, 100));
        assert!(!tracker.process_event(&CapturedInputEvent::KeyPress { keycode: 0xE2 }, 200));
        assert!(!tracker.process_event(&CapturedInputEvent::KeyPress { keycode: 0xE1 }, 250));

        // Reset (e.g. on mode transition)
        tracker.reset();

        // Backspace should not trigger now
        assert!(!tracker.process_event(&CapturedInputEvent::KeyPress { keycode: 0x2A }, 300));
    }

    #[test]
    fn emergency_unlock_on_triple_tap_escape() {
        let mut tracker = EmergencyUnlockTracker::new();

        // 1st escape
        assert!(!tracker.process_event(&CapturedInputEvent::KeyPress { keycode: 0x29 }, 100_000));
        assert!(!tracker.process_event(&CapturedInputEvent::KeyRelease { keycode: 0x29 }, 150_000));

        // 2nd escape
        assert!(!tracker.process_event(&CapturedInputEvent::KeyPress { keycode: 0x29 }, 300_000));
        assert!(!tracker.process_event(&CapturedInputEvent::KeyRelease { keycode: 0x29 }, 350_000));

        // 3rd escape within 1 second
        assert!(tracker.process_event(&CapturedInputEvent::KeyPress { keycode: 0x29 }, 600_000));
    }

    #[test]
    fn emergency_unlock_on_mashing_escape_without_key_releases() {
        let mut tracker = EmergencyUnlockTracker::new();

        // 1st escape (auto-repeat / mash without key release)
        assert!(!tracker.process_event(&CapturedInputEvent::KeyPress { keycode: 0x29 }, 100_000));
        // sub-30ms duplicate ignored
        assert!(!tracker.process_event(&CapturedInputEvent::KeyPress { keycode: 0x29 }, 110_000));
        // 2nd escape 50ms later
        assert!(!tracker.process_event(&CapturedInputEvent::KeyPress { keycode: 0x29 }, 160_000));
        // 3rd escape 50ms later
        assert!(tracker.process_event(&CapturedInputEvent::KeyPress { keycode: 0x29 }, 210_000));
    }

    #[test]
    fn slow_escape_presses_do_not_trigger_emergency_unlock() {
        let mut tracker = EmergencyUnlockTracker::new();

        // 1st escape at 1s
        assert!(!tracker.process_event(&CapturedInputEvent::KeyPress { keycode: 0x29 }, 1_000_000));
        // 2nd escape at 3s (> 1s later)
        assert!(!tracker.process_event(&CapturedInputEvent::KeyPress { keycode: 0x29 }, 3_000_000));
        // 3rd escape at 5s (> 1s later)
        assert!(!tracker.process_event(&CapturedInputEvent::KeyPress { keycode: 0x29 }, 5_000_000));
    }

    #[test]
    fn configured_tap_key_unlocks_and_escape_does_not() {
        let mut tracker = EmergencyUnlockTracker::new();
        let press = |keycode| CapturedInputEvent::KeyPress { keycode };

        assert!(!tracker.process_event_in_mode(&press(0x29), 100_000, F12, true));
        assert!(!tracker.process_event_in_mode(&press(0x29), 200_000, F12, true));
        assert!(!tracker.process_event_in_mode(&press(0x29), 300_000, F12, true));

        assert!(!tracker.process_event_in_mode(&press(F12), 400_000, F12, true));
        assert!(!tracker.process_event_in_mode(&press(F12), 500_000, F12, true));
        assert!(tracker.process_event_in_mode(&press(F12), 600_000, F12, true));
    }

    #[test]
    fn escape_chord_still_works_with_other_tap_key() {
        let mut tracker = EmergencyUnlockTracker::new();
        let press = |keycode| CapturedInputEvent::KeyPress { keycode };

        assert!(!tracker.process_event_in_mode(&press(0xE0), 100, F12, true));
        assert!(!tracker.process_event_in_mode(&press(0xE2), 200, F12, true));
        assert!(!tracker.process_event_in_mode(&press(0xE1), 250, F12, true));
        assert!(tracker.process_event_in_mode(&press(0x29), 300, F12, true));
    }

    #[test]
    fn changing_tap_key_resets_tap_count() {
        let mut tracker = EmergencyUnlockTracker::new();
        let press = |keycode| CapturedInputEvent::KeyPress { keycode };

        assert!(!tracker.process_event_in_mode(&press(F12), 100_000, F12, true));
        assert!(!tracker.process_event_in_mode(&press(F12), 200_000, F12, true));
        assert!(!tracker.process_event_in_mode(&press(0x29), 300_000, 0x29, true));
        assert!(!tracker.process_event_in_mode(&press(F12), 400_000, F12, true));
        assert!(!tracker.process_event_in_mode(&press(F12), 500_000, F12, true));
        assert!(tracker.process_event_in_mode(&press(F12), 600_000, F12, true));
    }

    #[test]
    fn disabled_tap_key_never_unlocks() {
        let mut tracker = EmergencyUnlockTracker::new();
        let press = |keycode| CapturedInputEvent::KeyPress { keycode };

        for (i, keycode) in [0x29, 0x29, 0x29, 0x00, 0x00, 0x00].into_iter().enumerate() {
            let now_us = 100_000 * (i as u64 + 1);
            assert!(!tracker.process_event_in_mode(&press(keycode), now_us, 0, true));
        }
    }

    #[test]
    fn taps_in_local_control_clear_remote_tap_count() {
        let mut tracker = EmergencyUnlockTracker::new();
        let press = |keycode| CapturedInputEvent::KeyPress { keycode };

        assert!(!tracker.process_event_in_mode(&press(F12), 100_000, F12, true));
        assert!(!tracker.process_event_in_mode(&press(F12), 200_000, F12, true));
        assert!(!tracker.process_event_in_mode(&press(F12), 300_000, F12, false));
        assert!(!tracker.process_event_in_mode(&press(F12), 400_000, F12, true));
        assert!(!tracker.process_event_in_mode(&press(F12), 500_000, F12, true));
        assert!(tracker.process_event_in_mode(&press(F12), 600_000, F12, true));
    }

    #[test]
    fn regular_keys_do_not_trigger_emergency_unlock() {
        let mut tracker = EmergencyUnlockTracker::new();

        // Press 'A' (0x04)
        assert!(!tracker.process_event(&CapturedInputEvent::KeyPress { keycode: 0x04 }, 100));
        assert!(!tracker.process_event(&CapturedInputEvent::KeyRelease { keycode: 0x04 }, 200));

        // Ctrl + A
        assert!(!tracker.process_event(&CapturedInputEvent::KeyPress { keycode: 0xE0 }, 300));
        assert!(!tracker.process_event(&CapturedInputEvent::KeyPress { keycode: 0x04 }, 400));
        assert!(!tracker.process_event(&CapturedInputEvent::KeyRelease { keycode: 0x04 }, 500));
        assert!(!tracker.process_event(&CapturedInputEvent::KeyRelease { keycode: 0xE0 }, 600));
    }

    #[test]
    fn translate_hid_for_target_platform_passes_through_modifiers() {
        use crate::core::platform::{translate_hid_for_target_platform, PlatformFamily};

        // Mac -> Windows passes through 1:1 when swap_alt_cmd is false
        assert_eq!(
            translate_hid_for_target_platform(
                0xE3,
                PlatformFamily::Macos,
                PlatformFamily::Windows,
                false,
                None,
            ),
            0xE3
        );
        assert_eq!(
            translate_hid_for_target_platform(
                0xE0,
                PlatformFamily::Macos,
                PlatformFamily::Windows,
                false,
                None,
            ),
            0xE0
        );
        assert_eq!(
            translate_hid_for_target_platform(
                0xE7,
                PlatformFamily::Macos,
                PlatformFamily::Windows,
                false,
                None,
            ),
            0xE7
        );
        assert_eq!(
            translate_hid_for_target_platform(
                0xE4,
                PlatformFamily::Macos,
                PlatformFamily::Windows,
                false,
                None,
            ),
            0xE4
        );

        // Windows -> Mac passes through 1:1 when swap_alt_cmd is false
        assert_eq!(
            translate_hid_for_target_platform(
                0xE0,
                PlatformFamily::Windows,
                PlatformFamily::Macos,
                false,
                None,
            ),
            0xE0
        );
        assert_eq!(
            translate_hid_for_target_platform(
                0xE3,
                PlatformFamily::Windows,
                PlatformFamily::Macos,
                false,
                None,
            ),
            0xE3
        );

        // Option / Alt (0xE2, 0xE6) and Shift (0xE1, 0xE5) remain unchanged
        assert_eq!(
            translate_hid_for_target_platform(
                0xE2,
                PlatformFamily::Macos,
                PlatformFamily::Windows,
                false,
                None,
            ),
            0xE2
        );
        assert_eq!(
            translate_hid_for_target_platform(
                0xE1,
                PlatformFamily::Macos,
                PlatformFamily::Windows,
                false,
                None,
            ),
            0xE1
        );

        // Option/Alt <-> Cmd/Win swapped when swap_alt_cmd is true
        assert_eq!(
            translate_hid_for_target_platform(
                0xE2,
                PlatformFamily::Macos,
                PlatformFamily::Windows,
                true,
                None,
            ),
            0xE3
        );
        assert_eq!(
            translate_hid_for_target_platform(
                0xE3,
                PlatformFamily::Macos,
                PlatformFamily::Windows,
                true,
                None,
            ),
            0xE2
        );

        // Same platform passes through
        assert_eq!(
            translate_hid_for_target_platform(
                0xE3,
                PlatformFamily::Macos,
                PlatformFamily::Macos,
                false,
                None,
            ),
            0xE3
        );
        assert_eq!(
            translate_hid_for_target_platform(
                0xE0,
                PlatformFamily::Windows,
                PlatformFamily::Windows,
                false,
                None,
            ),
            0xE0
        );
    }

    #[test]
    fn mouse_move_resets_keep_alive_timer() {
        use super::InputLoopState;
        use crate::events::{Event, Packet};
        use std::time::{Duration, Instant};

        let mut state = InputLoopState::new();
        let past = Instant::now() - Duration::from_secs(10);

        state.last_keep_alive = past;
        state.on_local_event(&CapturedInputEvent::KeyPress { keycode: 0x04 });
        assert_eq!(state.last_keep_alive, past);

        state.on_local_event(&CapturedInputEvent::MouseMove {
            x: 10,
            y: 20,
            dx: 1.0,
            dy: 1.0,
            locally_delivered: true,
        });
        assert!(state.last_keep_alive > past);

        let past2 = Instant::now() - Duration::from_secs(10);
        state.last_keep_alive = past2;
        state.on_inbound_packet(&Packet {
            version: crate::events::PROTOCOL_VERSION,
            seq: 1,
            sender_id: 1,
            t_mono_us: 0,
            events: vec![Event::KeyPress { keycode: 0x04 }],
        });
        assert_eq!(state.last_keep_alive, past2);

        state.on_inbound_packet(&Packet {
            version: crate::events::PROTOCOL_VERSION,
            seq: 2,
            sender_id: 1,
            t_mono_us: 0,
            events: vec![Event::MouseMove {
                dx: 1,
                dy: 1,
                total_dx: 1,
                total_dy: 1,
                motion_session_id: 1,
            }],
        });
        assert!(state.last_keep_alive > past2);
    }
}
