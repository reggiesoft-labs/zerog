use anyhow::Result;
use mouse_thing::core::platform::traits::ClipboardManager;
use mouse_thing::platform::shared::test_support::{
    reserve_local_port, wait_until, with_tokio_runtime, TestPeerPair, DEFAULT_WAIT_POLL,
    DEFAULT_WAIT_TIMEOUT,
};
use mouse_thing::platform::PeerEdge;
use std::time::Duration;

#[test]
fn test_full_clipboard_round_trip() -> Result<()> {
    with_tokio_runtime(|| {
        let pair = TestPeerPair::new(PeerEdge::Right, PeerEdge::Left)?;

        // Peer A sets clipboard to "Hello from A".
        pair.a.clipboard().set_clipboard_text("Hello from A")?;

        // Wait until Peer B's clipboard matches "Hello from A".
        wait_until(
            "Peer B clipboard sync from A",
            DEFAULT_WAIT_TIMEOUT,
            DEFAULT_WAIT_POLL,
            || pair.b.clipboard().get_clipboard_text().unwrap() == Some("Hello from A".to_string()),
            || {
                format!(
                    "B clipboard: {:?}",
                    pair.b.clipboard().get_clipboard_text().unwrap()
                )
            },
        )?;

        // Wait a bit to ensure Peer B's poll loop has consumed the suppression from the remote update
        std::thread::sleep(Duration::from_millis(100));

        // Peer B sets clipboard to "Hello from B".
        pair.b.clipboard().set_clipboard_text("Hello from B")?;

        // Wait until Peer A's clipboard matches "Hello from B".
        wait_until(
            "Peer A clipboard sync from B",
            DEFAULT_WAIT_TIMEOUT,
            DEFAULT_WAIT_POLL,
            || pair.a.clipboard().get_clipboard_text().unwrap() == Some("Hello from B".to_string()),
            || {
                format!(
                    "A clipboard: {:?}",
                    pair.a.clipboard().get_clipboard_text().unwrap()
                )
            },
        )?;

        // Verify set_clipboard_text call counts.
        // Peer A should have: 1 (local set) + 1 (sync from B) = 2
        // Peer B should have: 1 (sync from A) + 1 (local set) = 2
        // We tolerate slightly more due to potential echoes or multiple deliveries.
        let cb_a = pair.a.clipboard();
        let calls_a = cb_a.state.set_calls.lock().unwrap();
        let cb_b = pair.b.clipboard();
        let calls_b = cb_b.state.set_calls.lock().unwrap();
        assert!(
            calls_a.len() >= 2,
            "Peer A should have at least 2 set calls, got: {:?}",
            *calls_a
        );
        assert!(
            calls_b.len() >= 2,
            "Peer B should have at least 2 set calls, got: {:?}",
            *calls_b
        );

        Ok(())
    })
}

#[test]
fn test_bidirectional_simultaneous() -> Result<()> {
    with_tokio_runtime(|| {
        let pair = TestPeerPair::new(PeerEdge::Right, PeerEdge::Left)?;

        // Peer A sets "Alpha" and Peer B sets "Beta" at the same time.
        pair.a.clipboard().set_clipboard_text("Alpha")?;
        pair.b.clipboard().set_clipboard_text("Beta")?;

        // Verify that they don't enter an infinite echo loop and eventually settle.
        // One of them will win, but they must agree.
        wait_until(
            "simultaneous clipboard settle",
            Duration::from_secs(5),
            DEFAULT_WAIT_POLL,
            || {
                let val_a = pair.a.clipboard().get_clipboard_text().unwrap();
                let val_b = pair.b.clipboard().get_clipboard_text().unwrap();
                val_a.is_some() && val_a == val_b
            },
            || {
                format!(
                    "A: {:?}, B: {:?}",
                    pair.a.clipboard().get_clipboard_text().unwrap(),
                    pair.b.clipboard().get_clipboard_text().unwrap()
                )
            },
        )?;

        // Allow some time for any potential echoes to settle
        std::thread::sleep(Duration::from_millis(500));

        let val_a = pair.a.clipboard().get_clipboard_text().unwrap();
        let val_b = pair.b.clipboard().get_clipboard_text().unwrap();
        assert_eq!(val_a, val_b);

        // Verify no infinite loop: call counts should be small.
        assert!(
            pair.a.clipboard().state.set_calls.lock().unwrap().len() < 10,
            "Too many set calls on A: {:?}",
            *pair.a.clipboard().state.set_calls.lock().unwrap()
        );
        assert!(
            pair.b.clipboard().state.set_calls.lock().unwrap().len() < 10,
            "Too many set calls on B: {:?}",
            *pair.b.clipboard().state.set_calls.lock().unwrap()
        );

        Ok(())
    })
}

