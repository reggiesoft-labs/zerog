use anyhow::{Context, Result, anyhow};
use base64::{Engine, engine::general_purpose::STANDARD as BASE64_STANDARD};
use rcgen::{CertificateParams, DnType, ExtendedKeyUsagePurpose, KeyPair, KeyUsagePurpose};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use tracing::{debug, info, warn};

use crate::core::secure_fs::{restrict_permissions, write_private_file};

const KNOWN_PEERS_FILE: &str = "known_peers.toml";
const SELF_CERT_FILE: &str = "self.der";
const SELF_KEY_FILE: &str = "self.key";

pub struct CertificateStore {
    base_path: PathBuf,
    known_peers: HashMap<String, [u8; 32]>,
    dirty: Arc<AtomicBool>,
    self_cert_der: Option<Vec<u8>>,
    self_key_der: Option<Vec<u8>>,
}

impl CertificateStore {
    pub fn load_or_create(base_path: &std::path::Path) -> Result<Self> {
        fs::create_dir_all(base_path)
            .with_context(|| format!("failed to create cert directory: {:?}", base_path))?;
        if let Err(e) = restrict_permissions(base_path, 0o700) {
            warn!(error = %e, path = ?base_path, "failed to restrict cert directory permissions");
        }

        let known_peers_path = base_path.join(KNOWN_PEERS_FILE);
        let known_peers = if known_peers_path.exists() {
            Self::load_known_peers(&known_peers_path)?
        } else {
            HashMap::new()
        };

        let self_cert_path = base_path.join(SELF_CERT_FILE);
        let self_key_path = base_path.join(SELF_KEY_FILE);

        let self_cert_der = if self_cert_path.exists() {
            Some(fs::read(&self_cert_path).context("failed to read self certificate")?)
        } else {
            None
        };

        let self_key_der = if self_key_path.exists() {
            // Remediate keys created before restrictive permissions were applied.
            if let Err(e) = restrict_permissions(&self_key_path, 0o600) {
                warn!(error = %e, path = ?self_key_path, "failed to restrict private key permissions");
            }
            Some(fs::read(&self_key_path).context("failed to read self private key")?)
        } else {
            None
        };

        if known_peers_path.exists()
            && let Err(e) = restrict_permissions(&known_peers_path, 0o600)
        {
            warn!(error = %e, path = ?known_peers_path, "failed to restrict known peers permissions");
        }

        Ok(Self {
            base_path: base_path.to_path_buf(),
            known_peers,
            dirty: Arc::new(AtomicBool::new(false)),
            self_cert_der,
            self_key_der,
        })
    }

    pub fn get_or_create_self_cert(&mut self, peer_id: &str) -> Result<(Vec<u8>, Vec<u8>)> {
        if let (Some(cert_der), Some(key_der)) = (&self.self_cert_der, &self.self_key_der) {
            return Ok((cert_der.clone(), key_der.clone()));
        }

        let (cert_der, key_der) = generate_self_signed_cert(peer_id)?;

        let cert_path = self.base_path.join(SELF_CERT_FILE);
        let key_path = self.base_path.join(SELF_KEY_FILE);

        fs::write(&cert_path, &cert_der)
            .with_context(|| format!("failed to write self certificate to {:?}", cert_path))?;
        write_private_file(&key_path, &key_der)
            .with_context(|| format!("failed to write self private key to {:?}", key_path))?;

        self.self_cert_der = Some(cert_der.clone());
        self.self_key_der = Some(key_der.clone());

        info!(peer_id = %peer_id, "generated new self-signed certificate");

        Ok((cert_der, key_der))
    }

    pub fn is_known_peer(&self, peer_id: &str) -> bool {
        self.known_peers.contains_key(peer_id)
    }

    pub fn verify_fingerprint(&self, peer_id: &str, cert_der: &[u8]) -> bool {
        match self.known_peers.get(peer_id) {
            Some(stored_fp) => {
                let computed_fp = cert_fingerprint(cert_der);
                computed_fp == *stored_fp
            }
            None => false,
        }
    }

