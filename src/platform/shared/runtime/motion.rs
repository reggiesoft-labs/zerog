use crate::events::{Event, MotionCheckpoint};
use crate::platform::DisplayBounds;

const PENDING_MOTION_LIMIT: usize = 64;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct MotionFrame {
    pub(crate) dx: i32,
    pub(crate) dy: i32,
    pub(crate) total_dx: i64,
    pub(crate) total_dy: i64,
    pub(crate) session_id: u64,
}

impl MotionFrame {
    pub(crate) fn into_event(self) -> Event {
        Event::MouseMove {
            dx: self.dx,
            dy: self.dy,
            total_dx: self.total_dx,
            total_dy: self.total_dy,
            motion_session_id: self.session_id,
        }
    }
}

#[derive(Default)]
pub(crate) struct OutboundMotion {
    session_id: Option<u64>,
    total_dx: i64,
    total_dy: i64,
    subpixel_dx: f64,
    subpixel_dy: f64,
}

impl OutboundMotion {
    pub(crate) fn reset(&mut self) {
        self.session_id = None;
        self.total_dx = 0;
        self.total_dy = 0;
        self.subpixel_dx = 0.0;
        self.subpixel_dy = 0.0;
    }

    pub(crate) fn session_id(&self) -> Option<u64> {
        self.session_id
    }

    pub(crate) fn record(&mut self, session_id: u64, dx: f64, dy: f64) -> MotionFrame {
        if self.session_id != Some(session_id) {
            self.session_id = Some(session_id);
            self.total_dx = 0;
            self.total_dy = 0;
            self.subpixel_dx = 0.0;
            self.subpixel_dy = 0.0;
        }
        self.subpixel_dx += dx;
        self.subpixel_dy += dy;
        let int_dx = self.subpixel_dx.trunc() as i32;
        let int_dy = self.subpixel_dy.trunc() as i32;
        self.subpixel_dx -= f64::from(int_dx);
        self.subpixel_dy -= f64::from(int_dy);

        self.total_dx = self.total_dx.saturating_add(i64::from(int_dx));
        self.total_dy = self.total_dy.saturating_add(i64::from(int_dy));
        MotionFrame {
            dx: int_dx,
            dy: int_dy,
            total_dx: self.total_dx,
            total_dy: self.total_dy,
            session_id,
        }
    }

    pub(crate) fn take_tail(&mut self, checkpoint: Option<MotionCheckpoint>) -> (i32, i32) {
        let tail = checkpoint
            .filter(|checkpoint| Some(checkpoint.session_id) == self.session_id)
            .map(|checkpoint| {
                (
                    clamp_i64_to_i32(self.total_dx.saturating_sub(checkpoint.total_dx)),
                    clamp_i64_to_i32(self.total_dy.saturating_sub(checkpoint.total_dy)),
                )
            })
            .unwrap_or((0, 0));
        self.reset();
        tail
    }
}

fn clamp_i64_to_i32(value: i64) -> i32 {
    value.clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32
}

#[derive(Clone, Copy)]
struct PendingMotion {
    sender_id: u64,
    session_id: u64,
    packet_seq: u32,
    total_dx: i64,
    total_dy: i64,
}

pub(crate) struct RemoteMotionTracker {
    sender_id: Option<u64>,
    session_id: Option<u64>,
    last_packet_seq: Option<u32>,
    last_total: Option<(i64, i64)>,
    origin: Option<(f64, f64)>,
    bounds: Option<DisplayBounds>,
    sender_bounds: Option<DisplayBounds>,
    displays: Vec<DisplayBounds>,
    pending: Vec<PendingMotion>,
}

impl RemoteMotionTracker {
    pub(crate) fn new() -> Self {
        Self {
            sender_id: None,
            session_id: None,
            last_packet_seq: None,
            last_total: None,
            origin: None,
            bounds: None,
            sender_bounds: None,
            displays: Vec::new(),
            pending: Vec::new(),
        }
    }