#[test]
fn test_rapid_clipboard_changes() -> Result<()> {
    with_tokio_runtime(|| {
        let pair = TestPeerPair::new(PeerEdge::Right, PeerEdge::Left)?;

        // Peer A updates clipboard 10 times in quick succession.
        for i in 1..=10 {
            pair.a.clipboard().set_clipboard_text(&format!("v{}", i))?;
            // Small delay to ensure they are processed as separate changes by the poll loop
            std::thread::sleep(Duration::from_millis(60));
        }

        // Verify that Peer B eventually receives "v10".
        wait_until(
            "Peer B rapid clipboard sync",
            Duration::from_secs(5),
            DEFAULT_WAIT_POLL,
            || pair.b.clipboard().get_clipboard_text().unwrap() == Some("v10".to_string()),
            || {
                format!(
                    "B clipboard: {:?}",
                    pair.b.clipboard().get_clipboard_text().unwrap()
                )
            },
        )?;

        Ok(())
    })
}

#[test]
fn test_empty_clipboard_handling() -> Result<()> {
    with_tokio_runtime(|| {
        let pair = TestPeerPair::new(PeerEdge::Right, PeerEdge::Left)?;

        // Peer A sets non-empty content, wait for sync.
        pair.a.clipboard().set_clipboard_text("Initial")?;
        wait_until(
            "Initial sync",
            DEFAULT_WAIT_TIMEOUT,
            DEFAULT_WAIT_POLL,
            || pair.b.clipboard().get_clipboard_text().unwrap() == Some("Initial".to_string()),
            || {
                format!(
                    "B clipboard: {:?}",
                    pair.b.clipboard().get_clipboard_text().unwrap()
                )
            },
        )?;

        // Peer A clears clipboard.
        pair.a.clipboard().clear_clipboard()?;

        // Current behavior: Clearing clipboard is ignored by ClipboardSync (poll_clipboard returns on Ok(None)).
        // So B should still have "Initial".
        std::thread::sleep(Duration::from_millis(200));
        assert_eq!(
            pair.b.clipboard().get_clipboard_text().unwrap(),
            Some("Initial".to_string())
        );

        Ok(())
    })
}

#[test]
fn test_ack_timeout_retries_without_new_local_change() -> Result<()> {
    with_tokio_runtime(|| {
        let pair = TestPeerPair::new(PeerEdge::Right, PeerEdge::Left)?;

        // Simulate a dropped ACK path by forcing receiver clipboard apply to fail.
        pair.b.set_clipboard_set_failure(true);

        // A produces one local clipboard update.
        pair.a
            .clipboard()
            .set_clipboard_text("Content requiring retry")?;

        // Wait past test ACK timeout (200ms) and allow state transition.
        std::thread::sleep(Duration::from_millis(400));

        // Receiver recovers, so a retry would now succeed.
        pair.b.set_clipboard_set_failure(false);

        wait_until(
            "Peer B sync after ACK timeout without second local set",
            DEFAULT_WAIT_TIMEOUT,
            DEFAULT_WAIT_POLL,
            || {
                pair.b.clipboard().get_clipboard_text().unwrap()
                    == Some("Content requiring retry".to_string())
            },
            || {
                let a_calls = pair.a.clipboard().state.set_calls.lock().unwrap().clone();
                let b_calls = pair.b.clipboard().state.set_calls.lock().unwrap().clone();
                format!(
                    "A_clipboard={:?} B_clipboard={:?} A_set_calls={:?} B_set_calls={:?}",
                    pair.a.clipboard().get_clipboard_text().unwrap(),
                    pair.b.clipboard().get_clipboard_text().unwrap(),
                    a_calls,
                    b_calls,
                )
            },
        )?;

        Ok(())
    })
}

