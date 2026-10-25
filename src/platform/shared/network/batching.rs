use std::sync::Mutex;

use crate::events::Event;

struct PendingInputBatch {
    first_capture_us: Option<u64>,
    events: Vec<Event>,
    pending_move: Option<Event>,
    moves_coalesced: u64,
}

impl PendingInputBatch {
    fn new(max_events_per_packet: usize) -> Self {
        Self {
            first_capture_us: None,
            events: Vec::with_capacity(max_events_per_packet),
            pending_move: None,
            moves_coalesced: 0,
        }
    }
}

pub(crate) struct BatchManager {
    batch_window_us: u64,
    max_events_per_packet: usize,
    pending_input: Mutex<PendingInputBatch>,
}

impl BatchManager {
    pub(crate) fn new(batch_window_us: u64, max_events_per_packet: usize) -> Self {
        Self {
            batch_window_us,
            max_events_per_packet,
            pending_input: Mutex::new(PendingInputBatch::new(max_events_per_packet)),
        }
    }

    pub(crate) fn queue_event(&self, event: Event, capture_us: u64) -> Option<(Vec<Event>, u64)> {
        let is_movement = matches!(&event, Event::MouseMove { .. });
        let mut batch = self
            .pending_input
            .lock()
            .expect("pending_input lock poisoned");
        if batch.events.is_empty() && batch.pending_move.is_none() {
            batch.first_capture_us = Some(capture_us);
        }

        match &event {
            Event::MouseMove {
                dx,
                dy,
                total_dx,
                total_dy,
                motion_session_id,
            } => match batch.pending_move.take() {
                Some(Event::MouseMove {
                    dx: pending_dx,
                    dy: pending_dy,
                    motion_session_id: pending_session_id,
                    ..
                }) if pending_session_id == *motion_session_id => {
                    batch.moves_coalesced += 1;
                    batch.pending_move = Some(Event::MouseMove {
                        dx: pending_dx + *dx,
                        dy: pending_dy + *dy,
                        total_dx: *total_dx,
                        total_dy: *total_dy,
                        motion_session_id: *motion_session_id,
                    });
                }
                Some(pending) => {
                    batch.events.push(pending);
                    batch.pending_move = Some(event);
                }
                None => {
                    batch.pending_move = Some(event);
                }
            },
            _ => {
                // Non-movement: flush any pending move first (preserves ordering),
                // then push this event
                if let Some(mv) = batch.pending_move.take() {
                    batch.events.push(mv);
                }
                batch.events.push(event);
            }
        }

        let total_len = batch.events.len() + if batch.pending_move.is_some() { 1 } else { 0 };
        let first_capture_us = batch.first_capture_us.unwrap_or(capture_us);
        let max_events_reached = total_len >= self.max_events_per_packet;
        let batch_window_elapsed = self.batch_window_us == 0
            || !is_movement
            || capture_us.saturating_sub(first_capture_us) >= self.batch_window_us;

        if max_events_reached || batch_window_elapsed {
            return take_pending_events(&mut batch);
        }

        None
    }

    pub(crate) fn flush_if_due(&self, now_us: u64) -> Option<(Vec<Event>, u64)> {
        if self.batch_window_us == 0 {
            return None;
        }

        let mut batch = self
            .pending_input
            .lock()
            .expect("pending_input lock poisoned");
        let first_capture_us = batch.first_capture_us?;
        if (batch.events.is_empty() && batch.pending_move.is_none())
            || now_us.saturating_sub(first_capture_us) < self.batch_window_us
        {
            return None;
        }

        take_pending_events(&mut batch)
    }

    pub(crate) fn flush_now(&self) -> Option<(Vec<Event>, u64)> {
        let mut batch = self
            .pending_input
            .lock()
            .expect("pending_input lock poisoned");
        if batch.events.is_empty() && batch.pending_move.is_none() {
            return None;
        }

        take_pending_events(&mut batch)
    }

    pub(crate) fn clear(&self) {
        let mut batch = self
            .pending_input
            .lock()
            .expect("pending_input lock poisoned");
        batch.first_capture_us = None;
        batch.events.clear();
        batch.pending_move = None;
        batch.moves_coalesced = 0;
    }

