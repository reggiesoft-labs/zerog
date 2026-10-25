//! Edge transition detection and topology management.
//!
//! This module manages display topology discovery and change detection.
//! Cursor ownership and in-flight handoffs are managed by `CursorRuntime`.

use anyhow::Result;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use tracing::debug;

use crate::platform::DisplayManager;
pub(crate) use super::runtime::cursor_runtime::HandoffContext;
use super::topology::DisplayTopology;

/// Interval for refreshing display topology.
const DISPLAY_TOPOLOGY_REFRESH_US: u64 = 250_000;

pub(crate) struct TransitionState {
    last_topology_refresh_us: AtomicU64,
    topology: Mutex<DisplayTopology>,
}

impl TransitionState {
    pub(crate) fn new(display: &dyn DisplayManager) -> Result<Self> {
        Ok(Self {
            last_topology_refresh_us: AtomicU64::new(0),
            topology: Mutex::new(DisplayTopology::discover(display)?),
        })
    }

    pub(crate) fn topology_snapshot(&self) -> Result<DisplayTopology> {
        Ok(self
            .topology
            .lock()
            .expect("topology lock poisoned")
            .clone())
    }

    pub(crate) fn refresh_topology_if_due(
        &self,
        display: &dyn DisplayManager,
        now_us: u64,
    ) -> Result<()> {
        let previous = self.last_topology_refresh_us.load(Ordering::Relaxed);
        if previous != 0 && now_us.saturating_sub(previous) < DISPLAY_TOPOLOGY_REFRESH_US {
            return Ok(());
        }
        self.last_topology_refresh_us
            .store(now_us, Ordering::Relaxed);

        let discovered = DisplayTopology::discover(display)?;
        let mut guard = self.topology.lock().expect("topology lock poisoned");
        if guard.signature != discovered.signature {
            debug!(
                previous_signature = guard.signature,
                new_signature = discovered.signature,
                display_count = discovered.displays.len(),
                "display topology changed"
            );
            *guard = discovered;
        }

        Ok(())
    }
}

