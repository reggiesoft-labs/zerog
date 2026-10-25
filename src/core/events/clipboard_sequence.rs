use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

pub struct ClipboardSequence {
    counter: AtomicU64,
}

impl Default for ClipboardSequence {
    fn default() -> Self {
        Self::new()
    }
}

impl ClipboardSequence {
    pub fn new() -> Self {
        Self {
            counter: AtomicU64::new(0),
        }
    }

    pub fn next(&self) -> u64 {
        self.counter.fetch_add(1, Ordering::SeqCst)
    }

    pub fn current(&self) -> u64 {
        self.counter.load(Ordering::SeqCst)
    }

    pub fn timestamp(&self) -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_micros() as u64
    }
}
