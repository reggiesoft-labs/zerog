//! Derived layout contacts used by the runtime.
//!
//! Persisted layout placements are editor-space coordinates. This module turns
//! those placements into a runtime-oriented contact model describing which span
//! of a local edge maps to which span of a peer edge.

use std::collections::HashMap;

use crate::core::api::ConfigSnapshot;
use crate::core::events::{LayoutPlacement, LayoutUpdateEvent};
use crate::core::peer_state::canonical_peer_id;
use crate::core::platform::{DisplayBounds, PeerEdge};

const DEFAULT_BOUNDS_WIDTH: u32 = 1440;
const DEFAULT_BOUNDS_HEIGHT: u32 = 900;
const EDGE_MARGIN: f64 = 8.0;
const BASIS_POINTS: u16 = 10_000;
const MAX_NODE_WIDTH: f64 = 260.0;
const MAX_NODE_HEIGHT: f64 = 190.0;
const MIN_NODE_WIDTH: f64 = 96.0;
const MIN_NODE_HEIGHT: f64 = 64.0;
const REFERENCE_WIDTH: f64 = 1920.0;
const REFERENCE_HEIGHT: f64 = 1080.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EdgeSpanBp {
    pub edge: PeerEdge,
    pub start_bp: u16,
    pub end_bp: u16,
}

impl EdgeSpanBp {
    pub fn contains(self, position_bp: u16) -> bool {
        position_bp >= self.start_bp && position_bp <= self.end_bp
    }

    pub fn center_bp(self) -> u16 {
        self.start_bp + (self.end_bp - self.start_bp) / 2
    }

