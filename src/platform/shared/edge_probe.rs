use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::net::{IpAddr, Ipv4Addr, SocketAddr};

use crate::core::layout::EdgeSpanBp;
use crate::platform::{DisplayBounds, PeerEdge};

use super::edge_detector::EdgeDetector;
use super::layout_state::{OutboundTarget, RuntimeContact};
use super::topology::{DisplayTopology, clamp_to_display_union};

const ALL_EDGES: [PeerEdge; 4] = [
    PeerEdge::Left,
    PeerEdge::Right,
    PeerEdge::Top,
    PeerEdge::Bottom,
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EdgeProbeSample {
    pub at_edge: bool,
    pub edge: Option<PeerEdge>,
    pub dwell_elapsed: bool,
}

pub struct EdgeProbe {
    detector: EdgeDetector,
    topology: DisplayTopology,
}

impl EdgeProbe {
    pub fn new(displays: Vec<DisplayBounds>) -> Self {
        Self {
            detector: EdgeDetector::new(),
            topology: topology_from_displays(displays),
        }
    }

    pub fn update_displays(&mut self, displays: Vec<DisplayBounds>) {
        self.topology = topology_from_displays(displays);
        self.detector = EdgeDetector::new();
    }

    pub fn signature(&self) -> u64 {
        self.topology.signature
    }

    pub fn displays(&self) -> &[DisplayBounds] {
        &self.topology.displays
    }

    pub fn virtual_bounds(&self) -> DisplayBounds {
        self.topology.virtual_bounds
    }

    pub fn sample(&mut self, cursor_x: i32, cursor_y: i32, sample_us: u64) -> EdgeProbeSample {
        let (cursor_x, cursor_y) = clamp_to_display_union(cursor_x, cursor_y, &self.topology);
        let edge = ALL_EDGES.iter().copied().find(|edge| {
            super::topology::is_within_edge_buffer(
                cursor_x as f64,
                cursor_y as f64,
                *edge,
                &self.topology,
            )
        });

        let detected = edge.map(|edge| OutboundTarget {
            contact: RuntimeContact {
                peer_id: "probe".to_string(),
                addr: SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), edge as u16),
                local: EdgeSpanBp {
                    edge,
                    start_bp: 0,
                    end_bp: 10_000,
                },
                remote: EdgeSpanBp {
                    edge,
                    start_bp: 0,
                    end_bp: 10_000,
                },
            },
        });
        let result = self.detector.check_outbound_target(detected, sample_us, 0);

        EdgeProbeSample {
            at_edge: edge.is_some(),
            edge,
            dwell_elapsed: result.is_some(),
        }
    }
}

fn topology_from_displays(displays: Vec<DisplayBounds>) -> DisplayTopology {
    let virtual_bounds = compute_virtual_bounds(&displays);
    DisplayTopology {
        signature: display_signature(&displays),
        displays,
        virtual_bounds,
    }
}

fn compute_virtual_bounds(displays: &[DisplayBounds]) -> DisplayBounds {
    if displays.is_empty() {
        return DisplayBounds {
            x: 0,
            y: 0,
            width: 1,
            height: 1,
        };
    }

    let mut min_x = i32::MAX;
    let mut min_y = i32::MAX;
    let mut max_x = i32::MIN;
    let mut max_y = i32::MIN;

    for display in displays {
        min_x = min_x.min(display.x);
        min_y = min_y.min(display.y);
        max_x = max_x.max(display.x + display.width as i32);
        max_y = max_y.max(display.y + display.height as i32);
    }

    DisplayBounds {
        x: min_x,
        y: min_y,
        width: max_x.saturating_sub(min_x).max(1) as u32,
        height: max_y.saturating_sub(min_y).max(1) as u32,
    }
}

fn display_signature(displays: &[DisplayBounds]) -> u64 {
    let mut ordered = displays.to_vec();
    ordered.sort_unstable_by_key(|display| (display.x, display.y, display.width, display.height));

    let mut hasher = DefaultHasher::new();
    for display in &ordered {
        display.x.hash(&mut hasher);
        display.y.hash(&mut hasher);
        display.width.hash(&mut hasher);
        display.height.hash(&mut hasher);
    }
    hasher.finish()
}

#[cfg(test)]
mod tests {
    use super::EdgeProbe;
    use crate::platform::{DisplayBounds, PeerEdge};

    #[test]
    fn sample_reports_stacked_outer_edges_only() {
        let displays = vec![
            DisplayBounds {
                x: 0,
                y: 0,
                width: 1920,
                height: 1080,
            },
            DisplayBounds {
                x: 0,
                y: 1080,
                width: 1920,
                height: 1080,
            },
        ];
        let mut probe = EdgeProbe::new(displays);

        let top = probe.sample(500, 0, 0);
        assert_eq!(top.edge, Some(PeerEdge::Top));
        assert!(top.at_edge);

        let internal = probe.sample(500, 1080, 1_000);
        assert_eq!(internal.edge, None);
        assert!(!internal.at_edge);
    }

    #[test]
    fn sample_reports_dwell_once_per_edge_contact() {
        let displays = vec![DisplayBounds {
            x: 0,
            y: 0,
            width: 1920,
            height: 1080,
        }];
        let mut probe = EdgeProbe::new(displays);

        assert!(!probe.sample(1919, 400, 0).dwell_elapsed);
        assert!(!probe.sample(1919, 400, 40_000).dwell_elapsed);
        assert!(probe.sample(1919, 400, 50_000).dwell_elapsed);
        assert!(!probe.sample(1919, 400, 80_000).dwell_elapsed);

        let cleared = probe.sample(1000, 400, 90_000);
        assert!(!cleared.at_edge);

        assert!(!probe.sample(1919, 400, 100_000).dwell_elapsed);
        assert!(probe.sample(1919, 400, 150_000).dwell_elapsed);
    }

    #[test]
    fn sample_clamps_one_past_right_and_bottom_edges() {
        let displays = vec![DisplayBounds {
            x: 0,
            y: 0,
            width: 2056,
            height: 1329,
        }];
        let mut probe = EdgeProbe::new(displays);

        let right = probe.sample(2056, 640, 0);
        assert_eq!(right.edge, Some(PeerEdge::Right));
        assert!(right.at_edge);

        let bottom = probe.sample(1000, 1329, 1_000);
        assert_eq!(bottom.edge, Some(PeerEdge::Bottom));
        assert!(bottom.at_edge);
    }
}
