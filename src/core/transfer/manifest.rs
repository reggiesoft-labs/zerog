use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileItem {
    pub id: String,
    pub name: String,
    pub size: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileManifest {
    pub transfer_id: String,
    pub sender_peer_id: String,
    pub sender_name: String,
    pub target_peer_id: Option<String>,
    pub files: Vec<FileItem>,
    pub total_size: u64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TransferProgress {
    pub transfer_id: String,
    pub bytes_transferred: u64,
    pub total_bytes: u64,
    pub percent: f32,
    pub is_outgoing: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileChunkPayload {
    pub transfer_id: String,
    pub file_id: String,
    pub offset: u64,
    pub data: Vec<u8>,
    pub is_last: bool,
}
