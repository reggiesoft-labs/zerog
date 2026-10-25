//! Datagram fragment reassembly helpers for runtime packet processing.
//!
//! Reconstructs fragmented packet payloads and returns complete serialized
//! packets when all fragments have arrived.

use std::collections::HashMap;

use super::super::runtime::FragmentHeader;
use crate::events::Packet;

/// Maximum number of fragments a single message may declare.
///
/// `total_fragments` is attacker-controlled, so it must be bounded: without a
/// cap a peer could keep advertising new fragment indices forever. At the
/// ~1200 byte datagram MTU this still allows messages up to ~9.8 MiB.
const MAX_TOTAL_FRAGMENTS: u16 = 8192;

/// Maximum number of in-flight fragmented messages tracked at once.
///
/// Once the limit is reached a new fragment id clears the map instead of growing
/// it without bound. Legitimate reassembly is short-lived, so dropping stalled
/// partials is preferable to unbounded memory growth.
const MAX_REASSEMBLY_ENTRIES: usize = 64;

/// A partially received fragmented message.
///
/// Chunks are stored only when they arrive. Pre-sizing a slot vector from
/// `total_fragments` let a six-byte datagram force a ~196 KB allocation; storing
/// chunks lazily keeps memory proportional to data actually received.
pub(crate) struct PendingMessage {
    total_fragments: u16,
    chunks: HashMap<u16, Vec<u8>>,
}

