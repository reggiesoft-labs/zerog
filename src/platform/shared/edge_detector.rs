//! Edge detection state machine for cursor transitions.
//!
//! This module provides the `EdgeDetector` type which encapsulates in-zone state
//! and dwell timer for both outbound (LocalControl→RemoteControl) and return (RemoteControl→LocalControl)
//! edge transitions.
//!
//! # Design
//!
//! The detector does NOT own topology or peer_edges — these are passed as parameters
//! to each call. This avoids synchronization complexity and keeps EdgeDetector a pure
//! state machine that can be wrapped in a Mutex for thread-safe access.
//!
//! # Dwell Timer
//!
//! Both outbound and return transitions require the cursor to dwell in the edge zone
//! for `EDGE_BUFFER_TIMER_US` (5ms) before triggering. This prevents accidental
//! transitions from quick cursor movements.

#[cfg(test)]
use crate::platform::{PeerEdge, VirtualCoords};

use super::layout_state::OutboundTarget;
#[cfg(test)]
use super::topology::EDGE_BUFFER_PX;

const EDGE_BUFFER_TIMER_US: u64 = 50_000;
pub(crate) const OUTBOUND_PUSH_DISTANCE_PX: u32 = 4;

enum OutboundState {
    Tracking {
        target: OutboundTarget,
        started_us: u64,
        pushed_px: u32,
    },
    Triggered {
        target: OutboundTarget,
    },
}

pub(crate) struct EdgeDetector {
    outbound: Option<OutboundState>,
    #[cfg(test)]
    return_in_zone: bool,
    #[cfg(test)]
    return_dwell_started_us: Option<u64>,
}

impl EdgeDetector {
    pub(crate) fn new() -> Self {
        Self {
            outbound: None,
            #[cfg(test)]
            return_in_zone: false,
            #[cfg(test)]
            return_dwell_started_us: None,
        }
    }

    pub(crate) fn reset(&mut self) {
        self.outbound = None;
        #[cfg(test)]
        {
            self.return_in_zone = false;
            self.return_dwell_started_us = None;
        }
    }

    pub(crate) fn check_outbound_target(
        &mut self,
        detected: Option<OutboundTarget>,
        sample_us: u64,
        outward_push_px: u32,
    ) -> Option<OutboundTarget> {
        let Some(target) = detected else {
            self.outbound = None;
            return None;
        };

        let same_target = match &self.outbound {
            Some(OutboundState::Tracking { target: active, .. })
            | Some(OutboundState::Triggered { target: active }) => active == &target,
            None => false,
        };

        if !same_target {
            if outward_push_px >= OUTBOUND_PUSH_DISTANCE_PX {
                self.outbound = Some(OutboundState::Triggered {
                    target: target.clone(),
                });
                return Some(target);
            }

            self.outbound = Some(OutboundState::Tracking {
                target,
                started_us: sample_us,
                pushed_px: outward_push_px,
            });
            return None;
        }

        let triggered = match self.outbound.as_mut() {
            Some(OutboundState::Tracking {
                target,
                started_us,
                pushed_px,
            }) => {
                *pushed_px = pushed_px.saturating_add(outward_push_px);
                let dwell_elapsed = sample_us.saturating_sub(*started_us) >= EDGE_BUFFER_TIMER_US;
                (*pushed_px >= OUTBOUND_PUSH_DISTANCE_PX || dwell_elapsed).then(|| target.clone())
            }
            Some(OutboundState::Triggered { .. }) | None => None,
        };

        if let Some(target) = triggered {
            self.outbound = Some(OutboundState::Triggered {
                target: target.clone(),
            });
            return Some(target);
        }

        None
    }

