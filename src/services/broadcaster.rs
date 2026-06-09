//! Live-status broadcaster.
//!
//! The Tier B poller produces a fresh `LiveSnapshot` every minute. Anyone
//! who wants to receive these — currently the SSE handler — subscribes to
//! the broadcast channel and gets an `LiveEvent::Update` per tick.
//!
//! `tokio::sync::broadcast` semantics: lagging receivers drop the oldest
//! events but never block the publisher. That's exactly what we want for a
//! "the freshest snapshot wins" feed.

use std::sync::Arc;

use tokio::sync::broadcast;

use crate::cache::memory::LiveSnapshot;

/// One event on the live-status feed. `Update` carries an immutable snapshot.
#[derive(Clone, Debug)]
pub enum LiveEvent {
    Update(LiveSnapshot),
}

/// Wrapper around a broadcast sender so the rest of the code doesn't import
/// tokio types directly.
pub struct Broadcaster {
    tx: broadcast::Sender<LiveEvent>,
}

impl Broadcaster {
    pub fn new(capacity: usize) -> Arc<Self> {
        let (tx, _rx) = broadcast::channel(capacity.max(1));
        Arc::new(Self { tx })
    }

    /// Publish a snapshot. Errors only when there are zero subscribers, which
    /// is fine — we silently drop in that case.
    pub fn publish(&self, ev: LiveEvent) {
        let _ = self.tx.send(ev);
    }

    pub fn subscribe(&self) -> broadcast::Receiver<LiveEvent> {
        self.tx.subscribe()
    }

    pub fn subscriber_count(&self) -> usize {
        self.tx.receiver_count()
    }
}
