use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use tokio::sync::Mutex;

const MAX_DATAGRAM_SIZE: usize = 1200;

/// Upper bound for a control-stream frame. Control frames carry reliable
/// application packets (including clipboard text), so this is generous, but it
/// must be finite: the sender controls the length prefix and the reader would
/// otherwise allocate it eagerly. 16 MiB is far above any realistic packet.
const MAX_CONTROL_MESSAGE_SIZE: usize = 16 * 1024 * 1024;

fn validate_control_message_len(len: usize) -> anyhow::Result<()> {
    if len > MAX_CONTROL_MESSAGE_SIZE {
        anyhow::bail!(
            "control message length {} exceeds maximum {}",
            len,
            MAX_CONTROL_MESSAGE_SIZE
        );
    }
    Ok(())
}

const CONTROL_SEND_TIMEOUT: Duration = Duration::from_millis(250);
const CONTROL_SEND_BLOCKING_TIMEOUT: Duration = Duration::from_millis(500);

pub struct PeerConnection {
    conn: quinn::Connection,
    peer_id: Arc<std::sync::OnceLock<String>>,
    addr: SocketAddr,
    control_send_lock: Arc<Mutex<()>>,
    closed: Arc<AtomicBool>,
    readers_spawned: Arc<AtomicBool>,
}

impl PeerConnection {
    pub fn new(conn: quinn::Connection) -> Self {
        let addr = conn.remote_address();
        Self {
            conn,
            peer_id: Arc::new(std::sync::OnceLock::new()),
            addr,
            control_send_lock: Arc::new(Mutex::new(())),
            closed: Arc::new(AtomicBool::new(false)),
            readers_spawned: Arc::new(AtomicBool::new(false)),
        }
    }

    pub fn mark_readers_spawned(&self) -> bool {
        self.readers_spawned.swap(true, Ordering::SeqCst)
    }

    pub fn peer_id(&self) -> String {
        self.peer_id.get().cloned().unwrap_or_default()
    }

    pub fn set_peer_id(&self, peer_id: String) -> bool {
        if let Some(existing) = self.peer_id.get() {
            return existing == &peer_id;
        }
        self.peer_id.set(peer_id).is_ok()
    }

    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    pub fn connection(&self) -> &quinn::Connection {
        &self.conn
    }

    /// Returns the end-entity certificate presented by the remote during the
    /// QUIC/TLS handshake. This is the only certificate that may be trusted for
    /// peer identity, since the remote proved possession of its private key.
    pub fn peer_certificate(&self) -> Option<Vec<u8>> {
        let identity = self.conn.peer_identity()?;
        let certificates = identity
            .downcast::<Vec<rustls::pki_types::CertificateDer<'static>>>()
            .ok()?;
        certificates.first().map(|cert| cert.as_ref().to_vec())
    }

    pub fn send_datagram(&self, data: Vec<u8>) -> anyhow::Result<()> {
        if data.len() > MAX_DATAGRAM_SIZE {
            anyhow::bail!(
                "datagram exceeds MTU: {} > {}",
                data.len(),
                MAX_DATAGRAM_SIZE
            );
        }
        self.conn
            .send_datagram(bytes::Bytes::from(data))
            .map_err(|e| anyhow::anyhow!("failed to send datagram: {}", e))?;
        Ok(())
    }

    pub async fn recv_datagram(&self) -> anyhow::Result<Vec<u8>> {
        let data = self
            .conn
            .read_datagram()
            .await
            .map_err(|e| anyhow::anyhow!("failed to receive datagram: {}", e))?;
        Ok(data.to_vec())
    }

    pub async fn try_recv_datagram(&self) -> anyhow::Result<Vec<u8>> {
        tokio::select! {
            result = self.conn.read_datagram() => result
                .map(|data| data.to_vec())
                .map_err(|e| anyhow::anyhow!("failed to receive datagram: {}", e)),
            _ = tokio::task::yield_now() => Err(anyhow::anyhow!("would block")),
        }
    }

    pub async fn send_control(&self, data: Vec<u8>) -> anyhow::Result<()> {
        tokio::time::timeout(CONTROL_SEND_TIMEOUT, self.send_control_inner(data))
            .await
            .map_err(|_| anyhow::anyhow!("control send timed out after {:?}", CONTROL_SEND_TIMEOUT))?
    }

