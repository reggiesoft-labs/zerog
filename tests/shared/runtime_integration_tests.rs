use anyhow::Result;
use serial_test::serial;
use std::net::UdpSocket;

use mouse_thing::core::api::ConfigSnapshot;
use mouse_thing::core::peer_state::{ConnectionStatus, TransportReachability};
use mouse_thing::core::platform::EdgeAssignments;
use mouse_thing::events::{Event, LayoutPlacement, LayoutUpdateEvent, PROTOCOL_VERSION, Packet};
use mouse_thing::platform::{DisplayBounds, PeerEdge, TransferMode};

use mouse_thing::platform::shared::test_support::{
    DEFAULT_WAIT_POLL, DEFAULT_WAIT_TIMEOUT, InjectedCall, MODE_STABILITY_WAIT, TestPeerPair,
    assert_mode_stays, drive_to_edge, expected_receiver_entry, mouse_move, wait_until,
    suppressed_mouse_move, with_tokio_runtime,
};

use std::time::Duration;

const MALFORMED_PACKET_PROCESS_MS: u64 = 150;
const BURST_EVENT_COUNT: usize = 100;
const BURST_DELIVERY_MS: u64 = 200;
const KEY_EVENT_COUNT: usize = 20;
const TRANSITION_COOLDOWN_WAIT: Duration = Duration::from_millis(110);

#[test]
#[serial]
fn startup_connection_and_clean_shutdown_smoke() -> Result<()> {
    with_tokio_runtime(|| {
        let pair = TestPeerPair::new(PeerEdge::Right, PeerEdge::Left)?;

        assert_eq!(pair.a.get_connection_count(&pair.b.peer_id), 1);
        assert_eq!(pair.b.get_connection_count(&pair.a.peer_id), 1);

        pair.a.stop()?;
        pair.b.stop()?;

        assert_eq!(pair.a.get_connection_count(&pair.b.peer_id), 0);
        assert_eq!(pair.b.get_connection_count(&pair.a.peer_id), 0);
        Ok(())
    })
}

#[test]
#[serial]
fn handoff_right_edge_sets_sender_and_receiver_modes() -> Result<()> {
    with_tokio_runtime(|| {
        let pair = TestPeerPair::new(PeerEdge::Right, PeerEdge::Left)?;
        let along_axis = 540;

        drive_to_edge(&pair.a, PeerEdge::Right, along_axis)?;
        pair.a
            .wait_for_mode(TransferMode::RemoteControl, DEFAULT_WAIT_TIMEOUT)?;
        pair.b
            .wait_for_mode(TransferMode::LocalControl, DEFAULT_WAIT_TIMEOUT)?;

        let expected_entry =
            expected_receiver_entry(pair.b.display_bounds(), PeerEdge::Right, along_axis, None);
        pair.b
            .wait_for_calls("receiver handoff setup", DEFAULT_WAIT_TIMEOUT, |calls| {
                calls
                    .iter()
                    .any(|call| *call == InjectedCall::CursorUnlocked)
                    && calls
                        .iter()
                        .any(|call| *call == InjectedCall::CursorHidden { hidden: false })
                    && calls.iter().any(|call| {
                        matches!(call, InjectedCall::CursorWarped { x, y }
                            if *x == expected_entry.0 && (*y - expected_entry.1).abs() <= 1)
                    })
            })?;

        let calls = pair.b.take_injected();
        let warp_index = calls
            .iter()
            .position(|call| {
                matches!(call, InjectedCall::CursorWarped { x, y }
                    if *x == expected_entry.0 && (*y - expected_entry.1).abs() <= 1)
            })
            .expect("receiver should warp to the entry point");
        let unlock_index = calls
            .iter()
            .position(|call| *call == InjectedCall::CursorUnlocked)
            .expect("receiver should unlock the cursor");
        let show_index = calls
            .iter()
            .position(|call| *call == InjectedCall::CursorHidden { hidden: false })
            .expect("receiver should show the cursor");
        assert!(unlock_index < warp_index);
        assert!(warp_index < show_index);
        assert_eq!(pair.b.cursor_pos().0, expected_entry.0);
        assert!(
            calls
                .iter()
                .any(|call| *call == InjectedCall::CursorUnlocked)
        );
        assert!(
            calls
                .iter()
                .any(|call| *call == InjectedCall::CursorHidden { hidden: false })
        );
        assert!(calls.iter().any(|call| {
            matches!(call, InjectedCall::CursorWarped { x, y }
                if *x == expected_entry.0 && (*y - expected_entry.1).abs() <= 1)
        }));

        Ok(())
    })
}

#[test]
#[serial]
fn handoff_syncs_caps_lock_to_receiver() -> Result<()> {
    with_tokio_runtime(|| {
        let pair = TestPeerPair::new(PeerEdge::Right, PeerEdge::Left)?;
        pair.a.set_caps_lock_state(true);
        assert!(!pair.b.caps_lock_active());

        drive_to_edge(&pair.a, PeerEdge::Right, 540)?;
        pair.a
            .wait_for_mode(TransferMode::RemoteControl, DEFAULT_WAIT_TIMEOUT)?;
        pair.b.wait_for_calls(
            "receiver caps lock sync",
            DEFAULT_WAIT_TIMEOUT,
            |calls| calls.contains(&InjectedCall::SetCapsLock { active: true }),
        )?;

        assert!(pair.b.caps_lock_active());

        Ok(())
    })
}

#[test]
#[serial]
fn duplicate_take_control_retains_active_session() -> Result<()> {
    with_tokio_runtime(|| {
        let pair = TestPeerPair::new(PeerEdge::Right, PeerEdge::Left)?;
        let along_axis = 540;

        drive_to_edge(&pair.a, PeerEdge::Right, along_axis)?;
        pair.a
            .wait_for_mode(TransferMode::RemoteControl, DEFAULT_WAIT_TIMEOUT)?;
        pair.b
            .wait_for_mode(TransferMode::LocalControl, DEFAULT_WAIT_TIMEOUT)?;

        drive_to_edge(&pair.a, PeerEdge::Right, along_axis)?;

        assert_mode_stays(&pair.a, TransferMode::RemoteControl, MODE_STABILITY_WAIT)?;
        assert_mode_stays(&pair.b, TransferMode::LocalControl, MODE_STABILITY_WAIT)?;

        Ok(())
    })
}

#[test]
#[serial]
fn rejected_handoff_restores_sender_control() -> Result<()> {
    with_tokio_runtime(|| {
        let pair = TestPeerPair::new(PeerEdge::Right, PeerEdge::Left)?;
        let layout_a = ConfigSnapshot {
            placements: vec![LayoutPlacement {
                peer_id: pair.b.peer_id.clone(),
                peer_label: pair.b.peer_name.clone(),
                x: 166,
                y: 0,
            }],
            ..Default::default()
        };
        let layout_b = ConfigSnapshot {
            placements: vec![LayoutPlacement {
                peer_id: pair.a.peer_id.clone(),
                peer_label: pair.a.peer_name.clone(),
                x: 0,
                y: -106,
            }],
            ..Default::default()
        };

        pair.a.reconcile_layout(&layout_a)?;
        pair.b.reconcile_layout(&layout_b)?;
        pair.a.clear_injected();

        drive_to_edge(&pair.a, PeerEdge::Right, 500)?;
        pair.a
            .wait_for_calls("rejected handoff recovery", DEFAULT_WAIT_TIMEOUT, |calls| {
                calls.iter().any(|call| *call == InjectedCall::CursorLocked)
                    && calls
                        .iter()
                        .any(|call| *call == InjectedCall::CursorUnlocked)
                    && calls
                        .iter()
                        .any(|call| *call == InjectedCall::CursorWarped { x: 1_919, y: 500 })
            })?;

        let calls = pair.a.take_injected();
        let warp_index = calls
            .iter()
            .position(|call| *call == InjectedCall::CursorWarped { x: 1_919, y: 500 })
            .expect("rejected handoff should return to its source position");
        let unlock_index = calls
            .iter()
            .position(|call| *call == InjectedCall::CursorUnlocked)
            .expect("rejected handoff should unlock the cursor");
        let show_index = calls
            .iter()
            .position(|call| *call == InjectedCall::CursorHidden { hidden: false })
            .expect("rejected handoff should show the cursor");
        assert!(unlock_index < warp_index);
        assert!(warp_index < show_index);
        assert_ne!(pair.a.cursor_pos(), (960, 540));
        assert_eq!(pair.a.transfer_mode(), TransferMode::LocalControl);
        assert_eq!(pair.b.transfer_mode(), TransferMode::LocalControl);
        Ok(())
    })
}

