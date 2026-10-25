//! Network event processing for injection.

pub(crate) fn process_network_event_internal(
    injection: &dyn crate::platform::EventInjection,
    cursor: Option<std::sync::Arc<crate::platform::shared::cursor_state::CursorState>>,
    event: super::Event,
) -> anyhow::Result<()> {
    use super::Event;
    match event {
        Event::MouseMove { .. } => Ok(()),
        Event::MouseButton { button, pressed } => injection.inject_mouse_button(button, pressed),
        Event::MouseWheel { delta_y, delta_x } => injection.inject_mouse_wheel(delta_x, delta_y),
        Event::KeyPress { keycode } => injection.inject_key_press(keycode),
        Event::KeyRelease { keycode } => injection.inject_key_release(keycode),
        Event::TakeControl {
            edge: _,
            initial_x,
            initial_y,
            receiver_position_bp: _,
            motion_session_id: _,
            return_motion: _,
            caps_lock: _,
        } => process_legacy_take_control(injection, cursor, initial_x, initial_y),
        Event::TakeControlAck { .. } => Ok(()),
        Event::ReleaseControl => {
            if let Some(state) = cursor.as_ref() {
                state.transition_to_local_control(injection, None)?;
            }
            Ok(())
        }
        Event::Heartbeat { .. }
        | Event::Ping { .. }
        | Event::Pong { .. }
        | Event::LayoutUpdate(_)
        | Event::ConnectionRequest { .. }
        | Event::ConnectionAccept { .. }
        | Event::ConnectionReject { .. }
        | Event::DisconnectNotify { .. }
        | Event::UnpairNotify { .. }
        | Event::PairingPending { .. }
        | Event::PairingApproved { .. }
        | Event::ClipboardText { .. }
        | Event::ClipboardAck { .. }
        | Event::ClipboardSyncRequest { .. }
        | Event::FileOffer { .. }
        | Event::FileAccept { .. }
        | Event::FileReject { .. }
        | Event::FileChunk { .. }
        | Event::FileAck { .. }
        | Event::FileCancel { .. } => Ok(()),
    }
}

fn process_legacy_take_control(
    injection: &dyn crate::platform::EventInjection,
    cursor: Option<std::sync::Arc<crate::platform::shared::cursor_state::CursorState>>,
    initial_x: i32,
    initial_y: i32,
) -> anyhow::Result<()> {
    if let Some(state) = cursor.as_ref() {
        state.transition_to_local_control(injection, Some((initial_x, initial_y)))?;
        return Ok(());
    }
    injection.warp_cursor(initial_x, initial_y)
}

#[cfg(test)]
mod process_network_event_tests {
    use super::process_network_event_internal;
    use crate::events::Event;
    use crate::platform::{EventInjection, PeerEdge};
    use anyhow::Result;
    use std::sync::Mutex;

    #[derive(Default)]
    struct MockInjection {
        key_presses: Mutex<Vec<u32>>,
        key_releases: Mutex<Vec<u32>>,
        warp_calls: Mutex<Vec<(i32, i32)>>,
        keys_released: Mutex<bool>,
    }

    impl EventInjection for MockInjection {
        fn inject_mouse_move(&self, _x: i32, _y: i32) -> Result<()> {
            Ok(())
        }

        fn inject_mouse_button(&self, _button: u8, _pressed: bool) -> Result<()> {
            Ok(())
        }

        fn inject_mouse_wheel(&self, _delta_x: i32, _delta_y: i32) -> Result<()> {
            Ok(())
        }

        fn inject_key_press(&self, keycode: u32) -> Result<()> {
            self.key_presses
                .lock()
                .expect("key_presses lock poisoned")
                .push(keycode);
            Ok(())
        }

        fn inject_key_release(&self, keycode: u32) -> Result<()> {
            self.key_releases
                .lock()
                .expect("key_releases lock poisoned")
                .push(keycode);
            Ok(())
        }

        fn set_cursor_hidden(&self, _hidden: bool) -> Result<()> {
            Ok(())
        }

        fn warp_cursor(&self, x: i32, y: i32) -> Result<()> {
            self.warp_calls
                .lock()
                .expect("warp_calls lock poisoned")
                .push((x, y));
            Ok(())
        }

        fn lock_cursor_position(&self) -> Result<()> {
            Ok(())
        }

        fn unlock_cursor_position(&self) -> Result<()> {
            Ok(())
        }

        fn release_all_keys(&self) -> Result<()> {
            *self
                .keys_released
                .lock()
                .expect("keys_released lock poisoned") = true;
            Ok(())
        }
    }

    #[test]
    fn process_network_event_routes_keyboard_events() {
        let injection = MockInjection::default();

        process_network_event_internal(&injection, None, Event::KeyPress { keycode: 0x04 })
            .expect("key press handling should succeed");
        process_network_event_internal(&injection, None, Event::KeyRelease { keycode: 0x04 })
            .expect("key release handling should succeed");

        assert_eq!(
            *injection
                .key_presses
                .lock()
                .expect("key_presses lock poisoned"),
            vec![0x04]
        );
        assert_eq!(
            *injection
                .key_releases
                .lock()
                .expect("key_releases lock poisoned"),
            vec![0x04]
        );
    }

    #[test]
    fn process_network_event_handles_control_events_without_transition_state() {
        let injection = MockInjection::default();

        process_network_event_internal(
            &injection,
            None,
            Event::TakeControl {
                edge: PeerEdge::Right,
                initial_x: 12,
                initial_y: 34,
                receiver_position_bp: None,
                motion_session_id: 1,
                return_motion: None,
                caps_lock: None,
            },
        )
        .expect("take control should succeed");

        assert_eq!(
            *injection
                .warp_calls
                .lock()
                .expect("warp_calls lock poisoned"),
            vec![(12, 34)]
        );

        process_network_event_internal(&injection, None, Event::ReleaseControl)
            .expect("release control should succeed");
    }
}
