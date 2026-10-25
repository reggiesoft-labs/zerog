//! Wire protocol events for peer-to-peer communication.
//!
//! This module defines the event types serialized and sent between
//! mouse-thing peers. Events represent mouse movements, button clicks, keyboard
//! input, and connection management messages.
//!
//! # Wire Format
//!
//! Events are wrapped in [`Packet`] structures containing:
//! - Protocol version for compatibility checking
//! - Sender ID for packet deduplication
//! - Sequence number for ordering
//! - Monotonic timestamp for latency measurement
//! - Batched events for efficiency
//!
//! All serialization uses `bincode` via serde for compact binary encoding.

use serde::{Deserialize, Serialize};

use crate::core::platform::{DisplayBounds, PeerEdge};

/// Current protocol version. Peers with mismatched versions will reject packets.
pub const PROTOCOL_VERSION: u8 = 13;

/// Current transfer state of a peer in the connection.
///
/// Used in heartbeat messages to communicate ownership state.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub enum PeerState {
    /// Peer owns its local cursor.
    LocalControl,
    /// Peer is under remote control from another peer.
    RemoteControl,
}

impl PeerState {
}

/// A single peer's position in the visual layout grid.
///
/// Coordinates are relative to the local peer's position at (0, 0). Negative
/// values indicate positions to the left or above the local peer.
///
/// This is the canonical placement type used across config persistence,
/// API snapshots, and wire protocol events.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LayoutPlacement {
    /// Stable identity of the peer at this position.
    #[serde(alias = "peer_name")]
    pub peer_id: String,
    /// Last-known label of the peer at this position.
    #[serde(default)]
    pub peer_label: String,
    /// Horizontal offset from local peer (negative = left).
    pub x: i32,
    /// Vertical offset from local peer (negative = above).
    pub y: i32,
}

/// Complete layout state synchronized between peers.
///
/// Peers exchange layout updates to build a consistent view of the display
/// topology. The version clock and editor ID enable conflict resolution
/// when multiple peers edit the layout simultaneously.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LayoutUpdateEvent {
    /// Monotonically increasing version number for conflict resolution.
    pub version: u64,
    /// Unique identifier of the peer that last edited the layout.
    pub editor_id: u64,
    /// Milliseconds since UNIX epoch when this update was created.
    pub timestamp: u64,
    /// All known peer positions in the layout.
    pub placements: Vec<LayoutPlacement>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub struct MotionCheckpoint {
    pub session_id: u64,
    pub total_dx: i64,
    pub total_dy: i64,
}