    pub(crate) fn reset(&mut self) {
        self.sender_id = None;
        self.session_id = None;
        self.last_packet_seq = None;
        self.last_total = None;
        self.origin = None;
        self.sender_bounds = None;
        self.pending.clear();
    }

    pub(crate) fn matches_session(&self, sender_id: u64, session_id: u64) -> bool {
        self.sender_id == Some(sender_id) && self.session_id == Some(session_id)
    }

    pub(crate) fn set_origin(&mut self, origin: (f64, f64)) {
        self.origin = Some(origin);
    }

    pub(crate) fn set_sender_bounds(&mut self, sender_bounds: Option<DisplayBounds>) {
        self.sender_bounds = sender_bounds;
    }

    #[cfg(test)]
    pub(crate) fn set_bounds(&mut self, bounds: Option<DisplayBounds>) {
        self.bounds = bounds;
    }

    pub(crate) fn set_displays(&mut self, displays: Vec<DisplayBounds>, bounds: Option<DisplayBounds>) {
        self.displays = displays;
        self.bounds = bounds;
    }

    pub(crate) fn start(
        &mut self,
        sender_id: u64,
        handoff_seq: u32,
        session_id: u64,
    ) -> Vec<(f64, f64)> {
        self.sender_id = Some(sender_id);
        self.session_id = Some(session_id);
        self.last_packet_seq = Some(handoff_seq);
        self.last_total = None;

        let mut pending = std::mem::take(&mut self.pending);
        pending.retain(|motion| {
            motion.sender_id == sender_id
                && motion.session_id == session_id
                && sequence_is_newer(motion.packet_seq, handoff_seq)
        });
        pending.sort_by_key(|motion| motion.packet_seq.wrapping_sub(handoff_seq));

        pending
            .into_iter()
            .filter_map(|motion| self.accept(motion.packet_seq, motion.total_dx, motion.total_dy))
            .collect()
    }

    pub(crate) fn apply(
        &mut self,
        sender_id: u64,
        packet_seq: u32,
        session_id: u64,
        total_dx: i64,
        total_dy: i64,
    ) -> Option<(f64, f64)> {
        if self.sender_id != Some(sender_id) || self.session_id != Some(session_id) {
            self.buffer(PendingMotion {
                sender_id,
                session_id,
                packet_seq,
                total_dx,
                total_dy,
            });
            return None;
        }

        self.accept(packet_seq, total_dx, total_dy)
    }

    fn buffer(&mut self, motion: PendingMotion) {
        if self.pending.iter().any(|pending| {
            pending.sender_id == motion.sender_id
                && pending.session_id == motion.session_id
                && pending.packet_seq == motion.packet_seq
                && pending.total_dx == motion.total_dx
                && pending.total_dy == motion.total_dy
        }) {
            return;
        }

        if self.pending.len() == PENDING_MOTION_LIMIT {
            self.pending.remove(0);
        }
        self.pending.push(motion);
    }