    pub(crate) fn max_events_per_packet(&self) -> usize {
        self.max_events_per_packet
    }
}

fn take_pending_events(batch: &mut PendingInputBatch) -> Option<(Vec<Event>, u64)> {
    // Drain pending_move into the events vec
    if let Some(mv) = batch.pending_move.take() {
        batch.events.push(mv);
    }
    if batch.events.is_empty() {
        return None;
    }

    let coalesced = batch.moves_coalesced;
    batch.moves_coalesced = 0;
    let mut events = Vec::with_capacity(batch.events.len());
    std::mem::swap(&mut events, &mut batch.events);
    batch.first_capture_us = None;
    Some((events, coalesced))
}

#[cfg(test)]
mod tests {
    use super::BatchManager;
    use crate::events::Event;

    fn mouse_move() -> Event {
        Event::MouseMove {
            dx: 1,
            dy: 1,
            total_dx: 1,
            total_dy: 1,
            motion_session_id: 1,
        }
    }

    fn mouse_move_by(dx: i32, dy: i32) -> Event {
        Event::MouseMove {
            dx,
            dy,
            total_dx: i64::from(dx),
            total_dy: i64::from(dy),
            motion_session_id: 1,
        }
    }

    fn mouse_button(button: u8, pressed: bool) -> Event {
        Event::MouseButton { button, pressed }
    }

    fn key_press(keycode: u32) -> Event {
        Event::KeyPress { keycode }
    }

    #[test]
    fn queue_event_holds_batch_until_window_expires() {
        let manager = BatchManager::new(1_000, 4);

        assert!(manager.queue_event(mouse_move(), 100).is_none());
        assert!(manager.flush_if_due(500).is_none());
        let (events, _coalesced) = manager.flush_if_due(1_100).expect("batch should flush");
        assert_eq!(events.len(), 1);
    }

    #[test]
    fn queue_event_flushes_when_max_events_reached() {
        let manager = BatchManager::new(10_000, 2);

        // Two mouse moves coalesce to one pending_move, so we need non-move events
        // to actually fill the batch to 2. But with coalescing, two mouse moves
        // only produce 1 event. Use a button event to reach max.
        assert!(manager.queue_event(mouse_move(), 100).is_none());
        let result = manager.queue_event(mouse_button(1, true), 200);
        let (flushed, _coalesced) = result.expect("max batch size should flush");

        assert_eq!(flushed.len(), 2);
        assert!(manager.flush_now().is_none());
    }

    #[test]
    fn flush_now_returns_pending_events_and_clears_batch() {
        let manager = BatchManager::new(10_000, 4);

        assert!(manager.queue_event(mouse_move_by(1, 1), 100).is_none());
        assert!(manager.queue_event(mouse_move_by(2, 2), 200).is_none());

        let (flushed, _coalesced) = manager.flush_now().expect("flush_now should return events");
        assert_eq!(flushed.len(), 1);
        assert!(manager.flush_now().is_none());
    }

    #[test]
    fn coalesces_consecutive_mouse_moves() {
        let manager = BatchManager::new(10_000, 100);

        for i in 0..5 {
            assert!(
                manager
                    .queue_event(mouse_move_by(i, i), 100 + i as u64)
                    .is_none()
            );
        }

        let (events, coalesced) = manager.flush_now().expect("should have pending events");
        assert_eq!(events.len(), 1, "5 moves should coalesce to 1");
        assert_eq!(coalesced, 4, "4 moves should be counted as coalesced");
        match &events[0] {
            Event::MouseMove {
                dx,
                dy,
                total_dx,
                total_dy,
                ..
            } => {
                assert_eq!((*dx, *dy), (10, 10));
                assert_eq!((*total_dx, *total_dy), (4, 4));
            }
            other => panic!("expected MouseMove, got {:?}", other),
        }
    }