/// Input and control events transmitted between peers.
///
/// Events are batched into packets for efficient transmission. Input events
/// (mouse, keyboard) are only sent when actively controlling a remote peer.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Event {
    /// Relative mouse movement while another peer owns the cursor.
    MouseMove {
        /// Relative horizontal movement since last event.
        dx: i32,
        /// Relative vertical movement since last event.
        dy: i32,
        total_dx: i64,
        total_dy: i64,
        /// Identifier for the ownership handoff that owns this motion.
        motion_session_id: u64,
    },
    /// Mouse button press or release.
    MouseButton {
        /// Logical button index (0=left, 1=right, 2=middle, 3=X1/side, 4=X2/extra).
        button: u8,
        /// True if pressed, false if released.
        pressed: bool,
    },
    /// Mouse wheel scroll.
    ///
    /// Scroll deltas are normalized to 120 units per standard wheel notch across platforms.
    MouseWheel {
        /// Vertical scroll delta in standardized units (120 = 1 notch; positive = scroll up/away, negative = scroll down).
        #[serde(alias = "delta")]
        delta_y: i32,
        /// Horizontal scroll delta in standardized units (120 = 1 notch; positive = scroll right, negative = scroll left).
        #[serde(default)]
        delta_x: i32,
    },
    /// Keyboard key press.
    KeyPress {
        /// Platform-specific keycode.
        keycode: u32,
    },
    /// Keyboard key release.
    KeyRelease {
        /// Platform-specific keycode.
        keycode: u32,
    },
    /// Request to take control of the remote peer's input.
    TakeControl {
        /// The edge the sender is transitioning from.
        edge: PeerEdge,
        /// Legacy initial mouse Y position retained for wire compatibility.
        initial_y: i32,
        /// Legacy initial mouse X position retained for wire compatibility.
        initial_x: i32,
        /// Optional entry position along the receiver edge in basis points.
        ///
        /// The receiver uses this contact-scoped value for cursor placement.
        #[serde(default)]
        receiver_position_bp: Option<u16>,
        /// Identifier shared by this handoff and its subsequent motion.
        #[serde(default)]
        motion_session_id: u64,
        #[serde(default)]
        return_motion: Option<MotionCheckpoint>,
        /// Caps Lock state of the sender at the moment of handoff.
        ///
        /// The receiver applies this state so both machines (and the physical
        /// keyboard LED on the sender) agree on Caps Lock for the session.
        #[serde(default)]
        caps_lock: Option<bool>,
    },
    TakeControlAck {
        request_seq: u32,
        accepted: bool,
    },
    /// Release control of the remote peer's input.
    ReleaseControl,
    /// Periodic heartbeat with peer state for connection monitoring.
    Heartbeat {
        /// Current state of the sending peer.
        state: PeerState,
    },
    /// Latency measurement request.
    Ping {
        /// Unique identifier to match with Pong response.
        nonce: u64,
        /// Monotonic timestamp when ping was sent (microseconds).
        sent_mono_us: u64,
    },
    /// Latency measurement response.
    Pong {
        /// Nonce from the matching Ping request.
        nonce: u64,
        /// Monotonic timestamp from the Ping (echoed back).
        echo_mono_us: u64,
    },
    /// Layout topology update.
    LayoutUpdate(LayoutUpdateEvent),
    /// Legacy packet handshake request retained for wire decoding.
    ConnectionRequest {
        /// Stable identity of the peer requesting connection.
        peer_id: String,
        /// Display name of the peer requesting connection.
        peer_name: String,
        /// Bounds of the sender's virtual desktop for offset-aware cursor transitions.
        /// None if connecting from a pre-Phase 6 peer.
        display_bounds: Option<DisplayBounds>,
        /// Individual display rectangles that make up the sender's desktop shape.
        display_rects: Vec<DisplayBounds>,
    },
    /// Legacy packet handshake acceptance retained for wire decoding.
    ConnectionAccept {
        /// Stable identity of the accepting peer.
        peer_id: String,
        /// Display name of the accepting peer.
        peer_name: String,
        /// Bounds of the acceptor's virtual desktop for offset-aware cursor transitions.
        /// None if connecting from a pre-Phase 6 peer.
        display_bounds: Option<DisplayBounds>,
        /// Individual display rectangles that make up the acceptor's desktop shape.
        display_rects: Vec<DisplayBounds>,
    },
    /// Legacy packet handshake rejection retained for wire decoding.
    ConnectionReject {
        /// Stable identity of the rejecting peer.
        peer_id: String,
        /// Display name of the rejecting peer.
        peer_name: String,
        /// Human-readable reason for rejection.
        reason: String,
    },
    /// Notify peer of disconnection.
    DisconnectNotify {
        /// Stable identity of the disconnecting peer.
        peer_id: String,
        /// Display name of the disconnecting peer.
        peer_name: String,
    },
    /// Notify peer of unpairing.
    UnpairNotify {
        /// Stable identity of the unpairing peer.
        peer_id: String,
        /// Display name of the unpairing peer.
        peer_name: String,
    },
    /// Informs the connecting peer that pairing approval is pending on this device.
    PairingPending {
        peer_id: String,
    },
    /// Informs the connecting peer that pairing approval was granted by this device.
    PairingApproved {
        peer_id: String,
    },
    /// Clipboard text content from remote peer.
    ///
    /// Protocol behavior:
    /// - Reliable transport required (TCP-like delivery)
    /// - Idempotent: multiple deliveries of same content have no additional effect
    /// - Order not critical relative to other events
    ClipboardText {
        /// The clipboard text content.
        content: String,
        /// Sequence number for ACK-based deduplication.
        sequence: u64,
        /// SHA-256 truncated hash (first 64 bits) of content for integrity verification.
        content_hash: u64,
        /// Timestamp when this clipboard was originally set (milliseconds since epoch).
        timestamp: u64,
    },
    /// Acknowledgment for clipboard receipt.
    ClipboardAck {
        /// Sequence number being acknowledged.
        sequence: u64,
        /// Hash of the received content for verification.
        received_hash: u64,
    },
    /// Request clipboard content from peer.
    ClipboardSyncRequest {
        /// Unique identifier for this sync request.
        request_id: u64,
    },
    FileOffer {
        manifest: crate::core::transfer::FileManifest,
    },
    FileAccept {
        transfer_id: String,
    },
    FileReject {
        transfer_id: String,
        reason: String,
    },
    FileChunk {
        chunk: crate::core::transfer::FileChunkPayload,
    },
    FileAck {
        transfer_id: String,
        received_bytes: u64,
    },
    FileCancel {
        transfer_id: String,
    },
}