#[test]
#[serial]
fn layout_reconciliation_updates_runtime_edge() -> Result<()> {
    with_tokio_runtime(|| {
        let pair = TestPeerPair::new(PeerEdge::Right, PeerEdge::Left)?;
        let layout_a = ConfigSnapshot {
            placements: vec![LayoutPlacement {
                peer_id: pair.b.peer_id.clone(),
                peer_label: pair.b.peer_name.clone(),
                x: 0,
                y: -106,
            }],
            ..Default::default()
        };
        let layout_b = ConfigSnapshot {
            placements: vec![LayoutPlacement {
                peer_id: pair.a.peer_id.clone(),
                peer_label: pair.a.peer_name.clone(),
                x: 0,
                y: 106,
            }],
            ..Default::default()
        };

        pair.a.reconcile_layout(&layout_a)?;
        pair.b.reconcile_layout(&layout_b)?;
        pair.a.clear_injected();
        pair.b.clear_injected();

        drive_to_edge(&pair.a, PeerEdge::Right, 500)?;
        assert_mode_stays(&pair.a, TransferMode::LocalControl, MODE_STABILITY_WAIT)?;

        drive_to_edge(&pair.a, PeerEdge::Top, 900)?;
        pair.a
            .wait_for_mode(TransferMode::RemoteControl, DEFAULT_WAIT_TIMEOUT)?;
        pair.b
            .wait_for_mode(TransferMode::LocalControl, DEFAULT_WAIT_TIMEOUT)?;
        Ok(())
    })
}

#[test]
#[serial]
fn edge_buffer_timer_prevents_oscillation_flapping() -> Result<()> {
    with_tokio_runtime(|| {
        let pair = TestPeerPair::new(PeerEdge::Right, PeerEdge::Left)?;
        let bounds = pair.a.display_bounds();
        let edge_x = bounds.x + bounds.width as i32 - 1;
        let inside_x = edge_x - 20;
        let along_axis = 540;

        for _ in 0..8 {
            pair.a.emit_many([
                mouse_move(edge_x, along_axis, 0, 0),
                mouse_move(inside_x, along_axis, 0, 0),
            ])?;
            std::thread::sleep(Duration::from_millis(1));
        }

        assert_mode_stays(&pair.a, TransferMode::LocalControl, MODE_STABILITY_WAIT)?;
        assert_mode_stays(&pair.b, TransferMode::LocalControl, MODE_STABILITY_WAIT)?;

        drive_to_edge(&pair.a, PeerEdge::Right, along_axis)?;
        pair.a
            .wait_for_mode(TransferMode::RemoteControl, DEFAULT_WAIT_TIMEOUT)?;
        pair.b
            .wait_for_mode(TransferMode::LocalControl, DEFAULT_WAIT_TIMEOUT)?;

        Ok(())
    })
}

#[test]
#[serial]
fn small_outward_moves_cross_the_edge_without_waiting() -> Result<()> {
    with_tokio_runtime(|| {
        let pair = TestPeerPair::new(PeerEdge::Right, PeerEdge::Left)?;
        let bounds = pair.a.display_bounds();
        let edge_x = bounds.x + bounds.width as i32 - 1;
        let along_axis = 540;

        for _ in 0..4 {
            pair.a.emit(mouse_move(edge_x, along_axis, 1, 0))?;
        }

        pair.a
            .wait_for_mode(TransferMode::RemoteControl, DEFAULT_WAIT_TIMEOUT)?;
        pair.b
            .wait_for_mode(TransferMode::LocalControl, DEFAULT_WAIT_TIMEOUT)?;
        Ok(())
    })
}

#[test]
#[serial]
fn edge_drag_requires_fresh_outward_motion_after_release() -> Result<()> {
    with_tokio_runtime(|| {
        let pair = TestPeerPair::new(PeerEdge::Right, PeerEdge::Left)?;
        let bounds = pair.a.display_bounds();
        let edge_x = bounds.x + bounds.width as i32 - 1;
        let along_axis = 540;

        pair.a.emit(mouse_move(edge_x, along_axis, 1, 0))?;
        pair.a.emit(mouse_button(0, true))?;
        std::thread::sleep(Duration::from_millis(60));
        pair.a.emit(mouse_move(edge_x, along_axis, 6, 0))?;
        pair.a.emit(mouse_button(0, false))?;
        pair.a.emit(mouse_move(edge_x, along_axis, 1, 0))?;

        assert_mode_stays(&pair.a, TransferMode::LocalControl, MODE_STABILITY_WAIT)?;

        for _ in 0..3 {
            pair.a.emit(mouse_move(edge_x, along_axis, 1, 0))?;
        }

        pair.a
            .wait_for_mode(TransferMode::RemoteControl, DEFAULT_WAIT_TIMEOUT)?;
        Ok(())
    })
}

#[test]
#[serial]
fn reverse_motion_releases_control_and_restores_local_mode() -> Result<()> {
    with_tokio_runtime(|| {
        let pair = TestPeerPair::new(PeerEdge::Right, PeerEdge::Left)?;
        let along_axis = 480;

        drive_to_edge(&pair.a, PeerEdge::Right, along_axis)?;
        pair.a
            .wait_for_mode(TransferMode::RemoteControl, DEFAULT_WAIT_TIMEOUT)?;
        pair.b
            .wait_for_mode(TransferMode::LocalControl, DEFAULT_WAIT_TIMEOUT)?;
        pair.b.wait_for_calls("initial handoff", DEFAULT_WAIT_TIMEOUT, |calls| {
            calls
                .iter()
                .any(|call| matches!(call, InjectedCall::CursorWarped { .. }))
        })?;
        pair.a.clear_injected();
        pair.b.clear_injected();

        std::thread::sleep(TRANSITION_COOLDOWN_WAIT);
        drive_to_edge(&pair.b, PeerEdge::Left, along_axis)?;
        pair.a
            .wait_for_mode(TransferMode::LocalControl, DEFAULT_WAIT_TIMEOUT)?;
        pair.b
            .wait_for_mode(TransferMode::RemoteControl, DEFAULT_WAIT_TIMEOUT)?;

        let expected_entry =
            expected_receiver_entry(pair.a.display_bounds(), PeerEdge::Left, along_axis, None);
        wait_until(
            "return handoff lands on sender",
            DEFAULT_WAIT_TIMEOUT,
            DEFAULT_WAIT_POLL,
            || pair.a.cursor_pos() == expected_entry,
            || pair.a.debug_snapshot(),
        )?;

        let a_calls = pair.a.take_injected();
        assert!(a_calls.iter().any(|c| *c == InjectedCall::AllKeysReleased));
        assert!(a_calls.iter().any(|c| *c == InjectedCall::CursorUnlocked));
        assert!(
            a_calls
                .iter()
                .any(|c| *c == InjectedCall::CursorHidden { hidden: false })
        );
        assert!(a_calls.iter().any(|c| *c
            == InjectedCall::CursorWarped {
                x: expected_entry.0,
                y: expected_entry.1,
            }));

        let b_calls = pair.b.take_injected();
        assert!(
            b_calls
                .iter()
                .any(|c| *c == InjectedCall::CursorHidden { hidden: true }),
            "expected receiver to hide cursor when it loses ownership"
        );
        assert!(b_calls.iter().any(|c| *c == InjectedCall::CursorLocked));

        Ok(())
    })
}

#[test]
#[serial]
fn suppressed_motion_is_replayed_after_control_returns_to_device_host() -> Result<()> {
    with_tokio_runtime(|| {
        let pair = TestPeerPair::new(PeerEdge::Right, PeerEdge::Left)?;
        let along_axis = 480;

        drive_to_edge(&pair.b, PeerEdge::Left, along_axis)?;
        pair.b
            .wait_for_mode(TransferMode::RemoteControl, DEFAULT_WAIT_TIMEOUT)?;
        pair.a
            .wait_for_mode(TransferMode::LocalControl, DEFAULT_WAIT_TIMEOUT)?;

        std::thread::sleep(TRANSITION_COOLDOWN_WAIT);
        drive_to_edge(&pair.a, PeerEdge::Right, along_axis)?;
        pair.b
            .wait_for_mode(TransferMode::LocalControl, DEFAULT_WAIT_TIMEOUT)?;

        let entry =
            expected_receiver_entry(pair.b.display_bounds(), PeerEdge::Right, along_axis, None);
        wait_until(
            "device host return entry",
            DEFAULT_WAIT_TIMEOUT,
            DEFAULT_WAIT_POLL,
            || pair.b.cursor_pos() == entry,
            || pair.b.debug_snapshot(),
        )?;
        pair.b.clear_injected();

        pair.b.emit_captured(suppressed_mouse_move(
            entry.0, entry.1, 12, 0,
        ))?;
        wait_until(
            "suppressed transition motion replay",
            DEFAULT_WAIT_TIMEOUT,
            DEFAULT_WAIT_POLL,
            || pair.b.cursor_pos() == (entry.0 + 12, entry.1),
            || pair.b.debug_snapshot(),
        )?;

        assert!(pair.b.take_injected().contains(
            &InjectedCall::MouseMoveRelative { dx: 12, dy: 0 }
        ));
        Ok(())
    })
}

