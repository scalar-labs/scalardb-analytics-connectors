//! The caller-facing runtime of the connector contract.
//!
//! Connector implementations use the traits at the crate root; the connector
//! API drives connectors only through this module. It owns the contract's
//! units (library, Data Source, session, and operation), their lifetimes,
//! the one-request-per-session lease, session usability, lazy reads,
//! synchronous metadata requests, cancellation, timeouts, and panic
//! containment. Its results are the SDK's own types; it knows nothing of any C
//! representation.
//!
//! Every unit is shared by reference counting. A child keeps its parent's state
//! alive, so closing a parent first never frees memory a child uses; the
//! parent's closed flag makes later calls on the child return `invalid state`.

mod connector;
mod data_source;
mod library;
mod metadata;
mod operation;
mod session;
mod watcher;

pub use connector::{PlanningOutcome, SharedRemoteExecution};
pub use data_source::DataSourceState;
pub use library::{BuiltInConnector, Library};
pub use metadata::{Cancellation, Retrieval, SourceMetadata, TableMetadataResult, TableRead};
pub use operation::Operation;
pub use session::SessionState;

use std::{
    panic::{self, AssertUnwindSafe},
    pin::Pin,
    sync::{Arc, Mutex, MutexGuard, PoisonError},
    task::{Context, Poll, Wake, Waker},
    thread::{self, Thread},
};

use futures_core::Stream;

use crate::{ConnectorError, ErrorKind};

/// Polls the stream on the caller's thread until it yields.
///
/// Data Source access is synchronous; a stream that returns `Pending` wakes
/// this thread when it can make progress.
fn next_blocking<S: Stream + Unpin + ?Sized>(stream: &mut S) -> Option<S::Item> {
    struct ThreadWaker(Thread);
    impl Wake for ThreadWaker {
        fn wake(self: Arc<Self>) {
            self.0.unpark();
        }
    }
    let waker = Waker::from(Arc::new(ThreadWaker(thread::current())));
    let mut context = Context::from_waker(&waker);
    loop {
        match Pin::new(&mut *stream).poll_next(&mut context) {
            Poll::Ready(item) => return item,
            Poll::Pending => thread::park(),
        }
    }
}

/// Locks `mutex`, continuing past a panic in another holder.
///
/// A connector panic is converted into an `internal` error and the affected
/// state is reset by the code that caught it, so a poisoned lock does not
/// indicate inconsistent library state.
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// An error for a call that the order of calls or a unit's state forbids.
pub(crate) fn invalid_state(message: &str) -> ConnectorError {
    ConnectorError::new(ErrorKind::InvalidState, false, message)
}

/// The error returned in place of a Rust panic.
pub fn panic_error() -> ConnectorError {
    ConnectorError::new(ErrorKind::Internal, false, "the connector panicked")
}

/// Runs connector code, turning a panic into an `internal` error.
fn contain_panic<T>(call: impl FnOnce() -> Result<T, ConnectorError>) -> Result<T, ConnectorError> {
    panic::catch_unwind(AssertUnwindSafe(call)).unwrap_or_else(|_| Err(panic_error()))
}
