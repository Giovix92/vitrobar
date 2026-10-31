//! The hand-off between a producer thread (WinEvent callbacks, the tray host) and the UI
//! thread: the data travels in a process-owned queue, and the window message that wakes the
//! UI thread carries nothing.
//!
//! A message in the `WM_APP` range can be posted by any process at the same integrity level
//! (or by a stray broadcast), so its `wParam` / `lParam` are never trusted: a receiver that
//! gets the wake-up message only drains this queue, which only this process fills. A forged
//! wake-up drains the real events a little early (or nothing), never anything else.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

/// At most this many items wait (as many as a thread's message queue holds by default);
/// beyond that new items are dropped, as a failing `PostMessage` would drop them.
pub const QUEUE_CAP: usize = 10_000;

/// A FIFO with a "wake-up already posted" flag, so a burst costs one window message.
///
/// Single producer per queue: the wake-up flag is a hand-off between one pushing thread and
/// the consumer (the key hook's queue has exactly one producer, the hook thread). Several
/// producers would still be memory safe, but each queue should keep to one so the ordering
/// and the wake-up accounting stay simple.
pub struct WakeQueue<T> {
    items: Mutex<VecDeque<T>>,
    wake_pending: AtomicBool,
}

impl<T> Default for WakeQueue<T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T> WakeQueue<T> {
    pub const fn new() -> Self {
        Self {
            items: Mutex::new(VecDeque::new()),
            wake_pending: AtomicBool::new(false),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, VecDeque<T>> {
        self.items.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Queues `item`; returns `true` when the caller must post the wake-up message (none is
    /// pending). If that post fails, call [`WakeQueue::wake_failed`].
    pub fn push(&self, item: T) -> bool {
        {
            let mut q = self.lock();
            if q.len() >= QUEUE_CAP {
                return false;
            }
            q.push_back(item);
        }
        !self.wake_pending.swap(true, Ordering::AcqRel)
    }

    /// The wake-up could not be posted: the next push posts one again.
    pub fn wake_failed(&self) {
        self.wake_pending.store(false, Ordering::Release);
    }

    /// Takes everything queued, in order. Call when the wake-up message arrives (whoever sent
    /// it). Clears the pending flag first, so an item pushed meanwhile posts a new wake-up.
    pub fn drain(&self) -> Vec<T> {
        self.wake_pending.store(false, Ordering::Release);
        self.lock().drain(..).collect()
    }

    /// Drops everything queued (the producer stopped).
    pub fn clear(&self) {
        self.lock().clear();
        self.wake_pending.store(false, Ordering::Release);
    }

    pub fn len(&self) -> usize {
        self.lock().len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_wake_per_burst_and_fifo() {
        let q = WakeQueue::new();
        assert!(q.push(1));
        assert!(!q.push(2), "a wake-up is already pending");
        assert_eq!(q.drain(), vec![1, 2]);
        assert!(q.push(3), "after a drain the next push wakes again");
        q.wake_failed();
        assert!(q.push(4), "a failed wake-up is retried by the next push");
        assert_eq!(q.drain(), vec![3, 4]);
        assert!(q.drain().is_empty(), "a forged wake-up drains nothing");
    }

    #[test]
    fn capped_and_clearable() {
        let q = WakeQueue::new();
        for i in 0..QUEUE_CAP + 5 {
            q.push(i);
        }
        assert_eq!(q.len(), QUEUE_CAP);
        q.clear();
        assert!(q.is_empty());
        assert!(q.push(0));
    }
}