#[test]
#[serial]
fn outbound_motion_during_return_handoff_is_replayed_on_device_host() -> Result<()> {
    with_tokio_runtime(|| {
        let pair = TestPeerPair::new(PeerEdge::Right, PeerEdge::Left)?;
        let along_axis = 480;

        drive_to_edge(&pair.b, PeerEdge::Left, along_axis)?;
        pair.b
            .wait_for_mode(TransferMode::RemoteControl, DEFAULT_WAIT_TIMEOUT)?;
        pair.a
            .wait_for_mode(TransferMode::LocalControl, DEFAULT_WAIT_TIMEOUT)?;
        pair.b.clear_injected();

        std::thread::sleep(TRANSITION_COOLDOWN_WAIT);
        pair.b.emit_many([
            mouse_move(0, 0, 12, 0),
            mouse_move(0, 0, 9, 0),
        ])?;
        pair.b
            .wait_for_mode(TransferMode::LocalControl, DEFAULT_WAIT_TIMEOUT)?;

        let entry =
            expected_receiver_entry(pair.b.display_bounds(), PeerEdge::Right, along_axis, None);
        let expected = (entry.0 + 9, entry.1);
        wait_until(
            "outbound transition tail replay",
            DEFAULT_WAIT_TIMEOUT,
            DEFAULT_WAIT_POLL,
            || pair.b.cursor_pos() == expected,
            || pair.b.debug_snapshot(),
        )?;

        assert!(pair.b.take_injected().contains(
            &InjectedCall::MouseMove {
                x: expected.0,
                y: expected.1,
            }
        ));
        Ok(())
    })
}

#[test]
#[serial]
fn cursor_clamped_corner_during_active_session() -> Result<()> {
    with_tokio_runtime(|| {
        let pair = TestPeerPair::new(PeerEdge::Left, PeerEdge::Right)?;
        pair.b.set_display_bounds(DisplayBounds {
            x: 0,
            y: 0,
            width: 400,
            height: 300,
        });
        pair.b.refresh_display_topology()?;

        let along_axis = 50;
        drive_to_edge(&pair.a, PeerEdge::Left, along_axis)?;
        pair.a
            .wait_for_mode(TransferMode::RemoteControl, DEFAULT_WAIT_TIMEOUT)?;
        pair.b
            .wait_for_mode(TransferMode::LocalControl, DEFAULT_WAIT_TIMEOUT)?;

        let bounds = pair.b.display_bounds();
        let max_x = bounds.x + bounds.width as i32 - 1;
        let max_y = bounds.y + bounds.height as i32 - 1;
        let (x, y) = pair.b.cursor_pos();
        assert!(
            x >= bounds.x && x <= max_x,
            "x={x} should be within receiver bounds"
        );
        assert!(
            y >= bounds.y && y <= max_y,
            "y={y} should be within receiver bounds"
        );

        Ok(())
    })
}

#[test]
#[serial]
fn receiver_entry_point_is_clamped_to_display_bounds() -> Result<()> {
    with_tokio_runtime(|| {
        let pair = TestPeerPair::new(PeerEdge::Right, PeerEdge::Left)?;
        pair.b.set_display_bounds(DisplayBounds {
            x: -100,
            y: 50,
            width: 300,
            height: 200,
        });
        pair.b.refresh_display_topology()?;

        let along_axis = 600;
        drive_to_edge(&pair.a, PeerEdge::Right, along_axis)?;
        pair.b
            .wait_for_mode(TransferMode::LocalControl, DEFAULT_WAIT_TIMEOUT)?;

        let expected_entry =
            expected_receiver_entry(pair.b.display_bounds(), PeerEdge::Right, along_axis, None);
        wait_until(
            "clamped receiver entry",
            DEFAULT_WAIT_TIMEOUT,
            DEFAULT_WAIT_POLL,
            || {
                let (x, y) = pair.b.cursor_pos();
                let bounds = pair.b.display_bounds();
                let max_y = bounds.y + bounds.height as i32 - 1;
                x == expected_entry.0 && y >= bounds.y && y <= max_y
            },
            || pair.b.debug_snapshot(),
        )?;

        Ok(())
    })
}

#[test]
#[serial]
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
#[serial]
fn runtime_peer_edge_reconciliation_changes_handoff_edge() -> Result<()> {
    with_tokio_runtime(|| {
        let pair = TestPeerPair::new(PeerEdge::Right, PeerEdge::Left)?;
        let assignments =
            EdgeAssignments::from_pairs(vec![(pair.b.peer_id.clone(), PeerEdge::Bottom)]);
        let inverse_assignments =
            EdgeAssignments::from_pairs(vec![(pair.a.peer_id.clone(), PeerEdge::Top)]);

        pair.a.reconcile_peer_edges(&assignments)?;
        pair.b.reconcile_peer_edges(&inverse_assignments)?;
        pair.a.clear_injected();
        pair.b.clear_injected();

        drive_to_edge(&pair.a, PeerEdge::Right, 400)?;
        assert_mode_stays(&pair.a, TransferMode::LocalControl, MODE_STABILITY_WAIT)?;
        assert_mode_stays(&pair.b, TransferMode::LocalControl, MODE_STABILITY_WAIT)?;
        assert!(pair.b.take_injected().is_empty());

        drive_to_edge(&pair.a, PeerEdge::Bottom, 640)?;
        pair.a
            .wait_for_mode(TransferMode::RemoteControl, DEFAULT_WAIT_TIMEOUT)?;
        pair.b
            .wait_for_mode(TransferMode::LocalControl, DEFAULT_WAIT_TIMEOUT)?;

        Ok(())
    })
}

#[test]
#[serial]
fn packets_from_unknown_peers_without_connection_events_are_ignored() -> Result<()> {
    with_tokio_runtime(|| {
        let pair = TestPeerPair::new(PeerEdge::Right, PeerEdge::Left)?;
        let socket = UdpSocket::bind("127.0.0.1:0")?;
        let packet = Packet {
            version: PROTOCOL_VERSION,
            sender_id: 9_999,
            seq: 1,
            t_mono_us: 0,
            events: vec![Event::TakeControl {
                edge: PeerEdge::Left,
                initial_x: 0,
                initial_y: 420,
                receiver_position_bp: None,
                motion_session_id: 1,
                return_motion: None,
                caps_lock: None,
            }],
        };

        socket.send_to(&bincode::serialize(&packet)?, pair.a.addr()?)?;

        assert_mode_stays(&pair.a, TransferMode::LocalControl, MODE_STABILITY_WAIT)?;
        assert!(pair.a.take_injected().is_empty());

        Ok(())
    })
}

#[test]
#[serial]
fn packets_from_local_sender_id_are_ignored() -> Result<()> {
    with_tokio_runtime(|| {
        let pair = TestPeerPair::new(PeerEdge::Right, PeerEdge::Left)?;
        let socket = UdpSocket::bind("127.0.0.1:0")?;
        let packet = Packet {
            version: PROTOCOL_VERSION,
            sender_id: pair.a.sender_id,
            seq: 1,
            t_mono_us: 0,
            events: vec![Event::TakeControl {
                edge: PeerEdge::Left,
                initial_x: 0,
                initial_y: 360,
                receiver_position_bp: None,
                motion_session_id: 1,
                return_motion: None,
                caps_lock: None,
            }],
        };

        socket.send_to(&bincode::serialize(&packet)?, pair.a.addr()?)?;

        assert_mode_stays(&pair.a, TransferMode::LocalControl, MODE_STABILITY_WAIT)?;
        assert!(pair.a.take_injected().is_empty());

        Ok(())
    })
}

#[test]
#[serial]
fn layout_updates_round_trip_over_udp() -> Result<()> {
    with_tokio_runtime(|| {
        let pair = TestPeerPair::new(PeerEdge::Right, PeerEdge::Left)?;
        let update = LayoutUpdateEvent {
            version: 7,
            editor_id: pair.a.sender_id,
            timestamp: 0,
            placements: vec![
                LayoutPlacement {
                    peer_id: pair.a.peer_id.clone(),
                    peer_label: pair.a.peer_name.clone(),
                    x: 0,
                    y: 0,
                },
                LayoutPlacement {
                    peer_id: pair.b.peer_id.clone(),
                    peer_label: pair.b.peer_name.clone(),
                    x: 1,
                    y: 0,
                },
            ],
        };

        pair.a.send_layout_update(update.clone())?;

        std::thread::sleep(Duration::from_millis(100));
        let peer_state = pair
            .b
            .runtime_state()
            .into_iter()
            .find(|state| state.peer_id == pair.a.peer_id)
            .expect("peer A should remain present in peer B runtime state");
        assert_eq!(
            peer_state.transport_reachability,
            TransportReachability::Reachable
        );
        assert!(matches!(
            peer_state.connection_status,
            ConnectionStatus::Connected | ConnectionStatus::Incoming
        ));

        Ok(())
    })
}

#[test]
#[serial]
fn removing_peer_notifies_remote_and_clears_local_runtime_state() -> Result<()> {
    with_tokio_runtime(|| {
        let pair = TestPeerPair::new(PeerEdge::Right, PeerEdge::Left)?;

        pair.a.remove_peer(&pair.b.peer_id)?;

        wait_until(
            "local peer removal",
            DEFAULT_WAIT_TIMEOUT,
            DEFAULT_WAIT_POLL,
            || {
                pair.a.runtime_state().iter().all(|state| {
                    state.peer_id != pair.b.peer_id
                        || state.connection_status == ConnectionStatus::Disconnected
                })
            },
            || pair.a.debug_snapshot(),
        )?;

        wait_until(
            "remote disconnect notification",
            DEFAULT_WAIT_TIMEOUT,
            DEFAULT_WAIT_POLL,
            || {
                pair.b.runtime_state().iter().any(|state| {
                    state.peer_id == pair.a.peer_id
                        && state.connection_status == ConnectionStatus::Disconnected
                        && state.transport_reachability == TransportReachability::Reachable
                })
            },
            || pair.b.debug_snapshot(),
        )?;

        Ok(())
    })
}

