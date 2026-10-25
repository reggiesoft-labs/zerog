//! Display topology tracking for edge detection.
//!
//! This module provides utilities for tracking the display layout and
//! detecting when the cursor is at an outer edge segment that should trigger a
//! transition.
//!
//! # Edge Buffer and Hysteresis
//!
//! - `EDGE_BUFFER_PX` (3px): Zone at screen edge considered "at the edge"
//! - `EDGE_HYSTERESIS_PX` (6px): Additional distance required to re-arm
//!   edge detection after a transition
//!
//! The hysteresis prevents rapid back-and-forth transitions when the cursor
//! is near the edge.

use crate::core::layout::EdgeSpanBp;
use crate::platform::{DisplayBounds, DisplayManager, PeerEdge};
use anyhow::Result;

/// Distance from screen edge considered "at the edge" for transitions.
pub(crate) const EDGE_BUFFER_PX: f64 = 6.0;
/// Offset into the target display when entering across a boundary.
pub(crate) const ENTRY_OFFSET_PX: f64 = 8.0;

/// Snapshot of the current display configuration.
#[derive(Clone)]
pub(crate) struct DisplayTopology {
    /// Bounds of all connected displays.
    pub(crate) displays: Vec<DisplayBounds>,
    /// Combined virtual bounds spanning all displays.
    pub(crate) virtual_bounds: DisplayBounds,
    /// Signature that changes when display configuration changes.
    pub(crate) signature: u64,
}

impl DisplayTopology {
    /// Discovers the current display configuration.
    pub(crate) fn discover(display: &dyn DisplayManager) -> Result<Self> {
        Ok(Self {
            displays: display.discover_displays()?,
            virtual_bounds: display.get_virtual_bounds()?,
            signature: display.get_signature()?,
        })
    }
}

/// Returns true if the cursor is within the edge buffer zone of the union of
/// all display rectangles.
pub(crate) fn is_within_edge_buffer(
    x: f64,
    y: f64,
    edge: PeerEdge,
    topology: &DisplayTopology,
) -> bool {
    is_within_edge_buffer_with_threshold(x, y, edge, topology, EDGE_BUFFER_PX)
}

fn is_within_edge_buffer_with_threshold(
    x: f64,
    y: f64,
    edge: PeerEdge,
    topology: &DisplayTopology,
    threshold: f64,
) -> bool {
    if topology.displays.is_empty() {
        let (min_x, max_x, min_y, max_y) = display_limits(&topology.virtual_bounds);

        return match edge {
            PeerEdge::Left => y >= min_y && y <= max_y && x <= min_x + threshold,
            PeerEdge::Right => y >= min_y && y <= max_y && x >= max_x - threshold,
            PeerEdge::Top => x >= min_x && x <= max_x && y <= min_y + threshold,
            PeerEdge::Bottom => x >= min_x && x <= max_x && y >= max_y - threshold,
        };
    }

    topology.displays.iter().any(|display| {
        point_in_display(x, y, display)
            && is_near_display_edge(x, y, edge, display, threshold)
            && display_has_outer_edge_segment(
                display,
                edge,
                edge_axis(edge, x, y),
                &topology.displays,
            )
    })
}

fn point_in_display(x: f64, y: f64, display: &DisplayBounds) -> bool {
    let (min_x, max_x, min_y, max_y) = display_limits(display);
    x >= min_x && x <= max_x && y >= min_y && y <= max_y
}

fn is_near_display_edge(
    x: f64,
    y: f64,
    edge: PeerEdge,
    display: &DisplayBounds,
    threshold: f64,
) -> bool {
    let (min_x, max_x, min_y, max_y) = display_limits(display);

    match edge {
        PeerEdge::Left => y >= min_y && y <= max_y && x <= min_x + threshold,
        PeerEdge::Right => y >= min_y && y <= max_y && x >= max_x - threshold,
        PeerEdge::Top => x >= min_x && x <= max_x && y <= min_y + threshold,
        PeerEdge::Bottom => x >= min_x && x <= max_x && y >= max_y - threshold,
    }
}

fn edge_axis(edge: PeerEdge, x: f64, y: f64) -> f64 {
    match edge {
        PeerEdge::Left | PeerEdge::Right => y,
        PeerEdge::Top | PeerEdge::Bottom => x,
    }
}