    pub fn trust_peer(&mut self, peer_id: &str, cert_der: &[u8]) -> Result<()> {
        let fingerprint = cert_fingerprint(cert_der);

        if let Some(existing) = self.known_peers.get(peer_id) {
            if *existing != fingerprint {
                return Err(anyhow!(
                    "peer {} already trusted with different certificate",
                    peer_id
                ));
            }
            debug!(peer_id = %peer_id, "peer already trusted with same certificate");
            return Ok(());
        }

        self.known_peers.insert(peer_id.to_string(), fingerprint);
        self.dirty.store(true, Ordering::Relaxed);

        let peer_cert_path = self
            .base_path
            .join("peers")
            .join(format!("{}.der", peer_id));
        if let Some(parent) = peer_cert_path.parent() {
            fs::create_dir_all(parent)?;
            if let Err(e) = restrict_permissions(parent, 0o700) {
                warn!(error = %e, path = ?parent, "failed to restrict peer certificate directory permissions");
            }
        }
        fs::write(&peer_cert_path, cert_der)
            .with_context(|| format!("failed to write peer certificate to {:?}", peer_cert_path))?;

        info!(peer_id = %peer_id, "trusted new peer certificate");
        Ok(())
    }

    pub fn remove_peer(&mut self, peer_id: &str) {
        if self.known_peers.remove(peer_id).is_some() {
            self.dirty.store(true, Ordering::Relaxed);

            let peer_cert_path = self
                .base_path
                .join("peers")
                .join(format!("{}.der", peer_id));
            if peer_cert_path.exists()
                && let Err(e) = fs::remove_file(&peer_cert_path)
            {
                warn!(error = %e, path = ?peer_cert_path, "failed to remove peer certificate file");
            }

            info!(peer_id = %peer_id, "removed peer from trusted list");
        }
    }

    pub fn list_trusted_peers(&self) -> Vec<String> {
        self.known_peers.keys().cloned().collect()
    }

    pub fn persist(&self) -> Result<()> {
        if !self.dirty.load(Ordering::Relaxed) {
            return Ok(());
        }

        let known_peers_path = self.base_path.join(KNOWN_PEERS_FILE);

        let mut toml_content = String::from("version = 1\n\n[peers]\n");

        for (peer_id, fingerprint) in &self.known_peers {
            let fp_b64 = BASE64_STANDARD.encode(fingerprint);
            toml_content.push_str(&format!("\"{}\" = \"sha256:{}\"\n", peer_id, fp_b64));
        }

        write_private_file(&known_peers_path, toml_content.as_bytes())
            .with_context(|| format!("failed to write known peers to {:?}", known_peers_path))?;

        debug!(path = ?known_peers_path, count = self.known_peers.len(), "persisted known peers");
        Ok(())
    }

    pub fn get_self_fingerprint(&self) -> Option<[u8; 32]> {
        self.self_cert_der.as_ref().map(|der| cert_fingerprint(der))
    }

    pub fn get_self_cert_der(&self) -> Option<&[u8]> {
        self.self_cert_der.as_deref()
    }

    pub fn get_self_key_der(&self) -> Option<&[u8]> {
        self.self_key_der.as_deref()
    }

    fn load_known_peers(path: &std::path::Path) -> Result<HashMap<String, [u8; 32]>> {
        let content = fs::read_to_string(path)
            .with_context(|| format!("failed to read known peers from {:?}", path))?;

        let value: toml::Value = toml::from_str(&content)
            .with_context(|| format!("failed to parse known peers TOML from {:?}", path))?;

        let mut peers = HashMap::new();

        if let Some(peers_table) = value.get("peers").and_then(|v| v.as_table()) {
            for (peer_id, fp_value) in peers_table {
                if let Some(fp_str) = fp_value.as_str() {
                    if let Some(fp_b64) = fp_str.strip_prefix("sha256:") {
                        match BASE64_STANDARD.decode(fp_b64) {
                            Ok(bytes) if bytes.len() == 32 => {
                                let mut fingerprint = [0u8; 32];
                                fingerprint.copy_from_slice(&bytes);
                                peers.insert(peer_id.clone(), fingerprint);
                            }
                            Ok(_) => {
                                warn!(peer_id = %peer_id, "invalid fingerprint length, skipping");
                            }
                            Err(e) => {
                                warn!(peer_id = %peer_id, error = %e, "failed to decode fingerprint, skipping");
                            }
                        }
                    } else {
                        warn!(peer_id = %peer_id, fp = %fp_str, "fingerprint missing sha256: prefix, skipping");
                    }
                }
            }
        }

        debug!(path = ?path, count = peers.len(), "loaded known peers");
        Ok(peers)
    }
}