pub(crate) fn reassemble_fragment(
    reassembly: &mut HashMap<u16, PendingMessage>,
    data: Vec<u8>,
) -> Option<Vec<u8>> {
    // Non-fragmented packets are complete serialized Packets. Try direct
    // deserialization first to avoid accidentally interpreting the leading
    // bytes as a FragmentHeader (a FragmentHeader is only 6 bytes and can
    // match any packet's version/sender_id prefix).
    if bincode::deserialize::<Packet>(&data).is_ok() {
        return Some(data);
    }

    let header_size = bincode::serialized_size(&FragmentHeader {
        fragment_id: 0,
        fragment_index: 0,
        total_fragments: 0,
    })
    .ok()? as usize;

    if data.len() < header_size {
        return None;
    }

    let header: FragmentHeader = bincode::deserialize(&data[..header_size]).ok()?;
    let payload = data[header_size..].to_vec();

    if header.total_fragments <= 1 {
        // Single-fragment: payload is the complete packet.
        if bincode::deserialize::<Packet>(&payload).is_ok() {
            return Some(payload);
        }
        return None;
    }

    if header.total_fragments > MAX_TOTAL_FRAGMENTS {
        return None;
    }

    if !reassembly.contains_key(&header.fragment_id) && reassembly.len() >= MAX_REASSEMBLY_ENTRIES {
        reassembly.clear();
    }

    let entry = reassembly
        .entry(header.fragment_id)
        .or_insert_with(|| PendingMessage {
            total_fragments: header.total_fragments,
            chunks: HashMap::new(),
        });

    // Fragments of one message must agree on the declared count, and an index
    // outside the declared range must not count towards completion.
    if entry.total_fragments != header.total_fragments
        || header.fragment_index >= entry.total_fragments
    {
        return None;
    }

    entry.chunks.insert(header.fragment_index, payload);

    if entry.chunks.len() != entry.total_fragments as usize {
        return None;
    }

    // Every index in `0..total_fragments` is present because keys are unique,
    // in range, and count exactly `total_fragments`.
    let complete: Vec<u8> = (0..entry.total_fragments)
        .filter_map(|index| entry.chunks.get(&index))
        .flat_map(|chunk| chunk.iter().copied())
        .collect();
    reassembly.remove(&header.fragment_id);
    Some(complete)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::test_bytes::ByteFuzzer;

    fn fragment_bytes(
        fragment_id: u16,
        fragment_index: u16,
        total_fragments: u16,
        payload: &[u8],
    ) -> Vec<u8> {
        let header = FragmentHeader {
            fragment_id,
            fragment_index,
            total_fragments,
        };
        let mut data = bincode::serialize(&header).unwrap();
        data.extend_from_slice(payload);
        data
    }

    #[test]
    fn rejects_fragment_count_above_limit() {
        let mut map = HashMap::new();
        let data = fragment_bytes(1, 0, MAX_TOTAL_FRAGMENTS + 1, &[0u8; 8]);
        assert!(reassemble_fragment(&mut map, data).is_none());
        assert!(map.is_empty(), "oversized declaration must not be tracked");
    }

    #[test]
    fn bounds_number_of_inflight_reassembly_entries() {
        let mut map = HashMap::new();
        for id in 0..(MAX_REASSEMBLY_ENTRIES as u16 + 8) {
            let data = fragment_bytes(id, 0, 4, &[0u8; 8]);
            let _ = reassemble_fragment(&mut map, data);
        }
        assert!(
            map.len() <= MAX_REASSEMBLY_ENTRIES,
            "reassembly map grew past its cap: {}",
            map.len()
        );
    }

    #[test]
    fn completes_a_normal_fragmented_message() {
        let mut map = HashMap::new();
        let first = fragment_bytes(7, 0, 2, &[1u8; 8]);
        assert!(reassemble_fragment(&mut map, first).is_none());
        let second = fragment_bytes(7, 1, 2, &[2u8; 8]);
        let complete = reassemble_fragment(&mut map, second).expect("message should reassemble");
        assert_eq!(complete.len(), 16);
        assert!(map.is_empty());
    }

    #[test]
    fn arbitrary_datagrams_never_panic_and_stay_bounded() {
        let mut fuzzer = ByteFuzzer::new(0xF00D_9876);
        let mut map = HashMap::new();
        for _ in 0..5_000 {
            let len = fuzzer.len(2048);
            let _ = reassemble_fragment(&mut map, fuzzer.bytes(len));
            assert!(
                map.len() <= MAX_REASSEMBLY_ENTRIES,
                "reassembly map grew past its cap: {}",
                map.len()
            );
            for pending in map.values() {
                assert!(pending.chunks.len() <= pending.total_fragments as usize);
                assert!(pending.chunks.len() <= MAX_TOTAL_FRAGMENTS as usize);
            }
        }
    }

    #[test]
    fn hostile_fragment_declarations_are_bounded() {
        let mut fuzzer = ByteFuzzer::new(0xABCD_1234);
        let mut map = HashMap::new();
        for _ in 0..2_000 {
            // Declarations right around the cap are the expensive case: the
            // reassembler must reject them without trusting the count.
            let total = fuzzer.len(MAX_TOTAL_FRAGMENTS as usize + 64) as u16;
            let index = fuzzer.len(64) as u16;
            let fragment_id = fuzzer.len(u16::MAX as usize) as u16;
            let payload_len = fuzzer.len(2048);
            let data = fragment_bytes(fragment_id, index, total, &fuzzer.bytes(payload_len));
            let _ = reassemble_fragment(&mut map, data);

            assert!(map.len() <= MAX_REASSEMBLY_ENTRIES);
            for pending in map.values() {
                assert!(pending.chunks.len() <= pending.total_fragments as usize);
                assert!(pending.chunks.len() <= MAX_TOTAL_FRAGMENTS as usize);
            }
        }
    }

    #[test]
    fn one_small_datagram_cannot_presize_storage() {
        let mut map = HashMap::new();
        // A six-byte header declaring the maximum fragment count used to
        // allocate a ~196 KB slot vector. Storage must track received data.
        let data = fragment_bytes(1, 0, MAX_TOTAL_FRAGMENTS, &[]);
        assert!(reassemble_fragment(&mut map, data).is_none());

        let pending = map.get(&1).expect("fragment should be tracked");
        assert_eq!(pending.total_fragments, MAX_TOTAL_FRAGMENTS);
        assert_eq!(pending.chunks.len(), 1);
        assert!(
            pending.chunks.capacity() < 16,
            "storage grew with the declared fragment count: capacity {}",
            pending.chunks.capacity()
        );
    }
}