#[test]
fn test_delayed_ack_allows_follow_up_update() -> Result<()> {
    with_tokio_runtime(|| {
        let pair = TestPeerPair::new(PeerEdge::Right, PeerEdge::Left)?;

        pair.b.set_clipboard_set_failure(true);
        pair.a.clipboard().set_clipboard_text("delayed-ack-1")?;

        std::thread::sleep(Duration::from_millis(320));
        pair.b.set_clipboard_set_failure(false);

        wait_until(
            "first clipboard update eventually delivered after delayed ACK",
            Duration::from_secs(4),
            DEFAULT_WAIT_POLL,
            || {
                pair.b.clipboard().get_clipboard_text().unwrap()
                    == Some("delayed-ack-1".to_string())
            },
            || {
                format!(
                    "A={:?} B={:?}",
                    pair.a.clipboard().get_clipboard_text().unwrap(),
                    pair.b.clipboard().get_clipboard_text().unwrap()
                )
            },
        )?;

        pair.a.clipboard().set_clipboard_text("delayed-ack-2")?;
        wait_until(
            "follow-up clipboard update after delayed ACK",
            Duration::from_secs(4),
            DEFAULT_WAIT_POLL,
            || {
                pair.b.clipboard().get_clipboard_text().unwrap()
                    == Some("delayed-ack-2".to_string())
            },
            || {
                format!(
                    "A={:?} B={:?}",
                    pair.a.clipboard().get_clipboard_text().unwrap(),
                    pair.b.clipboard().get_clipboard_text().unwrap()
                )
            },
        )?;

        Ok(())
    })
}

#[test]
fn test_partition_reconnect_resyncs_clipboard() -> Result<()> {
    with_tokio_runtime(|| {
        let pair = TestPeerPair::new(PeerEdge::Right, PeerEdge::Left)?;

        pair.a.clipboard().set_clipboard_text("before-partition")?;
        wait_until(
            "baseline sync before partition",
            DEFAULT_WAIT_TIMEOUT,
            DEFAULT_WAIT_POLL,
            || {
                pair.b.clipboard().get_clipboard_text().unwrap()
                    == Some("before-partition".to_string())
            },
            || format!("B={:?}", pair.b.clipboard().get_clipboard_text().unwrap()),
        )?;

        pair.b.stop()?;
        std::thread::sleep(Duration::from_millis(250));

        pair.a.clipboard().set_clipboard_text("during-disconnect")?;

        let port_b = reserve_local_port()?;
        let b2 = mouse_thing::platform::shared::test_support::TestPeer::new(
            "peer-b",
            "Peer B",
            PeerEdge::Left,
            port_b,
            303,
        )?;
        b2.start()?;
        let b2_addr = b2.addr()?;
        pair.a.add_peer(b2.peer_ref(), b2_addr, PeerEdge::Right)?;
        b2.add_peer(pair.a.peer_ref(), pair.a.addr()?, PeerEdge::Left)?;

        wait_until(
            "clipboard re-sync after reconnect",
            Duration::from_secs(6),
            DEFAULT_WAIT_POLL,
            || {
                b2.clipboard().get_clipboard_text().unwrap()
                    == Some("during-disconnect".to_string())
            },
            || {
                format!(
                    "A={:?} B2={:?}",
                    pair.a.clipboard().get_clipboard_text().unwrap(),
                    b2.clipboard().get_clipboard_text().unwrap()
                )
            },
        )?;

        b2.clipboard().set_clipboard_text("post-reconnect")?;
        wait_until(
            "bidirectional sync after reconnect",
            Duration::from_secs(4),
            DEFAULT_WAIT_POLL,
            || {
                pair.a.clipboard().get_clipboard_text().unwrap()
                    == Some("post-reconnect".to_string())
            },
            || {
                format!(
                    "A={:?} B2={:?}",
                    pair.a.clipboard().get_clipboard_text().unwrap(),
                    b2.clipboard().get_clipboard_text().unwrap()
                )
            },
        )?;

        Ok(())
    })
}

#[test]
fn test_hash_mismatch_rejected() -> Result<()> {
    with_tokio_runtime(|| {
        let pair = TestPeerPair::new(PeerEdge::Right, PeerEdge::Left)?;

        // Establish sync
        pair.a.clipboard().set_clipboard_text("Initial")?;
        wait_until(
            "Initial sync",
            DEFAULT_WAIT_TIMEOUT,
            DEFAULT_WAIT_POLL,
            || pair.b.clipboard().get_clipboard_text().unwrap() == Some("Initial".to_string()),
            || {
                format!(
                    "B clipboard: {:?}",
                    pair.b.clipboard().get_clipboard_text().unwrap()
                )
            },
        )?;

        // Manually inject a ClipboardText event from Peer A to Peer B with a corrupted hash.
        let corrupted_event = mouse_thing::core::events::Event::ClipboardText {
            content: "Corrupted".to_string(),
            sequence: 999,
            content_hash: 12345, // Wrong hash
            timestamp: 999999999,
        };

        // We need to send it specifically to B. send_events broadcasts to all peers.
        pair.a.send_events(vec![corrupted_event])?;

        // Wait a bit to ensure it would have been processed if it was valid.
        std::thread::sleep(Duration::from_millis(200));

        // Verify Peer B still has "Initial".
        assert_eq!(
            pair.b.clipboard().get_clipboard_text().unwrap(),
            Some("Initial".to_string())
        );

        Ok(())
    })
}

