//! The queues between a recorder and the rest of a session: `queue.Queue(maxsize=n)` with
//! `putDroppingOldestOnFull`, and `_DiscardQueue`.
//!
//! A recorder's callback must never wait: a full queue gives up its oldest item for the new one, so a
//! consumer that falls behind loses the past, not the present.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex, MutexGuard};

use super::phrases::{Chunk, ChunkSource};

/// Chunks queued for recognition (`_AUDIO_QUEUE_MAXSIZE`): about a minute of speech at the usual 3 s each.
pub const AUDIO_QUEUE_SIZE: usize = 20;

pub struct Queue<T> {
    items: Arc<Mutex<VecDeque<T>>>,
    /// `None` keeps nothing (`_DiscardQueue`).
    capacity: Option<usize>,
}

impl<T> Clone for Queue<T> {
    fn clone(&self) -> Self {
        Queue { items: Arc::clone(&self.items), capacity: self.capacity }
    }
}

impl<T> Queue<T> {
    pub fn bounded(capacity: usize) -> Self {
        Queue { items: Arc::new(Mutex::new(VecDeque::new())), capacity: Some(capacity.max(1)) }
    }

    /// Takes every item and keeps none: for a recorder whose audio nobody wants (the volume meter alone).
    pub fn discarding() -> Self {
        Queue { items: Arc::new(Mutex::new(VecDeque::new())), capacity: None }
    }

    fn items(&self) -> MutexGuard<'_, VecDeque<T>> {
        // A panicking holder cannot leave the deque half-changed: every operation is a single call.
        self.items.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Queues `item` without waiting. True if the queue was full and its oldest item was dropped for it.
    pub fn put_dropping_oldest(&self, item: T) -> bool {
        let Some(capacity) = self.capacity else { return false };
        let mut items = self.items();
        let full = items.len() >= capacity;
        if full {
            items.pop_front();
        }
        items.push_back(item);
        full
    }

    /// How many items it keeps; `None` for a queue that keeps nothing.
    pub fn capacity(&self) -> Option<usize> {
        self.capacity
    }

    pub fn try_pop(&self) -> Option<T> {
        self.items().pop_front()
    }

    pub fn is_empty(&self) -> bool {
        self.items().is_empty()
    }

    pub fn len(&self) -> usize {
        self.items().len()
    }

    pub fn clear(&self) {
        self.items().clear();
    }
}

impl ChunkSource for Queue<Chunk> {
    fn pop(&mut self) -> Option<Chunk> {
        self.try_pop()
    }

    fn is_empty(&self) -> bool {
        Queue::is_empty(self)
    }
}