impl Drop for CertificateStore {
    fn drop(&mut self) {
        if self.dirty.load(Ordering::Relaxed)
            && let Err(e) = self.persist()
        {
            warn!(error = %e, "failed to persist certificate store on drop");
        }
    }
}

fn generate_self_signed_cert(peer_id: &str) -> Result<(Vec<u8>, Vec<u8>)> {
    let mut params = CertificateParams::new(vec![])
        .map_err(|e| anyhow!("failed to create certificate params: {}", e))?;
    params.distinguished_name.push(DnType::CommonName, peer_id);
    params.key_usages.push(KeyUsagePurpose::DigitalSignature);
    params
        .extended_key_usages
        .push(ExtendedKeyUsagePurpose::ServerAuth);
    params
        .extended_key_usages
        .push(ExtendedKeyUsagePurpose::ClientAuth);

    let key_pair =
        KeyPair::generate().map_err(|e| anyhow!("failed to generate key pair: {}", e))?;

    let cert = params
        .self_signed(&key_pair)
        .map_err(|e| anyhow!("failed to sign certificate: {}", e))?;

    let cert_der = cert.der().to_vec();
    let key_der = key_pair.serialize_der();

    Ok((cert_der, key_der))
}

pub fn cert_fingerprint(cert_der: &[u8]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(cert_der);
    let result = hasher.finalize();
    let mut fingerprint = [0u8; 32];
    fingerprint.copy_from_slice(&result);
    fingerprint
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn cert_store_tofu_first_connection_trusts_any_cert() {
        let dir = TempDir::new().unwrap();
        let mut store = CertificateStore::load_or_create(dir.path()).unwrap();

        let (cert_der, _) = generate_self_signed_cert("test-peer").unwrap();
        assert!(!store.is_known_peer("test-peer"));

        store.trust_peer("test-peer", &cert_der).unwrap();
        assert!(store.is_known_peer("test-peer"));
        assert!(store.verify_fingerprint("test-peer", &cert_der));
    }

    #[test]
    fn cert_store_rejects_changed_fingerprint() {
        let dir = TempDir::new().unwrap();
        let mut store = CertificateStore::load_or_create(dir.path()).unwrap();

        let (cert1, _) = generate_self_signed_cert("peer-a").unwrap();
        let (cert2, _) = generate_self_signed_cert("peer-a").unwrap();

        store.trust_peer("peer-a", &cert1).unwrap();
        assert!(!store.verify_fingerprint("peer-a", &cert2));
    }

    #[test]
    fn cert_store_persists_across_reloads() {
        let dir = TempDir::new().unwrap();
        let (cert_der, _) = generate_self_signed_cert("persist-peer").unwrap();

        {
            let mut store = CertificateStore::load_or_create(dir.path()).unwrap();
            store.trust_peer("persist-peer", &cert_der).unwrap();
            store.persist().unwrap();
        }

        let store = CertificateStore::load_or_create(dir.path()).unwrap();
        assert!(store.is_known_peer("persist-peer"));
        assert!(store.verify_fingerprint("persist-peer", &cert_der));
    }

    #[test]
    fn cert_store_removes_peer() {
        let dir = TempDir::new().unwrap();
        let mut store = CertificateStore::load_or_create(dir.path()).unwrap();

        let (cert_der, _) = generate_self_signed_cert("remove-peer").unwrap();
        store.trust_peer("remove-peer", &cert_der).unwrap();
        assert!(store.is_known_peer("remove-peer"));

        store.remove_peer("remove-peer");
        assert!(!store.is_known_peer("remove-peer"));
    }

    #[test]
    fn cert_store_generates_self_cert() {
        let dir = TempDir::new().unwrap();
        let mut store = CertificateStore::load_or_create(dir.path()).unwrap();

        let (cert_der, _) = store.get_or_create_self_cert("my-peer-id").unwrap();
        assert!(!cert_der.is_empty());

        let fingerprint = store.get_self_fingerprint().unwrap();
        assert_ne!(fingerprint, [0u8; 32]);
    }

    #[cfg(unix)]
    #[test]
    fn private_files_are_restricted_on_unix() {
        use std::os::unix::fs::PermissionsExt;

        let dir = TempDir::new().unwrap();
        let mut store = CertificateStore::load_or_create(dir.path()).unwrap();
        let _ = store.get_or_create_self_cert("perm-peer").unwrap();
        store.trust_peer("other-peer", b"not-a-real-cert").unwrap();
        store.persist().unwrap();

        let dir_mode = fs::metadata(dir.path())
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(dir_mode, 0o700, "cert directory must not be world accessible");

        let key_mode = fs::metadata(dir.path().join(SELF_KEY_FILE))
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(key_mode, 0o600, "private key must be owner-only");

        let known_peers_mode = fs::metadata(dir.path().join(KNOWN_PEERS_FILE))
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(known_peers_mode, 0o600, "known peers must be owner-only");
    }

    #[test]
    fn cert_store_get_self_fingerprint_after_generation() {
        let dir = TempDir::new().unwrap();
        let mut store = CertificateStore::load_or_create(dir.path()).unwrap();
        let _ = store.get_or_create_self_cert("peer-self").unwrap();
        assert!(store.get_self_fingerprint().is_some());
    }

    #[test]
    fn cert_store_reuses_self_cert() {
        let dir = TempDir::new().unwrap();

        let cert_der1 = {
            let mut store = CertificateStore::load_or_create(dir.path()).unwrap();
            let (der, _) = store.get_or_create_self_cert("my-peer-id").unwrap();
            der
        };

        let cert_der2 = {
            let mut store = CertificateStore::load_or_create(dir.path()).unwrap();
            let (der, _) = store.get_or_create_self_cert("my-peer-id").unwrap();
            der
        };

        assert_eq!(cert_der1, cert_der2);
    }

    #[test]
    fn fingerprint_is_consistent() {
        let (cert_der, _) = generate_self_signed_cert("test").unwrap();
        let fp1 = cert_fingerprint(&cert_der);
        let fp2 = cert_fingerprint(&cert_der);
        assert_eq!(fp1, fp2);
    }

    #[test]
    fn different_certs_have_different_fingerprints() {
        let (cert1, _) = generate_self_signed_cert("peer1").unwrap();
        let (cert2, _) = generate_self_signed_cert("peer2").unwrap();
        let fp1 = cert_fingerprint(&cert1);
        let fp2 = cert_fingerprint(&cert2);
        assert_ne!(fp1, fp2);
    }

    #[test]
    fn key_format_works_with_rustls() {
        let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();

        let (cert_der, key_der) = generate_self_signed_cert("test").unwrap();

        let key = rustls::pki_types::PrivateKeyDer::try_from(key_der.clone())
            .expect("key should parse as PrivateKeyDer");

        let cert = rustls::pki_types::CertificateDer::from(cert_der.clone());

        let server_config = rustls::ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(vec![cert], key)
            .expect("should create server config with cert and key");

        let _quic_config = quinn::crypto::rustls::QuicServerConfig::try_from(server_config)
            .expect("should create QUIC server config");
    }
}

#[cfg(test)]
mod endpoint_tests {
    use super::*;
    use crate::platform::shared::quic::endpoint::QuicEndpoint;

    #[test]
    fn quic_endpoint_creation_works() {
        let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();

        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("tokio runtime should build");

        rt.block_on(async {
            let dir = tempfile::TempDir::new().unwrap();
            let cert_store = CertificateStore::load_or_create(dir.path()).unwrap();
            let endpoint = QuicEndpoint::new(0, "test-peer", cert_store);

            if let Err(e) = endpoint {
                panic!("failed to create endpoint: {e:?}");
            }
        });
    }
}