fn display_has_outer_edge_segment(
    display: &DisplayBounds,
    edge: PeerEdge,
    axis: f64,
    displays: &[DisplayBounds],
) -> bool {
    if !display_contains_edge_axis(display, edge, axis) {
        return false;
    }

    let (probe_x, probe_y) = outward_probe_point(display, edge, axis);
    displays
        .iter()
        .all(|candidate| !point_in_display(probe_x, probe_y, candidate))
}

fn display_contains_edge_axis(display: &DisplayBounds, edge: PeerEdge, axis: f64) -> bool {
    let (min_x, max_x, min_y, max_y) = display_limits(display);

    match edge {
        PeerEdge::Left | PeerEdge::Right => axis >= min_y && axis <= max_y,
        PeerEdge::Top | PeerEdge::Bottom => axis >= min_x && axis <= max_x,
    }
}

fn outward_probe_point(display: &DisplayBounds, edge: PeerEdge, axis: f64) -> (f64, f64) {
    let (min_x, max_x, min_y, max_y) = display_limits(display);

    match edge {
        PeerEdge::Left => (min_x - 1.0, axis.clamp(min_y, max_y)),
        PeerEdge::Right => (max_x + 1.0, axis.clamp(min_y, max_y)),
        PeerEdge::Top => (axis.clamp(min_x, max_x), min_y - 1.0),
        PeerEdge::Bottom => (axis.clamp(min_x, max_x), max_y + 1.0),
    }
}

#[cfg(test)]
/// Calculates the re-entry point when transitioning back from remote control.
///
/// Places the cursor at the edge of the local display, offset by the
/// hysteresis distance to prevent immediate re-transition.
///
/// When `peer_bounds` is provided and the screens have different sizes,
/// the perpendicular axis is proportionally mapped rather than clamped,
/// so a cursor at 80% of a tall remote screen lands at 80% of the local
/// screen instead of being pinned to the bottom.
#[cfg(test)]
pub(crate) fn reentry_point_from_virtual(
    edge: PeerEdge,
    topology: &DisplayTopology,
    virtual_x: i32,
    virtual_y: i32,
    peer_bounds: Option<DisplayBounds>,
) -> (i32, i32) {
    let target_display = find_display_at(virtual_x, virtual_y, topology).unwrap_or_else(|| {
        best_display_at_edge(
            edge,
            topology,
            edge_axis(edge, virtual_x as f64, virtual_y as f64),
        )
    });

    let min_x = target_display.x as f64;
    let min_y = target_display.y as f64;
    let max_x = target_display.x as f64 + target_display.width as f64 - 1.0;
    let max_y = target_display.y as f64 + target_display.height as f64 - 1.0;
    let reentry_offset = ENTRY_OFFSET_PX;

    // For the perpendicular axis, proportionally map using the peer's display
    // bounds when available, so different aspect ratios translate smoothly.
    let mapped_x = proportional_map(
        virtual_x as f64,
        peer_bounds.map(|b| (b.x as f64, b.width as f64)),
        min_x,
        max_x,
    );
    let mapped_y = proportional_map(
        virtual_y as f64,
        peer_bounds.map(|b| (b.y as f64, b.height as f64)),
        min_y,
        max_y,
    );

    match edge {
        PeerEdge::Left => (
            (min_x + reentry_offset).round() as i32,
            mapped_y.round() as i32,
        ),
        PeerEdge::Right => (
            (max_x - reentry_offset).round() as i32,
            mapped_y.round() as i32,
        ),
        PeerEdge::Top => (
            mapped_x.round() as i32,
            (min_y + reentry_offset).round() as i32,
        ),
        PeerEdge::Bottom => (
            mapped_x.round() as i32,
            (max_y - reentry_offset).round() as i32,
        ),
    }
}

