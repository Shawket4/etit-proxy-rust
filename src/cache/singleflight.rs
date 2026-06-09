//! A small single-flight implementation.
//!
//! When N concurrent callers ask for the same key, only the first one runs
//! the closure; the rest wait on the same `Notify` and clone the resulting
//! value. This is what stops the history endpoint from hammering the
//! upstream when a dashboard with K vehicles asks for the same day at once.
//!
//! Returned values must be `Clone` because all waiters need their own copy.

use std::collections::HashMap;
use std::hash::Hash;
use std::sync::Arc;

use parking_lot::Mutex;
use tokio::sync::Notify;

enum Slot<V> {
    InFlight(Arc<Notify>),
    Done(V),
}

pub struct SingleFlight<K, V> {
    inner: Arc<Mutex<HashMap<K, Slot<V>>>>,
}

impl<K: Eq + Hash + Clone, V: Clone> Default for SingleFlight<K, V> {
    fn default() -> Self {
        Self {
            inner: Arc::new(Mutex::new(HashMap::new())),
        }
    }
}

impl<K: Eq + Hash + Clone + Send + 'static, V: Clone + Send + 'static> SingleFlight<K, V> {
    pub fn new() -> Self {
        Self::default()
    }

    /// Run `f()` once per key, in flight; concurrent callers get the same
    /// result. The map entry is removed once the first call completes
    /// (success or failure), so the next caller will retry rather than
    /// keep observing a stale error.
    pub async fn run<F, Fut, E>(&self, key: K, f: F) -> Result<V, E>
    where
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = Result<V, E>>,
    {
        // Phase 1: register or join.
        let either = {
            let mut g = self.inner.lock();
            match g.get(&key) {
                Some(Slot::InFlight(n)) => Either::Wait(n.clone()),
                Some(Slot::Done(v))     => Either::Have(v.clone()),
                None => {
                    let notify = Arc::new(Notify::new());
                    g.insert(key.clone(), Slot::InFlight(notify.clone()));
                    Either::Lead
                }
            }
        };

        match either {
            Either::Have(v) => Ok(v),
            Either::Wait(notify) => {
                notify.notified().await;
                // Re-check map.
                let done_val = {
                    let g = self.inner.lock();
                    match g.get(&key) {
                        Some(Slot::Done(v)) => Some(v.clone()),
                        _ => None,
                    }
                };
                if let Some(v) = done_val {
                    Ok(v)
                } else {
                    f().await
                }
            }
            Either::Lead => {
                let result = f().await;
                let notify = {
                    let mut g = self.inner.lock();
                    match (&result, g.remove(&key)) {
                        (Ok(v), Some(Slot::InFlight(n))) => {
                            // Briefly publish the result so any waiter that
                            // hasn't yet been notified can pick it up.
                            g.insert(key.clone(), Slot::Done(v.clone()));
                            n
                        }
                        (Ok(_), _) => Arc::new(Notify::new()),
                        (Err(_), Some(Slot::InFlight(n))) => n,
                        (Err(_), _) => Arc::new(Notify::new()),
                    }
                };
                notify.notify_waiters();

                // Schedule a tiny grace window before evicting the cached
                // value so late-arriving waiters see it.
                let inner = self.inner.clone();
                let k = key.clone();
                tokio::spawn(async move {
                    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                    inner.lock().remove(&k);
                });

                result
            }
        }
    }
}

enum Either<V> {
    Lead,
    Wait(Arc<Notify>),
    Have(V),
}
