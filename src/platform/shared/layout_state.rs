use crate::core::layout::{EdgeSpanBp, ResolvedLayout};
use crate::platform::PeerEdge;
use std::collections::{HashMap, HashSet};
use std::net::SocketAddr;
use std::sync::Mutex;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct OutboundTarget {
    pub(crate) contact: RuntimeContact,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RuntimeContact {
    pub(crate) peer_id: String,
    pub(crate) addr: SocketAddr,
    pub(crate) local: EdgeSpanBp,
    pub(crate) remote: EdgeSpanBp,
    /// Whole-edge stand-in for a connected peer the layout does not place.
    /// Placed contacts win wherever both cover an edge position.
    pub(crate) fallback: bool,
}

#[derive(Default)]
pub(crate) struct LayoutState {
    contacts: Mutex<Vec<RuntimeContact>>,
}

impl LayoutState {
    pub(crate) fn replace(&self, contacts: Vec<RuntimeContact>) {
        *self.contacts.lock().expect("layout state lock poisoned") = contacts;
    }

    pub(crate) fn contacts(&self) -> Vec<RuntimeContact> {
        self.contacts
            .lock()
            .expect("layout state lock poisoned")
            .clone()
    }

    pub(crate) fn contact_for_position(
        &self,
        edge: PeerEdge,
        position_bp: u16,
    ) -> Option<RuntimeContact> {
        self.contacts()
            .into_iter()
            .filter(|contact| contact.local.edge == edge && contact.local.contains(position_bp))
            .min_by_key(|contact| {
                let center = i32::from(contact.local.center_bp());
                let position = i32::from(position_bp);
                (contact.fallback, (center - position).abs())
            })
    }

    pub(crate) fn contact_for_peer_local_position(
        &self,
        peer_id: &str,
        edge: PeerEdge,
        position_bp: u16,
    ) -> Option<RuntimeContact> {
        self.contacts()
            .into_iter()
            .filter(|contact| contact.peer_id == peer_id && contact.local.edge == edge)
            .min_by_key(|contact| {
                let center = i32::from(contact.local.center_bp());
                let position = i32::from(position_bp);
                (center - position).abs()
            })
    }
}

pub(crate) fn runtime_contacts_for_peers(
    resolved: &ResolvedLayout,
    peer_addrs: &HashMap<String, SocketAddr>,
    fallback_edges: &HashMap<String, PeerEdge>,
) -> Vec<RuntimeContact> {
    let mut contacts = Vec::new();
    let mut seen_peers = HashSet::new();
    let mut used_edges = HashSet::new();

    for contact in resolved.contacts() {
        let Some(addr) = peer_addrs.get(&contact.peer_id).copied() else {
            continue;
        };
        seen_peers.insert(contact.peer_id.clone());
        used_edges.insert(contact.local.edge);
        contacts.push(RuntimeContact {
            peer_id: contact.peer_id.clone(),
            addr,
            local: contact.local,
            remote: contact.remote,
            fallback: false,
        });
    }

    // Only peers missing from the layout get a stand-in contact. A placed peer
    // without a contact sits apart from this display (for example beyond
    // another machine), so giving it a whole edge would send the cursor past
    // the display in between. Peers are visited in id order and spread over
    // free edges so two unplaced peers never compete for the same edge.
    // Mirrors `assignFallbackEdges` in `gui/src/layoutModel.js`.
    let mut unplaced: Vec<_> = peer_addrs
        .iter()
        .filter(|(peer_id, _)| !seen_peers.contains(*peer_id) && !resolved.is_placed(peer_id))
        .collect();
    unplaced.sort_by(|a, b| a.0.cmp(b.0));

    for (peer_id, addr) in unplaced {
        let preferred = fallback_edges
            .get(peer_id)
            .copied()
            .unwrap_or(PeerEdge::Right);
        let edge = std::iter::once(preferred)
            .chain(FALLBACK_EDGE_ORDER)
            .find(|edge| !used_edges.contains(edge))
            .unwrap_or(preferred);
        used_edges.insert(edge);
        contacts.push(RuntimeContact {
            peer_id: peer_id.clone(),
            addr: *addr,
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
            fallback: true,
        });
    }

    contacts
}

const FALLBACK_EDGE_ORDER: [PeerEdge; 4] = [
    PeerEdge::Right,
    PeerEdge::Left,
    PeerEdge::Top,
    PeerEdge::Bottom,
];

#[cfg(test)]
mod tests {
    use super::{runtime_contacts_for_peers, LayoutState};
    use crate::core::api::ConfigSnapshot;
    use crate::core::events::LayoutPlacement;
    use crate::core::layout::{
        EdgeSpanBp, LayoutContact, ResolvedLayout, resolved_layout_from_config,
    };
    use crate::platform::PeerEdge;
    use std::collections::HashMap;
    use std::net::SocketAddr;

    fn addr(port: u16) -> SocketAddr {
        SocketAddr::from(([127, 0, 0, 1], port))
    }

    fn placement(peer_id: &str, x: i32, y: i32) -> LayoutPlacement {
        LayoutPlacement {
            peer_id: peer_id.to_string(),
            peer_label: String::new(),
            x,
            y,
        }
    }

    #[test]
    fn placed_peer_beyond_a_neighbour_gets_no_fallback_edge() {
        let config = ConfigSnapshot {
            placements: vec![
                placement("peer-far", 332, 0),
                placement("peer-middle", 166, 0),
            ],
            ..Default::default()
        };
        let resolved = resolved_layout_from_config(&config, None, &HashMap::new());
        let peer_addrs = HashMap::from([
            ("peer-far".to_string(), addr(5001)),
            ("peer-middle".to_string(), addr(5002)),
        ]);
        let fallback_edges = HashMap::from([
            ("peer-far".to_string(), PeerEdge::Right),
            ("peer-middle".to_string(), PeerEdge::Right),
        ]);

        let contacts = runtime_contacts_for_peers(&resolved, &peer_addrs, &fallback_edges);

        assert_eq!(contacts.len(), 1);
        assert_eq!(contacts[0].peer_id, "peer-middle");
    }

    #[test]
    fn unplaced_peers_spread_over_free_edges_in_id_order() {
        let config = ConfigSnapshot {
            placements: vec![placement("peer-placed", 166, 0)],
            ..Default::default()
        };
        let resolved = resolved_layout_from_config(&config, None, &HashMap::new());
        let peer_addrs = HashMap::from([
            ("peer-placed".to_string(), addr(5001)),
            ("peer-z".to_string(), addr(5002)),
            ("peer-y".to_string(), addr(5003)),
        ]);
        let fallback_edges = HashMap::from([
            ("peer-placed".to_string(), PeerEdge::Right),
            ("peer-z".to_string(), PeerEdge::Right),
            ("peer-y".to_string(), PeerEdge::Right),
        ]);

        for _ in 0..8 {
            let contacts = runtime_contacts_for_peers(&resolved, &peer_addrs, &fallback_edges);
            let edge_of = |peer_id: &str| {
                contacts
                    .iter()
                    .find(|contact| contact.peer_id == peer_id)
                    .map(|contact| (contact.local.edge, contact.fallback))
            };
            assert_eq!(edge_of("peer-placed"), Some((PeerEdge::Right, false)));
            assert_eq!(edge_of("peer-y"), Some((PeerEdge::Left, true)));
            assert_eq!(edge_of("peer-z"), Some((PeerEdge::Top, true)));
        }
    }

    #[test]
    fn placed_contact_wins_over_fallback_on_a_shared_edge() {
        let state = LayoutState::default();
        let placed_span = EdgeSpanBp {
            edge: PeerEdge::Right,
            start_bp: 0,
            end_bp: 6_000,
        };
        let full_span = EdgeSpanBp {
            edge: PeerEdge::Right,
            start_bp: 0,
            end_bp: 10_000,
        };
        state.replace(vec![
            super::RuntimeContact {
                peer_id: "peer-fallback".to_string(),
                addr: addr(5001),
                local: full_span,
                remote: full_span,
                fallback: true,
            },
            super::RuntimeContact {
                peer_id: "peer-placed".to_string(),
                addr: addr(5002),
                local: placed_span,
                remote: placed_span,
                fallback: false,
            },
        ]);

        let at = |position_bp| {
            state
                .contact_for_position(PeerEdge::Right, position_bp)
                .map(|contact| contact.peer_id)
        };
        assert_eq!(at(5_000).as_deref(), Some("peer-placed"));
        assert_eq!(at(8_000).as_deref(), Some("peer-fallback"));
    }

    #[test]
    fn layout_state_replaces_contacts() {
        let state = LayoutState::default();
        let contact = super::RuntimeContact {
            peer_id: "peer".to_string(),
            addr: "127.0.0.1:5001".parse().expect("valid addr"),
            local: EdgeSpanBp {
                edge: PeerEdge::Right,
                start_bp: 0,
                end_bp: 10_000,
            },
            remote: EdgeSpanBp {
                edge: PeerEdge::Left,
                start_bp: 0,
                end_bp: 10_000,
            },
            fallback: false,
        };

        state.replace(vec![contact.clone()]);

        assert_eq!(state.contacts(), vec![contact]);
    }

    #[test]
    fn contact_for_position_chooses_matching_runtime_contact() {
        let state = LayoutState::default();
        let resolved = ResolvedLayout::new(vec![
            LayoutContact {
                peer_id: "peer-top".to_string(),
                local: EdgeSpanBp {
                    edge: PeerEdge::Right,
                    start_bp: 0,
                    end_bp: 4_000,
                },
                remote: EdgeSpanBp {
                    edge: PeerEdge::Left,
                    start_bp: 0,
                    end_bp: 5_000,
                },
            },
            LayoutContact {
                peer_id: "peer-bottom".to_string(),
                local: EdgeSpanBp {
                    edge: PeerEdge::Right,
                    start_bp: 4_000,
                    end_bp: 10_000,
                },
                remote: EdgeSpanBp {
                    edge: PeerEdge::Left,
                    start_bp: 2_000,
                    end_bp: 10_000,
                },
            },
        ]);
        let peer_addrs = HashMap::from([
            (
                "peer-top".to_string(),
                "127.0.0.1:5001".parse().expect("valid addr"),
            ),
            (
                "peer-bottom".to_string(),
                "127.0.0.1:5002".parse().expect("valid addr"),
            ),
        ]);
        let fallback_edges = HashMap::from([
            ("peer-top".to_string(), PeerEdge::Right),
            ("peer-bottom".to_string(), PeerEdge::Right),
        ]);
        state.replace(runtime_contacts_for_peers(
            &resolved,
            &peer_addrs,
            &fallback_edges,
        ));

        let top = state
            .contact_for_position(PeerEdge::Right, 2_500)
            .expect("top contact should resolve");
        let bottom = state
            .contact_for_position(PeerEdge::Right, 7_500)
            .expect("bottom contact should resolve");

        assert_eq!(top.peer_id, "peer-top");
        assert_eq!(bottom.peer_id, "peer-bottom");
    }
}