    #[test]
    fn preserves_non_move_ordering_with_coalescing() {
        let manager = BatchManager::new(10_000, 100);

        // Queue: Move1, Move2 -> non-movement Button flushes immediately with (Move1+Move2, Button)
        assert!(manager.queue_event(mouse_move_by(1, 1), 100).is_none());
        assert!(manager.queue_event(mouse_move_by(2, 2), 200).is_none());
        let (batch1, coalesced1) = manager
            .queue_event(mouse_button(1, true), 300)
            .expect("button should flush pending moves + button");
        assert_eq!(batch1.len(), 2);
        assert_eq!(coalesced1, 1);
        match &batch1[0] {
            Event::MouseMove { dx, dy, .. } => assert_eq!((*dx, *dy), (3, 3)),
            other => panic!("expected MouseMove(3,3), got {:?}", other),
        }
        assert!(matches!(
            &batch1[1],
            Event::MouseButton {
                button: 1,
                pressed: true
            }
        ));

        // Queue: Move3, Move4 -> non-movement KeyPress flushes immediately with (Move3+Move4, KeyPress)
        assert!(manager.queue_event(mouse_move_by(3, 3), 400).is_none());
        assert!(manager.queue_event(mouse_move_by(4, 4), 500).is_none());
        let (batch2, coalesced2) = manager
            .queue_event(key_press(0x04), 600)
            .expect("key press should flush pending moves + key");
        assert_eq!(batch2.len(), 2);
        assert_eq!(coalesced2, 1);
        match &batch2[0] {
            Event::MouseMove { dx, dy, .. } => assert_eq!((*dx, *dy), (7, 7)),
            other => panic!("expected MouseMove(7,7), got {:?}", other),
        }
        assert!(matches!(&batch2[1], Event::KeyPress { keycode: 0x04 }));

        assert!(manager.flush_now().is_none());
    }

    #[test]
    fn immediate_mode_still_flushes_single_move() {
        let manager = BatchManager::new(0, 100);

        let result = manager.queue_event(mouse_move_by(10, 20), 100);
        let (events, coalesced) = result.expect("immediate mode should flush");
        assert_eq!(events.len(), 1);
        assert_eq!(coalesced, 0);
    }

    #[test]
    fn immediate_mode_flushes_each_event_separately() {
        let manager = BatchManager::new(0, 100);

        let result1 = manager.queue_event(mouse_move_by(10, 20), 100);
        assert!(result1.is_some(), "move should flush immediately");

        let result2 = manager.queue_event(mouse_button(1, true), 200);
        assert!(result2.is_some(), "button should flush immediately");
    }

    #[test]
    fn flush_now_drains_pending_move() {
        let manager = BatchManager::new(10_000, 100);

        assert!(manager.queue_event(mouse_move_by(5, 5), 100).is_none());

        let (events, _coalesced) = manager.flush_now().expect("should drain pending move");
        assert_eq!(events.len(), 1);

        assert!(
            manager.flush_now().is_none(),
            "second flush should return None"
        );
    }

    #[test]
    fn coalesced_count_returned_in_flush() {
        let manager = BatchManager::new(10_000, 100);

        // Queue 10 MouseMove events
        for i in 0..10 {
            assert!(
                manager
                    .queue_event(mouse_move_by(i, i * 2), 100 + i as u64)
                    .is_none()
            );
        }

        let (events, coalesced) = manager.flush_now().expect("should have pending events");
        assert_eq!(events.len(), 1, "10 moves should coalesce to 1");
        assert_eq!(coalesced, 9, "9 moves should be counted as coalesced");
        match &events[0] {
            Event::MouseMove { dx, dy, .. } => {
                assert_eq!((*dx, *dy), (45, 90));
            }
            other => panic!("expected MouseMove, got {:?}", other),
        }
    }

    #[test]
    fn discrete_events_flush_immediately_with_large_batch_window() {
        let manager = BatchManager::new(50_000, 100);

        let key_press = Event::KeyPress { keycode: 0x04 };
        let result = manager.queue_event(key_press, 100);
        let (events, _) = result.expect("key press should flush immediately");
        assert_eq!(events.len(), 1);
        assert!(matches!(&events[0], Event::KeyPress { keycode: 0x04 }));

        let key_release = Event::KeyRelease { keycode: 0x04 };
        let result = manager.queue_event(key_release, 150);
        let (events, _) = result.expect("key release should flush immediately");
        assert_eq!(events.len(), 1);
        assert!(matches!(&events[0], Event::KeyRelease { keycode: 0x04 }));
    }
}