    #[cfg(test)]
    pub(crate) fn check_return_edge(
        &mut self,
        virtual_cursor: VirtualCoords,
        sample_us: u64,
        active_edge: PeerEdge,
    ) -> bool {
        let past_return_edge = match active_edge {
            PeerEdge::Right => virtual_cursor.x < -(EDGE_BUFFER_PX as i32),
            PeerEdge::Left => virtual_cursor.x > EDGE_BUFFER_PX as i32,
            PeerEdge::Top => virtual_cursor.y > EDGE_BUFFER_PX as i32,
            PeerEdge::Bottom => virtual_cursor.y < -(EDGE_BUFFER_PX as i32),
        };

        match (past_return_edge, self.return_in_zone) {
            (false, true) => {
                self.return_in_zone = false;
                self.return_dwell_started_us = None;
                false
            }
            (true, false) => {
                self.return_in_zone = true;
                self.return_dwell_started_us = Some(sample_us);
                false
            }
            (true, true) => {
                if let Some(start) = self.return_dwell_started_us {
                    if sample_us >= start && sample_us - start >= EDGE_BUFFER_TIMER_US {
                        self.return_dwell_started_us = None;
                        return true;
                    }
                }
                false
            }
            _ => false,
        }
    }
}

impl Default for EdgeDetector {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use std::net::{IpAddr, Ipv4Addr, SocketAddr};

    use super::EdgeDetector;
    use crate::core::layout::EdgeSpanBp;
    use crate::platform::shared::layout_state::{OutboundTarget, RuntimeContact};
    use crate::platform::{PeerEdge, VirtualCoords};

    fn test_addr() -> SocketAddr {
        SocketAddr::new(IpAddr::V4(Ipv4Addr::new(192, 168, 1, 100)), 12345)
    }

    fn target(edge: PeerEdge, addr: SocketAddr) -> OutboundTarget {
        OutboundTarget {
            contact: RuntimeContact {
                peer_id: "peer".to_string(),
                addr,
                local: EdgeSpanBp {
                    edge,
                    start_bp: 0,
                    end_bp: 10_000,
                },
                remote: EdgeSpanBp {
                    edge: match edge {
                        PeerEdge::Left => PeerEdge::Right,
                        PeerEdge::Right => PeerEdge::Left,
                        PeerEdge::Top => PeerEdge::Bottom,
                        PeerEdge::Bottom => PeerEdge::Top,
                    },
                    start_bp: 0,
                    end_bp: 10_000,
                },
            },
        }
    }

    #[test]
    fn outbound_dwell_elapsed_after_timeout() {
        let mut detector = EdgeDetector::new();
        let expected = target(PeerEdge::Right, test_addr());

        assert!(
            detector
                .check_outbound_target(Some(expected.clone()), 0, 0)
                .is_none(),
            "first call should return None (timer started)"
        );

        assert!(
            detector
                .check_outbound_target(Some(expected.clone()), 40_000, 0)
                .is_none(),
            "call before timeout should return None"
        );

        let result = detector.check_outbound_target(Some(expected.clone()), 50_000, 0);
        assert_eq!(result, Some(expected));
    }

    #[test]
    fn outbound_immediate_transition_on_directed_push() {
        let mut detector = EdgeDetector::new();
        let expected = target(PeerEdge::Right, test_addr());

        let result = detector.check_outbound_target(Some(expected.clone()), 0, 4);
        assert_eq!(result, Some(expected));
    }

    #[test]
    fn outbound_accumulates_small_outward_moves() {
        let mut detector = EdgeDetector::new();
        let expected = target(PeerEdge::Right, test_addr());

        for sample_us in [0, 1_000, 2_000] {
            assert!(
                detector
                    .check_outbound_target(Some(expected.clone()), sample_us, 1)
                    .is_none()
            );
        }

        let result = detector.check_outbound_target(Some(expected.clone()), 3_000, 1);
        assert_eq!(result, Some(expected));
    }

    #[test]
    fn outbound_dwell_reset_on_exit() {
        let mut detector = EdgeDetector::new();
        let expected = target(PeerEdge::Right, test_addr());

        assert!(
            detector
                .check_outbound_target(Some(expected.clone()), 0, 0)
                .is_none()
        );

        let result = detector.check_outbound_target(None, 10_000, 0);
        assert!(result.is_none());

        assert!(
            detector
                .check_outbound_target(Some(expected.clone()), 20_000, 0)
                .is_none(),
            "re-entering zone should start fresh timer"
        );

        let result = detector.check_outbound_target(Some(expected.clone()), 70_000, 0);
        assert_eq!(result, Some(expected));
    }

