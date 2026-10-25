use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::runtime::Handle;
use tokio::sync::{broadcast, mpsc};

use crate::core::events::Event;
use crate::core::platform::traits::NetworkState;
use crate::core::transfer::{
    DEFAULT_CHUNK_SIZE, FileChunkPayload, FileManifest, TransferEngine, TransferProgress,
};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum TransferEvent {
    OfferReceived(FileManifest),
    Progress(TransferProgress),
    Complete {
        transfer_id: String,
        success: bool,
        error: Option<String>,
        saved_path: Option<String>,
    },
}

pub struct TransferSync {
    engine: Arc<Mutex<TransferEngine>>,
    network: Arc<dyn NetworkState>,
    handle: Handle,
    events_tx: broadcast::Sender<TransferEvent>,
    chunk_tx: mpsc::UnboundedSender<FileChunkPayload>,
    worker_task: tokio::task::JoinHandle<()>,
}

impl TransferSync {
    pub fn new(network: Arc<dyn NetworkState>, handle: Handle) -> Self {
        let (events_tx, _) = broadcast::channel(128);
        let (chunk_tx, mut chunk_rx) = mpsc::unbounded_channel::<FileChunkPayload>();
        let engine = Arc::new(Mutex::new(TransferEngine::new()));

        let engine_for_worker = Arc::clone(&engine);
        let events_tx_for_worker = events_tx.clone();
        let network_for_worker = Arc::clone(&network);
        let worker_task = handle.spawn(async move {
            let mut last_progress_emit = Instant::now() - Duration::from_secs(1);
            let mut check_interval = tokio::time::interval(Duration::from_secs(2));
            check_interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

            loop {
                tokio::select! {
                    chunk_opt = chunk_rx.recv() => {
                        let Some(chunk) = chunk_opt else {
                            break;
                        };
                        let transfer_id = chunk.transfer_id.clone();
                        let result = {
                            let mut engine = engine_for_worker.lock().unwrap_or_else(|e| e.into_inner());
                            if !engine.has_incoming_transfer(&transfer_id) {
                                continue;
                            }
                            engine.write_chunk(&chunk)
                        };

                        match result {
                            Ok((progress, just_completed, saved_path)) => {
                                let now = Instant::now();
                                if just_completed || now.duration_since(last_progress_emit) >= Duration::from_millis(50) {
                                    last_progress_emit = now;
                                    let _ = events_tx_for_worker.send(TransferEvent::Progress(progress));
                                }
                                if just_completed {
                                    let _ = events_tx_for_worker.send(TransferEvent::Complete {
                                        transfer_id,
                                        success: true,
                                        error: None,
                                        saved_path: saved_path.map(|p| p.to_string_lossy().to_string()),
                                    });
                                }
                            }
                            Err(e) => {
                                {
                                    let mut engine = engine_for_worker.lock().unwrap_or_else(|e| e.into_inner());
                                    engine.abort_incoming(&transfer_id);
                                }
                                let cancel_event = Event::FileCancel {
                                    transfer_id: transfer_id.clone(),
                                };
                                let _ = network_for_worker.broadcast_packet(vec![cancel_event]);
                                let _ = events_tx_for_worker.send(TransferEvent::Complete {
                                    transfer_id,
                                    success: false,
                                    error: Some(format!("write error: {e}")),
                                    saved_path: None,
                                });
                            }
                        }
                    }
                    _ = check_interval.tick() => {
                        let timed_out = {
                            let mut engine = engine_for_worker.lock().unwrap_or_else(|e| e.into_inner());
                            engine.check_timed_out_incoming(Duration::from_secs(10))
                        };
                        for transfer_id in timed_out {
                            let cancel_event = Event::FileCancel {
                                transfer_id: transfer_id.clone(),
                            };
                            let _ = network_for_worker.broadcast_packet(vec![cancel_event]);
                            let _ = events_tx_for_worker.send(TransferEvent::Complete {
                                transfer_id,
                                success: false,
                                error: Some("transfer timed out: sender stopped responding".to_string()),
                                saved_path: None,
                            });
                        }
                    }
                }
            }
        });

        Self {
            engine,
            network,
            handle,
            events_tx,
            chunk_tx,
            worker_task,
        }
    }

