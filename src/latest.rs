use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

/// Mailbox on one slot: the newest value wins and older ones are dropped.
///
/// A bounded `mpsc::sync_channel(n)` queues frames, so under load the viewer
/// receives stale pictures and latency grows. For live video only the latest
/// frame is useful, so producers overwrite and consumers take.
pub struct LatestSlot<T> {
    inner: Mutex<Option<T>>,
    pushed: AtomicU64,
    taken: AtomicU64,
}

impl<T> Default for LatestSlot<T> {
    fn default() -> Self {
        Self {
            inner: Mutex::new(None),
            pushed: AtomicU64::new(0),
            taken: AtomicU64::new(0),
        }
    }
}

impl<T> LatestSlot<T> {
    pub fn new() -> Self {
        Self::default()
    }

    /// Stores the value, replacing whatever was waiting there.
    pub fn put(&self, value: T) {
        let mut guard = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        *guard = Some(value);
        self.pushed.fetch_add(1, Ordering::Relaxed);
    }

    /// Takes the newest value if any is waiting.
    pub fn take(&self) -> Option<T> {
        let mut guard = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let value = guard.take();
        if value.is_some() {
            self.taken.fetch_add(1, Ordering::Relaxed);
        }
        value
    }

    pub fn is_empty(&self) -> bool {
        self.inner.lock().unwrap_or_else(|e| e.into_inner()).is_none()
    }

    pub fn pushed(&self) -> u64 {
        self.pushed.load(Ordering::Relaxed)
    }

    pub fn taken(&self) -> u64 {
        self.taken.load(Ordering::Relaxed)
    }

    /// Frames produced and then overwritten before anyone took them.
    pub fn overwritten(&self) -> u64 {
        self.pushed().saturating_sub(self.taken())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn newest_wins() {
        let slot = LatestSlot::new();
        slot.put(1);
        slot.put(2);
        slot.put(3);
        assert_eq!(slot.take(), Some(3));
        assert_eq!(slot.take(), None);
    }

    #[test]
    fn counts_overwrites() {
        let slot = LatestSlot::new();
        slot.put("a");
        slot.put("b");
        assert_eq!(slot.take(), Some("b"));
        assert_eq!(slot.pushed(), 2);
        assert_eq!(slot.taken(), 1);
        assert_eq!(slot.overwritten(), 1);
    }
}