pub(crate) fn edge_point_from_contact_span_bp(
    span: EdgeSpanBp,
    topology: &DisplayTopology,
    position_bp: u16,
) -> (i32, i32) {
    let bounds = topology.virtual_bounds;
    let axis = edge_axis_from_span(span, position_bp, bounds);
    let target_display = best_display_at_edge(span.edge, topology, axis);
    let min_x = target_display.x as f64;
    let min_y = target_display.y as f64;
    let max_x = target_display.x as f64 + target_display.width as f64 - 1.0;
    let max_y = target_display.y as f64 + target_display.height as f64 - 1.0;
    let offset = ENTRY_OFFSET_PX;

    match span.edge {
        PeerEdge::Left => (
            (min_x + offset).round() as i32,
            axis.clamp(min_y, max_y).round() as i32,
        ),
        PeerEdge::Right => (
            (max_x - offset).round() as i32,
            axis.clamp(min_y, max_y).round() as i32,
        ),
        PeerEdge::Top => (
            axis.clamp(min_x, max_x).round() as i32,
            (min_y + offset).round() as i32,
        ),
        PeerEdge::Bottom => (
            axis.clamp(min_x, max_x).round() as i32,
            (max_y - offset).round() as i32,
        ),
    }
}

fn edge_axis_from_span(span: EdgeSpanBp, position_bp: u16, bounds: DisplayBounds) -> f64 {
    let (origin, size) = match span.edge {
        PeerEdge::Left | PeerEdge::Right => (bounds.y, bounds.height),
        PeerEdge::Top | PeerEdge::Bottom => (bounds.x, bounds.width),
    };
    let max = f64::from(origin) + f64::from(size.max(1)) - 1.0;
    let span_start = map_basis_points(
        f64::from(span.start_bp),
        0.0,
        10_000.0,
        f64::from(origin),
        max,
    );
    let span_end = map_basis_points(
        f64::from(span.end_bp),
        0.0,
        10_000.0,
        f64::from(origin),
        max,
    );
    map_basis_points(
        f64::from(span.clamp(position_bp)),
        f64::from(span.start_bp),
        f64::from(span.end_bp.max(span.start_bp.saturating_add(1))),
        span_start,
        span_end,
    )
}

fn map_basis_points(
    value: f64,
    source_start: f64,
    source_end: f64,
    target_start: f64,
    target_end: f64,
) -> f64 {
    let source_size = (source_end - source_start).max(1.0);
    let target_size = target_end - target_start + 1.0;
    let fraction = (value - source_start) / source_size;
    (target_start + fraction * target_size).clamp(target_start, target_end)
}

pub(crate) fn edge_position_bp_from_bounds(
    edge: PeerEdge,
    bounds: DisplayBounds,
    virtual_x: i32,
    virtual_y: i32,
) -> u16 {
    let (origin, size, value) = match edge {
        PeerEdge::Left | PeerEdge::Right => (bounds.y, bounds.height.max(1), virtual_y),
        PeerEdge::Top | PeerEdge::Bottom => (bounds.x, bounds.width.max(1), virtual_x),
    };
    if size <= 1 {
        return 0;
    }
    let max_value = origin + size as i32 - 1;
    let clamped = value.clamp(origin, max_value) - origin;
    let ratio = f64::from(clamped.max(0)) / f64::from(size - 1);
    ((ratio * 10_000.0).round() as u16).min(10_000)
}

#[cfg(test)]
pub(crate) fn proportional_map(
    value: f64,
    peer_range: Option<(f64, f64)>,
    local_min: f64,
    local_max: f64,
) -> f64 {
    let Some((peer_origin, peer_size)) = peer_range else {
        return value.clamp(local_min, local_max);
    };
    if peer_size <= 0.0 {
        return value.clamp(local_min, local_max);
    }
    let local_size = local_max - local_min + 1.0;
    let ratio = (value - peer_origin) / peer_size;
    (local_min + ratio * local_size).clamp(local_min, local_max)
}

/// Returns a safe center point for cursor recovery.
///
/// Used when the cursor position is unknown or invalid.
pub(crate) fn recovery_point(topology: &DisplayTopology) -> (i32, i32) {
    let bounds = topology
        .displays
        .iter()
        .copied()
        .find(|display| {
            0 >= display.x
                && 0 < display.x.saturating_add(display.width as i32)
                && 0 >= display.y
                && 0 < display.y.saturating_add(display.height as i32)
        })
        .or_else(|| topology.displays.first().copied())
        .unwrap_or(topology.virtual_bounds);
    let min_x = bounds.x as f64;
    let min_y = bounds.y as f64;
    let max_x = bounds.x as f64 + bounds.width as f64 - 1.0;
    let max_y = bounds.y as f64 + bounds.height as f64 - 1.0;
    (
        ((min_x + max_x) / 2.0).round() as i32,
        ((min_y + max_y) / 2.0).round() as i32,
    )
}

