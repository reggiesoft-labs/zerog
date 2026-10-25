use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use serde::{Deserialize, Serialize};
use std::time::{SystemTime, UNIX_EPOCH};

pub const DEFAULT_LICENSE_PUBKEY: [u8; 32] = [
    0x5a, 0x1f, 0x6e, 0x8b, 0x22, 0xd4, 0x93, 0x11, 0x7a, 0x3c, 0xf0, 0x89, 0x14, 0xe2, 0x3d, 0x5b,
    0x91, 0x47, 0x68, 0x2e, 0xc1, 0x05, 0x77, 0xfa, 0xd9, 0x34, 0x61, 0xbb, 0x80, 0x29, 0xfe, 0x63,
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LicenseTier {
    Free,
    Pro,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LicensePayload {
    pub licensee: String,
    pub tier: LicenseTier,
    pub expires_at: Option<u64>,
    pub issued_at: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LicenseInfo {
    pub licensee: String,
    pub tier: LicenseTier,
    pub expires_at: Option<u64>,
    pub issued_at: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum LicenseStatus {
    Free,
    Active {
        licensee: String,
        tier: LicenseTier,
        expires_at: Option<u64>,
    },
    Expired {
        licensee: String,
        expires_at: u64,
    },
    Invalid {
        reason: String,
    },
}

impl LicenseStatus {
    pub fn is_pro(&self) -> bool {
        matches!(self, LicenseStatus::Active { tier: LicenseTier::Pro, .. })
    }
}

pub fn verify_license_key(key: &str, pubkey_bytes: &[u8; 32]) -> LicenseStatus {
    let trimmed = key.trim();
    if trimmed.is_empty() {
        return LicenseStatus::Free;
    }

    let parts: Vec<&str> = trimmed.split('.').collect();
    if parts.len() != 3 {
        return LicenseStatus::Invalid {
            reason: "invalid license format".to_string(),
        };
    }

    if parts[0] != "v1" {
        return LicenseStatus::Invalid {
            reason: "unsupported license version".to_string(),
        };
    }

    let payload_bytes = match BASE64.decode(parts[1]) {
        Ok(bytes) => bytes,
        Err(_) => {
            return LicenseStatus::Invalid {
                reason: "invalid payload encoding".to_string(),
            };
        }
    };

    let sig_bytes = match BASE64.decode(parts[2]) {
        Ok(bytes) => bytes,
        Err(_) => {
            return LicenseStatus::Invalid {
                reason: "invalid signature encoding".to_string(),
            };
        }
    };

    let signature_array: [u8; 64] = match sig_bytes.try_into() {
        Ok(arr) => arr,
        Err(_) => {
            return LicenseStatus::Invalid {
                reason: "invalid signature length".to_string(),
            };
        }
    };
    let signature = Signature::from_bytes(&signature_array);

    let verifying_key = match VerifyingKey::from_bytes(pubkey_bytes) {
        Ok(vk) => vk,
        Err(_) => {
            return LicenseStatus::Invalid {
                reason: "invalid public key".to_string(),
            };
        }
    };

    if verifying_key.verify(&payload_bytes, &signature).is_err() {
        return LicenseStatus::Invalid {
            reason: "signature verification failed".to_string(),
        };
    }

    let payload: LicensePayload = match serde_json::from_slice(&payload_bytes) {
        Ok(p) => p,
        Err(_) => {
            return LicenseStatus::Invalid {
                reason: "invalid payload json".to_string(),
            };
        }
    };

    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    if let Some(expires_at) = payload.expires_at {
        if now > expires_at {
            return LicenseStatus::Expired {
                licensee: payload.licensee,
                expires_at,
            };
        }
    }

    LicenseStatus::Active {
        licensee: payload.licensee,
        tier: payload.tier,
        expires_at: payload.expires_at,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::{Signer, SigningKey};

    fn make_test_key(
        payload: &LicensePayload,
        signing_key: &SigningKey,
    ) -> String {
        let json_bytes = serde_json::to_vec(payload).unwrap();
        let signature = signing_key.sign(&json_bytes);
        format!(
            "v1.{}.{}",
            BASE64.encode(json_bytes),
            BASE64.encode(signature.to_bytes())
        )
    }

    #[test]
    fn empty_license_is_free() {
        assert_eq!(
            verify_license_key("", &DEFAULT_LICENSE_PUBKEY),
            LicenseStatus::Free
        );
        assert_eq!(
            verify_license_key("   ", &DEFAULT_LICENSE_PUBKEY),
            LicenseStatus::Free
        );
    }

    #[test]
    fn valid_perpetual_pro_license() {
        let signing_key = SigningKey::from_bytes(&[42u8; 32]);
        let pubkey = signing_key.verifying_key().to_bytes();
        let payload = LicensePayload {
            licensee: "Test Licensee".to_string(),
            tier: LicenseTier::Pro,
            expires_at: None,
            issued_at: 1727100000,
        };
        let key = make_test_key(&payload, &signing_key);
        let status = verify_license_key(&key, &pubkey);
        assert!(status.is_pro());
        assert_eq!(
            status,
            LicenseStatus::Active {
                licensee: "Test Licensee".to_string(),
                tier: LicenseTier::Pro,
                expires_at: None,
            }
        );
    }

    #[test]
    fn expired_pro_license() {
        let signing_key = SigningKey::from_bytes(&[42u8; 32]);
        let pubkey = signing_key.verifying_key().to_bytes();
        let payload = LicensePayload {
            licensee: "Alice".to_string(),
            tier: LicenseTier::Pro,
            expires_at: Some(1000),
            issued_at: 500,
        };
        let key = make_test_key(&payload, &signing_key);
        let status = verify_license_key(&key, &pubkey);
        assert!(!status.is_pro());
        assert_eq!(
            status,
            LicenseStatus::Expired {
                licensee: "Alice".to_string(),
                expires_at: 1000,
            }
        );
    }

    #[test]
    fn tampered_signature_fails() {
        let signing_key = SigningKey::from_bytes(&[42u8; 32]);
        let pubkey = signing_key.verifying_key().to_bytes();
        let payload = LicensePayload {
            licensee: "Bob".to_string(),
            tier: LicenseTier::Pro,
            expires_at: None,
            issued_at: 1727100000,
        };
        let key = make_test_key(&payload, &signing_key);
        let tampered = format!("{}extra", key);
        let status = verify_license_key(&tampered, &pubkey);
        assert!(matches!(status, LicenseStatus::Invalid { .. }));
    }
}