    async fn send_control_inner(&self, data: Vec<u8>) -> anyhow::Result<()> {
        validate_control_message_len(data.len())?;
        let _send_guard = self.control_send_lock.lock().await;
        let mut stream = self
            .conn
            .open_uni()
            .await
            .map_err(|e| anyhow::anyhow!("failed to open control stream: {}", e))?;
        let len = (data.len() as u32).to_le_bytes();
        let mut frame = Vec::with_capacity(4 + data.len());
        frame.extend_from_slice(&len);
        frame.extend_from_slice(&data);

        tokio::io::AsyncWriteExt::write_all(&mut stream, &frame)
            .await
            .map_err(|e| anyhow::anyhow!("failed to write control data: {}", e))?;
        stream
            .finish()
            .map_err(|e| anyhow::anyhow!("failed to finish control stream: {}", e))?;

        Ok(())
    }

    pub fn send_control_blocking(self: &Arc<Self>, data: Vec<u8>) -> anyhow::Result<()> {
        let connection = Arc::clone(self);
        let (result_tx, result_rx) = std::sync::mpsc::sync_channel(1);
        crate::platform::shared::tokio_runtime::global_runtime_handle().spawn(async move {
            let result = connection.send_control(data).await;
            tokio::task::yield_now().await;
            let _ = result_tx.send(result);
        });
        result_rx
            .recv_timeout(CONTROL_SEND_BLOCKING_TIMEOUT)
            .map_err(|e| anyhow::anyhow!("control send task failed or timed out: {}", e))?
    }

    pub async fn recv_control(&self) -> anyhow::Result<Vec<u8>> {
        let mut stream = self
            .conn
            .accept_uni()
            .await
            .map_err(|e| anyhow::anyhow!("failed to accept control stream: {}", e))?;
        read_control_message(&mut stream).await
    }

    pub async fn try_recv_control(&self) -> anyhow::Result<Vec<u8>> {
        let mut stream = tokio::select! {
            result = self.conn.accept_uni() => result
                .map_err(|e| anyhow::anyhow!("failed to accept control stream: {}", e))?,
            _ = tokio::task::yield_now() => {
                return Err(anyhow::anyhow!("would block"));
            }
        };
        read_control_message(&mut stream).await
    }

    pub fn is_closed(&self) -> bool {
        self.closed.load(Ordering::Relaxed) || self.conn.close_reason().is_some()
    }

    pub fn close(&self, reason: &[u8]) {
        if !self.closed.swap(true, Ordering::Relaxed) {
            self.conn.close(0u32.into(), reason);
        }
    }

    pub fn stats(&self) -> quinn::ConnectionStats {
        self.conn.stats()
    }
}

async fn read_control_message(stream: &mut quinn::RecvStream) -> anyhow::Result<Vec<u8>> {
    let mut len_buf = [0u8; 4];
    tokio::io::AsyncReadExt::read_exact(stream, &mut len_buf)
        .await
        .map_err(|e| anyhow::anyhow!("failed to read length prefix: {}", e))?;
    let len = u32::from_le_bytes(len_buf) as usize;
    validate_control_message_len(len)?;

    let mut data = vec![0u8; len];
    tokio::io::AsyncReadExt::read_exact(stream, &mut data)
        .await
        .map_err(|e| anyhow::anyhow!("failed to read control data: {}", e))?;

    Ok(data)
}

impl std::fmt::Debug for PeerConnection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PeerConnection")
            .field("peer_id", &self.peer_id())
            .field("addr", &self.addr)
            .field("closed", &self.is_closed())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn control_length_validation_rejects_oversized_prefixes() {
        assert!(validate_control_message_len(0).is_ok());
        assert!(validate_control_message_len(MAX_CONTROL_MESSAGE_SIZE).is_ok());
        assert!(validate_control_message_len(MAX_CONTROL_MESSAGE_SIZE + 1).is_err());
        // A hostile u32 length must be rejected before any allocation happens.
        assert!(validate_control_message_len(u32::MAX as usize).is_err());
    }
}
