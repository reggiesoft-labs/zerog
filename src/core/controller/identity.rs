use crate::core::config;
use std::path::PathBuf;

#[derive(Debug, Clone)]
pub(super) struct LocalIdentity {
    pub(super) peer_id: String,
    pub(super) name: String,
}

impl LocalIdentity {
    pub(super) fn load() -> Self {
        let peer_id = config::load_or_create_peer_id().unwrap_or_else(|_| {
            tracing::warn!("failed to load peer id, generating temporary one");
            uuid::Uuid::new_v4().to_string()
        });

        let name = hostname::get()
            .ok()
            .and_then(|hostname| hostname.into_string().ok())
            .filter(|hostname| !hostname.trim().is_empty())
            .unwrap_or_else(|| peer_id.clone());

        Self { peer_id, name }
    }

    pub(super) fn peer_ref(&self) -> crate::core::api::PeerRefSnapshot {
        crate::core::api::PeerRefSnapshot {
            peer_id: self.peer_id.clone(),
            name: self.name.clone(),
            platform: Some(std::env::consts::OS.to_string()),
        }
    }

    pub(super) fn discovery_hostname(&self) -> String {
        hostname::get()
            .ok()
            .and_then(|hostname| hostname.into_string().ok())
            .filter(|hostname| !hostname.trim().is_empty())
            .unwrap_or_else(|| format!("zerog-{}", std::process::id()))
    }
}

pub(super) fn cert_path() -> PathBuf {
    if let Some(dir) = std::env::var_os("ZEROG_CERT_DIR").filter(|p| !p.is_empty()) {
        return PathBuf::from(dir);
    }
    let base = config::home_dir()
        .map(|h| {
            #[cfg(target_os = "macos")]
            {
                h.join("Library").join("Application Support")
            }
            #[cfg(not(target_os = "macos"))]
            {
                std::env::var_os("APPDATA")
                    .filter(|p| !p.is_empty())
                    .map(PathBuf::from)
                    .or_else(|| {
                        std::env::var_os("XDG_CONFIG_HOME")
                            .filter(|p| !p.is_empty())
                            .map(PathBuf::from)
                    })
                    .or_else(dirs::config_dir)
                    .unwrap_or_else(|| h.join(".config"))
            }
        })
        .unwrap_or_else(|_| {
            std::env::var_os("APPDATA")
                .filter(|p| !p.is_empty())
                .map(PathBuf::from)
                .or_else(dirs::config_dir)
                .unwrap_or_else(|| PathBuf::from("."))
        });
    let new_path = base.join("zerog").join("certs");
    if !new_path.exists() {
        let legacy_path = base.join("mouse-thing").join("certs");
        if legacy_path.exists() {
            return legacy_path;
        }
    }
    new_path
}
