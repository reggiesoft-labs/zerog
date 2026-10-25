use super::motion::RemoteMotionTracker;

pub(crate) struct MovementSmoother {
    pub(crate) active: bool,
    /// The latest target position received from remote.
    target: Option<(f64, f64)>,
    /// The last position actually injected.
    current: Option<(f64, f64)>,
    motion: RemoteMotionTracker,
}

impl MovementSmoother {
    pub(crate) fn new() -> Self {
        Self {
            active: false,
            target: None,
            current: None,
            motion: RemoteMotionTracker::new(),
        }
    }

    pub(crate) fn start_motion(
        &mut self,
        sender_id: u64,
        handoff_seq: u32,
        session_id: u64,
    ) -> Vec<(f64, f64)> {
        let targets = self.motion.start(sender_id, handoff_seq, session_id);
        if let Some(&target) = targets.last() {
            self.target = Some(target);
        }
        targets
    }

    pub(crate) fn matches_motion_session(&self, sender_id: u64, session_id: u64) -> bool {
        self.motion.matches_session(sender_id, session_id)
    }

    pub(crate) fn apply_remote_motion(
        &mut self,
        sender_id: u64,
        packet_seq: u32,
        session_id: u64,
        total_dx: i64,
        total_dy: i64,
    ) -> Option<(f64, f64)> {
        let target = self
            .motion
            .apply(sender_id, packet_seq, session_id, total_dx, total_dy)?;
        self.target = Some(target);
        Some(target)
    }

    /// Called on each poll tick. Returns the position to inject, or None if
    /// no injection is needed this tick.
    pub(crate) fn tick(&self) -> Option<(f64, f64)> {
        if !self.active {
            return None;
        }
        self.target.filter(|target| Some(*target) != self.current)
    }

    /// Commits a position after the operating system accepts the injection.
    pub(crate) fn commit_injection(&mut self, position: (f64, f64)) {
        self.current = Some(position);
    }

    /// Activate movement tracking when TakeControl is received.
    pub(crate) fn activate(&mut self) {
        self.active = true;
        self.target = None;
        self.current = None;
        self.motion.reset();
    }

    /// Deactivate movement tracking when ReleaseControl is received.
    pub(crate) fn deactivate(&mut self) {
        self.active = false;
        self.target = None;
        self.current = None;
        self.motion.reset();
    }

    /// Immediately snap to a position used for TakeControl initial placement.
    pub(crate) fn snap_to(&mut self, x: f64, y: f64) {
        self.current = Some((x, y));
        self.target = Some((x, y));
        self.motion.set_origin((x, y));
    }

    pub(crate) fn set_displays(
        &mut self,
        displays: Vec<crate::platform::DisplayBounds>,
        bounds: Option<crate::platform::DisplayBounds>,
        sender_bounds: Option<crate::platform::DisplayBounds>,
    ) {
        self.motion.set_displays(displays, bounds);
        self.motion.set_sender_bounds(sender_bounds);
    }
}

#[cfg(test)]
mod tests {
    use super::MovementSmoother;

    fn receiving_motion() -> MovementSmoother {
        let mut smoother = MovementSmoother::new();
        smoother.activate();
        smoother.snap_to(0.0, 0.0);
        smoother.start_motion(7, 10, 20);
        smoother
    }

    #[test]
    fn every_target_is_presented_immediately_including_reversals() {
        let mut smoother = receiving_motion();
        for (seq, x, y) in [(11, 10, 2), (12, 20, 4), (13, 15, -2), (14, 100, 0)] {
            smoother.apply_remote_motion(7, seq, 20, x, y);
            let target = (x as f64, y as f64);
            assert_eq!(smoother.tick(), Some(target));
            smoother.commit_injection(target);
            assert_eq!(smoother.tick(), None);
        }
    }

    #[test]
    fn stopped_motion_does_not_predict_or_retreat() {
        let mut smoother = receiving_motion();
        smoother.apply_remote_motion(7, 11, 20, 10, 0);
        smoother.commit_injection(smoother.tick().unwrap());
        smoother.apply_remote_motion(7, 12, 20, 20, 0);
        smoother.commit_injection(smoother.tick().unwrap());
        for _ in 0..100 {
            assert_eq!(smoother.tick(), None);
        }
        assert_eq!(smoother.current, Some((20.0, 0.0)));
    }

    #[test]
    fn failed_injection_remains_pending_until_committed() {
        let mut smoother = receiving_motion();
        smoother.apply_remote_motion(7, 11, 20, 10, 0);
        assert_eq!(smoother.tick(), Some((10.0, 0.0)));
        assert_eq!(smoother.current, Some((0.0, 0.0)));
        assert_eq!(smoother.tick(), Some((10.0, 0.0)));
        smoother.commit_injection((10.0, 0.0));
        assert_eq!(smoother.tick(), None);
    }

    #[test]
    fn packet_loss_and_reordering_preserve_latest_position() {
        let mut smoother = receiving_motion();
        smoother.apply_remote_motion(7, 11, 20, 10, 0);
        smoother.commit_injection(smoother.tick().unwrap());
        smoother.apply_remote_motion(7, 14, 20, 40, 0);
        assert_eq!(smoother.apply_remote_motion(7, 12, 20, 20, 0), None);
        assert_eq!(smoother.tick(), Some((40.0, 0.0)));
        smoother.commit_injection((40.0, 0.0));
        assert_eq!(smoother.apply_remote_motion(7, 14, 20, 40, 0), None);
        assert_eq!(smoother.tick(), None);
    }

    #[test]
    fn handoff_replays_buffered_motion_without_waiting() {
        let mut smoother = MovementSmoother::new();
        smoother.activate();
        smoother.snap_to(100.0, 50.0);
        assert_eq!(smoother.apply_remote_motion(7, 11, 20, 10, 5), None);
        assert_eq!(smoother.tick(), None);
        smoother.start_motion(7, 10, 20);
        assert_eq!(smoother.tick(), Some((110.0, 55.0)));
        smoother.deactivate();
        assert_eq!(smoother.tick(), None);
        smoother.activate();
        assert_eq!(smoother.tick(), None);
    }
}
