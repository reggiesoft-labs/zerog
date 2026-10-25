use crate::core::platform::PlatformEngineTestExt;
use crate::core::platform::engine::RuntimeContactSnapshot;
use crate::events::Event;
use crate::platform::shared::runtime::{is_input_event, process_network_event_internal};
use crate::platform::{EventCapture, EventInjection, TransferMode};

use super::{PlatformEngineImpl, RuntimeEngineImpl};

impl PlatformEngineTestExt for PlatformEngineImpl {
    fn get_capture(&self) -> &dyn EventCapture {
        self.capture.as_ref()
    }

    fn get_injection(&self) -> &dyn EventInjection {
        self.display.as_ref();
        panic!("platform-only engine does not have injection");
    }

    fn get_transfer_mode(&self) -> TransferMode {
        TransferMode::LocalControl
    }

    fn set_transfer_mode(&self, _mode: TransferMode) -> anyhow::Result<()> {
        Ok(())
    }

    fn process_network_event(&self, _event: Event) -> anyhow::Result<()> {
        anyhow::bail!("platform-only engine cannot process network events")
    }

    fn send_events(&self, _events: Vec<Event>) -> anyhow::Result<()> {
        anyhow::bail!("platform-only engine cannot send events")
    }

    fn get_runtime_contacts(&self) -> Vec<RuntimeContactSnapshot> {
        Vec::new()
    }

    fn refresh_display_topology(&self) -> anyhow::Result<()> {
        Ok(())
    }

    fn close_connection_to(&self, _peer_id: &str) {}

    fn advance_peer_last_recv(&self, _addr: std::net::SocketAddr, _elapsed_us: u64) {}

    fn get_connection_count(&self, _peer_id: &str) -> usize {
        0
    }
}

impl PlatformEngineTestExt for RuntimeEngineImpl {
    fn get_capture(&self) -> &dyn EventCapture {
        self.platform.capture.as_ref()
    }

    fn get_injection(&self) -> &dyn EventInjection {
        self.session.injection()
    }

    fn get_transfer_mode(&self) -> TransferMode {
        self.session.cursor().mode()
    }

    fn set_transfer_mode(&self, mode: TransferMode) -> anyhow::Result<()> {
        if let Some(cursor) = self.session.runtime_cursor() {
            cursor.set_mode(mode, self.session.injection())?;
        } else {
            self.session
                .cursor()
                .set_mode(mode, self.session.injection())?;
        }
        Ok(())
    }

    fn process_network_event(&self, event: Event) -> anyhow::Result<()> {
        let cursor = self.session.runtime_cursor();
        process_network_event_internal(self.session.injection(), cursor, event)
    }

    fn send_events(&self, events: Vec<Event>) -> anyhow::Result<()> {
        let network = self
            .session
            .runtime_network()
            .ok_or_else(|| anyhow::anyhow!("network runtime is not active"))?;

        for event in events {
            if is_input_event(&event) {
                network.mark_input();
                let capture_us = network.mono_us();
                network.queue_input_event(event, capture_us)?;
            } else {
                network.flush_pending_input_now()?;
                network.broadcast_packet(vec![event])?;
            }
        }
        Ok(())
    }

    fn get_runtime_contacts(&self) -> Vec<RuntimeContactSnapshot> {
        self.session
            .runtime_layout()
            .map(|layout| {
                layout
                    .contacts()
                    .into_iter()
                    .map(|contact| RuntimeContactSnapshot {
                        peer_id: contact.peer_id,
                        addr: contact.addr,
                        local: contact.local,
                        remote: contact.remote,
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    fn refresh_display_topology(&self) -> anyhow::Result<()> {
        let Some(transition) = self.session.runtime_transition() else {
            return Ok(());
        };

        transition.refresh_topology_if_due(self.session.display(), u64::MAX)
    }

    fn close_connection_to(&self, peer_id: &str) {
        let network = self.session.runtime_network();
        if let Some(network) = network {
            if let Some(conn) = network.endpoint().get_connection(peer_id) {
                conn.close(b"test partition");
                return;
            }
            for addr in network.peer_addrs() {
                if let Some(id) = network.peer_id_for_addr(addr)
                    && id == peer_id
                    && let Some(conn) = network.endpoint().get_connection_by_addr(addr)
                {
                    conn.close(b"test partition");
                    return;
                }
            }
            for conn in network.endpoint().all_connections() {
                conn.close(b"test partition");
            }
        }
    }

    fn advance_peer_last_recv(&self, addr: std::net::SocketAddr, elapsed_us: u64) {
        let network = self.session.runtime_network();
        if let Some(network) = network {
            let now_us = network.mono_us();
            let fake_last_recv = now_us.saturating_sub(elapsed_us).max(1);
            network.set_peer_last_recv(addr, fake_last_recv);
        }
    }

    fn get_connection_count(&self, peer_id: &str) -> usize {
        let network = self.session.runtime_network();
        if let Some(network) = network
            && let Some(conn) = network.endpoint().get_connection(peer_id)
        {
            return if conn.is_closed() { 0 } else { 1 };
        }
        0
    }

    fn mark_peer_connected(&self, peer_id: &str) {
        if let Some(network) = self.session.runtime_network() {
            network.mark_connected(peer_id);
        }
    }
}