    fn accept(&mut self, packet_seq: u32, total_dx: i64, total_dy: i64) -> Option<(f64, f64)> {
        let last_seq = self.last_packet_seq?;
        let is_new_packet = sequence_is_newer(packet_seq, last_seq);
        let is_new_event_in_packet =
            packet_seq == last_seq && self.last_total != Some((total_dx, total_dy));
        if !is_new_packet && !is_new_event_in_packet {
            return None;
        }

        let (mut origin_x, mut origin_y) = self.origin?;

        let total_dx_f64 = total_dx as f64;
        let total_dy_f64 = total_dy as f64;

        let local_display = active_display_bounds(&self.displays, self.bounds, origin_x, origin_y);

        let (scaled_total_dx, scaled_total_dy) = match (self.sender_bounds, local_display) {
            (Some(sender), Some(local))
                if sender.width > 0 && sender.height > 0 && local.width > 0 && local.height > 0 =>
            {
                let sender_diag = f64::from(sender.width).hypot(f64::from(sender.height));
                let local_diag = f64::from(local.width).hypot(f64::from(local.height));
                let scale = local_diag / sender_diag;
                (total_dx_f64 * scale, total_dy_f64 * scale)
            }
            _ => (total_dx_f64, total_dy_f64),
        };

        let next_x = origin_x + scaled_total_dx;
        let next_y = origin_y + scaled_total_dy;

        let (final_x, final_y) = if !self.displays.is_empty() || self.bounds.is_some() {
            let (clamped_x, clamped_y) =
                clamp_to_displays(next_x, next_y, &self.displays, self.bounds);

            if clamped_x != next_x {
                origin_x = clamped_x - scaled_total_dx;
            }
            if clamped_y != next_y {
                origin_y = clamped_y - scaled_total_dy;
            }
            self.origin = Some((origin_x, origin_y));
            (clamped_x, clamped_y)
        } else {
            (next_x, next_y)
        };

        self.last_packet_seq = Some(packet_seq);
        self.last_total = Some((total_dx, total_dy));
        Some((final_x, final_y))
    }
}

fn active_display_bounds(
    displays: &[DisplayBounds],
    virtual_bounds: Option<DisplayBounds>,
    x: f64,
    y: f64,
) -> Option<DisplayBounds> {
    for d in displays {
        let min_x = f64::from(d.x);
        let min_y = f64::from(d.y);
        let max_x = min_x + f64::from(d.width);
        let max_y = min_y + f64::from(d.height);
        if x >= min_x && x < max_x && y >= min_y && y < max_y {
            return Some(*d);
        }
    }
    displays.first().copied().or(virtual_bounds)
}

pub(crate) fn clamp_to_displays(
    x: f64,
    y: f64,
    displays: &[DisplayBounds],
    virtual_bounds: Option<DisplayBounds>,
) -> (f64, f64) {
    if displays.is_empty() {
        if let Some(bounds) = virtual_bounds {
            let min_x = f64::from(bounds.x);
            let min_y = f64::from(bounds.y);
            let max_x = min_x + f64::from(bounds.width) - 1.0;
            let max_y = min_y + f64::from(bounds.height) - 1.0;
            return (x.clamp(min_x, max_x), y.clamp(min_y, max_y));
        }
        return (x, y);
    }

    for d in displays {
        let min_x = f64::from(d.x);
        let min_y = f64::from(d.y);
        let max_x = min_x + f64::from(d.width);
        let max_y = min_y + f64::from(d.height);

        if x >= min_x && x < max_x && y >= min_y && y < max_y {
            return (x, y);
        }
    }

    let fallback_bounds = virtual_bounds.unwrap_or(displays[0]);
    let min_fb_x = f64::from(fallback_bounds.x);
    let min_fb_y = f64::from(fallback_bounds.y);
    let max_fb_x = min_fb_x + f64::from(fallback_bounds.width) - 1.0;
    let max_fb_y = min_fb_y + f64::from(fallback_bounds.height) - 1.0;
    let mut best = (x.clamp(min_fb_x, max_fb_x), y.clamp(min_fb_y, max_fb_y));
    let mut best_distance = f64::INFINITY;

    for display in displays {
        let min_x = f64::from(display.x);
        let min_y = f64::from(display.y);
        let max_x = min_x + f64::from(display.width) - 1.0;
        let max_y = min_y + f64::from(display.height) - 1.0;
        let candidate = (x.clamp(min_x, max_x), y.clamp(min_y, max_y));
        let dx = x - candidate.0;
        let dy = y - candidate.1;
        let distance = dx * dx + dy * dy;
        if distance < best_distance {
            best_distance = distance;
            best = candidate;
        }
    }

    best
}

fn sequence_is_newer(candidate: u32, previous: u32) -> bool {
    let distance = candidate.wrapping_sub(previous);
    distance != 0 && distance < (u32::MAX / 2)
}