    pub fn clamp(self, position_bp: u16) -> u16 {
        position_bp.clamp(self.start_bp, self.end_bp)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LayoutContact {
    pub peer_id: String,
    pub local: EdgeSpanBp,
    pub remote: EdgeSpanBp,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ResolvedLayout {
    contacts: Vec<LayoutContact>,
}

impl ResolvedLayout {
    pub fn new(contacts: Vec<LayoutContact>) -> Self {
        Self { contacts }
    }

    pub fn contacts(&self) -> &[LayoutContact] {
        &self.contacts
    }

    pub fn contacts_on_edge(&self, edge: PeerEdge) -> impl Iterator<Item = &LayoutContact> {
        self.contacts
            .iter()
            .filter(move |contact| contact.local.edge == edge)
    }

    pub fn contact_for_position(&self, edge: PeerEdge, position_bp: u16) -> Option<&LayoutContact> {
        self.contacts_on_edge(edge)
            .filter(|contact| contact.local.contains(position_bp))
            .min_by_key(|contact| {
                let center = i32::from(contact.local.center_bp());
                let position = i32::from(position_bp);
                (center - position).abs()
            })
    }
}

pub fn resolved_layout_from_config(
    config: &ConfigSnapshot,
    local_bounds: Option<DisplayBounds>,
    peer_bounds_by_id: &HashMap<String, DisplayBounds>,
) -> ResolvedLayout {
    let local_bounds = local_bounds.unwrap_or(default_bounds());
    let mut contacts = Vec::new();

    for placement in &config.placements {
        let peer_id = canonical_peer_id(&placement.peer_id);
        if peer_id.is_empty() {
            continue;
        }

        let peer_bounds = peer_bounds_by_id
            .get(&peer_id)
            .copied()
            .unwrap_or_else(default_bounds);
        let Some(contact) = contact_from_placement(&peer_id, placement, local_bounds, peer_bounds)
        else {
            continue;
        };
        contacts.push(contact);
    }

    ResolvedLayout::new(contacts)
}

pub fn bootstrap_edge_for_placement(
    placement: &LayoutPlacement,
    peer_bounds: Option<DisplayBounds>,
) -> PeerEdge {
    let local_bounds = default_bounds();
    let peer_bounds = peer_bounds.unwrap_or_else(default_bounds);
    contact_edge_for_placement(placement, local_bounds, peer_bounds)
}

pub fn map_position_bp(position_bp: u16, from: EdgeSpanBp, to: EdgeSpanBp) -> u16 {
    let from_len = u32::from(from.end_bp.saturating_sub(from.start_bp).max(1));
    let to_len = u32::from(to.end_bp.saturating_sub(to.start_bp).max(1));
    let clamped = from.clamp(position_bp);
    let relative = u32::from(clamped.saturating_sub(from.start_bp));
    let mapped = u32::from(to.start_bp) + ((relative * to_len) / from_len);
    mapped.min(u32::from(to.end_bp)) as u16
}

pub fn rebase_layout_update(
    mut update: LayoutUpdateEvent,
    sender_peer_id: &str,
    receiver_peer_id: &str,
) -> Option<LayoutUpdateEvent> {
    let sender_peer_id = canonical_peer_id(sender_peer_id);
    let receiver_peer_id = canonical_peer_id(receiver_peer_id);
    if sender_peer_id.is_empty()
        || receiver_peer_id.is_empty()
        || sender_peer_id == receiver_peer_id
    {
        return None;
    }

    let receiver_origin = update
        .placements
        .iter()
        .find(|placement| canonical_peer_id(&placement.peer_id) == receiver_peer_id)
        .map(|placement| (placement.x, placement.y))?;

    let mut sender_present = false;
    let mut placements = Vec::with_capacity(update.placements.len() + 1);
    for mut placement in update.placements {
        let peer_id = canonical_peer_id(&placement.peer_id);
        if peer_id.is_empty() || peer_id == receiver_peer_id {
            continue;
        }

        placement.peer_id = peer_id.clone();
        placement.x = placement.x.saturating_sub(receiver_origin.0);
        placement.y = placement.y.saturating_sub(receiver_origin.1);
        sender_present |= peer_id == sender_peer_id;
        placements.push(placement);
    }

    if !sender_present {
        placements.push(LayoutPlacement {
            peer_id: sender_peer_id,
            peer_label: String::new(),
            x: receiver_origin.0.saturating_neg(),
            y: receiver_origin.1.saturating_neg(),
        });
    }

    update.placements = placements;
    Some(update)
}

fn default_bounds() -> DisplayBounds {
    DisplayBounds {
        x: 0,
        y: 0,
        width: DEFAULT_BOUNDS_WIDTH,
        height: DEFAULT_BOUNDS_HEIGHT,
    }
}

fn contact_from_placement(
    peer_id: &str,
    placement: &LayoutPlacement,
    local_bounds: DisplayBounds,
    peer_bounds: DisplayBounds,
) -> Option<LayoutContact> {
    let local_size = node_size(local_bounds);
    let peer_size = node_size(peer_bounds);
    let edge = contact_edge_for_placement(placement, local_bounds, peer_bounds);

    let (local_start, local_end, peer_start, peer_end, remote_edge) = match edge {
        PeerEdge::Left | PeerEdge::Right => {
            let local_top = -(local_size.height / 2.0);
            let local_bottom = local_size.height / 2.0;
            let peer_top = f64::from(placement.y) - peer_size.height / 2.0;
            let peer_bottom = f64::from(placement.y) + peer_size.height / 2.0;
            let overlap_start = local_top.max(peer_top);
            let overlap_end = local_bottom.min(peer_bottom);
            if overlap_end < overlap_start {
                return None;
            }

            (
                overlap_start - local_top,
                overlap_end - local_top,
                overlap_start - peer_top,
                overlap_end - peer_top,
                opposite_edge(edge),
            )
        }
        PeerEdge::Top | PeerEdge::Bottom => {
            let local_left = -(local_size.width / 2.0);
            let local_right = local_size.width / 2.0;
            let peer_left = f64::from(placement.x) - peer_size.width / 2.0;
            let peer_right = f64::from(placement.x) + peer_size.width / 2.0;
            let overlap_start = local_left.max(peer_left);
            let overlap_end = local_right.min(peer_right);
            if overlap_end < overlap_start {
                return None;
            }

            (
                overlap_start - local_left,
                overlap_end - local_left,
                overlap_start - peer_left,
                overlap_end - peer_left,
                opposite_edge(edge),
            )
        }
    };

    Some(LayoutContact {
        peer_id: peer_id.to_string(),
        local: interval_to_span(edge, local_start, local_end, edge_length(local_size, edge)),
        remote: interval_to_span(
            remote_edge,
            peer_start,
            peer_end,
            edge_length(peer_size, remote_edge),
        ),
    })
}

fn opposite_edge(edge: PeerEdge) -> PeerEdge {
    match edge {
        PeerEdge::Left => PeerEdge::Right,
        PeerEdge::Right => PeerEdge::Left,
        PeerEdge::Top => PeerEdge::Bottom,
        PeerEdge::Bottom => PeerEdge::Top,
    }
}

fn edge_length(size: NodeSize, edge: PeerEdge) -> f64 {
    match edge {
        PeerEdge::Left | PeerEdge::Right => size.height,
        PeerEdge::Top | PeerEdge::Bottom => size.width,
    }
}

fn interval_to_span(edge: PeerEdge, start: f64, end: f64, total: f64) -> EdgeSpanBp {
    let total = total.max(1.0);
    let clamped_start = start.clamp(0.0, total);
    let clamped_end = end.clamp(0.0, total);
    let mut start_bp = ((clamped_start / total) * f64::from(BASIS_POINTS))
        .floor()
        .round() as u16;
    let mut end_bp = ((clamped_end / total) * f64::from(BASIS_POINTS))
        .ceil()
        .round() as u16;

    start_bp = start_bp.min(BASIS_POINTS);
    end_bp = end_bp.min(BASIS_POINTS);

    if end_bp <= start_bp {
        if start_bp >= BASIS_POINTS {
            start_bp = BASIS_POINTS - 1;
            end_bp = BASIS_POINTS;
        } else {
            end_bp = start_bp + 1;
        }
    }

    EdgeSpanBp {
        edge,
        start_bp,
        end_bp,
    }
}

fn contact_edge_for_placement(
    placement: &LayoutPlacement,
    local_bounds: DisplayBounds,
    peer_bounds: DisplayBounds,
) -> PeerEdge {
    let anchor = node_rect(local_bounds, 0.0, 0.0);
    let peer = node_rect(peer_bounds, f64::from(placement.x), f64::from(placement.y));
    project_to_connected_edge(f64::from(placement.x), f64::from(placement.y), anchor, peer).edge
}

#[derive(Clone, Copy)]
struct NodeSize {
    width: f64,
    height: f64,
}

#[derive(Clone, Copy)]
struct NodeRect {
    x: f64,
    y: f64,
    width: f64,
    height: f64,
}

#[derive(Clone, Copy)]
struct EdgeProjection {
    edge: PeerEdge,
    x: f64,
    y: f64,
}

fn node_rect(bounds: DisplayBounds, x: f64, y: f64) -> NodeRect {
    let size = node_size(bounds);
    NodeRect {
        x,
        y,
        width: size.width,
        height: size.height,
    }
}

fn node_size(bounds: DisplayBounds) -> NodeSize {
    let width = f64::from(bounds.width.max(1));
    let height = f64::from(bounds.height.max(1));
    let aspect_ratio = width / height;

    let (width, height) = if aspect_ratio >= 1.0 {
        let mut scaled_width =
            ((width / REFERENCE_WIDTH) * 180.0).clamp(MIN_NODE_WIDTH, MAX_NODE_WIDTH);
        let mut scaled_height =
            (scaled_width / aspect_ratio).clamp(MIN_NODE_HEIGHT, MAX_NODE_HEIGHT);
        if scaled_height < MIN_NODE_HEIGHT {
            scaled_height = MIN_NODE_HEIGHT;
            scaled_width = (scaled_height * aspect_ratio).clamp(MIN_NODE_WIDTH, MAX_NODE_WIDTH);
        }
        (scaled_width, scaled_height)
    } else {
        let mut scaled_height =
            ((height / REFERENCE_HEIGHT) * 120.0).clamp(MIN_NODE_HEIGHT, MAX_NODE_HEIGHT);
        let mut scaled_width = (scaled_height * aspect_ratio).clamp(MIN_NODE_WIDTH, MAX_NODE_WIDTH);
        if scaled_width < MIN_NODE_WIDTH {
            scaled_width = MIN_NODE_WIDTH;
            scaled_height = (scaled_width / aspect_ratio).clamp(MIN_NODE_HEIGHT, MAX_NODE_HEIGHT);
        }
        (scaled_width, scaled_height)
    };

    NodeSize {
        width: width.round(),
        height: height.round(),
    }
}

fn project_to_connected_edge(
    target_x: f64,
    target_y: f64,
    anchor: NodeRect,
    peer: NodeRect,
) -> EdgeProjection {
    let contacts = edge_contact_coordinates(anchor, peer);
    let right_range = edge_slide_range(anchor, peer, PeerEdge::Right);
    let left_range = edge_slide_range(anchor, peer, PeerEdge::Left);
    let top_range = edge_slide_range(anchor, peer, PeerEdge::Top);
    let bottom_range = edge_slide_range(anchor, peer, PeerEdge::Bottom);

    let candidates = [
        EdgeProjection {
            edge: PeerEdge::Right,
            x: contacts.right_x,
            y: clamp(target_y, right_range.0, right_range.1),
        },
        EdgeProjection {
            edge: PeerEdge::Left,
            x: contacts.left_x,
            y: clamp(target_y, left_range.0, left_range.1),
        },
        EdgeProjection {
            edge: PeerEdge::Top,
            x: clamp(target_x, top_range.0, top_range.1),
            y: contacts.top_y,
        },
        EdgeProjection {
            edge: PeerEdge::Bottom,
            x: clamp(target_x, bottom_range.0, bottom_range.1),
            y: contacts.bottom_y,
        },
    ];

    let priority = [
        PeerEdge::Right,
        PeerEdge::Left,
        PeerEdge::Top,
        PeerEdge::Bottom,
    ];

    let mut winner = candidates[0];
    let mut best_distance = ((target_x - winner.x).powi(2) + (target_y - winner.y).powi(2)).sqrt();

    for candidate in candidates.iter().skip(1) {
        let distance = ((target_x - candidate.x).powi(2) + (target_y - candidate.y).powi(2)).sqrt();
        if distance + 1e-9 < best_distance {
            winner = *candidate;
            best_distance = distance;
            continue;
        }

        if (distance - best_distance).abs() <= 1e-9
            && edge_priority(candidate.edge, &priority) < edge_priority(winner.edge, &priority)
        {
            winner = *candidate;
            best_distance = distance;
        }
    }

    winner
}

fn edge_priority(edge: PeerEdge, priority: &[PeerEdge]) -> usize {
    priority
        .iter()
        .position(|candidate| *candidate == edge)
        .unwrap_or(usize::MAX)
}

fn clamp(value: f64, min: f64, max: f64) -> f64 {
    value.max(min).min(max)
}

fn edge_contact_coordinates(anchor: NodeRect, peer: NodeRect) -> EdgeContacts {
    let anchor_half_width = anchor.width / 2.0;
    let anchor_half_height = anchor.height / 2.0;
    let peer_half_width = peer.width / 2.0;
    let peer_half_height = peer.height / 2.0;

    EdgeContacts {
        top_y: anchor.y - anchor_half_height - EDGE_MARGIN - peer_half_height,
        right_x: anchor.x + anchor_half_width + EDGE_MARGIN + peer_half_width,
        bottom_y: anchor.y + anchor_half_height + EDGE_MARGIN + peer_half_height,
        left_x: anchor.x - anchor_half_width - EDGE_MARGIN - peer_half_width,
    }
}

#[derive(Clone, Copy)]
struct EdgeContacts {
    top_y: f64,
    right_x: f64,
    bottom_y: f64,
    left_x: f64,
}

fn edge_slide_range(anchor: NodeRect, peer: NodeRect, edge: PeerEdge) -> (f64, f64) {
    match edge {
        PeerEdge::Left | PeerEdge::Right => {
            let half_span = (anchor.height + peer.height) / 2.0;
            (anchor.y - half_span, anchor.y + half_span)
        }
        PeerEdge::Top | PeerEdge::Bottom => {
            let half_span = (anchor.width + peer.width) / 2.0;
            (anchor.x - half_span, anchor.x + half_span)
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::{
        BASIS_POINTS, EdgeSpanBp, bootstrap_edge_for_placement, map_position_bp,
        rebase_layout_update, resolved_layout_from_config,
    };
    use crate::core::api::ConfigSnapshot;
    use crate::core::events::{LayoutPlacement, LayoutUpdateEvent};
    use crate::platform::{DisplayBounds, PeerEdge};

    #[test]
    fn bootstrap_edge_follows_connected_placement() {
        let right = LayoutPlacement {
            peer_id: "peer-right".to_string(),
            peer_label: String::new(),
            x: 166,
            y: 35,
        };
        let top = LayoutPlacement {
            peer_id: "peer-top".to_string(),
            peer_label: String::new(),
            x: 15,
            y: -106,
        };

        assert_eq!(bootstrap_edge_for_placement(&right, None), PeerEdge::Right);
        assert_eq!(bootstrap_edge_for_placement(&top, None), PeerEdge::Top);
    }

    #[test]
    fn rebase_layout_update_changes_coordinate_origin() {
        let update = LayoutUpdateEvent {
            version: 4,
            editor_id: 8,
            timestamp: 12,
            placements: vec![
                LayoutPlacement {
                    peer_id: "peer-a".to_string(),
                    peer_label: "A".to_string(),
                    x: 0,
                    y: 0,
                },
                LayoutPlacement {
                    peer_id: "peer-b".to_string(),
                    peer_label: "B".to_string(),
                    x: 166,
                    y: 40,
                },
                LayoutPlacement {
                    peer_id: "peer-c".to_string(),
                    peer_label: "C".to_string(),
                    x: 20,
                    y: 80,
                },
            ],
        };

        let rebased = rebase_layout_update(update, "peer-a", "peer-b")
            .expect("receiver must be present in the sender layout");

        assert_eq!(rebased.placements.len(), 2);
        assert_eq!(rebased.placements[0].peer_id, "peer-a");
        assert_eq!(rebased.placements[0].x, -166);
        assert_eq!(rebased.placements[0].y, -40);
        assert_eq!(rebased.placements[1].peer_id, "peer-c");
        assert_eq!(rebased.placements[1].x, -146);
        assert_eq!(rebased.placements[1].y, 40);
    }

    #[test]
    fn resolved_layout_builds_asymmetric_spans() {
        let config = ConfigSnapshot {
            placements: vec![LayoutPlacement {
                peer_id: "peer-b".to_string(),
                peer_label: String::new(),
                x: 166,
                y: 40,
            }],
            ..Default::default()
        };
        let local_bounds = Some(DisplayBounds {
            x: 0,
            y: 0,
            width: 2560,
            height: 1440,
        });
        let mut peer_bounds = HashMap::new();
        peer_bounds.insert(
            "peer-b".to_string(),
            DisplayBounds {
                x: 0,
                y: 0,
                width: 1920,
                height: 1080,
            },
        );

        let resolved = resolved_layout_from_config(&config, local_bounds, &peer_bounds);
        let contact = resolved.contacts().first().expect("contact should exist");

        assert_eq!(contact.local.edge, PeerEdge::Right);
        assert_eq!(contact.remote.edge, PeerEdge::Left);
        assert!(contact.local.start_bp < contact.local.end_bp);
        assert!(contact.remote.start_bp < contact.remote.end_bp);
        assert_ne!(
            (contact.local.start_bp, contact.local.end_bp),
            (contact.remote.start_bp, contact.remote.end_bp)
        );
        assert!(contact.local.end_bp <= BASIS_POINTS);
        assert!(contact.remote.end_bp <= BASIS_POINTS);
    }

    #[test]
    fn resolved_layout_picks_contact_for_edge_position() {
        let config = ConfigSnapshot {
            placements: vec![
                LayoutPlacement {
                    peer_id: "peer-top".to_string(),
                    peer_label: String::new(),
                    x: 166,
                    y: -30,
                },
                LayoutPlacement {
                    peer_id: "peer-bottom".to_string(),
                    peer_label: String::new(),
                    x: 166,
                    y: 45,
                },
            ],
            ..Default::default()
        };
        let resolved = resolved_layout_from_config(&config, None, &HashMap::new());

        let top = resolved
            .contact_for_position(PeerEdge::Right, 2_500)
            .expect("top contact should match");
        let bottom = resolved
            .contact_for_position(PeerEdge::Right, 7_500)
            .expect("bottom contact should match");

        assert_eq!(top.peer_id, "peer-top");
        assert_eq!(bottom.peer_id, "peer-bottom");
    }

    #[test]
    fn map_position_bp_remaps_between_asymmetric_spans() {
        let from = EdgeSpanBp {
            edge: PeerEdge::Right,
            start_bp: 2_000,
            end_bp: 6_000,
        };
        let to = EdgeSpanBp {
            edge: PeerEdge::Left,
            start_bp: 1_000,
            end_bp: 9_000,
        };

        assert_eq!(map_position_bp(2_000, from, to), 1_000);
        assert_eq!(map_position_bp(4_000, from, to), 5_000);
        assert_eq!(map_position_bp(6_000, from, to), 9_000);
    }

    #[test]
    fn edge_span_bp_contains_inclusive_upper_bound() {
        let span = EdgeSpanBp {
            edge: PeerEdge::Right,
            start_bp: 2_000,
            end_bp: 6_000,
        };

        assert!(!span.contains(1_999));
        assert!(span.contains(2_000));
        assert!(span.contains(4_000));
        assert!(span.contains(6_000));
        assert!(!span.contains(6_001));
    }
}