pub(crate) fn display_limits(bounds: &DisplayBounds) -> (f64, f64, f64, f64) {
    let min_x = bounds.x as f64;
    let min_y = bounds.y as f64;
    let max_x = min_x + bounds.width as f64 - 1.0;
    let max_y = min_y + bounds.height as f64 - 1.0;
    (min_x, max_x, min_y, max_y)
}

pub(crate) fn clamp_to_display_union(x: i32, y: i32, topology: &DisplayTopology) -> (i32, i32) {
    if find_display_at(x, y, topology).is_some() {
        return (x, y);
    }

    let mut best = clamp_point_to_bounds(x, y, topology.virtual_bounds);
    let mut best_distance = i64::MAX;

    for display in &topology.displays {
        let candidate = clamp_point_to_bounds(x, y, *display);
        let dx = i64::from(x) - i64::from(candidate.0);
        let dy = i64::from(y) - i64::from(candidate.1);
        let distance = dx * dx + dy * dy;
        if distance < best_distance {
            best_distance = distance;
            best = candidate;
        }
    }

    best
}

fn clamp_point_to_bounds(x: i32, y: i32, bounds: DisplayBounds) -> (i32, i32) {
    let max_x = bounds.x + bounds.width as i32 - 1;
    let max_y = bounds.y + bounds.height as i32 - 1;
    (x.clamp(bounds.x, max_x), y.clamp(bounds.y, max_y))
}

pub(crate) fn find_display_at(x: i32, y: i32, topology: &DisplayTopology) -> Option<DisplayBounds> {
    topology
        .displays
        .iter()
        .find(|d| {
            let x_i64 = x as i64;
            let y_i64 = y as i64;
            let d_x_i64 = d.x as i64;
            let d_y_i64 = d.y as i64;
            let d_w_i64 = d.width as i64;
            let d_h_i64 = d.height as i64;

            x_i64 >= d_x_i64
                && x_i64 < d_x_i64 + d_w_i64
                && y_i64 >= d_y_i64
                && y_i64 < d_y_i64 + d_h_i64
        })
        .copied()
}

pub(crate) fn find_display_at_edge(
    edge: PeerEdge,
    topology: &DisplayTopology,
    axis: f64,
) -> Option<DisplayBounds> {
    topology
        .displays
        .iter()
        .find(|display| display_has_outer_edge_segment(display, edge, axis, &topology.displays))
        .copied()
}

pub(crate) fn best_display_at_edge(
    edge: PeerEdge,
    topology: &DisplayTopology,
    axis: f64,
) -> DisplayBounds {
    if let Some(display) = find_display_at_edge(edge, topology, axis) {
        return display;
    }

    let outer_displays: Vec<DisplayBounds> = topology
        .displays
        .iter()
        .copied()
        .filter(|d| {
            let (min_x, max_x, min_y, max_y) = display_limits(d);
            let test_axis = match edge {
                PeerEdge::Left | PeerEdge::Right => (min_y + max_y) / 2.0,
                PeerEdge::Top | PeerEdge::Bottom => (min_x + max_x) / 2.0,
            };
            display_has_outer_edge_segment(d, edge, test_axis, &topology.displays)
        })
        .collect();

    let candidate_pool = if !outer_displays.is_empty() {
        &outer_displays
    } else if !topology.displays.is_empty() {
        &topology.displays
    } else {
        return topology.virtual_bounds;
    };

    candidate_pool
        .iter()
        .copied()
        .min_by(|a, b| {
            let primary_cmp = match edge {
                PeerEdge::Left => a.x.cmp(&b.x),
                PeerEdge::Right => (b.x + b.width as i32).cmp(&(a.x + a.width as i32)),
                PeerEdge::Top => a.y.cmp(&b.y),
                PeerEdge::Bottom => (b.y + b.height as i32).cmp(&(a.y + a.height as i32)),
            };
            if primary_cmp != std::cmp::Ordering::Equal {
                return primary_cmp;
            }
            let dist_a = display_axis_distance(a, edge, axis);
            let dist_b = display_axis_distance(b, edge, axis);
            dist_a.total_cmp(&dist_b)
        })
        .unwrap_or(topology.virtual_bounds)
}

