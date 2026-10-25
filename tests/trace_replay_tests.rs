use anyhow::Result;
use mouse_thing::core::platform::types::CapturedInputEvent;
use mouse_thing::platform::shared::test_support::{
    DEFAULT_WAIT_TIMEOUT, InjectedCall, TestPeerPair, drive_to_edge, with_tokio_runtime,
};
use mouse_thing::platform::{PeerEdge, TransferMode};
use serde::Deserialize;
use serial_test::serial;
use std::fs;
use std::path::Path;

const DEFAULT_ALONG_AXIS: i32 = 500;

#[derive(Debug, Deserialize)]
struct TraceFixture {
    source_os: String,
    #[allow(dead_code)]
    description: String,
    events: Vec<TraceEvent>,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum TraceEvent {
    MouseMove {
        x: i32,
        y: i32,
        dx: f64,
        dy: f64,
        locally_delivered: bool,
    },
    MouseButton {
        button: u8,
        pressed: bool,
    },
    MouseWheel {
        delta_x: i32,
        delta_y: i32,
    },
    KeyPress {
        keycode: u32,
    },
    KeyRelease {
        keycode: u32,
    },
}

impl From<TraceEvent> for CapturedInputEvent {
    fn from(event: TraceEvent) -> Self {
        match event {
            TraceEvent::MouseMove {
                x,
                y,
                dx,
                dy,
                locally_delivered,
            } => CapturedInputEvent::MouseMove {
                x,
                y,
                dx,
                dy,
                locally_delivered,
            },
            TraceEvent::MouseButton { button, pressed } => {
                CapturedInputEvent::MouseButton { button, pressed }
            }
            TraceEvent::MouseWheel { delta_x, delta_y } => {
                CapturedInputEvent::MouseWheel { delta_x, delta_y }
            }
            TraceEvent::KeyPress { keycode } => CapturedInputEvent::KeyPress { keycode },
            TraceEvent::KeyRelease { keycode } => CapturedInputEvent::KeyRelease { keycode },
        }
    }
}

fn load_fixture(name: &str) -> Result<TraceFixture> {
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    let fixture_path = Path::new(manifest_dir)
        .join("tests")
        .join("fixtures")
        .join("traces")
        .join(name);
    let content = fs::read_to_string(fixture_path)?;
    let fixture: TraceFixture = serde_json::from_str(&content)?;
    Ok(fixture)
}

#[test]
#[serial]
fn macos_trace_replays_normalized_wheel_and_buttons() -> Result<()> {
    with_tokio_runtime(|| {
        let fixture = load_fixture("macos_input_trace.json")?;
        assert_eq!(fixture.source_os, "macos");

        let pair = TestPeerPair::new(PeerEdge::Right, PeerEdge::Left)?;
        drive_to_edge(&pair.a, PeerEdge::Right, DEFAULT_ALONG_AXIS)?;
        pair.a
            .wait_for_mode(TransferMode::RemoteControl, DEFAULT_WAIT_TIMEOUT)?;
        pair.b
            .wait_for_mode(TransferMode::LocalControl, DEFAULT_WAIT_TIMEOUT)?;
        pair.b.clear_injected();

        let captured: Vec<CapturedInputEvent> = fixture.events.into_iter().map(Into::into).collect();
        pair.a.emit_many(captured)?;

        pair.b
            .wait_for_calls("macos trace injected calls", DEFAULT_WAIT_TIMEOUT, |calls| {
                let has_button_down = calls
                    .iter()
                    .any(|call| *call == InjectedCall::MouseButton { button: 0, pressed: true });
                let has_button_up = calls
                    .iter()
                    .any(|call| *call == InjectedCall::MouseButton { button: 0, pressed: false });
                let has_wheel = calls
                    .iter()
                    .any(|call| *call == InjectedCall::MouseWheel { delta_x: -60, delta_y: 120 });
                has_button_down && has_button_up && has_wheel
            })?;

        let calls = pair.b.take_injected();
        assert!(calls.contains(&InjectedCall::MouseButton { button: 0, pressed: true }));
        assert!(calls.contains(&InjectedCall::MouseButton { button: 0, pressed: false }));
        assert!(calls.contains(&InjectedCall::MouseWheel { delta_x: -60, delta_y: 120 }));

        Ok(())
    })
}

#[test]
#[serial]
fn windows_trace_replays_multi_button_and_negative_wheel() -> Result<()> {
    with_tokio_runtime(|| {
        let fixture = load_fixture("windows_input_trace.json")?;
        assert_eq!(fixture.source_os, "windows");

        let pair = TestPeerPair::new(PeerEdge::Right, PeerEdge::Left)?;
        drive_to_edge(&pair.a, PeerEdge::Right, DEFAULT_ALONG_AXIS)?;
        pair.a
            .wait_for_mode(TransferMode::RemoteControl, DEFAULT_WAIT_TIMEOUT)?;
        pair.b
            .wait_for_mode(TransferMode::LocalControl, DEFAULT_WAIT_TIMEOUT)?;
        pair.b.clear_injected();

        let captured: Vec<CapturedInputEvent> = fixture.events.into_iter().map(Into::into).collect();
        pair.a.emit_many(captured)?;

        pair.b
            .wait_for_calls("windows trace injected calls", DEFAULT_WAIT_TIMEOUT, |calls| {
                let has_right = calls
                    .iter()
                    .any(|call| *call == InjectedCall::MouseButton { button: 1, pressed: true });
                let has_middle = calls
                    .iter()
                    .any(|call| *call == InjectedCall::MouseButton { button: 2, pressed: true });
                let has_x1 = calls
                    .iter()
                    .any(|call| *call == InjectedCall::MouseButton { button: 3, pressed: true });
                let has_x2 = calls
                    .iter()
                    .any(|call| *call == InjectedCall::MouseButton { button: 4, pressed: true });
                let has_wheel = calls
                    .iter()
                    .any(|call| *call == InjectedCall::MouseWheel { delta_x: 0, delta_y: -120 });
                has_right && has_middle && has_x1 && has_x2 && has_wheel
            })?;

        let calls = pair.b.take_injected();
        assert!(calls.contains(&InjectedCall::MouseButton { button: 1, pressed: true }));
        assert!(calls.contains(&InjectedCall::MouseButton { button: 2, pressed: true }));
        assert!(calls.contains(&InjectedCall::MouseButton { button: 3, pressed: true }));
        assert!(calls.contains(&InjectedCall::MouseButton { button: 4, pressed: true }));
        assert!(calls.contains(&InjectedCall::MouseWheel { delta_x: 0, delta_y: -120 }));

        Ok(())
    })
}

#[test]
#[serial]
fn edge_handoff_trace_replays_keys_and_buttons() -> Result<()> {
    with_tokio_runtime(|| {
        let fixture = load_fixture("edge_handoff_trace.json")?;

        let pair = TestPeerPair::new(PeerEdge::Right, PeerEdge::Left)?;
        drive_to_edge(&pair.a, PeerEdge::Right, DEFAULT_ALONG_AXIS)?;
        pair.a
            .wait_for_mode(TransferMode::RemoteControl, DEFAULT_WAIT_TIMEOUT)?;
        pair.b
            .wait_for_mode(TransferMode::LocalControl, DEFAULT_WAIT_TIMEOUT)?;
        pair.b.clear_injected();

        let captured: Vec<CapturedInputEvent> = fixture.events.into_iter().map(Into::into).collect();
        pair.a.emit_many(captured)?;

        pair.b
            .wait_for_calls("edge handoff trace injected calls", DEFAULT_WAIT_TIMEOUT, |calls| {
                let has_key_down = calls
                    .iter()
                    .any(|call| *call == InjectedCall::KeyPress { keycode: 30 });
                let has_key_up = calls
                    .iter()
                    .any(|call| *call == InjectedCall::KeyRelease { keycode: 30 });
                let has_button_down = calls
                    .iter()
                    .any(|call| *call == InjectedCall::MouseButton { button: 0, pressed: true });
                let has_button_up = calls
                    .iter()
                    .any(|call| *call == InjectedCall::MouseButton { button: 0, pressed: false });
                has_key_down && has_key_up && has_button_down && has_button_up
            })?;

        let calls = pair.b.take_injected();
        assert!(calls.contains(&InjectedCall::KeyPress { keycode: 30 }));
        assert!(calls.contains(&InjectedCall::KeyRelease { keycode: 30 }));
        assert!(calls.contains(&InjectedCall::MouseButton { button: 0, pressed: true }));
        assert!(calls.contains(&InjectedCall::MouseButton { button: 0, pressed: false }));

        Ok(())
    })
}
