//! Per-process source marker for injected mouse events.

use std::collections::hash_map::RandomState;
use std::hash::BuildHasher;
use std::sync::OnceLock;
use std::time::{SystemTime, UNIX_EPOCH};

static LOCAL_MOUSE_MARKER: OnceLock<usize> = OnceLock::new();

pub(crate) fn local_mouse_marker() -> usize {
    *LOCAL_MOUSE_MARKER.get_or_init(generate_local_mouse_marker)
}

fn generate_local_mouse_marker() -> usize {
    let pid = u64::from(std::process::id());
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();

    let timestamp_nanos = now
        .as_secs()
        .wrapping_mul(1_000_000_000)
        .wrapping_add(u64::from(now.subsec_nanos()));

    // RandomState is keyed from process-random seeds; this acts as our random salt.
    let random_salt = RandomState::new().hash_one((pid, timestamp_nanos));

    let mixed = splitmix64(pid ^ timestamp_nanos.rotate_left(17) ^ random_salt.rotate_left(31));
    marker_from_u64(mixed)
}

fn marker_from_u64(value: u64) -> usize {
    if usize::BITS >= 64 {
        let marker = value as usize;
        if marker == 0 { 1 } else { marker }
    } else {
        let marker = (value as u32) as usize;
        if marker == 0 { 1 } else { marker }
    }
}

fn splitmix64(mut x: u64) -> u64 {
    x = x.wrapping_add(0x9E37_79B9_7F4A_7C15);
    x = (x ^ (x >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    x ^ (x >> 31)
}