fn display_axis_distance(display: &DisplayBounds, edge: PeerEdge, axis: f64) -> f64 {
    let (min_x, max_x, min_y, max_y) = display_limits(display);
    match edge {
        PeerEdge::Left | PeerEdge::Right => {
            if axis < min_y {
                min_y - axis
            } else if axis > max_y {
                axis - max_y
            } else {
                0.0
            }
        }
        PeerEdge::Top | PeerEdge::Bottom => {
            if axis < min_x {
                min_x - axis
            } else if axis > max_x {
                axis - max_x
            } else {
                0.0
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        DisplayTopology, best_display_at_edge, clamp_to_display_union,
        edge_point_from_contact_span_bp, find_display_at_edge, is_within_edge_buffer,
        reentry_point_from_virtual,
    };
    use crate::core::layout::EdgeSpanBp;
    use crate::platform::{DisplayBounds, PeerEdge};

    #[test]
    fn edge_buffer_uses_virtual_bounds_with_negative_origin() {
        let topology = DisplayTopology {
            displays: vec![
                DisplayBounds {
                    x: -1920,
                    y: -200,
                    width: 1920,
                    height: 1080,
                },
                DisplayBounds {
                    x: 0,
                    y: -100,
                    width: 2560,
                    height: 1440,
                },
            ],
            virtual_bounds: DisplayBounds {
                x: -1920,
                y: -200,
                width: 4480,
                height: 1540,
            },
            signature: 7,
        };

        assert!(is_within_edge_buffer(
            -1919.0,
            600.0,
            PeerEdge::Left,
            &topology
        ));
        assert!(is_within_edge_buffer(
            2559.0,
            0.0,
            PeerEdge::Right,
            &topology
        ));
        assert!(is_within_edge_buffer(
            -1910.0,
            -199.0,
            PeerEdge::Top,
            &topology
        ));
        assert!(is_within_edge_buffer(
            10.0,
            1339.0,
            PeerEdge::Bottom,
            &topology
        ));
    }

    #[test]
    fn edge_buffer_ignores_internal_display_edges() {
        let topology = DisplayTopology {
            displays: vec![
                DisplayBounds {
                    x: 0,
                    y: 0,
                    width: 1920,
                    height: 1080,
                },
                DisplayBounds {
                    x: 2400,
                    y: 0,
                    width: 1920,
                    height: 1080,
                },
            ],
            virtual_bounds: DisplayBounds {
                x: 0,
                y: 0,
                width: 4320,
                height: 1080,
            },
            signature: 9,
        };

        assert!(is_within_edge_buffer(
            1919.0,
            400.0,
            PeerEdge::Right,
            &topology
        ));
        assert!(is_within_edge_buffer(
            2400.0,
            400.0,
            PeerEdge::Left,
            &topology
        ));
        assert!(is_within_edge_buffer(
            4319.0,
            400.0,
            PeerEdge::Right,
            &topology
        ));
    }

    #[test]
    fn edge_buffer_ignores_shared_edge_when_display_is_stacked_above() {
        let topology = DisplayTopology {
            displays: vec![
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
            ],
            virtual_bounds: DisplayBounds {
                x: 0,
                y: 0,
                width: 1920,
                height: 2160,
            },
            signature: 10,
        };

        assert!(!is_within_edge_buffer(
            500.0,
            1080.0,
            PeerEdge::Top,
            &topology
        ));
        assert!(is_within_edge_buffer(500.0, 0.0, PeerEdge::Top, &topology));
        assert!(is_within_edge_buffer(
            500.0,
            2159.0,
            PeerEdge::Bottom,
            &topology
        ));
    }

    #[test]
    fn edge_buffer_uses_union_boundary_for_partial_overlap() {
        let topology = DisplayTopology {
            displays: vec![
                DisplayBounds {
                    x: 0,
                    y: 0,
                    width: 1200,
                    height: 700,
                },
                DisplayBounds {
                    x: 200,
                    y: 700,
                    width: 1200,
                    height: 700,
                },
            ],
            virtual_bounds: DisplayBounds {
                x: 0,
                y: 0,
                width: 1400,
                height: 1400,
            },
            signature: 11,
        };

        assert!(is_within_edge_buffer(
            1300.0,
            700.0,
            PeerEdge::Top,
            &topology
        ));
        assert!(!is_within_edge_buffer(
            300.0,
            700.0,
            PeerEdge::Top,
            &topology
        ));
    }

    #[test]
    fn find_display_at_edge_uses_edge_segment_axis() {
        let top = DisplayBounds {
            x: 0,
            y: 0,
            width: 1920,
            height: 1080,
        };
        let bottom = DisplayBounds {
            x: 0,
            y: 1200,
            width: 1920,
            height: 1080,
        };
        let topology = DisplayTopology {
            displays: vec![top, bottom],
            virtual_bounds: DisplayBounds {
                x: 0,
                y: 0,
                width: 1920,
                height: 2280,
            },
            signature: 12,
        };

        assert_eq!(
            find_display_at_edge(PeerEdge::Left, &topology, 200.0),
            Some(top)
        );
        assert_eq!(
            find_display_at_edge(PeerEdge::Left, &topology, 1400.0),
            Some(bottom)
        );
        assert_eq!(
            find_display_at_edge(PeerEdge::Left, &topology, 1150.0),
            None
        );
    }

    #[test]
    fn best_display_at_edge_falls_back_to_closest_outer_display() {
        let left = DisplayBounds {
            x: 0,
            y: 0,
            width: 1920,
            height: 1080,
        };
        let right = DisplayBounds {
            x: 1920,
            y: 0,
            width: 1920,
            height: 1080,
        };
        let topology = DisplayTopology {
            displays: vec![left, right],
            virtual_bounds: DisplayBounds {
                x: 0,
                y: 0,
                width: 3840,
                height: 1080,
            },
            signature: 15,
        };

        // On the right edge, even with out-of-range axis, it chooses the right display
        assert_eq!(
            best_display_at_edge(PeerEdge::Right, &topology, 1500.0),
            right
        );
        // On the left edge, it chooses the left display
        assert_eq!(
            best_display_at_edge(PeerEdge::Left, &topology, 1500.0),
            left
        );
    }

    #[test]
    fn clamp_to_display_union_pulls_one_past_right_and_bottom_back_in_bounds() {
        let topology = DisplayTopology {
            displays: vec![DisplayBounds {
                x: 0,
                y: 0,
                width: 2056,
                height: 1329,
            }],
            virtual_bounds: DisplayBounds {
                x: 0,
                y: 0,
                width: 2056,
                height: 1329,
            },
            signature: 13,
        };

        assert_eq!(clamp_to_display_union(2056, 640, &topology), (2055, 640));
        assert_eq!(clamp_to_display_union(1000, 1329, &topology), (1000, 1328));
    }

    #[test]
    fn reentry_point_from_virtual_preserves_perpendicular_axis() {
        let topology = DisplayTopology {
            displays: vec![DisplayBounds {
                x: 0,
                y: 0,
                width: 2560,
                height: 1440,
            }],
            virtual_bounds: DisplayBounds {
                x: 0,
                y: 0,
                width: 2560,
                height: 1440,
            },
            signature: 1,
        };

        // reentry_offset = ENTRY_OFFSET_PX = 8
        assert_eq!(
            reentry_point_from_virtual(PeerEdge::Right, &topology, -12, 200, None),
            (2551, 200)
        );
        assert_eq!(
            reentry_point_from_virtual(PeerEdge::Left, &topology, 18, 1337, None),
            (8, 1337)
        );
        assert_eq!(
            reentry_point_from_virtual(PeerEdge::Top, &topology, 1440, 15, None),
            (1440, 8)
        );
        assert_eq!(
            reentry_point_from_virtual(PeerEdge::Bottom, &topology, 32, -15, None),
            (32, 1431)
        );
    }

    #[test]
    fn reentry_point_from_virtual_clamps_to_virtual_bounds() {
        let topology = DisplayTopology {
            displays: vec![DisplayBounds {
                x: -100,
                y: 50,
                width: 300,
                height: 200,
            }],
            virtual_bounds: DisplayBounds {
                x: -100,
                y: 50,
                width: 300,
                height: 200,
            },
            signature: 42,
        };

        // reentry_offset = 8, max_x = -100+300-1 = 199, max_y = 50+200-1 = 249
        assert_eq!(
            reentry_point_from_virtual(PeerEdge::Right, &topology, 10_000, -10_000, None),
            (191, 50)
        );
        assert_eq!(
            reentry_point_from_virtual(PeerEdge::Bottom, &topology, 10_000, -10_000, None),
            (199, 241)
        );
    }

    #[test]
    fn reentry_proportional_maps_perpendicular_axis_with_peer_bounds() {
        // Local display: 2560x1440
        let topology = DisplayTopology {
            displays: vec![DisplayBounds {
                x: 0,
                y: 0,
                width: 2560,
                height: 1440,
            }],
            virtual_bounds: DisplayBounds {
                x: 0,
                y: 0,
                width: 2560,
                height: 1440,
            },
            signature: 1,
        };

        // Peer display: 1920x2560 (tall portrait monitor)
        let peer = Some(DisplayBounds {
            x: 0,
            y: 0,
            width: 1920,
            height: 2560,
        });

        // Cursor at y=2000 on peer (2000/2559 ≈ 78.2% down)
        // Should map to 78.2% of local height: 0.782 * 1439 ≈ 1125
        let (rx, ry) = reentry_point_from_virtual(PeerEdge::Right, &topology, -5, 2000, peer);
        assert_eq!(rx, 2551); // reentry offset from right edge
        assert!((ry - 1125).abs() <= 1, "expected ~1125, got {ry}");

        // Cursor at y=0 on peer (top) should map to top of local
        let (_, ry) = reentry_point_from_virtual(PeerEdge::Right, &topology, -5, 0, peer);
        assert_eq!(ry, 0);

        // Cursor at y=2559 on peer (bottom) should map to bottom of local
        let (_, ry) = reentry_point_from_virtual(PeerEdge::Right, &topology, -5, 2559, peer);
        assert_eq!(ry, 1439);

        // Cursor at y=1280 (50% of peer) should map to ~720 (50% of local)
        let (_, ry) = reentry_point_from_virtual(PeerEdge::Right, &topology, -5, 1280, peer);
        assert_eq!(ry, 720);
    }

    #[test]
    fn edge_point_from_contact_span_bp_resolves_within_contact_segment() {
        let topology = DisplayTopology {
            displays: vec![DisplayBounds {
                x: 100,
                y: 50,
                width: 300,
                height: 200,
            }],
            virtual_bounds: DisplayBounds {
                x: 100,
                y: 50,
                width: 300,
                height: 200,
            },
            signature: 1,
        };
        let span = EdgeSpanBp {
            edge: PeerEdge::Left,
            start_bp: 2_000,
            end_bp: 6_000,
        };

        assert_eq!(
            edge_point_from_contact_span_bp(span, &topology, span.start_bp),
            (108, 90)
        );
        assert_eq!(
            edge_point_from_contact_span_bp(span, &topology, 4_000),
            (108, 131)
        );
        assert_eq!(
            edge_point_from_contact_span_bp(span, &topology, span.end_bp),
            (108, 170)
        );
    }

    #[test]
    fn edge_position_bp_from_bounds_reaches_edge_endpoint() {
        let bounds = DisplayBounds {
            x: 100,
            y: 50,
            width: 300,
            height: 200,
        };

        assert_eq!(
            super::edge_position_bp_from_bounds(PeerEdge::Left, bounds, 100, 50),
            0
        );
        assert_eq!(
            super::edge_position_bp_from_bounds(PeerEdge::Left, bounds, 100, 249),
            10_000
        );
        assert_eq!(
            super::edge_position_bp_from_bounds(PeerEdge::Top, bounds, 399, 50),
            10_000
        );
    }
}
