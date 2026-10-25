use crate::core::api::ConfigSnapshot;
use crate::core::config;
use crate::core::error::{CoreError, CoreResult};
use crate::core::events::LayoutUpdateEvent;
use crate::core::platform::types::PeerEdge;
use std::sync::{Arc, Mutex};

#[derive(Clone)]
pub(super) struct ConfigStore {
    snapshot: Arc<Mutex<ConfigSnapshot>>,
}

impl Default for ConfigStore {
    fn default() -> Self {
        Self {
            snapshot: Arc::new(Mutex::new(ConfigSnapshot::default())),
        }
    }
}

impl ConfigStore {
    pub(super) fn load() -> Self {
        let snapshot = config::load_or_default()
            .map(ConfigSnapshot::from)
            .unwrap_or_default();
        Self {
            snapshot: Arc::new(Mutex::new(snapshot)),
        }
    }

    pub(super) fn snapshot(&self) -> ConfigSnapshot {
        self.snapshot
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    pub(super) fn replace(&self, snapshot: ConfigSnapshot) {
        *self.snapshot.lock().unwrap_or_else(|e| e.into_inner()) = snapshot;
    }

    pub(super) fn persist(&self) -> CoreResult<()> {
        let snapshot = self.snapshot();
        let app_config = config::AppConfig::from(&snapshot);
        config::save(&app_config).map_err(|e| CoreError::Config(e.to_string()))?;
        Ok(())
    }

    pub(super) fn pair_peer(&self, peer_id: &str) -> CoreResult<ConfigSnapshot> {
        let mut snapshot = self.snapshot();
        let canonical = crate::core::peer_state::canonical_peer_id(peer_id);
        if !snapshot.is_peer_paired(&canonical) {
            snapshot.paired_peers.push(canonical);
            self.replace(snapshot.clone());
            self.persist()?;
        }
        Ok(snapshot)
    }

    pub(super) fn unpair_peer(&self, peer_id: &str) -> CoreResult<ConfigSnapshot> {
        let mut snapshot = self.snapshot();
        snapshot.unpair_peer(peer_id);
        self.replace(snapshot.clone());
        self.persist()?;
        Ok(snapshot)
    }

    pub(super) fn pause_peer(&self, peer_id: &str) -> CoreResult<ConfigSnapshot> {
        let mut snapshot = self.snapshot();
        if !snapshot.is_peer_paused(peer_id) {
            snapshot.pause_peer(peer_id);
            self.replace(snapshot.clone());
            self.persist()?;
        }
        Ok(snapshot)
    }

    pub(super) fn resume_peer(&self, peer_id: &str) -> CoreResult<ConfigSnapshot> {
        let mut snapshot = self.snapshot();
        if snapshot.is_peer_paused(peer_id) {
            snapshot.resume_peer(peer_id);
            self.replace(snapshot.clone());
            self.persist()?;
        }
        Ok(snapshot)
    }

    pub(super) fn apply_layout_update(
        &self,
        update: LayoutUpdateEvent,
    ) -> CoreResult<Option<ConfigSnapshot>> {
        let mut snapshot = self.snapshot.lock().unwrap_or_else(|e| e.into_inner());
        if snapshot.placements == update.placements {
            return Ok(None);
        }

        snapshot.placements = update.placements;
        drop(snapshot);
        self.persist()?;
        Ok(Some(self.snapshot()))
    }
}

pub(super) fn primary_peer_edge(config: &ConfigSnapshot) -> PeerEdge {
    if config.left.is_some() {
        PeerEdge::Left
    } else if config.right.is_some() {
        PeerEdge::Right
    } else if config.top.is_some() {
        PeerEdge::Top
    } else if config.bottom.is_some() {
        PeerEdge::Bottom
    } else {
        PeerEdge::Right
    }
}