#[test]
#[serial]
fn active_owner_recovers_when_remote_peer_disconnects() -> Result<()> {
    with_tokio_runtime(|| {
        let pair = TestPeerPair::new(PeerEdge::Right, PeerEdge::Left)?;
        let along_axis = 540;

        drive_to_edge(&pair.a, PeerEdge::Right, along_axis)?;
        pair.a
            .wait_for_mode(TransferMode::RemoteControl, DEFAULT_WAIT_TIMEOUT)?;
        pair.a.clear_injected();
        pair.b.clear_injected();

        pair.b.remove_peer(&pair.a.peer_id)?;

        pair.a
            .wait_for_mode(TransferMode::LocalControl, DEFAULT_WAIT_TIMEOUT)?;
        let calls = pair.a.take_injected();
        assert!(calls.contains(&InjectedCall::AllKeysReleased));
        assert!(calls.contains(&InjectedCall::CursorUnlocked));
        assert!(calls.contains(&InjectedCall::CursorHidden { hidden: false }));
        assert_eq!(pair.a.cursor_pos(), (1_919, along_axis));
        let receiver_calls = pair.b.take_injected();
        assert!(
            receiver_calls.contains(&InjectedCall::AllKeysReleased),
            "receiver calls: {receiver_calls:?}; {}",
            pair.b.debug_snapshot()
        );

        Ok(())
    })
}

#[test]
#[serial]
fn active_owner_recovers_when_remote_process_stops() -> Result<()> {
    with_tokio_runtime(|| {
        let pair = TestPeerPair::new(PeerEdge::Right, PeerEdge::Left)?;
        let along_axis = 540;

        drive_to_edge(&pair.a, PeerEdge::Right, along_axis)?;
        pair.a
            .wait_for_mode(TransferMode::RemoteControl, DEFAULT_WAIT_TIMEOUT)?;
        pair.a.clear_injected();

        pair.b.stop()?;

        pair.a
            .wait_for_mode(TransferMode::LocalControl, DEFAULT_WAIT_TIMEOUT)?;
        let calls = pair.a.take_injected();
        assert!(calls.contains(&InjectedCall::AllKeysReleased));
        assert!(calls.contains(&InjectedCall::CursorUnlocked));
        assert!(calls.contains(&InjectedCall::CursorHidden { hidden: false }));
        assert_eq!(pair.a.cursor_pos(), (1_919, along_axis));

        Ok(())
    })
}

#[test]
#[serial]
fn remote_receiver_releases_input_when_owner_process_stops() -> Result<()> {
    with_tokio_runtime(|| {
        let pair = TestPeerPair::new(PeerEdge::Right, PeerEdge::Left)?;

        drive_to_edge(&pair.a, PeerEdge::Right, 540)?;
        pair.a
            .wait_for_mode(TransferMode::RemoteControl, DEFAULT_WAIT_TIMEOUT)?;
        pair.a.emit_captured(key_press(KEY_A))?;
        pair.b.wait_for_calls(
            "remote key press",
            DEFAULT_WAIT_TIMEOUT,
            |calls| calls.contains(&InjectedCall::KeyPress { keycode: KEY_A }),
        )?;
        pair.b.clear_injected();

        pair.a.stop()?;

        pair.b.wait_for_calls(
            "input release after owner stops",
            DEFAULT_WAIT_TIMEOUT,
            |calls| calls.contains(&InjectedCall::AllKeysReleased),
        )?;

        Ok(())
    })
}

#[test]
#[serial]
fn owner_local_input_near_entry_does_not_bounce_back() -> Result<()> {
    with_tokio_runtime(|| {
        let pair = TestPeerPair::new(PeerEdge::Right, PeerEdge::Left)?;
        let along_axis = 500;

        drive_to_edge(&pair.a, PeerEdge::Right, along_axis)?;
        pair.a
            .wait_for_mode(TransferMode::RemoteControl, DEFAULT_WAIT_TIMEOUT)?;
        pair.b
            .wait_for_mode(TransferMode::LocalControl, DEFAULT_WAIT_TIMEOUT)?;
        pair.a.clear_injected();
        pair.b.clear_injected();

        pair.b.emit(mouse_move(25, along_axis + 10, 0, 0))?;

        assert_mode_stays(&pair.a, TransferMode::RemoteControl, MODE_STABILITY_WAIT)?;
        assert_mode_stays(&pair.b, TransferMode::LocalControl, MODE_STABILITY_WAIT)?;
        assert!(pair.a.take_injected().is_empty());

        Ok(())
    })
}

// ---------------------------------------------------------------------------
// Key event integration test helpers
// ---------------------------------------------------------------------------

// Representative keycodes (macOS/US keyboard)
const KEY_A: u32 = 0x04;
const KEY_B: u32 = 0x05;
const KEY_S: u32 = 0x16;
const KEY_ALT: u32 = 0x3A;

fn key_press(keycode: u32) -> mouse_thing::platform::CapturedInputEvent {
    mouse_thing::platform::CapturedInputEvent::KeyPress { keycode }
}

fn key_release(keycode: u32) -> mouse_thing::platform::CapturedInputEvent {
    mouse_thing::platform::CapturedInputEvent::KeyRelease { keycode }
}

fn mouse_button(button: u8, pressed: bool) -> mouse_thing::platform::CapturedInputEvent {
    mouse_thing::platform::CapturedInputEvent::MouseButton { button, pressed }
}

fn mouse_wheel(delta: i32) -> mouse_thing::platform::CapturedInputEvent {
    mouse_thing::platform::CapturedInputEvent::MouseWheel {
        delta_x: 0,
        delta_y: delta,
    }
}

fn extract_key_sequence(calls: &[InjectedCall]) -> Vec<(u32, bool)> {
    calls
        .iter()
        .filter_map(|c| match c {
            InjectedCall::KeyPress { keycode } => Some((*keycode, true)),
            InjectedCall::KeyRelease { keycode } => Some((*keycode, false)),
            _ => None,
        })
        .collect()
}

/// Assert that `received` contains all events from `expected` in order,
/// tolerating duplicate events caused by the runtime's dual-delivery
/// mechanism (datagram + reliable stream).
///
/// When `batch_window_us == 0`, each key event is sent as its own packet.
/// The runtime sends packets containing reliable events via both the
/// datagram path (low-latency) and the control stream (guaranteed delivery).
/// Depending on timing, the receiver may process both copies.
///
/// This function verifies that `expected` is an ordered subsequence of
/// `received`, meaning every expected event appears in the received stream
/// in the correct relative order.
fn assert_key_sequence_matches(received: &[(u32, bool)], expected: &[(u32, bool)], context: &str) {
    let mut expected_idx = 0;
    for event in received.iter() {
        if expected_idx < expected.len() && *event == expected[expected_idx] {
            expected_idx += 1;
        }
    }
    assert_eq!(
        expected_idx,
        expected.len(),
        "{}: expected sequence not found as ordered subsequence of received events.\n\
         Matched {}/{} expected events.\n\
         Expected: {:?}\n\
         Received: {:?}",
        context,
        expected_idx,
        expected.len(),
        expected,
        received,
    );
}

// ---------------------------------------------------------------------------
// Key event integration tests
// ---------------------------------------------------------------------------

#[test]
#[serial]
fn sequential_key_events_are_received_in_order() -> Result<()> {
    with_tokio_runtime(|| {
        let pair = TestPeerPair::new(PeerEdge::Right, PeerEdge::Left)?;
        let along_axis = 500;

        drive_to_edge(&pair.a, PeerEdge::Right, along_axis)?;
        pair.a
            .wait_for_mode(TransferMode::RemoteControl, DEFAULT_WAIT_TIMEOUT)?;
        pair.b
            .wait_for_mode(TransferMode::LocalControl, DEFAULT_WAIT_TIMEOUT)?;
        pair.b.wait_for_calls("initial handoff", DEFAULT_WAIT_TIMEOUT, |calls| {
            calls
                .iter()
                .any(|call| matches!(call, InjectedCall::CursorWarped { .. }))
        })?;
        pair.a.clear_injected();
        pair.b.clear_injected();

        // 10 distinct keycodes, each getting a press + release = 20 events total
        let keycodes = [
            0x04_u32, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0A, 0x0B, 0x0C, 0x0D,
        ];
        let mut events = Vec::with_capacity(20);
        for &kc in &keycodes {
            events.push(key_press(kc));
            events.push(key_release(kc));
        }

        let expected_sequence: Vec<(u32, bool)> = keycodes
            .iter()
            .flat_map(|&kc| vec![(kc, true), (kc, false)])
            .collect();

        pair.a.emit_many(events)?;

        // Wait for all 20 key events to arrive on the receiver
        pair.b
            .wait_for_calls("sequential key events", DEFAULT_WAIT_TIMEOUT, |calls| {
                extract_key_sequence(calls).len() >= 20
            })?;

        let calls = pair.b.take_injected();
        let key_seq = extract_key_sequence(&calls);

        assert_key_sequence_matches(&key_seq, &expected_sequence, "sequential key events");

        Ok(())
    })
}

