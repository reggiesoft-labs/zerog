pub mod engine;
pub mod manifest;
pub mod sync;

pub use engine::{DEFAULT_CHUNK_SIZE, IncomingTransfer, OutgoingTransfer, TransferEngine};
pub use manifest::{FileChunkPayload, FileItem, FileManifest, TransferProgress};
pub use sync::{TransferEvent, TransferSync};

pub fn default_download_dir() -> std::path::PathBuf {
    dirs::download_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join("mouse-thing")
}
