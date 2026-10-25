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
                (center - position).abs()
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

    for contact in resolved.contacts() {
        let Some(addr) = peer_addrs.get(&contact.peer_id).copied() else {
            continue;
        };
        seen_peers.insert(contact.peer_id.clone());
        contacts.push(RuntimeContact {
            peer_id: contact.peer_id.clone(),
            addr,
            local: contact.local,
            remote: contact.remote,
        });
    }

    for (peer_id, addr) in peer_addrs {
        if seen_peers.contains(peer_id) {
            continue;
        }
        let edge = fallback_edges
            .get(peer_id)
            .copied()
            .unwrap_or(PeerEdge::Right);
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
        });
    }

    contacts
}

#[cfg(test)]
mod tests {
    use super::{runtime_contacts_for_peers, LayoutState};
    use crate::core::layout::{EdgeSpanBp, LayoutContact, ResolvedLayout};
    use crate::platform::PeerEdge;
    use std::collections::HashMap;

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