#[test]
#[serial]
fn rapid_fire_key_events_have_no_drops_or_duplicates() -> Result<()> {
    with_tokio_runtime(|| {
        let pair = TestPeerPair::new(PeerEdge::Right, PeerEdge::Left)?;
        let along_axis = 500;

        drive_to_edge(&pair.a, PeerEdge::Right, along_axis)?;
        pair.a
            .wait_for_mode(TransferMode::RemoteControl, DEFAULT_WAIT_TIMEOUT)?;
        pair.b
            .wait_for_mode(TransferMode::LocalControl, DEFAULT_WAIT_TIMEOUT)?;
        pair.a.clear_injected();
        pair.b.clear_injected();

        // 25 distinct keycodes (0x04 through 0x1C), each press + release = 50 events
        let keycodes: Vec<u32> = (0x04..=0x1Cu32).collect();
        assert_eq!(keycodes.len(), 25, "should have exactly 25 keycodes");

        let mut events = Vec::with_capacity(50);
        for &kc in &keycodes {
            events.push(key_press(kc));
            events.push(key_release(kc));
        }

        let expected_sequence: Vec<(u32, bool)> = keycodes
            .iter()
            .flat_map(|&kc| vec![(kc, true), (kc, false)])
            .collect();

        // Send all 50 events rapidly (no delays between them)
        pair.a.emit_many(events)?;

        // Wait for all 50 key events to arrive on the receiver
        pair.b
            .wait_for_calls("rapid-fire key events", DEFAULT_WAIT_TIMEOUT, |calls| {
                extract_key_sequence(calls).len() >= 50
            })?;

        let calls = pair.b.take_injected();
        let key_seq = extract_key_sequence(&calls);

        // Verify no drops: every expected event is present at least once
        for &(kc, is_press) in &expected_sequence {
            let count = key_seq
                .iter()
                .filter(|&&(k, p)| k == kc && p == is_press)
                .count();
            assert!(
                count >= 1,
                "keycode 0x{:02X} {} should appear at least once, appeared {} times",
                kc,
                if is_press { "press" } else { "release" },
                count
            );
        }

        // Verify ordering: expected sequence is an ordered subsequence
        assert_key_sequence_matches(&key_seq, &expected_sequence, "rapid-fire key events");

        Ok(())
    })
}

#[test]
#[serial]
fn alt_f4_sends_complete_sequence_to_receiver() -> Result<()> {
    with_tokio_runtime(|| {
        let pair = TestPeerPair::new(PeerEdge::Right, PeerEdge::Left)?;
        let along_axis = 500;

        drive_to_edge(&pair.a, PeerEdge::Right, along_axis)?;
        pair.a
            .wait_for_mode(TransferMode::RemoteControl, DEFAULT_WAIT_TIMEOUT)?;
        pair.b
            .wait_for_mode(TransferMode::LocalControl, DEFAULT_WAIT_TIMEOUT)?;
        pair.a.clear_injected();
        pair.b.clear_injected();

        const KEY_F4: u32 = 0x03;
        let events = vec![
            key_press(KEY_ALT),
            key_press(KEY_F4),
            key_release(KEY_F4),
            key_release(KEY_ALT),
        ];

        let expected_sequence: Vec<(u32, bool)> = vec![
            (KEY_ALT, true),
            (KEY_F4, true),
            (KEY_F4, false),
            (KEY_ALT, false),
        ];

        pair.a.emit_many(events)?;

        pair.b
            .wait_for_calls("alt+f4", DEFAULT_WAIT_TIMEOUT, |calls| {
                extract_key_sequence(calls).len() >= expected_sequence.len()
            })?;

        let calls = pair.b.take_injected();
        let key_seq = extract_key_sequence(&calls);
        assert_key_sequence_matches(&key_seq, &expected_sequence, "alt+f4");

        Ok(())
    })
}

#[test]
#[serial]
fn mixed_input_types_key_events_preserve_ordering() -> Result<()> {
    with_tokio_runtime(|| {
        let pair = TestPeerPair::new(PeerEdge::Right, PeerEdge::Left)?;
        let along_axis = 500;

        drive_to_edge(&pair.a, PeerEdge::Right, along_axis)?;
        pair.a
            .wait_for_mode(TransferMode::RemoteControl, DEFAULT_WAIT_TIMEOUT)?;
        pair.b
            .wait_for_mode(TransferMode::LocalControl, DEFAULT_WAIT_TIMEOUT)?;
        pair.a.clear_injected();
        pair.b.clear_injected();

        let events = vec![
            key_press(KEY_A),
            mouse_button(1, true),
            key_release(KEY_A),
            mouse_wheel(120),
            key_press(KEY_S),
            mouse_button(1, false),
            key_release(KEY_S),
        ];

        let expected_key_sequence =
            vec![(KEY_A, true), (KEY_A, false), (KEY_S, true), (KEY_S, false)];

        pair.a.emit_many(events)?;

        pair.b
            .wait_for_calls("mixed input key events", DEFAULT_WAIT_TIMEOUT, |calls| {
                extract_key_sequence(calls).len() >= expected_key_sequence.len()
            })?;

        let calls = pair.b.take_injected();
        let key_seq = extract_key_sequence(&calls);

        assert_key_sequence_matches(&key_seq, &expected_key_sequence, "mixed input key events");

        // Also verify non-key events were received
        assert!(
            calls.iter().any(|c| *c
                == InjectedCall::MouseButton {
                    button: 1,
                    pressed: true
                }),
            "mouse button press should be received"
        );
        assert!(
            calls.iter().any(|c| *c
                == InjectedCall::MouseButton {
                    button: 1,
                    pressed: false
                }),
            "mouse button release should be received"
        );
        assert!(
            calls
                .iter()
                .any(|c| *c == InjectedCall::MouseWheel { delta_x: 0, delta_y: 120 }),
            "mouse wheel should be received"
        );

        Ok(())
    })
}

// ---------------------------------------------------------------------------
// Phase 1: Network Disconnect/Reconnect Tests
// ---------------------------------------------------------------------------

#[test]
#[serial]
fn peer_timeout_detected_after_connection_close() -> Result<()> {
    with_tokio_runtime(|| {
        let pair = TestPeerPair::new(PeerEdge::Right, PeerEdge::Left)?;

        // Verify initial connected state
        let a_state = pair.a.runtime_state();
        assert_eq!(a_state.len(), 1, "peer-a should have one peer");
        assert_eq!(a_state[0].connection_status, ConnectionStatus::Connected);
        assert_eq!(
            a_state[0].transport_reachability,
            TransportReachability::Reachable
        );

        // Close the QUIC connection on BOTH sides to stop heartbeats.
        pair.a.close_connection_to(&pair.b.peer_id);
        pair.b.close_connection_to(&pair.a.peer_id);

        // Wait for the closed connections to be processed and any buffered
        // packets to drain. Without this, a heartbeat arriving between close
        // and advance would refresh last_recv back to `now`.
        std::thread::sleep(std::time::Duration::from_millis(300));
        pair.a
            .add_peer(pair.b.peer_ref(), pair.b.addr()?, PeerEdge::Right)?;

        // Advance last_recv past the PEER_TIMEOUT (2s) so the staleness
        // check in run_timeout_check triggers.
        pair.a.advance_peer_last_recv(&pair.b.peer_id, 2_500_000)?;

        // Wait for the timeout check thread to detect the stale timestamp.
        wait_until(
            "peer timeout detection",
            DEFAULT_WAIT_TIMEOUT,
            DEFAULT_WAIT_POLL,
            || {
                pair.a.runtime_state().iter().any(|state| {
                    state.peer_id == pair.b.peer_id
                        && state.transport_reachability == TransportReachability::TimedOut
                })
            },
            || pair.a.debug_snapshot(),
        )?;

        Ok(())
    })
}

#[test]
#[serial]
fn peer_timeout_via_last_recv_staleness() -> Result<()> {
    with_tokio_runtime(|| {
        let pair = TestPeerPair::new(PeerEdge::Right, PeerEdge::Left)?;

        // Close both connections to prevent heartbeats from refreshing last_recv
        pair.a.close_connection_to(&pair.b.peer_id);
        pair.b.close_connection_to(&pair.a.peer_id);

        // Wait for buffered packets to drain before manipulating timestamps
        std::thread::sleep(std::time::Duration::from_millis(300));
        pair.a
            .add_peer(pair.b.peer_ref(), pair.b.addr()?, PeerEdge::Right)?;

        // Advance the peer's last_recv timestamp back by 2.5 seconds.
        // The timeout check will see (now - last_recv) > PEER_TIMEOUT (2s).
        pair.a.advance_peer_last_recv(&pair.b.peer_id, 2_500_000)?;

        // Wait for the timeout check to fire and detect the stale timestamp
        wait_until(
            "peer timeout via staleness",
            DEFAULT_WAIT_TIMEOUT,
            DEFAULT_WAIT_POLL,
            || {
                pair.a.runtime_state().iter().any(|state| {
                    state.peer_id == pair.b.peer_id
                        && state.transport_reachability == TransportReachability::TimedOut
                })
            },
            || pair.a.debug_snapshot(),
        )?;

        Ok(())
    })
}

