//! The library thread that stops reads whose deadline passed.
//!
//! The SDK checks the deadline when a pull starts, but a pull blocked at the
//! Data Source only returns once someone stops it. The watcher does that for
//! every running read through its `ExecutionCancelHandle::expire`.

use std::{
    collections::BTreeMap,
    panic::{self, AssertUnwindSafe},
    sync::{Arc, Condvar, Mutex},
    thread::{self, JoinHandle},
    time::Instant,
};

use crate::{ConnectorError, ErrorKind, ExecutionCancelHandle};

use super::lock;

/// The watcher thread and the deadlines it watches.
pub(crate) struct Watcher {
    shared: Arc<Shared>,
    thread: Option<JoinHandle<()>>,
}

struct Shared {
    state: Mutex<WatchState>,
    changed: Condvar,
}

struct WatchState {
    /// Running reads by deadline; the second key element keeps entries distinct.
    deadlines: BTreeMap<(Instant, u64), ExecutionCancelHandle>,
    next_id: u64,
    stopping: bool,
}

/// One read's place in the watcher, removed when the read ends.
pub(crate) struct WatchRegistration {
    shared: Arc<Shared>,
    key: (Instant, u64),
}

impl Drop for WatchRegistration {
    fn drop(&mut self) {
        lock(&self.shared.state).deadlines.remove(&self.key);
    }
}

impl Watcher {
    pub(super) fn start() -> Result<Self, ConnectorError> {
        let shared = Arc::new(Shared {
            state: Mutex::new(WatchState {
                deadlines: BTreeMap::new(),
                next_id: 0,
                stopping: false,
            }),
            changed: Condvar::new(),
        });
        let thread = thread::Builder::new()
            .name("connector-timeout-watcher".to_owned())
            .spawn({
                let shared = Arc::clone(&shared);
                move || run(&shared)
            })
            .map_err(|_| {
                ConnectorError::new(
                    ErrorKind::Resource,
                    true,
                    "the timeout watcher thread could not be started",
                )
            })?;
        Ok(Self {
            shared,
            thread: Some(thread),
        })
    }

    /// Stops the read through `cancel` once `deadline` passes, until the
    /// returned registration is dropped.
    pub(super) fn register(
        &self,
        deadline: Instant,
        cancel: ExecutionCancelHandle,
    ) -> WatchRegistration {
        let mut state = lock(&self.shared.state);
        let key = (deadline, state.next_id);
        state.next_id += 1;
        state.deadlines.insert(key, cancel);
        self.shared.changed.notify_one();
        WatchRegistration {
            shared: Arc::clone(&self.shared),
            key,
        }
    }
}

impl Drop for Watcher {
    /// Stops the thread once the library's last unit is gone.
    ///
    /// Shutdown does not stop it: reads still running when the application
    /// shuts down keep their deadlines until their operations are released.
    fn drop(&mut self) {
        lock(&self.shared.state).stopping = true;
        self.shared.changed.notify_one();
        if let Some(thread) = self.thread.take() {
            // Each expiry is contained, so the thread does not panic, and a
            // destructor has nowhere to report a failure.
            let _ = thread.join();
        }
    }
}

fn run(shared: &Shared) {
    let mut state = lock(&shared.state);
    while !state.stopping {
        let now = Instant::now();
        let mut expired = Vec::new();
        while let Some(entry) = state.deadlines.first_entry() {
            if entry.key().0 > now {
                break;
            }
            expired.push(entry.remove());
        }
        if !expired.is_empty() {
            drop(state);
            for cancel in expired {
                // The read's next pull reports `timeout` whatever the
                // connector's cancellation returns or whether it panics, and
                // no caller waits for this call. Containing a panic keeps the
                // thread alive for every other read.
                let _ = panic::catch_unwind(AssertUnwindSafe(move || {
                    let _ = cancel.expire();
                }));
            }
            state = lock(&shared.state);
            continue;
        }
        state = match state.deadlines.first_key_value() {
            Some(((deadline, _), _)) => {
                let wait = deadline.saturating_duration_since(now);
                shared
                    .changed
                    .wait_timeout(state, wait)
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .0
            }
            None => shared
                .changed
                .wait(state)
                .unwrap_or_else(std::sync::PoisonError::into_inner),
        };
    }
}