#[test]
fn test_large_clipboard_content() -> Result<()> {
    with_tokio_runtime(|| {
        let pair = TestPeerPair::new(PeerEdge::Right, PeerEdge::Left)?;

        // 64KB payload (fragmentation test).
        // Note: Larger payloads (~1MB) currently hit QUIC stream flow control
        // and polling limitations in the test environment.
        let large_content = "A".repeat(64 * 1024);

        pair.a.clipboard().set_clipboard_text(&large_content)?;

        wait_until(
            "Large clipboard sync",
            Duration::from_secs(5),
            Duration::from_millis(100),
            || pair.b.clipboard().get_clipboard_text().unwrap() == Some(large_content.clone()),
            || {
                let current = pair.b.clipboard().get_clipboard_text().unwrap();
                format!("B clipboard length: {:?}", current.map(|s| s.len()))
            },
        )?;

        Ok(())
    })
}

#[test]
fn test_peer_disconnect_clears_state() -> Result<()> {
    with_tokio_runtime(|| {
        let pair = TestPeerPair::new(PeerEdge::Right, PeerEdge::Left)?;

        pair.a.clipboard().set_clipboard_text("Before disconnect")?;
        wait_until(
            "Sync before disconnect",
            DEFAULT_WAIT_TIMEOUT,
            DEFAULT_WAIT_POLL,
            || {
                pair.b.clipboard().get_clipboard_text().unwrap()
                    == Some("Before disconnect".to_string())
            },
            || {
                format!(
                    "B clipboard: {:?}",
                    pair.b.clipboard().get_clipboard_text().unwrap()
                )
            },
        )?;

        // Simulate Peer B disconnect by stopping it.
        // This will send DisconnectNotify to A.
        pair.b.stop()?;

        // Wait for A to process DisconnectNotify
        std::thread::sleep(Duration::from_millis(200));

        // Now Peer A's ClipboardSync should be Idle.
        // Reconnect and verify sync still works.
        // We need to start B again and re-add peers because stop() cleaned up.
        // Actually, creating a new pair or just restarting B might be complicated.
        // Let's just use a new Peer B.

        let port_b = reserve_local_port()?;
        let b2 = mouse_thing::platform::shared::test_support::TestPeer::new(
            "peer-b",
            "Peer B",
            PeerEdge::Left,
            port_b,
            203,
        )?;
        b2.start()?;
        let b2_addr = b2.addr()?;
        pair.a.add_peer(b2.peer_ref(), b2_addr, PeerEdge::Right)?;
        b2.add_peer(pair.a.peer_ref(), pair.a.addr()?, PeerEdge::Left)?;

        pair.a.clipboard().set_clipboard_text("After reconnect")?;

        wait_until(
            "Sync after reconnect",
            DEFAULT_WAIT_TIMEOUT,
            DEFAULT_WAIT_POLL,
            || b2.clipboard().get_clipboard_text().unwrap() == Some("After reconnect".to_string()),
            || {
                format!(
                    "B2 clipboard: {:?}",
                    b2.clipboard().get_clipboard_text().unwrap()
                )
            },
        )?;

        Ok(())
    })
}

#[test]
fn test_clipboard_sharing_toggle() -> Result<()> {
    with_tokio_runtime(|| {
        let pair = TestPeerPair::new(PeerEdge::Right, PeerEdge::Left)?;

        let layout_a_disabled = mouse_thing::core::api::ConfigSnapshot {
            share_clipboard: false,
            ..Default::default()
        };
        pair.a.reconcile_layout(&layout_a_disabled)?;

        pair.a.clipboard().set_clipboard_text("Secret text")?;
        std::thread::sleep(Duration::from_millis(200));

        assert_ne!(
            pair.b.clipboard().get_clipboard_text().unwrap(),
            Some("Secret text".to_string())
        );

        let layout_a_enabled = mouse_thing::core::api::ConfigSnapshot {
            share_clipboard: true,
            ..Default::default()
        };
        pair.a.reconcile_layout(&layout_a_enabled)?;

        pair.a.clipboard().set_clipboard_text("Public text")?;

        wait_until(
            "Peer B receives Public text after clipboard sharing enabled",
            DEFAULT_WAIT_TIMEOUT,
            DEFAULT_WAIT_POLL,
            || pair.b.clipboard().get_clipboard_text().unwrap() == Some("Public text".to_string()),
            || {
                format!(
                    "B clipboard: {:?}",
                    pair.b.clipboard().get_clipboard_text().unwrap()
                )
            },
        )?;

        Ok(())
    })
}