#[cfg(test)]
mod tests {
    use super::{OutboundMotion, RemoteMotionTracker};
    use crate::events::MotionCheckpoint;

    #[test]
    fn outbound_motion_resets_totals_for_a_new_session() {
        let mut motion = OutboundMotion::default();

        assert_eq!(motion.record(1, 4.0, -2.0).total_dx, 4);
        assert_eq!(motion.record(1, 3.0, 5.0).total_dy, 3);
        let frame = motion.record(2, 8.0, 9.0);

        assert_eq!((frame.total_dx, frame.total_dy), (8, 9));
        assert_eq!(motion.session_id(), Some(2));
    }

    #[test]
    fn outbound_motion_returns_only_the_unconsumed_tail() {
        let mut motion = OutboundMotion::default();
        motion.record(7, 12.0, -2.0);
        motion.record(7, 9.0, 5.0);

        let tail = motion.take_tail(Some(MotionCheckpoint {
            session_id: 7,
            total_dx: 12,
            total_dy: -2,
        }));

        assert_eq!(tail, (9, 5));
        assert_eq!(motion.session_id(), None);
    }

    #[test]
    fn receiver_accepts_only_new_cumulative_positions() {
        let mut tracker = RemoteMotionTracker::new();
        tracker.set_origin((100.0, 200.0));
        tracker.start(7, 10, 20);

        assert_eq!(tracker.apply(7, 12, 20, 25, -10), Some((125.0, 190.0)));
        assert_eq!(tracker.apply(7, 11, 20, 5, -2), None);
        assert_eq!(tracker.apply(7, 14, 20, 40, -20), Some((140.0, 180.0)));
    }

    #[test]
    fn receiver_buffers_motion_until_handoff_session_is_started() {
        let mut tracker = RemoteMotionTracker::new();
        tracker.set_origin((100.0, 200.0));

        assert_eq!(tracker.apply(7, 12, 20, 25, -10), None);
        assert_eq!(tracker.start(7, 10, 20), vec![(125.0, 190.0)]);
    }

    #[test]
    fn receiver_does_not_move_back_for_reordered_frames() {
        let mut tracker = RemoteMotionTracker::new();
        tracker.set_origin((100.0, 200.0));
        tracker.start(7, 10, 20);

        assert_eq!(tracker.apply(7, 12, 20, 40, -20), Some((140.0, 180.0)));
        assert_eq!(tracker.apply(7, 11, 20, 25, -10), None);
    }

    #[test]
    fn receiver_clamps_boundary_and_reverses_instantly() {
        let mut tracker = RemoteMotionTracker::new();
        tracker.set_origin((100.0, 200.0));
        tracker.set_bounds(Some(crate::platform::DisplayBounds {
            x: 0,
            y: 0,
            width: 1920,
            height: 1080,
        }));
        tracker.start(7, 10, 20);

        assert_eq!(tracker.apply(7, 11, 20, 3000, 0), Some((1919.0, 200.0)));
        assert_eq!(tracker.apply(7, 12, 20, 3500, 0), Some((1919.0, 200.0)));
        assert_eq!(tracker.apply(7, 13, 20, 3490, 0), Some((1909.0, 200.0)));
    }

    #[test]
    fn receiver_clamps_multi_display_union() {
        let mut tracker = RemoteMotionTracker::new();
        tracker.set_origin((500.0, 500.0));
        let display1 = crate::platform::DisplayBounds {
            x: 0,
            y: 0,
            width: 1920,
            height: 1080,
        };
        let display2 = crate::platform::DisplayBounds {
            x: 1920,
            y: 500,
            width: 1920,
            height: 1080,
        };
        let virtual_bounds = crate::platform::DisplayBounds {
            x: 0,
            y: 0,
            width: 3840,
            height: 1580,
        };
        tracker.set_displays(vec![display1, display2], Some(virtual_bounds));
        tracker.start(7, 10, 20);

        assert_eq!(tracker.apply(7, 11, 20, 1500, 100), Some((2000.0, 600.0)));

        let clamped = tracker.apply(7, 12, 20, 1500, -300).unwrap();
        assert!(
            (clamped.0 >= 0.0 && clamped.0 < 1920.0 && clamped.1 >= 0.0 && clamped.1 < 1080.0)
                || (clamped.0 >= 1920.0 && clamped.0 < 3840.0 && clamped.1 >= 500.0 && clamped.1 < 1580.0)
        );
    }