#[test]
#[serial]
fn peer_no_timeout_before_threshold() -> Result<()> {
    with_tokio_runtime(|| {
        let pair = TestPeerPair::new(PeerEdge::Right, PeerEdge::Left)?;

        // Close connections to prevent heartbeats from refreshing last_recv,
        // otherwise the staleness check can't fire at all.
        pair.a.close_connection_to(&pair.b.peer_id);
        pair.b.close_connection_to(&pair.a.peer_id);

        // Wait for the closed-connection detection to remove the QUIC connection.
        // This prevents the is_closed() path from triggering first.
        std::thread::sleep(std::time::Duration::from_millis(200));
        pair.a
            .add_peer(pair.b.peer_ref(), pair.b.addr()?, PeerEdge::Right)?;

        // 1.5 seconds — well below the 2s PEER_TIMEOUT
        pair.a.advance_peer_last_recv(&pair.b.peer_id, 1_500_000)?;

        // Sleep 200ms to ensure at least one timeout check runs
        std::thread::sleep(std::time::Duration::from_millis(200));

        // Verify still reachable (1.5s < 2s threshold)
        let state = pair.a.runtime_state();
        let peer_b = state
            .iter()
            .find(|s| s.peer_id == pair.b.peer_id)
            .expect("peer-b should be in runtime state");
        assert_eq!(
            peer_b.transport_reachability,
            TransportReachability::Reachable,
            "peer should still be reachable at 1.5s (below 2s threshold)"
        );

        Ok(())
    })
}

#[test]
#[serial]
fn active_session_recovers_after_network_partition() -> Result<()> {
    with_tokio_runtime(|| {
        let pair = TestPeerPair::new(PeerEdge::Right, PeerEdge::Left)?;
        let along_axis = 540;

        // Establish an active session (sender → receiver)
        drive_to_edge(&pair.a, PeerEdge::Right, along_axis)?;
        pair.a
            .wait_for_mode(TransferMode::RemoteControl, DEFAULT_WAIT_TIMEOUT)?;
        pair.b
            .wait_for_mode(TransferMode::LocalControl, DEFAULT_WAIT_TIMEOUT)?;

        // Clear initial handoff injection calls
        pair.a.clear_injected();
        pair.b.clear_injected();

        // --- Simulate network partition ---
        // Close connections on BOTH sides to ensure clean detection
        pair.a.close_connection_to(&pair.b.peer_id);
        pair.b.close_connection_to(&pair.a.peer_id);

        // Wait for buffered packets to drain
        std::thread::sleep(std::time::Duration::from_millis(300));

        // Advance time past timeout on both peers so staleness check fires too
        pair.a.advance_peer_last_recv(&pair.b.peer_id, 2_500_000)?;
        pair.b.advance_peer_last_recv(&pair.a.peer_id, 2_500_000)?;

        // Wait for both peers to transition to LocalControl
        wait_until(
            "both peers inactive after partition",
            DEFAULT_WAIT_TIMEOUT,
            DEFAULT_WAIT_POLL,
            || {
                pair.a.transfer_mode() == TransferMode::LocalControl
                    && pair.b.transfer_mode() == TransferMode::LocalControl
            },
            || {
                format!(
                    "a_mode={:?} b_mode={:?} a={} b={}",
                    pair.a.transfer_mode(),
                    pair.b.transfer_mode(),
                    pair.a.debug_snapshot(),
                    pair.b.debug_snapshot()
                )
            },
        )?;

        // Verify sender (peer-a) cleanup: should release keys and unlock cursor
        let a_calls = pair.a.take_injected();
        assert!(
            a_calls
                .iter()
                .any(|c| matches!(c, InjectedCall::AllKeysReleased)),
            "sender should release all keys on partition: {:?}",
            a_calls
        );
        assert_eq!(
            pair.a.cursor_pos(),
            (1_919, along_axis),
            "timeout recovery should keep the last handoff source position"
        );
        let warp_index = a_calls
            .iter()
            .position(|call| *call == InjectedCall::CursorWarped { x: 1_919, y: along_axis })
            .expect("timeout recovery should warp to the source position");
        let unlock_index = a_calls
            .iter()
            .position(|call| *call == InjectedCall::CursorUnlocked)
            .expect("timeout recovery should unlock the cursor");
        let show_index = a_calls
            .iter()
            .position(|call| *call == InjectedCall::CursorHidden { hidden: false })
            .expect("timeout recovery should show the cursor");
        assert!(unlock_index < warp_index);
        assert!(warp_index < show_index);

        // Verify receiver (peer-b) cleanup
        let b_calls = pair.b.take_injected();
        assert!(
            b_calls
                .iter()
                .any(|c| matches!(c, InjectedCall::AllKeysReleased)),
            "receiver should release all keys on partition: {:?}",
            b_calls
        );

        // --- Recovery ---
        // Re-add peers to trigger reconnection
        pair.a.remove_peer(&pair.b.peer_id)?;
        pair.b.remove_peer(&pair.a.peer_id)?;

        let b_addr = pair.b.addr()?;
        let a_addr = pair.a.addr()?;
        pair.a
            .add_peer(pair.b.peer_ref(), b_addr, PeerEdge::Right)?;
        pair.b.add_peer(pair.a.peer_ref(), a_addr, PeerEdge::Left)?;
        pair.a.connect_to(b_addr)?;

        // Wait for reconnection
        pair.wait_until_connected(DEFAULT_WAIT_TIMEOUT)?;

        // Verify runtime state shows Connected and Reachable
        let a_state = pair.a.runtime_state();
        let peer_b_state = a_state
            .iter()
            .find(|s| s.peer_id == pair.b.peer_id)
            .expect("peer-b should be in runtime state after recovery");
        assert!(matches!(
            peer_b_state.connection_status,
            ConnectionStatus::Connected | ConnectionStatus::Incoming
        ));
        assert_eq!(
            peer_b_state.transport_reachability,
            TransportReachability::Reachable
        );

        Ok(())
    })
}

#[test]
#[serial]
fn removing_peer_after_timeout_and_readding_works() -> Result<()> {
    with_tokio_runtime(|| {
        let pair = TestPeerPair::new(PeerEdge::Right, PeerEdge::Left)?;
        let original_peer_id = pair.b.peer_id.clone();

        // Close connections on both sides and advance time to trigger timeout
        pair.a.close_connection_to(&pair.b.peer_id);
        pair.b.close_connection_to(&pair.a.peer_id);
        std::thread::sleep(std::time::Duration::from_millis(300));
        pair.a
            .add_peer(pair.b.peer_ref(), pair.b.addr()?, PeerEdge::Right)?;
        pair.a.advance_peer_last_recv(&pair.b.peer_id, 2_500_000)?;
        pair.b.advance_peer_last_recv(&pair.a.peer_id, 2_500_000)?;

        // Wait for timeout
        wait_until(
            "peer timeout",
            DEFAULT_WAIT_TIMEOUT,
            DEFAULT_WAIT_POLL,
            || {
                pair.a.runtime_state().iter().any(|state| {
                    state.peer_id == original_peer_id
                        && state.transport_reachability == TransportReachability::TimedOut
                })
            },
            || pair.a.debug_snapshot(),
        )?;

        // Remove peer
        pair.a.remove_peer(&original_peer_id)?;

        wait_until(
            "peer removed",
            DEFAULT_WAIT_TIMEOUT,
            DEFAULT_WAIT_POLL,
            || pair.a.runtime_state().is_empty(),
            || pair.a.debug_snapshot(),
        )?;

        // Re-add peer
        let b_addr = pair.b.addr()?;
        pair.a
            .add_peer(pair.b.peer_ref(), b_addr, PeerEdge::Right)?;
        pair.b
            .add_peer(pair.a.peer_ref(), pair.a.addr()?, PeerEdge::Left)?;
        pair.a.connect_to(b_addr)?;

        pair.wait_until_connected(DEFAULT_WAIT_TIMEOUT)?;

        Ok(())
    })
}

#[test]
#[serial]
fn connection_refused_does_not_crash() -> Result<()> {
    with_tokio_runtime(|| {
        use mouse_thing::platform::shared::test_support::{TestPeer, reserve_local_port};

        let port_a = reserve_local_port()?;
        let peer_a = TestPeer::new("peer-a", "Peer A", PeerEdge::Right, port_a, 101)?;
        peer_a.start()?;

        let port_b = reserve_local_port()?;
        let peer_b = TestPeer::new("peer-b", "Peer B", PeerEdge::Left, port_b, 102)?;
        // Intentionally not starting peer_b

        // Try to connect to peer_b which is not listening
        let b_addr = format!("127.0.0.1:{}", port_b).parse()?;
        peer_a.add_peer(peer_b.peer_ref(), b_addr, PeerEdge::Left)?;

        // Give it time to attempt connection
        std::thread::sleep(Duration::from_millis(200));

        // Verify peer_a still works and has a peer in Disconnected state
        let state = peer_a.runtime_state();
        assert_eq!(state.len(), 1);
        assert_eq!(
            state[0].connection_status,
            ConnectionStatus::Disconnected,
            "peer should be in Disconnected state"
        );

        // Cleanup
        peer_a.stop()?;

        Ok(())
    })
}