    #[test]
    fn outbound_resets_when_target_changes() {
        let mut detector = EdgeDetector::new();
        let first = target(PeerEdge::Left, test_addr());
        let second = target(
            PeerEdge::Left,
            SocketAddr::new(IpAddr::V4(Ipv4Addr::new(192, 168, 1, 101)), 12345),
        );

        assert!(
            detector
                .check_outbound_target(Some(first.clone()), 0, 0)
                .is_none(),
            "first target should start dwell timer"
        );

        assert!(
            detector
                .check_outbound_target(Some(second.clone()), 50_000, 0)
                .is_none(),
            "switching target should reset dwell timer"
        );

        let result = detector.check_outbound_target(Some(second.clone()), 100_000, 0);
        assert_eq!(result, Some(second));
    }

    #[test]
    fn outbound_timestamp_drift_backward() {
        let mut detector = EdgeDetector::new();
        let expected = target(PeerEdge::Right, test_addr());

        assert!(
            detector
                .check_outbound_target(Some(expected.clone()), 200_000, 0)
                .is_none()
        );

        assert!(
            detector
                .check_outbound_target(Some(expected.clone()), 190_000, 0)
                .is_none(),
            "timestamp going backward should not trigger"
        );

        let result = detector.check_outbound_target(Some(expected.clone()), 250_000, 0);
        assert_eq!(result, Some(expected));
    }

    #[test]
    fn return_edge_triggers_after_dwell() {
        let mut detector = EdgeDetector::new();

        let past_right = VirtualCoords::new(-7, 100);

        assert!(
            !detector.check_return_edge(past_right, 0, PeerEdge::Right),
            "first call should return false (timer started)"
        );

        assert!(
            !detector.check_return_edge(past_right, 40_000, PeerEdge::Right),
            "call before timeout should return false"
        );

        assert!(
            detector.check_return_edge(past_right, 50_000, PeerEdge::Right),
            "call after timeout should return true"
        );
    }

    #[test]
    fn return_edge_resets_on_reentry() {
        let mut detector = EdgeDetector::new();

        let past_right = VirtualCoords::new(-7, 100);
        let in_bounds = VirtualCoords::new(10, 100);

        assert!(!detector.check_return_edge(past_right, 0, PeerEdge::Right));

        assert!(
            !detector.check_return_edge(in_bounds, 10_000, PeerEdge::Right),
            "returning to bounds should reset state"
        );

        assert!(
            !detector.check_return_edge(past_right, 20_000, PeerEdge::Right),
            "re-entering return zone should start fresh timer"
        );

        assert!(
            detector.check_return_edge(past_right, 70_000, PeerEdge::Right),
            "should trigger after fresh dwell period"
        );
    }

    #[test]
    fn return_edge_directions() {
        let mut detector = EdgeDetector::new();

        let past_left = VirtualCoords::new(7, 100);
        assert!(!detector.check_return_edge(past_left, 0, PeerEdge::Left));
        assert!(detector.check_return_edge(past_left, 50_000, PeerEdge::Left));

        let mut detector = EdgeDetector::new();
        let past_top = VirtualCoords::new(100, 7);
        assert!(!detector.check_return_edge(past_top, 0, PeerEdge::Top));
        assert!(detector.check_return_edge(past_top, 50_000, PeerEdge::Top));

        let mut detector = EdgeDetector::new();
        let past_bottom = VirtualCoords::new(100, -7);
        assert!(!detector.check_return_edge(past_bottom, 0, PeerEdge::Bottom));
        assert!(detector.check_return_edge(past_bottom, 50_000, PeerEdge::Bottom));
    }

    #[test]
    fn reset_clears_both_states() {
        let mut detector = EdgeDetector::new();
        let target = target(PeerEdge::Right, test_addr());

        assert!(
            detector
                .check_outbound_target(Some(target.clone()), 0, 0)
                .is_none()
        );
        assert!(!detector.check_return_edge(VirtualCoords::new(-7, 100), 0, PeerEdge::Right));

        detector.reset();

        assert!(
            detector
                .check_outbound_target(Some(target.clone()), 0, 0)
                .is_none(),
            "after reset, outbound should start fresh"
        );

        let result = detector.check_outbound_target(Some(target.clone()), 40_000, 0);
        assert!(
            result.is_none(),
            "after reset, 40ms dwell should not trigger (timer started at 0)"
        );

        let result = detector.check_outbound_target(Some(target.clone()), 50_000, 0);
        assert_eq!(result, Some(target));
    }
}
