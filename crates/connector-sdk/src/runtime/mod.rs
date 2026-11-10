//! The caller-facing runtime of the connector contract.
//!
//! Connector implementations use the traits at the crate root; the connector
//! API drives connectors only through this module. It owns the contract's
//! units (library, Data Source, session, and operation), their lifetimes,
//! the one-operation-per-session lease, session usability, lazy reads and
//! metadata requests, cancellation, timeouts, and panic containment. It also
//! lays out metadata as the contract's Arrow rows. It knows nothing of any C
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
pub use metadata::{MetadataRequest, read_table_input};
pub use operation::Operation;
pub use session::SessionState;

use std::{
    panic::{self, AssertUnwindSafe},
    sync::{Mutex, MutexGuard, PoisonError},
};

use crate::{ConnectorError, ErrorKind};

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
