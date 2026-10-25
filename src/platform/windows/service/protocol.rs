use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

pub const PROTOCOL_VERSION: u8 = 1;
pub const PIPE_NAME: &str = r"\\.\pipe\zerog-input";
pub const SERVICE_NAME: &str = "zerog-input";
pub const SERVICE_DISPLAY_NAME: &str = "ZeroG Input Service";
pub const MAX_FRAME_BYTES: usize = 4096;

/// Upper bound on simultaneous pipe instances. The service only ever serves the
/// single application process (plus brief reconnect probes), so a small cap
/// keeps a stuck or hostile client from pinning unlimited server threads.
pub const MAX_PIPE_INSTANCES: u32 = 4;

/// DACL for the input pipe: full access for the service account (SYSTEM) and
/// read/write for exactly one user, the owner of the session the service
/// injects into.
///
/// `AU` (Authenticated Users) must never be used here. It grants every local
/// account access to the pipe, which lets any of them drive input injection.
pub fn pipe_security_descriptor(user_sid: &str) -> String {
    format!("D:(A;;GA;;;SY)(A;;GRGW;;;{user_sid})")
}

pub fn pipe_name() -> String {
    std::env::var("ZEROG_INPUT_PIPE")
        .ok()
        .filter(|p| !p.trim().is_empty())
        .unwrap_or_else(|| PIPE_NAME.to_string())
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Envelope {
    pub version: u8,
    pub marker: u64,
    pub op: InputOp,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum InputOp {
    MouseMove { x: i32, y: i32 },
    MouseMoveRelative { dx: i32, dy: i32 },
    MouseButton { button: u8, pressed: bool },
    MouseWheel { delta_x: i32, delta_y: i32 },
    KeyPress { keycode: u32 },
    KeyRelease { keycode: u32 },
    ReleaseAll,
    Ping,
}

pub fn encode_frame(envelope: &Envelope) -> Result<Vec<u8>> {
    let payload = bincode::serialize(envelope).context("encode input envelope")?;
    if payload.len() > MAX_FRAME_BYTES {
        bail!("input envelope exceeds {MAX_FRAME_BYTES} bytes");
    }
    let mut frame = Vec::with_capacity(4 + payload.len());
    frame.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    frame.extend_from_slice(&payload);
    Ok(frame)
}

pub fn decode_frame(buffer: &[u8]) -> Result<(Envelope, usize)> {
    if buffer.len() < 4 {
        bail!("truncated input frame header");
    }
    let len = u32::from_le_bytes([buffer[0], buffer[1], buffer[2], buffer[3]]) as usize;
    if len > MAX_FRAME_BYTES {
        bail!("input frame exceeds {MAX_FRAME_BYTES} bytes");
    }
    if buffer.len() < 4 + len {
        bail!("truncated input frame payload");
    }
    let envelope: Envelope =
        bincode::deserialize(&buffer[4..4 + len]).context("decode input envelope")?;
    if envelope.version != PROTOCOL_VERSION {
        bail!("unsupported input protocol version {}", envelope.version);
    }
    Ok((envelope, 4 + len))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn round_trip(op: InputOp) {
        let envelope = Envelope {
            version: PROTOCOL_VERSION,
            marker: 0xA5A5_5A5A_1234_5678,
            op,
        };
        let frame = encode_frame(&envelope).unwrap();
        let (decoded, consumed) = decode_frame(&frame).unwrap();
        assert_eq!(decoded, envelope);
        assert_eq!(consumed, frame.len());
    }

    #[test]
    fn all_ops_round_trip() {
        round_trip(InputOp::MouseMove { x: -1920, y: 540 });
        round_trip(InputOp::MouseMoveRelative { dx: 4, dy: -7 });
        round_trip(InputOp::MouseButton {
            button: 3,
            pressed: true,
        });
        round_trip(InputOp::MouseWheel {
            delta_x: 120,
            delta_y: -240,
        });
        round_trip(InputOp::KeyPress { keycode: 0x04 });
        round_trip(InputOp::KeyRelease { keycode: 0xE0 });
        round_trip(InputOp::ReleaseAll);
        round_trip(InputOp::Ping);
    }

    #[test]
    fn pipe_descriptor_grants_only_system_and_the_session_user() {
        let sddl = pipe_security_descriptor("S-1-5-21-1-2-3-1001");
        assert!(
            sddl.contains("(A;;GA;;;SY)"),
            "service account keeps access: {sddl}"
        );
        assert!(
            sddl.contains("(A;;GRGW;;;S-1-5-21-1-2-3-1001)"),
            "session user gets read/write: {sddl}"
        );
        assert!(
            !sddl.contains(";;;AU)"),
            "authenticated users must not reach the pipe: {sddl}"
        );
        assert!(
            !sddl.contains(";;;WD)"),
            "everyone must not reach the pipe: {sddl}"
        );
    }

    #[test]
    fn decode_rejects_truncated_and_foreign_frames() {
        assert!(decode_frame(&[1, 2, 3]).is_err());
        let envelope = Envelope {
            version: PROTOCOL_VERSION,
            marker: 1,
            op: InputOp::Ping,
        };
        let mut frame = encode_frame(&envelope).unwrap();
        assert!(decode_frame(&frame[..frame.len() - 1]).is_err());
        frame[4] = 0xFF;
        assert!(decode_frame(&frame).is_err());
        let foreign = Envelope {
            version: PROTOCOL_VERSION + 1,
            marker: 1,
            op: InputOp::Ping,
        };
        let foreign_frame = encode_frame(&foreign).unwrap();
        assert!(decode_frame(&foreign_frame).is_err());
    }
}