    pub fn stop(&self) {
        self.worker_task.abort();
    }
}

impl Drop for TransferSync {
    fn drop(&mut self) {
        self.worker_task.abort();
    }
}

impl TransferSync {

    pub fn subscribe(&self) -> broadcast::Receiver<TransferEvent> {
        self.events_tx.subscribe()
    }

    pub fn start_transfer(
        &self,
        paths: Vec<PathBuf>,
        sender_peer_id: &str,
        sender_name: &str,
        target_peer_id: Option<String>,
    ) -> Result<FileManifest> {
        let manifest = {
            let mut engine = self.engine.lock().unwrap_or_else(|e| e.into_inner());
            engine.create_offer(sender_peer_id, sender_name, target_peer_id, paths)?
        };

        let event = Event::FileOffer {
            manifest: manifest.clone(),
        };
        self.network.broadcast_packet(vec![event])?;
        Ok(manifest)
    }

    pub fn accept_transfer(&self, transfer_id: &str, target_dir: PathBuf) -> Result<()> {
        {
            let mut engine = self.engine.lock().unwrap_or_else(|e| e.into_inner());
            engine.accept_offer(transfer_id, target_dir)?;
        }

        let event = Event::FileAccept {
            transfer_id: transfer_id.to_string(),
        };
        self.network.broadcast_packet(vec![event])?;
        Ok(())
    }

    pub fn reject_transfer(&self, transfer_id: &str, reason: &str) -> Result<()> {
        {
            let mut engine = self.engine.lock().unwrap_or_else(|e| e.into_inner());
            engine.reject_offer(transfer_id);
        }

        let event = Event::FileReject {
            transfer_id: transfer_id.to_string(),
            reason: reason.to_string(),
        };
        self.network.broadcast_packet(vec![event])?;
        Ok(())
    }

    pub fn handle_received_offer(&self, manifest: FileManifest) {
        let is_new = {
            let mut engine = self.engine.lock().unwrap_or_else(|e| e.into_inner());
            engine.receive_offer(manifest.clone())
        };
        if is_new {
            let _ = self.events_tx.send(TransferEvent::OfferReceived(manifest));
        }
    }

    pub fn handle_received_accept(&self, transfer_id: String) {
        {
            let mut engine = self.engine.lock().unwrap_or_else(|e| e.into_inner());
            if !engine.mark_outgoing_started(&transfer_id) {
                return;
            }
        }

        let engine_clone = Arc::clone(&self.engine);
        let network_clone = Arc::clone(&self.network);
        let events_tx = self.events_tx.clone();
        let tx_id = transfer_id.clone();

        self.handle.spawn(async move {
            let mut last_progress_emit = Instant::now() - Duration::from_secs(1);
            loop {
                if !network_clone.has_any_peer() {
                    let _ = events_tx.send(TransferEvent::Complete {
                        transfer_id: tx_id.clone(),
                        success: false,
                        error: Some("transfer failed: peer disconnected".to_string()),
                        saved_path: None,
                    });
                    break;
                }

                let next_chunk = {
                    let mut engine = engine_clone.lock().unwrap_or_else(|e| e.into_inner());
                    engine.read_next_chunk(&tx_id, DEFAULT_CHUNK_SIZE)
                };

                match next_chunk {
                    Ok(Some((payload, progress, is_transfer_done))) => {
                        let event = Event::FileChunk { chunk: payload };
                        if let Err(e) = network_clone.broadcast_packet(vec![event]) {
                            let _ = events_tx.send(TransferEvent::Complete {
                                transfer_id: tx_id.clone(),
                                success: false,
                                error: Some(format!("failed to send chunk: {e}")),
                                saved_path: None,
                            });
                            break;
                        }

                        let now = Instant::now();
                        if is_transfer_done || now.duration_since(last_progress_emit) >= Duration::from_millis(50) {
                            last_progress_emit = now;
                            let _ = events_tx.send(TransferEvent::Progress(progress));
                        }

                        if is_transfer_done {
                            let is_active = {
                                let mut engine = engine_clone.lock().unwrap_or_else(|e| e.into_inner());
                                engine.cancel(&tx_id)
                            };
                            if is_active {
                                let _ = events_tx.send(TransferEvent::Complete {
                                    transfer_id: tx_id.clone(),
                                    success: true,
                                    error: None,
                                    saved_path: None,
                                });
                            }
                            break;
                        }
                    }
                    Ok(None) => {
                        break;
                    }
                    Err(e) => {
                        let cancel_event = Event::FileCancel {
                            transfer_id: tx_id.clone(),
                        };
                        let _ = network_clone.broadcast_packet(vec![cancel_event]);
                        let _ = events_tx.send(TransferEvent::Complete {
                            transfer_id: tx_id.clone(),
                            success: false,
                            error: Some(format!("read error: {e}")),
                            saved_path: None,
                        });
                        break;
                    }
                }
                tokio::time::sleep(Duration::from_millis(2)).await;
            }
        });
    }