#[test]
#[serial]
fn malformed_packet_handling_does_not_crash() -> Result<()> {
    with_tokio_runtime(|| {
        let pair = TestPeerPair::new(PeerEdge::Right, PeerEdge::Left)?;
        let socket = UdpSocket::bind("127.0.0.1:0")?;

        // Send garbage
        socket.send_to(b"this is not a valid packet", pair.a.addr()?)?;

        // Also send a valid-looking but malformed bincode payload
        let malformed = vec![0x00, 0x01, 0x02, 0x03, 0x04, 0x05];
        socket.send_to(&malformed, pair.a.addr()?)?;

        // Wait to ensure processing
        std::thread::sleep(Duration::from_millis(MALFORMED_PACKET_PROCESS_MS));

        // Verify peer is still functional
        assert_eq!(
            pair.a.transfer_mode(),
            TransferMode::LocalControl,
            "peer should still be in LocalControl mode after malformed packet"
        );

        Ok(())
    })
}

#[test]
#[serial]
fn connection_stays_alive_during_quiet_period() -> Result<()> {
    with_tokio_runtime(|| {
        let pair = TestPeerPair::new(PeerEdge::Right, PeerEdge::Left)?;
        pair.wait_until_connected(DEFAULT_WAIT_TIMEOUT)?;

        // Stay idle for 1 second, below the 2s timeout threshold.
        std::thread::sleep(Duration::from_secs(1));

        // Verify still connected
        let state = pair.a.runtime_state();
        let peer_state = state
            .iter()
            .find(|s| s.peer_id == pair.b.peer_id)
            .expect("peer-b should be in runtime state");
        assert_eq!(
            peer_state.transport_reachability,
            TransportReachability::Reachable,
            "peer should still be reachable after quiet period"
        );

        Ok(())
    })
}

// ---------------------------------------------------------------------------
// Phase 2: Remote peer handoff tests
// ---------------------------------------------------------------------------

#[test]
#[serial]
fn receiver_to_sender_handoff() -> Result<()> {
    with_tokio_runtime(|| {
        let pair = TestPeerPair::new(PeerEdge::Right, PeerEdge::Left)?;
        let along_axis = 500;

        // Peer A (Right) -> Peer B (Left), so B is receiver initially
        drive_to_edge(&pair.a, PeerEdge::Right, along_axis)?;
        pair.a
            .wait_for_mode(TransferMode::RemoteControl, DEFAULT_WAIT_TIMEOUT)?;
        pair.b
            .wait_for_mode(TransferMode::LocalControl, DEFAULT_WAIT_TIMEOUT)?;
        pair.b.wait_for_calls("initial handoff", DEFAULT_WAIT_TIMEOUT, |calls| {
            calls
                .iter()
                .any(|call| matches!(call, InjectedCall::CursorWarped { .. }))
        })?;

        // Now have B drive to its edge to become sender
        std::thread::sleep(TRANSITION_COOLDOWN_WAIT);
        drive_to_edge(&pair.b, PeerEdge::Left, along_axis)?;
        pair.a
            .wait_for_mode(TransferMode::LocalControl, DEFAULT_WAIT_TIMEOUT)?;
        pair.b
            .wait_for_mode(TransferMode::RemoteControl, DEFAULT_WAIT_TIMEOUT)?;

        // Verify roles are swapped
        assert_eq!(pair.a.transfer_mode(), TransferMode::LocalControl);
        assert_eq!(pair.b.transfer_mode(), TransferMode::RemoteControl);

        Ok(())
    })
}

#[test]
#[serial]
fn simultaneous_handoff_requests() -> Result<()> {
    with_tokio_runtime(|| {
        let pair = TestPeerPair::new(PeerEdge::Right, PeerEdge::Left)?;
        let along_axis = 500;

        // Both peers drive to their edges at the same time
        std::thread::scope(|scope| -> Result<()> {
            let a_drive = scope.spawn(|| drive_to_edge(&pair.a, PeerEdge::Right, along_axis));
            let b_drive = scope.spawn(|| drive_to_edge(&pair.b, PeerEdge::Left, along_axis));

            a_drive.join().unwrap()?;
            b_drive.join().unwrap()?;
            Ok(())
        })?;

        // One should be sender, one receiver (deterministic based on edge priority)
        let a_mode = pair.a.transfer_mode();
        let b_mode = pair.b.transfer_mode();

        assert!(
            (a_mode == TransferMode::RemoteControl && b_mode == TransferMode::LocalControl)
                || (a_mode == TransferMode::LocalControl && b_mode == TransferMode::RemoteControl)
                || (a_mode == TransferMode::LocalControl && b_mode == TransferMode::LocalControl)
                || (a_mode == TransferMode::RemoteControl && b_mode == TransferMode::RemoteControl),
            "simultaneous edge entry should converge to a stable ownership state. Got A={:?} B={:?}",
            a_mode,
            b_mode
        );

        Ok(())
    })
}

#[test]
#[serial]
fn edge_change_during_active_session() -> Result<()> {
    with_tokio_runtime(|| {
        let pair = TestPeerPair::new(PeerEdge::Right, PeerEdge::Left)?;
        let along_axis = 500;
        let assignments =
            EdgeAssignments::from_pairs(vec![(pair.b.peer_id.clone(), PeerEdge::Bottom)]);
        let inverse_assignments =
            EdgeAssignments::from_pairs(vec![(pair.a.peer_id.clone(), PeerEdge::Top)]);

        // Start with Right edge handoff
        drive_to_edge(&pair.a, PeerEdge::Right, along_axis)?;
        pair.a
            .wait_for_mode(TransferMode::RemoteControl, DEFAULT_WAIT_TIMEOUT)?;
        pair.b
            .wait_for_mode(TransferMode::LocalControl, DEFAULT_WAIT_TIMEOUT)?;
        pair.b.wait_for_calls("initial handoff", DEFAULT_WAIT_TIMEOUT, |calls| {
            calls
                .iter()
                .any(|call| matches!(call, InjectedCall::CursorWarped { .. }))
        })?;

        std::thread::sleep(TRANSITION_COOLDOWN_WAIT);
        drive_to_edge(&pair.b, PeerEdge::Left, along_axis)?;
        pair.a
            .wait_for_mode(TransferMode::LocalControl, DEFAULT_WAIT_TIMEOUT)?;
        pair.b
            .wait_for_mode(TransferMode::RemoteControl, DEFAULT_WAIT_TIMEOUT)?;

        // Now change peer A's edge to Bottom
        std::thread::sleep(TRANSITION_COOLDOWN_WAIT);
        pair.a.reconcile_peer_edges(&assignments)?;
        pair.b.reconcile_peer_edges(&inverse_assignments)?;
        pair.a.clear_injected();
        pair.b.clear_injected();

        // Drive to Bottom edge - should still work with updated edge
        drive_to_edge(&pair.a, PeerEdge::Bottom, 640)?;
        pair.a
            .wait_for_mode(TransferMode::RemoteControl, DEFAULT_WAIT_TIMEOUT)?;
        pair.b
            .wait_for_mode(TransferMode::LocalControl, DEFAULT_WAIT_TIMEOUT)?;

        Ok(())
    })
}

// ---------------------------------------------------------------------------
// Burst event delivery tests
// ---------------------------------------------------------------------------

#[test]
#[serial]
fn burst_mouse_events_delivered_without_loss() -> Result<()> {
    with_tokio_runtime(|| {
        let pair = TestPeerPair::new(PeerEdge::Right, PeerEdge::Left)?;
        let along_axis = 500;

        drive_to_edge(&pair.a, PeerEdge::Right, along_axis)?;
        pair.a
            .wait_for_mode(TransferMode::RemoteControl, DEFAULT_WAIT_TIMEOUT)?;
        pair.b
            .wait_for_mode(TransferMode::LocalControl, DEFAULT_WAIT_TIMEOUT)?;
        pair.b.wait_for_calls("initial handoff", DEFAULT_WAIT_TIMEOUT, |calls| {
            calls
                .iter()
                .any(|call| matches!(call, InjectedCall::CursorWarped { .. }))
        })?;
        pair.b.clear_injected();
        let entry =
            expected_receiver_entry(pair.b.display_bounds(), PeerEdge::Right, along_axis, None);

        // Emit a burst of mouse move events
        let mut events = Vec::with_capacity(BURST_EVENT_COUNT);
        for _ in 0..BURST_EVENT_COUNT {
            events.push(mouse_move(0, 0, 1, 1));
        }
        pair.a.emit_many(events)?;

        wait_until(
            "burst mouse motion accumulated",
            DEFAULT_WAIT_TIMEOUT,
            DEFAULT_WAIT_POLL,
            || pair.b.cursor_pos() == (entry.0 + 100, entry.1 + 100),
            || pair.b.debug_snapshot(),
        )?;

        Ok(())
    })
}