    #[test]
    fn receiver_scales_motion_for_different_display_resolutions() {
        let mut tracker = RemoteMotionTracker::new();
        tracker.set_origin((0.0, 0.0));
        let sender_bounds = crate::platform::DisplayBounds {
            x: 0,
            y: 0,
            width: 1000,
            height: 500,
        };
        let local_bounds = crate::platform::DisplayBounds {
            x: 0,
            y: 0,
            width: 2000,
            height: 1000,
        };
        tracker.set_sender_bounds(Some(sender_bounds));
        tracker.set_bounds(Some(local_bounds));
        tracker.start(1, 1, 1);

        assert_eq!(tracker.apply(1, 2, 1, 100, 50), Some((200.0, 100.0)));
    }

    #[test]
    fn receiver_preserves_1_to_1_motion_when_display_resolutions_match() {
        let mut tracker = RemoteMotionTracker::new();
        tracker.set_origin((0.0, 0.0));
        let sender_bounds = crate::platform::DisplayBounds {
            x: 0,
            y: 0,
            width: 1920,
            height: 1080,
        };
        let local_bounds = crate::platform::DisplayBounds {
            x: 0,
            y: 0,
            width: 1920,
            height: 1080,
        };
        tracker.set_sender_bounds(Some(sender_bounds));
        tracker.set_bounds(Some(local_bounds));
        tracker.start(1, 1, 1);

        assert_eq!(tracker.apply(1, 2, 1, 100, 50), Some((100.0, 50.0)));
    }

    #[test]
    fn receiver_preserves_isotropic_angles_across_differing_aspect_ratios() {
        let mut tracker = RemoteMotionTracker::new();
        tracker.set_origin((0.0, 0.0));
        let sender_bounds = crate::platform::DisplayBounds {
            x: 0,
            y: 0,
            width: 1920,
            height: 1080,
        };
        let local_bounds = crate::platform::DisplayBounds {
            x: 0,
            y: 0,
            width: 3440,
            height: 1440,
        };
        tracker.set_sender_bounds(Some(sender_bounds));
        tracker.set_bounds(Some(local_bounds));
        tracker.start(1, 1, 1);

        let result = tracker.apply(1, 2, 1, 100, 100).unwrap();
        assert!((result.0 - result.1).abs() < 1e-6);
    }

    #[test]
    fn receiver_uses_active_display_in_multi_display_topology() {
        let mut tracker = RemoteMotionTracker::new();
        tracker.set_origin((500.0, 500.0));
        let display1 = crate::platform::DisplayBounds {
            x: 0,
            y: 0,
            width: 1920,
            height: 1080,
        };
        let display2 = crate::platform::DisplayBounds {
            x: 1920,
            y: 0,
            width: 1920,
            height: 1080,
        };
        let virtual_bounds = crate::platform::DisplayBounds {
            x: 0,
            y: 0,
            width: 3840,
            height: 1080,
        };
        let sender_display = crate::platform::DisplayBounds {
            x: 0,
            y: 0,
            width: 1920,
            height: 1080,
        };
        tracker.set_displays(vec![display1, display2], Some(virtual_bounds));
        tracker.set_sender_bounds(Some(sender_display));
        tracker.start(1, 1, 1);

        let result = tracker.apply(1, 2, 1, 100, 50).unwrap();
        assert_eq!(result, (600.0, 550.0));
    }
}