    pub fn handle_received_chunk(&self, chunk: FileChunkPayload) {
        let _ = self.chunk_tx.send(chunk);
    }

    pub fn handle_received_reject(&self, transfer_id: &str, reason: &str) {
        let removed = {
            let mut engine = self.engine.lock().unwrap_or_else(|e| e.into_inner());
            engine.cancel(transfer_id)
        };
        if removed {
            let _ = self.events_tx.send(TransferEvent::Complete {
                transfer_id: transfer_id.to_string(),
                success: false,
                error: Some(reason.to_string()),
                saved_path: None,
            });
        }
    }

    pub fn handle_received_cancel(&self, transfer_id: &str) {
        let removed = {
            let mut engine = self.engine.lock().unwrap_or_else(|e| e.into_inner());
            engine.cancel(transfer_id)
        };
        if removed {
            let _ = self.events_tx.send(TransferEvent::Complete {
                transfer_id: transfer_id.to_string(),
                success: false,
                error: Some("cancelled by remote peer".to_string()),
                saved_path: None,
            });
        }
    }

    pub fn notify_peer_disconnected(&self, peer_id: Option<&str>) {
        if peer_id.is_none() && self.network.has_any_peer() {
            return;
        }

        let aborted = {
            let mut engine = self.engine.lock().unwrap_or_else(|e| e.into_inner());
            engine.abort_transfers_for_peer(peer_id)
        };

        for (transfer_id, is_incoming) in aborted {
            let error_msg = if is_incoming {
                "transfer failed: sender disconnected"
            } else {
                "transfer failed: receiver disconnected"
            };
            let _ = self.events_tx.send(TransferEvent::Complete {
                transfer_id,
                success: false,
                error: Some(error_msg.to_string()),
                saved_path: None,
            });
        }
    }

