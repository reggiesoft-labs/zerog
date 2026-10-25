use anyhow::{Context, Result, bail};
use std::collections::HashMap;
use std::fs::{File, OpenOptions, create_dir_all};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use uuid::Uuid;

use super::manifest::{FileChunkPayload, FileItem, FileManifest, TransferProgress};

pub const DEFAULT_CHUNK_SIZE: usize = 65536;

pub struct OutgoingTransfer {
    pub manifest: FileManifest,
    pub paths: Vec<PathBuf>,
    pub current_file_idx: usize,
    pub current_offset: u64,
    pub bytes_sent: u64,
    current_file_handle: Option<File>,
    is_started: bool,
}

pub struct IncomingTransfer {
    pub manifest: FileManifest,
    pub target_dir: PathBuf,
    pub current_file_idx: usize,
    pub bytes_received: u64,
    current_file_handle: Option<File>,
    is_completed: bool,
    pub last_activity: Instant,
}

pub struct TransferEngine {
    outgoing: HashMap<String, OutgoingTransfer>,
    incoming: HashMap<String, IncomingTransfer>,
    pending_offers: HashMap<String, FileManifest>,
}

impl Default for TransferEngine {
    fn default() -> Self {
        Self::new()
    }
}

impl TransferEngine {
    pub fn new() -> Self {
        Self {
            outgoing: HashMap::new(),
            incoming: HashMap::new(),
            pending_offers: HashMap::new(),
        }
    }

    pub fn create_offer(
        &mut self,
        sender_peer_id: &str,
        sender_name: &str,
        target_peer_id: Option<String>,
        paths: Vec<PathBuf>,
    ) -> Result<FileManifest> {
        if paths.is_empty() {
            bail!("cannot create file transfer offer with empty path list");
        }

        let mut files = Vec::new();
        let mut total_size = 0u64;

        for path in &paths {
            let metadata = std::fs::metadata(path)
                .with_context(|| format!("failed to read metadata for {}", path.display()))?;
            let name = path
                .file_name()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_else(|| "unnamed_file".to_string());
            let size = metadata.len();
            total_size += size;

            files.push(FileItem {
                id: Uuid::new_v4().to_string(),
                name,
                size,
            });
        }

        let transfer_id = Uuid::new_v4().to_string();
        let manifest = FileManifest {
            transfer_id: transfer_id.clone(),
            sender_peer_id: sender_peer_id.to_string(),
            sender_name: sender_name.to_string(),
            target_peer_id,
            files,
            total_size,
        };

        let outgoing = OutgoingTransfer {
            manifest: manifest.clone(),
            paths,
            current_file_idx: 0,
            current_offset: 0,
            bytes_sent: 0,
            current_file_handle: None,
            is_started: false,
        };

        self.outgoing.insert(transfer_id, outgoing);
        Ok(manifest)
    }

    pub fn mark_outgoing_started(&mut self, transfer_id: &str) -> bool {
        if let Some(outgoing) = self.outgoing.get_mut(transfer_id) {
            if outgoing.is_started {
                return false;
            }
            outgoing.is_started = true;
            return true;
        }
        false
    }

    pub fn has_incoming_transfer(&self, transfer_id: &str) -> bool {
        self.incoming.contains_key(transfer_id)
    }

    pub fn has_outgoing_transfer(&self, transfer_id: &str) -> bool {
        self.outgoing.contains_key(transfer_id)
    }

    pub fn read_next_chunk(
        &mut self,
        transfer_id: &str,
        chunk_size: usize,
    ) -> Result<Option<(FileChunkPayload, TransferProgress, bool)>> {
        let outgoing = match self.outgoing.get_mut(transfer_id) {
            Some(t) => {
                t
            }
            None => {
                return Ok(None);
            }
        };

        if outgoing.current_file_idx >= outgoing.paths.len() {
            return Ok(None);
        }

        if outgoing.current_file_handle.is_none() {
            let path = &outgoing.paths[outgoing.current_file_idx];
            let file = File::open(path)
                .with_context(|| format!("failed to open file {} for reading", path.display()))?;
            outgoing.current_file_handle = Some(file);
        }

        let file = outgoing
            .current_file_handle
            .as_mut()
            .expect("file handle initialized above");
        let file_meta = &outgoing.manifest.files[outgoing.current_file_idx];

        file.seek(SeekFrom::Start(outgoing.current_offset))?;
        let mut buffer = vec![0u8; chunk_size];
        let bytes_read = file.read(&mut buffer)?;
        buffer.truncate(bytes_read);

        let offset = outgoing.current_offset;
        outgoing.current_offset += bytes_read as u64;
        outgoing.bytes_sent += bytes_read as u64;

        let is_last = outgoing.current_offset >= file_meta.size || bytes_read == 0;
        let payload = FileChunkPayload {
            transfer_id: transfer_id.to_string(),
            file_id: file_meta.id.clone(),
            offset,
            data: buffer,
            is_last,
        };

        if is_last {
            outgoing.current_file_handle = None;
            outgoing.current_file_idx += 1;
            outgoing.current_offset = 0;
        }

        let is_transfer_done = outgoing.current_file_idx >= outgoing.paths.len();
        let total = outgoing.manifest.total_size;
        let percent = if total > 0 {
            (outgoing.bytes_sent as f32 / total as f32).min(1.0)
        } else {
            1.0
        };

        let progress = TransferProgress {
            transfer_id: transfer_id.to_string(),
            bytes_transferred: outgoing.bytes_sent,
            total_bytes: total,
            percent,
            is_outgoing: true,
        };

        Ok(Some((payload, progress, is_transfer_done)))
    }