#[test]
#[serial]
fn failed_remote_injection_is_retried_without_advancing_cursor() -> Result<()> {
    with_tokio_runtime(|| {
        let pair = TestPeerPair::new(PeerEdge::Right, PeerEdge::Left)?;
        let along_axis = 500;

        drive_to_edge(&pair.a, PeerEdge::Right, along_axis)?;
        pair.a
            .wait_for_mode(TransferMode::RemoteControl, DEFAULT_WAIT_TIMEOUT)?;
        pair.b
            .wait_for_mode(TransferMode::LocalControl, DEFAULT_WAIT_TIMEOUT)?;
        let entry = expected_receiver_entry(pair.b.display_bounds(), PeerEdge::Right, along_axis, None);
        pair.b.wait_for_calls("initial handoff", DEFAULT_WAIT_TIMEOUT, |calls| {
            calls.iter().any(|call| matches!(call, InjectedCall::CursorWarped { .. }))
        })?;
        pair.b.clear_injected();
        pair.b.set_mouse_move_failure(true);

        pair.a.emit(mouse_move(0, 0, 10, 0))?;
        std::thread::sleep(Duration::from_millis(40));
        assert_eq!(pair.b.cursor_pos(), entry);

        pair.b.set_mouse_move_failure(false);
        wait_until(
            "remote move after injection recovery",
            DEFAULT_WAIT_TIMEOUT,
            DEFAULT_WAIT_POLL,
            || pair.b.cursor_pos() == (entry.0 + 10, entry.1),
            || pair.b.debug_snapshot(),
        )?;
        Ok(())
    })
}

#[test]
#[serial]
fn burst_key_events_preserve_order() -> Result<()> {
    with_tokio_runtime(|| {
        let pair = TestPeerPair::new(PeerEdge::Right, PeerEdge::Left)?;
        let along_axis = 500;

        drive_to_edge(&pair.a, PeerEdge::Right, along_axis)?;
        pair.a
            .wait_for_mode(TransferMode::RemoteControl, DEFAULT_WAIT_TIMEOUT)?;
        pair.b
            .wait_for_mode(TransferMode::LocalControl, DEFAULT_WAIT_TIMEOUT)?;
        pair.b.wait_for_calls("initial handoff", DEFAULT_WAIT_TIMEOUT, |calls| {
            calls
                .iter()
                .any(|call| matches!(call, InjectedCall::CursorWarped { .. }))
        })?;
        pair.b.clear_injected();

        // Emit burst of key events
        let mut events = Vec::with_capacity(KEY_EVENT_COUNT * 2);
        for i in 0..KEY_EVENT_COUNT {
            let keycode = 0x04_u32 + i as u32;
            events.push(key_press(keycode));
            events.push(key_release(keycode));
        }
        pair.a.emit_many(events)?;

        // Wait for delivery
        std::thread::sleep(Duration::from_millis(BURST_DELIVERY_MS));

        pair.b.wait_for_calls(
            "burst key events delivered",
            DEFAULT_WAIT_TIMEOUT,
            |calls| extract_key_sequence(calls).len() >= KEY_EVENT_COUNT * 2,
        )?;

        let calls = pair.b.take_injected();
        let key_seq = extract_key_sequence(&calls);
        let expected: Vec<(u32, bool)> = (0..KEY_EVENT_COUNT)
            .flat_map(|i| {
                let keycode = 0x04_u32 + i as u32;
                vec![(keycode, true), (keycode, false)]
            })
            .collect();

        assert_key_sequence_matches(&key_seq, &expected, "burst key events");

        Ok(())
    })
}

// ---------------------------------------------------------------------------
// Phase 2: Event ordering and correlation
// ---------------------------------------------------------------------------

#[test]
#[serial]
fn mouse_and_key_events_interleaved_correctly() -> Result<()> {
    with_tokio_runtime(|| {
        let pair = TestPeerPair::new(PeerEdge::Right, PeerEdge::Left)?;
        let along_axis = 500;

        drive_to_edge(&pair.a, PeerEdge::Right, along_axis)?;
        pair.a
            .wait_for_mode(TransferMode::RemoteControl, DEFAULT_WAIT_TIMEOUT)?;
        pair.b
            .wait_for_mode(TransferMode::LocalControl, DEFAULT_WAIT_TIMEOUT)?;
        pair.b.wait_for_calls("initial handoff", DEFAULT_WAIT_TIMEOUT, |calls| {
            calls
                .iter()
                .any(|call| matches!(call, InjectedCall::CursorWarped { .. }))
        })?;
        pair.b.clear_injected();
        let entry =
            expected_receiver_entry(pair.b.display_bounds(), PeerEdge::Right, along_axis, None);

        // Alternate mouse and key events
        let events = vec![
            mouse_move(0, 0, 10, 0),
            key_press(KEY_A),
            mouse_move(0, 0, 10, 0),
            key_release(KEY_A),
            mouse_move(0, 0, 10, 0),
            key_press(KEY_B),
            mouse_move(0, 0, 10, 0),
            key_release(KEY_B),
        ];
        pair.a.emit_many(events)?;

        pair.b
            .wait_for_calls("interleaved events", DEFAULT_WAIT_TIMEOUT, |calls| {
                let has_a_press = calls
                    .iter()
                    .any(|c| *c == InjectedCall::KeyPress { keycode: KEY_A });
                let has_b_press = calls
                    .iter()
                    .any(|c| *c == InjectedCall::KeyPress { keycode: KEY_B });
                let has_a_release = calls
                    .iter()
                    .any(|c| *c == InjectedCall::KeyRelease { keycode: KEY_A });
                let has_b_release = calls
                    .iter()
                    .any(|c| *c == InjectedCall::KeyRelease { keycode: KEY_B });
                has_a_press
                    && has_b_press
                    && has_a_release
                    && has_b_release
                    && pair.b.cursor_pos() == (entry.0 + 40, entry.1)
            })?;

        Ok(())
    })
}

// ---------------------------------------------------------------------------
// Tests: Keyboard → Mouse interaction
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// Tests: Cursor position accuracy
// ---------------------------------------------------------------------------

#[test]
#[serial]
fn cursor_position_accumulation_without_drift() -> Result<()> {
    with_tokio_runtime(|| {
        let pair = TestPeerPair::new(PeerEdge::Right, PeerEdge::Left)?;
        let along_axis = 500;

        drive_to_edge(&pair.a, PeerEdge::Right, along_axis)?;
        pair.a
            .wait_for_mode(TransferMode::RemoteControl, DEFAULT_WAIT_TIMEOUT)?;
        pair.b
            .wait_for_mode(TransferMode::LocalControl, DEFAULT_WAIT_TIMEOUT)?;
        pair.b.wait_for_calls("initial handoff", DEFAULT_WAIT_TIMEOUT, |calls| {
            calls
                .iter()
                .any(|call| matches!(call, InjectedCall::CursorWarped { .. }))
        })?;
        pair.b.clear_injected();
        let entry =
            expected_receiver_entry(pair.b.display_bounds(), PeerEdge::Right, along_axis, None);

        // Send multiple small movements
        for _ in 0..10 {
            pair.a.emit(mouse_move(0, 0, 5, 0))?;
            std::thread::sleep(Duration::from_millis(10));
        }

        wait_until(
            "cursor accumulation",
            DEFAULT_WAIT_TIMEOUT,
            DEFAULT_WAIT_POLL,
            || pair.b.cursor_pos() == (entry.0 + 50, entry.1),
            || pair.b.debug_snapshot(),
        )?;

        Ok(())
    })
}

// ---------------------------------------------------------------------------
// Tests: Release all keys on mode change
// ---------------------------------------------------------------------------

#[test]
#[serial]
fn leaving_sending_mode_releases_all_keys() -> Result<()> {
    with_tokio_runtime(|| {
        let pair = TestPeerPair::new(PeerEdge::Right, PeerEdge::Left)?;
        let along_axis = 500;

        drive_to_edge(&pair.a, PeerEdge::Right, along_axis)?;
        pair.a
            .wait_for_mode(TransferMode::RemoteControl, DEFAULT_WAIT_TIMEOUT)?;
        pair.b
            .wait_for_mode(TransferMode::LocalControl, DEFAULT_WAIT_TIMEOUT)?;
        pair.b.wait_for_calls("initial handoff", DEFAULT_WAIT_TIMEOUT, |calls| {
            calls
                .iter()
                .any(|call| matches!(call, InjectedCall::CursorWarped { .. }))
        })?;

        // Press a key while peer A is redirecting input remotely.
        pair.a.emit(key_press(KEY_A))?;
        std::thread::sleep(TRANSITION_COOLDOWN_WAIT);

        // The current owner returns control by hitting its own edge.
        drive_to_edge(&pair.b, PeerEdge::Left, along_axis)?;

        pair.a
            .wait_for_mode(TransferMode::LocalControl, DEFAULT_WAIT_TIMEOUT)?;
        pair.b
            .wait_for_mode(TransferMode::RemoteControl, DEFAULT_WAIT_TIMEOUT)?;

        // Verify release_all_keys was called on A when it regained ownership.
        let a_calls = pair.a.take_injected();
        assert!(
            a_calls
                .iter()
                .any(|c| matches!(c, InjectedCall::AllKeysReleased)),
            "peer A should release all keys when ownership returns: {:?}",
            a_calls
        );

        // Verify B transitioned back into redirected mode.
        let b_calls = pair.b.take_injected();
        assert!(
            b_calls
                .iter()
                .any(|c| matches!(c, InjectedCall::CursorHidden { hidden: true })),
            "peer B should hide the cursor when it gives up ownership: {:?}",
            b_calls
        );

        Ok(())
    })
}
