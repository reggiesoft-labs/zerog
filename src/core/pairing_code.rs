//! Short authentication string (SAS) for pairing confirmation.
//!
//! Trust-on-first-use cannot tell a real peer from a man in the middle. The
//! pairing code gives the two humans something to compare out of band: it is
//! derived from *both* certificates, so a relay sitting between the peers
//! produces a different code on each side and the comparison fails.

use sha2::{Digest, Sha256};

/// Domain separator so this hash cannot be confused with a fingerprint.
const PAIRING_CODE_CONTEXT: &[u8] = b"zerog-pairing-code-v1";

/// Number of decimal digits in a pairing code.
pub const PAIRING_CODE_DIGITS: usize = 6;

/// Derives the shared pairing code from the two certificate fingerprints.
///
/// The fingerprints are ordered before hashing, so both peers compute the same
/// code no matter which of them is the client. The result is always
/// [`PAIRING_CODE_DIGITS`] digits, zero padded.
pub fn pairing_code(first: &[u8; 32], second: &[u8; 32]) -> String {
    let (lower, upper) = if first <= second {
        (first, second)
    } else {
        (second, first)
    };

    let mut hasher = Sha256::new();
    hasher.update(PAIRING_CODE_CONTEXT);
    hasher.update(lower);
    hasher.update(upper);
    let digest = hasher.finalize();

    let mut bytes = [0u8; 8];
    bytes.copy_from_slice(&digest[..8]);
    let modulus = 10u64.pow(PAIRING_CODE_DIGITS as u32);
    let value = u64::from_be_bytes(bytes) % modulus;

    format!("{:0width$}", value, width = PAIRING_CODE_DIGITS)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fingerprint(seed: u8) -> [u8; 32] {
        [seed; 32]
    }

    #[test]
    fn code_is_independent_of_argument_order() {
        let a = fingerprint(1);
        let b = fingerprint(2);
        assert_eq!(pairing_code(&a, &b), pairing_code(&b, &a));
    }

    #[test]
    fn code_is_six_digits() {
        let code = pairing_code(&fingerprint(7), &fingerprint(9));
        assert_eq!(code.len(), PAIRING_CODE_DIGITS);
        assert!(code.chars().all(|c| c.is_ascii_digit()), "code: {code}");
    }

    #[test]
    fn different_peers_produce_different_codes() {
        let local = fingerprint(1);
        assert_ne!(
            pairing_code(&local, &fingerprint(2)),
            pairing_code(&local, &fingerprint(3)),
            "a different remote certificate must change the code"
        );
    }

    #[test]
    fn code_is_stable_for_the_same_pair() {
        let a = fingerprint(0xAB);
        let b = fingerprint(0xCD);
        assert_eq!(pairing_code(&a, &b), pairing_code(&a, &b));
    }

    #[test]
    fn code_derives_from_the_context_label() {
        // Guards against silently dropping the domain separator, which would
        // turn this into a bare fingerprint comparison.
        let mut hasher = Sha256::new();
        hasher.update(PAIRING_CODE_CONTEXT);
        hasher.update(fingerprint(1));
        hasher.update(fingerprint(2));
        let digest = hasher.finalize();
        let mut bytes = [0u8; 8];
        bytes.copy_from_slice(&digest[..8]);
        let expected = format!("{:06}", u64::from_be_bytes(bytes) % 1_000_000);
        assert_eq!(pairing_code(&fingerprint(1), &fingerprint(2)), expected);
    }
}
