//! Local event relay for loopback testing.
//!
//! This module provides a simple event loop that receives captured events
//! and immediately re-injects them into the local system. This is used
//! to verify that capture and injection are working correctly without
//! involving the network.

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use tokio::sync::mpsc::Receiver;
use tokio::sync::mpsc::error::TryRecvError;
use tracing::debug;

use crate::platform::{CapturedInputEvent, EventInjection};

/// Relays events from the receiver to the injection subsystem until shutdown.
pub(crate) fn relay_events_until_shutdown(
    event_rx: &mut Receiver<CapturedInputEvent>,
    injection: &dyn EventInjection,
    shutdown: &AtomicBool,
) {
    while !shutdown.load(Ordering::SeqCst) {
        match event_rx.try_recv() {
            Ok(CapturedInputEvent::MouseMove { x, y, .. }) => {
                if let Err(err) = injection.inject_mouse_move(x, y) {
                    debug!(error = %err, "failed to inject loopback mouse move");
                }
            }
            Ok(CapturedInputEvent::MouseButton { button, pressed }) => {
                if let Err(err) = injection.inject_mouse_button(button, pressed) {
                    debug!(error = %err, "failed to inject loopback mouse button");
                }
            }
            Ok(CapturedInputEvent::MouseWheel { delta_x, delta_y }) => {
                if let Err(err) = injection.inject_mouse_wheel(delta_x, delta_y) {
                    debug!(error = %err, "failed to inject loopback mouse wheel");
                }
            }
            Ok(CapturedInputEvent::KeyPress { keycode }) => {
                if let Err(err) = injection.inject_key_press(keycode) {
                    debug!(error = %err, "failed to inject loopback key press");
                }
            }
            Ok(CapturedInputEvent::KeyRelease { keycode }) => {
                if let Err(err) = injection.inject_key_release(keycode) {
                    debug!(error = %err, "failed to inject loopback key release");
                }
            }
            Err(TryRecvError::Empty) => {
                std::thread::sleep(Duration::from_millis(5));
                continue;
            }
            Err(TryRecvError::Disconnected) => break,
        }
    }
}
