//! Peer information structure.

use crate::core::platform::{DisplayBounds, PlatformFamily};
use std::net::SocketAddr;

use super::session::SessionState;

#[derive(Clone, Debug)]
pub(crate) struct PeerInfo {
    pub(crate) peer_id: String,
    pub(crate) name: String,
    pub(crate) addr: Option<SocketAddr>,
    pub(crate) last_recv_us: u64,
    pub(crate) timed_out: bool,
    pub(crate) session_state: SessionState,
    pub(crate) session_state_changed_at_us: u64,
    pub(crate) screen: Option<String>,
    pub(crate) display_bounds: Option<DisplayBounds>,
    pub(crate) platform: PlatformFamily,
}
