//! Helpers for mDNS peer discovery and status reporting.
//!
//! Provides utilities for blocking wait on peers and pretty-printing
//! the list of discovered peers to the console.

use anyhow::{bail, Result};
use std::time::{Duration, Instant};
use tokio::sync::mpsc;

use crate::core::discovery::{DiscoveryClient, DiscoveryEvent, DiscoveryState, PeerInfo};

/// Waits for a peer to appear in the mDNS discovery client.
///
/// Uses the new push-based DiscoveryClient API with background thread.
/// Note: peer_id will be empty until the stream hello handshake completes.
pub(crate) fn wait_for_peer(
    port: u16,
    own_hostname: &str,
    preferred_peer_name: Option<&str>,
) -> Result<PeerInfo> {
    let wait_step = Duration::from_millis(500);
    let max_wait = Duration::from_secs(30);

    let deadline = Instant::now() + max_wait;
    let local_peer_id = uuid::Uuid::new_v4().to_string();

    // Create and start discovery client
    let mut discovery = DiscoveryClient::new()?;
    let mut rx = discovery.start(port, None, Some(&local_peer_id), None)?;

    // Create local state to track peers
    let mut state = DiscoveryState::default();

    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            bail!("Timed out waiting for peer");
        }

        // Use try_recv with manual sleep to avoid blocking indefinitely
        match rx.try_recv() {
            Ok(event) => {
                // Apply event to local state
                state.apply(event.clone(), Instant::now());

                // Check if we found a suitable peer
                if let DiscoveryEvent::Discovered(peer) = &event {
                    if peer.hostname == own_hostname {
                        continue;
                    }
                    if let Some(preferred) = preferred_peer_name {
                        if peer.hostname == preferred {
                            return Ok(PeerInfo {
                                peer_id: String::new(), // Unknown until the stream hello handshake
                                name: peer.hostname.clone(),
                                hostname: peer.hostname.clone(),
                                addr: peer.primary_addr,
                                platform: None,
                                last_seen: Instant::now(),
                                discovery_status: crate::core::discovery::DiscoveryStatus::Online,
                            });
                        }
                    }
                }
            }
            Err(mpsc::error::TryRecvError::Empty) => {
                std::thread::sleep(wait_step.min(remaining));

                if let Some(peer) =
                    find_first_matching_peer(&state, own_hostname, preferred_peer_name)
                {
                    return Ok(peer);
                }
            }
            Err(mpsc::error::TryRecvError::Disconnected) => {
                bail!("Discovery channel closed unexpectedly");
            }
        }
    }
}

fn find_first_matching_peer(
    state: &DiscoveryState,
    own_hostname: &str,
    preferred_peer_name: Option<&str>,
) -> Option<PeerInfo> {
    let now = Instant::now();
    let peers = state.snapshot(now);

    // Filter out self and find matching peer
    for peer in &peers {
        if peer.hostname == own_hostname {
            continue;
        }
        if let Some(preferred) = preferred_peer_name {
            if peer.name == preferred {
                return Some(peer.clone());
            }
        }
    }

    // If no preferred match, return any non-self peer
    peers.into_iter().find(|p| p.hostname != own_hostname)
}

/// Alternative version that waits for any peer (no preference)
pub(crate) fn wait_for_any_peer(port: u16, own_hostname: &str) -> Result<PeerInfo> {
    wait_for_peer(port, own_hostname, None)
}

pub(crate) fn print_peer_list(mut peers: Vec<PeerInfo>) {
    peers.sort_by(|a, b| a.name.cmp(&b.name).then(a.addr.cmp(&b.addr)));
    if peers.is_empty() {
        println!("No peers discovered");
        return;
    }

    println!("Discovered peers:");
    for peer in peers {
        println!(
            "- {} ({}) {} last_seen={}ms",
            peer.name,
            peer.hostname,
            peer.addr,
            peer.last_seen.elapsed().as_millis()
        );
    }
}