#[derive(Debug, thiserror::Error)]
pub enum ClipboardError {
    #[error("Clipboard content too large: {size} bytes (max {max})")]
    ContentTooLarge { size: usize, max: usize },
    #[error("Hash verification failed")]
    HashMismatch,
    #[error("Timestamp verification failed - stale content rejected")]
    StaleContent { local_ts: u64, remote_ts: u64 },
}

/// Wire packet containing batched events.
///
/// Packets are the unit of transmission over the wire. Each packet includes
/// metadata for deduplication, ordering, and latency measurement.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Packet {
    /// Protocol version for compatibility checking.
    pub version: u8,
    /// Unique identifier of the sending peer.
    pub sender_id: u64,
    /// Sequence number for ordering and duplicate detection.
    pub seq: u32,
    /// Monotonic timestamp when packet was created (microseconds).
    pub t_mono_us: u64,
    /// Batched events to deliver.
    pub events: Vec<Event>,
}

mod clipboard_sequence;
pub use clipboard_sequence::ClipboardSequence;

#[cfg(test)]
mod wire_format_tests {
    use super::*;
    use crate::core::platform::{DisplayBounds, PeerEdge};
    use crate::core::test_bytes::ByteFuzzer;
    use sha2::{Digest, Sha256};

    /// Number of variants in [`Event`]. Bumping this is only correct when a
    /// variant is added to both the enum and [`canonical_packet`].
    const EVENT_VARIANT_COUNT: usize = 28;