    pub fn get_pending_offer(&self, transfer_id: &str) -> Option<FileManifest> {
        let engine = self.engine.lock().unwrap_or_else(|e| e.into_inner());
        engine.get_pending_offer(transfer_id).cloned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::File;
    use std::io::Write;
    use std::net::SocketAddr;
    use uuid::Uuid;

    struct TestNetwork {
        sent: Mutex<Vec<Event>>,
    }

    impl NetworkState for TestNetwork {
        fn broadcast_packet(&self, events: Vec<Event>) -> anyhow::Result<()> {
            self.sent.lock().unwrap().extend(events);
            Ok(())
        }

        fn active_target(&self) -> Option<SocketAddr> {
            None
        }

        fn has_any_peer(&self) -> bool {
            true
        }
    }

    #[tokio::test]
    async fn transfer_sync_async_worker_delivers_file() {
        let temp_dir = std::env::temp_dir().join(format!("mouse_thing_sync_test_{}", Uuid::new_v4()));
        std::fs::create_dir_all(&temp_dir).unwrap();

        let src_file = temp_dir.join("payload.bin");
        {
            let mut f = File::create(&src_file).unwrap();
            f.write_all(b"async worker payload test").unwrap();
        }

        let network = Arc::new(TestNetwork {
            sent: Mutex::new(Vec::new()),
        });
        let sync = TransferSync::new(network, Handle::current());

        let manifest = sync
            .start_transfer(vec![src_file], "peer_a", "Sender", None)
            .unwrap();

        sync.handle_received_offer(manifest.clone());

        let dest_dir = temp_dir.join("dest");
        sync.accept_transfer(&manifest.transfer_id, dest_dir.clone())
            .unwrap();

        let mut rx = sync.subscribe();

        let chunk = FileChunkPayload {
            transfer_id: manifest.transfer_id.clone(),
            file_id: manifest.files[0].id.clone(),
            offset: 0,
            data: b"async worker payload test".to_vec(),
            is_last: true,
        };

        sync.handle_received_chunk(chunk);

        let mut complete_received = false;
        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline {
            if let Ok(event) = rx.try_recv() {
                if let TransferEvent::Complete { success, saved_path, .. } = event {
                    assert!(success);
                    assert!(saved_path.is_some());
                    complete_received = true;
                    break;
                }
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }

        assert!(complete_received);

        let dest_file = dest_dir.join("payload.bin");
        let content = std::fs::read(&dest_file).unwrap();
        assert_eq!(content, b"async worker payload test");

        let _ = std::fs::remove_dir_all(&temp_dir);
    }

    #[tokio::test]
    async fn transfer_sync_disconnect_aborts_incoming_and_cleans_file() {
        let temp_dir = std::env::temp_dir().join(format!("mouse_thing_sync_abort_{}", Uuid::new_v4()));
        std::fs::create_dir_all(&temp_dir).unwrap();

        let src_file = temp_dir.join("partial.bin");
        {
            let mut f = File::create(&src_file).unwrap();
            f.write_all(b"partial content before disconnect").unwrap();
        }

        let network = Arc::new(TestNetwork {
            sent: Mutex::new(Vec::new()),
        });
        let sync = TransferSync::new(network, Handle::current());

        let manifest = sync
            .start_transfer(vec![src_file], "peer_a", "Sender", None)
            .unwrap();

        sync.handle_received_offer(manifest.clone());

        let dest_dir = temp_dir.join("dest");
        sync.accept_transfer(&manifest.transfer_id, dest_dir.clone())
            .unwrap();

        let mut rx = sync.subscribe();

        let chunk = FileChunkPayload {
            transfer_id: manifest.transfer_id.clone(),
            file_id: manifest.files[0].id.clone(),
            offset: 0,
            data: b"partial content".to_vec(),
            is_last: false,
        };

        sync.handle_received_chunk(chunk);

        let dest_file = dest_dir.join("partial.bin");
        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline {
            if dest_file.exists() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(dest_file.exists());

        sync.notify_peer_disconnected(Some("peer_a"));

        let mut failed_received = false;
        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline {
            if let Ok(event) = rx.try_recv() {
                if let TransferEvent::Complete { success, .. } = event {
                    assert!(!success);
                    failed_received = true;
                    break;
                }
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }

        assert!(failed_received);
        assert!(!dest_file.exists());

        let _ = std::fs::remove_dir_all(&temp_dir);
    }

    #[tokio::test]
    async fn transfer_sync_disconnect_aborts_outgoing() {
        let temp_dir = std::env::temp_dir().join(format!("mouse_thing_out_abort_{}", Uuid::new_v4()));
        std::fs::create_dir_all(&temp_dir).unwrap();

        let src_file = temp_dir.join("outgoing.bin");
        {
            let mut f = File::create(&src_file).unwrap();
            f.write_all(b"outgoing data").unwrap();
        }

        let network = Arc::new(TestNetwork {
            sent: Mutex::new(Vec::new()),
        });
        let sync = TransferSync::new(network, Handle::current());

        let manifest = sync
            .start_transfer(vec![src_file], "peer_local", "Sender", Some("peer_b".to_string()))
            .unwrap();

        let mut rx = sync.subscribe();

        sync.notify_peer_disconnected(Some("peer_b"));

        let mut failed_received = false;
        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline {
            if let Ok(event) = rx.try_recv() {
                if let TransferEvent::Complete { success, transfer_id, .. } = event {
                    if transfer_id == manifest.transfer_id {
                        assert!(!success);
                        failed_received = true;
                        break;
                    }
                }
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }

        assert!(failed_received);
        let _ = std::fs::remove_dir_all(&temp_dir);
    }
}