    pub fn receive_offer(&mut self, manifest: FileManifest) -> bool {
        if self.pending_offers.contains_key(&manifest.transfer_id)
            || self.incoming.contains_key(&manifest.transfer_id)
        {
            return false;
        }
        self.pending_offers
            .insert(manifest.transfer_id.clone(), manifest);
        true
    }

    pub fn get_pending_offer(&self, transfer_id: &str) -> Option<&FileManifest> {
        self.pending_offers.get(transfer_id)
    }

    pub fn accept_offer(&mut self, transfer_id: &str, target_dir: PathBuf) -> Result<FileManifest> {
        let manifest = self
            .pending_offers
            .remove(transfer_id)
            .ok_or_else(|| anyhow::anyhow!("transfer {} not found in pending offers", transfer_id))?;

        create_dir_all(&target_dir).with_context(|| {
            format!(
                "failed to create target directory {}",
                target_dir.display()
            )
        })?;

        let incoming = IncomingTransfer {
            manifest: manifest.clone(),
            target_dir,
            current_file_idx: 0,
            bytes_received: 0,
            current_file_handle: None,
            is_completed: false,
            last_activity: Instant::now(),
        };

        self.incoming.insert(transfer_id.to_string(), incoming);
        Ok(manifest)
    }

    pub fn reject_offer(&mut self, transfer_id: &str) -> Option<FileManifest> {
        self.pending_offers.remove(transfer_id)
    }

    pub fn write_chunk(
        &mut self,
        chunk: &FileChunkPayload,
    ) -> Result<(TransferProgress, bool, Option<PathBuf>)> {
        let incoming = self
            .incoming
            .get_mut(&chunk.transfer_id)
            .ok_or_else(|| anyhow::anyhow!("unknown incoming transfer {}", chunk.transfer_id))?;

        incoming.last_activity = Instant::now();

        if incoming.current_file_idx >= incoming.manifest.files.len() {
            let total = incoming.manifest.total_size;
            let saved_path = if incoming.manifest.files.len() == 1 {
                let safe_name = Path::new(&incoming.manifest.files[0].name)
                    .file_name()
                    .map(|s| s.to_string_lossy().to_string())
                    .unwrap_or_else(|| "file_0".to_string());
                Some(incoming.target_dir.join(safe_name))
            } else {
                Some(incoming.target_dir.clone())
            };
            return Ok((
                TransferProgress {
                    transfer_id: chunk.transfer_id.clone(),
                    bytes_transferred: total,
                    total_bytes: total,
                    percent: 1.0,
                    is_outgoing: false,
                },
                false,
                saved_path,
            ));
        }

        let file_meta = &incoming.manifest.files[incoming.current_file_idx];
        if file_meta.id != chunk.file_id {
            bail!(
                "chunk file id {} does not match expected {}",
                chunk.file_id,
                file_meta.id
            );
        }

        if incoming.current_file_handle.is_none() {
            let safe_name = Path::new(&file_meta.name)
                .file_name()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_else(|| format!("file_{}", incoming.current_file_idx));
            let dest_path = incoming.target_dir.join(safe_name);
            let file = OpenOptions::new()
                .create(true)
                .write(true)
                .truncate(chunk.offset == 0)
                .open(&dest_path)
                .with_context(|| format!("failed to open destination file {}", dest_path.display()))?;
            incoming.current_file_handle = Some(file);
        }

        let file = incoming
            .current_file_handle
            .as_mut()
            .expect("file handle initialized above");
        file.seek(SeekFrom::Start(chunk.offset))?;
        file.write_all(&chunk.data)?;

        incoming.bytes_received += chunk.data.len() as u64;

        if chunk.is_last {
            file.flush()?;
            incoming.current_file_handle = None;
            incoming.current_file_idx += 1;
        }

        let is_complete = incoming.current_file_idx >= incoming.manifest.files.len();
        let just_completed = if is_complete && !incoming.is_completed {
            incoming.is_completed = true;
            true
        } else {
            false
        };

        let total = incoming.manifest.total_size;
        let percent = if total > 0 {
            (incoming.bytes_received as f32 / total as f32).min(1.0)
        } else {
            1.0
        };

        let saved_path = if is_complete {
            if incoming.manifest.files.len() == 1 {
                let safe_name = Path::new(&incoming.manifest.files[0].name)
                    .file_name()
                    .map(|s| s.to_string_lossy().to_string())
                    .unwrap_or_else(|| "file_0".to_string());
                Some(incoming.target_dir.join(safe_name))
            } else {
                Some(incoming.target_dir.clone())
            }
        } else {
            None
        };

        Ok((
            TransferProgress {
                transfer_id: chunk.transfer_id.clone(),
                bytes_transferred: incoming.bytes_received,
                total_bytes: total,
                percent,
                is_outgoing: false,
            },
            just_completed,
            saved_path,
        ))
    }