    /// Stable label for every [`Event`] variant.
    ///
    /// The match is deliberately exhaustive so adding a variant fails to
    /// compile here, forcing the golden snapshot to be reviewed.
    fn variant_label(event: &Event) -> &'static str {
        match event {
            Event::MouseMove { .. } => "MouseMove",
            Event::MouseButton { .. } => "MouseButton",
            Event::MouseWheel { .. } => "MouseWheel",
            Event::KeyPress { .. } => "KeyPress",
            Event::KeyRelease { .. } => "KeyRelease",
            Event::TakeControl { .. } => "TakeControl",
            Event::TakeControlAck { .. } => "TakeControlAck",
            Event::ReleaseControl => "ReleaseControl",
            Event::Heartbeat { .. } => "Heartbeat",
            Event::Ping { .. } => "Ping",
            Event::Pong { .. } => "Pong",
            Event::LayoutUpdate(_) => "LayoutUpdate",
            Event::ConnectionRequest { .. } => "ConnectionRequest",
            Event::ConnectionAccept { .. } => "ConnectionAccept",
            Event::ConnectionReject { .. } => "ConnectionReject",
            Event::DisconnectNotify { .. } => "DisconnectNotify",
            Event::UnpairNotify { .. } => "UnpairNotify",
            Event::PairingPending { .. } => "PairingPending",
            Event::PairingApproved { .. } => "PairingApproved",
            Event::ClipboardText { .. } => "ClipboardText",
            Event::ClipboardAck { .. } => "ClipboardAck",
            Event::ClipboardSyncRequest { .. } => "ClipboardSyncRequest",
            Event::FileOffer { .. } => "FileOffer",
            Event::FileAccept { .. } => "FileAccept",
            Event::FileReject { .. } => "FileReject",
            Event::FileChunk { .. } => "FileChunk",
            Event::FileAck { .. } => "FileAck",
            Event::FileCancel { .. } => "FileCancel",
        }
    }

    fn bounds() -> DisplayBounds {
        DisplayBounds {
            x: -1920,
            y: 1080,
            width: 2560,
            height: 1440,
        }
    }

    /// A packet containing exactly one of every [`Event`] variant with fixed
    /// values, so its serialization is byte-for-byte reproducible.
    fn canonical_packet() -> Packet {
        let events = vec![
            Event::MouseMove {
                dx: 11,
                dy: -22,
                total_dx: 333,
                total_dy: -444,
                motion_session_id: 55,
            },
            Event::MouseButton {
                button: 2,
                pressed: true,
            },
            Event::MouseWheel {
                delta_y: -120,
                delta_x: 240,
            },
            Event::KeyPress { keycode: 0x0007_0004 },
            Event::KeyRelease { keycode: 0x0007_0004 },
            Event::TakeControl {
                edge: PeerEdge::Right,
                initial_y: 540,
                initial_x: 1280,
                receiver_position_bp: Some(5_000),
                motion_session_id: 55,
                return_motion: Some(MotionCheckpoint {
                    session_id: 44,
                    total_dx: -7,
                    total_dy: 8,
                }),
                caps_lock: Some(true),
            },
            Event::TakeControlAck {
                request_seq: 9,
                accepted: false,
            },
            Event::ReleaseControl,
            Event::Heartbeat {
                state: PeerState::RemoteControl,
            },
            Event::Ping {
                nonce: 0x1122_3344_5566_7788,
                sent_mono_us: 123_456,
            },
            Event::Pong {
                nonce: 0x1122_3344_5566_7788,
                echo_mono_us: 123_000,
            },
            Event::LayoutUpdate(LayoutUpdateEvent {
                version: 3,
                editor_id: 0xDEAD_BEEF,
                timestamp: 1_700_000_000_123,
                placements: vec![
                    LayoutPlacement {
                        peer_id: "peer-a".to_string(),
                        peer_label: "Desk".to_string(),
                        x: 0,
                        y: 0,
                    },
                    LayoutPlacement {
                        peer_id: "peer-b".to_string(),
                        peer_label: "Laptop".to_string(),
                        x: 1,
                        y: -1,
                    },
                ],
            }),
            Event::ConnectionRequest {
                peer_id: "peer-a".to_string(),
                peer_name: "Desk".to_string(),
                display_bounds: Some(bounds()),
                display_rects: vec![bounds()],
            },
            Event::ConnectionAccept {
                peer_id: "peer-b".to_string(),
                peer_name: "Laptop".to_string(),
                display_bounds: Some(bounds()),
                display_rects: vec![bounds()],
            },
            Event::ConnectionReject {
                peer_id: "peer-b".to_string(),
                peer_name: "Laptop".to_string(),
                reason: "busy".to_string(),
            },
            Event::DisconnectNotify {
                peer_id: "peer-a".to_string(),
                peer_name: "Desk".to_string(),
            },
            Event::UnpairNotify {
                peer_id: "peer-a".to_string(),
                peer_name: "Desk".to_string(),
            },
            Event::PairingPending {
                peer_id: "peer-a".to_string(),
            },
            Event::PairingApproved {
                peer_id: "peer-a".to_string(),
            },
            Event::ClipboardText {
                content: "hello".to_string(),
                sequence: 12,
                content_hash: 0xABCD_EF01_2345_6789,
                timestamp: 1_700_000_000_456,
            },
            Event::ClipboardAck {
                sequence: 12,
                received_hash: 0xABCD_EF01_2345_6789,
            },
            Event::ClipboardSyncRequest { request_id: 77 },
            Event::FileOffer {
                manifest: crate::core::transfer::FileManifest {
                    transfer_id: "tx-1".to_string(),
                    sender_peer_id: "peer-a".to_string(),
                    sender_name: "Desk".to_string(),
                    target_peer_id: Some("peer-b".to_string()),
                    files: vec![crate::core::transfer::FileItem {
                        id: "f-1".to_string(),
                        name: "test.txt".to_string(),
                        size: 4,
                    }],
                    total_size: 4,
                },
            },
            Event::FileAccept {
                transfer_id: "tx-1".to_string(),
            },
            Event::FileReject {
                transfer_id: "tx-1".to_string(),
                reason: "cancelled".to_string(),
            },
            Event::FileChunk {
                chunk: crate::core::transfer::FileChunkPayload {
                    transfer_id: "tx-1".to_string(),
                    file_id: "f-1".to_string(),
                    offset: 0,
                    data: vec![1, 2, 3, 4],
                    is_last: true,
                },
            },
            Event::FileAck {
                transfer_id: "tx-1".to_string(),
                received_bytes: 4,
            },
            Event::FileCancel {
                transfer_id: "tx-1".to_string(),
            },
        ];

        Packet {
            version: PROTOCOL_VERSION,
            sender_id: 0x0102_0304_0506_0708,
            seq: 42,
            t_mono_us: 1_700_000_000_000_000,
            events,
        }
    }

    #[test]
    fn canonical_packet_covers_every_event_variant_exactly_once() {
        let events = canonical_packet().events;
        assert_eq!(
            events.len(),
            EVENT_VARIANT_COUNT,
            "the canonical packet must contain every Event variant; update \
             EVENT_VARIANT_COUNT and add the new variant"
        );

        let mut labels: Vec<&'static str> = events.iter().map(variant_label).collect();
        labels.sort_unstable();
        labels.dedup();
        assert_eq!(
            labels.len(),
            EVENT_VARIANT_COUNT,
            "the canonical packet must not repeat an Event variant"
        );
    }

    #[test]
    fn peer_state_and_edge_wire_shapes_are_locked() {
        // bincode encodes enum variants as a little-endian u32 index. These
        // constants are wire-visible: reordering an enum silently changes them.
        assert_eq!(
            bincode::serialize(&PeerState::LocalControl).unwrap(),
            vec![0, 0, 0, 0]
        );
        assert_eq!(
            bincode::serialize(&PeerState::RemoteControl).unwrap(),
            vec![1, 0, 0, 0]
        );
        assert_eq!(bincode::serialize(&PeerEdge::Left).unwrap(), vec![0, 0, 0, 0]);
        assert_eq!(bincode::serialize(&PeerEdge::Right).unwrap(), vec![1, 0, 0, 0]);
        assert_eq!(bincode::serialize(&PeerEdge::Top).unwrap(), vec![2, 0, 0, 0]);
        assert_eq!(
            bincode::serialize(&PeerEdge::Bottom).unwrap(),
            vec![3, 0, 0, 0]
        );
    }

    #[test]
    fn wire_format_matches_the_golden_snapshot() {
        let bytes = bincode::serialize(&canonical_packet()).expect("packet should serialize");
        let digest = Sha256::digest(&bytes);
        let hex: String = digest.iter().map(|byte| format!("{byte:02x}")).collect();

        assert_eq!(
            hex, "ff99386fff7715265fdddfecc7eb39a68934418f7be5e4c44d98183a94d5090d",
            "wire format changed ({} bytes). If this is intentional, bump \
             PROTOCOL_VERSION and update the golden digest here.",
            bytes.len()
        );
    }

    #[test]
    fn arbitrary_packet_bytes_never_panic() {
        let mut fuzzer = ByteFuzzer::new(0x5EED_1234);
        for _ in 0..5_000 {
            let len = fuzzer.len(4096);
            let candidate = fuzzer.bytes(len);
            let _ = bincode::deserialize::<Packet>(&candidate);
        }
    }

    #[test]
    fn every_truncation_of_a_valid_packet_decodes_without_panicking() {
        let valid = bincode::serialize(&canonical_packet()).expect("packet should serialize");
        for end in 0..valid.len() {
            let _ = bincode::deserialize::<Packet>(&valid[..end]);
        }
    }

    #[test]
    fn single_byte_corruptions_never_panic() {
        let valid = bincode::serialize(&canonical_packet()).expect("packet should serialize");
        for index in 0..valid.len() {
            for mask in [0x01u8, 0x80, 0xFF] {
                let mut corrupted = valid.clone();
                corrupted[index] ^= mask;
                let _ = bincode::deserialize::<Packet>(&corrupted);
            }
        }
    }
}
