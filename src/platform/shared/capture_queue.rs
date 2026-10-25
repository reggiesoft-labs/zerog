use std::sync::atomic::{AtomicU64, Ordering};
use tokio::sync::mpsc::{Sender, error::TrySendError};

use crate::platform::CapturedInputEvent;

pub(crate) const EVENT_QUEUE_CAPACITY: usize = 2048;

pub(crate) fn try_send(
    sender: &Sender<CapturedInputEvent>,
    event: CapturedInputEvent,
    dropped: &AtomicU64,
    backend: &'static str,
) {
    match sender.try_send(event) {
        Ok(()) => {}
        Err(TrySendError::Full(_)) => {
            let count = dropped.fetch_add(1, Ordering::Relaxed) + 1;
            if count == 1 || count.is_power_of_two() {
                tracing::warn!(backend, dropped = count, "capture queue is full");
            }
        }
        Err(TrySendError::Closed(_)) => {}
    }
}