    pub fn abort_incoming(&mut self, transfer_id: &str) -> Option<PathBuf> {
        let mut incoming = self.incoming.remove(transfer_id)?;
        if incoming.is_completed {
            return None;
        }
        incoming.current_file_handle = None;
        let mut last_path = None;
        for (idx, file_meta) in incoming.manifest.files.iter().enumerate() {
            if idx <= incoming.current_file_idx {
                let safe_name = Path::new(&file_meta.name)
                    .file_name()
                    .map(|s| s.to_string_lossy().to_string())
                    .unwrap_or_else(|| format!("file_{idx}"));
                let partial_path = incoming.target_dir.join(safe_name);
                let _ = std::fs::remove_file(&partial_path);
                last_path = Some(partial_path);
            }
        }
        last_path
    }

    pub fn abort_transfers_for_peer(
        &mut self,
        peer_id: Option<&str>,
    ) -> Vec<(String, bool)> {
        let mut aborted = Vec::new();

        let incoming_ids: Vec<String> = self
            .incoming
            .iter()
            .filter(|(_, inc)| {
                if inc.is_completed {
                    return false;
                }
                match peer_id {
                    Some(id) => inc.manifest.sender_peer_id == id,
                    None => true,
                }
            })
            .map(|(id, _)| id.clone())
            .collect();

        for id in incoming_ids {
            self.abort_incoming(&id);
            aborted.push((id, true));
        }

        let outgoing_ids: Vec<String> = self
            .outgoing
            .iter()
            .filter(|(_, out)| match peer_id {
                Some(id) => out.manifest.target_peer_id.as_deref() == Some(id),
                None => true,
            })
            .map(|(id, _)| id.clone())
            .collect();

        for id in outgoing_ids {
            self.outgoing.remove(&id);
            aborted.push((id, false));
        }

        let offer_ids: Vec<String> = self
            .pending_offers
            .iter()
            .filter(|(_, off)| match peer_id {
                Some(id) => off.sender_peer_id == id,
                None => true,
            })
            .map(|(id, _)| id.clone())
            .collect();

        for id in offer_ids {
            self.pending_offers.remove(&id);
            aborted.push((id, true));
        }

        aborted
    }

    pub fn check_timed_out_incoming(&mut self, timeout: Duration) -> Vec<String> {
        let now = Instant::now();
        let timed_out: Vec<String> = self
            .incoming
            .iter()
            .filter(|(_, inc)| {
                !inc.is_completed && now.duration_since(inc.last_activity) >= timeout
            })
            .map(|(id, _)| id.clone())
            .collect();

        for id in &timed_out {
            self.abort_incoming(id);
        }
        timed_out
    }

    pub fn cancel(&mut self, transfer_id: &str) -> bool {
        let o = self.outgoing.remove(transfer_id).is_some();
        let i = self.incoming.contains_key(transfer_id);
        if i {
            self.abort_incoming(transfer_id);
        }
        let p = self.pending_offers.remove(transfer_id).is_some();
        o || i || p
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn transfer_engine_round_trip() {
        let temp_dir = std::env::temp_dir().join(format!("mouse_thing_test_{}", Uuid::new_v4()));
        create_dir_all(&temp_dir).unwrap();

        let src_file = temp_dir.join("source.txt");
        {
            let mut f = File::create(&src_file).unwrap();
            f.write_all(b"Hello ZeroG File Transfer!").unwrap();
        }

        let mut sender = TransferEngine::new();
        let manifest = sender
            .create_offer("sender_1", "Host", None, vec![src_file.clone()])
            .unwrap();

        assert_eq!(manifest.files.len(), 1);
        assert_eq!(manifest.files[0].name, "source.txt");
        assert_eq!(manifest.total_size, 26);

        let mut receiver = TransferEngine::new();
        receiver.receive_offer(manifest.clone());

        let dest_dir = temp_dir.join("dest");
        receiver.accept_offer(&manifest.transfer_id, dest_dir.clone()).unwrap();

        let mut chunks = Vec::new();
        while let Some((chunk, _progress, _)) = sender.read_next_chunk(&manifest.transfer_id, 8).unwrap() {
            chunks.push(chunk);
        }

        assert_eq!(chunks.len(), 4);

        for chunk in &chunks {
            receiver.write_chunk(chunk).unwrap();
        }

        let dup_res = receiver.write_chunk(&chunks[0]).unwrap();
        assert!(!dup_res.1);

        let dest_file = dest_dir.join("source.txt");
        let content = std::fs::read_to_string(&dest_file).unwrap();
        assert_eq!(content, "Hello ZeroG File Transfer!");

        let _ = std::fs::remove_dir_all(&temp_dir);
    }
}
