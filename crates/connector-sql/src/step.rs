//! Cancellation of a catalog read that runs several statements in turn.

use std::sync::{Mutex, PoisonError};

use connector_adbc::{AdbcConnector, AdbcQueryCancelHandle};
use connector_sdk::{ConnectorError, ErrorKind, ExecutionCancel};

/// Cancels whichever statement of a catalog read is running.
///
/// The read's stream starts each step, and the caller cancels from another
/// thread, so both share this state.
pub(crate) struct StepCancel<C> {
    current: Mutex<Step<C>>,
}

/// The step a cancellation stops.
pub(crate) enum Step<C> {
    /// No step is running.
    Idle,
    /// One statement on the connection.
    Query(AdbcQueryCancelHandle<C>),
    /// Cancellation was requested; no further step may start.
    Cancelled,
}

impl<C> StepCancel<C> {
    pub(crate) fn new() -> Self {
        Self {
            current: Mutex::new(Step::Idle),
        }
    }

    /// Makes `step` the one cancellation stops.
    ///
    /// Returns the `cancellation` error instead when the read was already
    /// cancelled, so that the step is never started.
    pub(crate) fn start(&self, step: Step<C>) -> Result<(), ConnectorError> {
        let mut current = self.current.lock().unwrap_or_else(PoisonError::into_inner);
        if matches!(*current, Step::Cancelled) {
            return Err(ConnectorError::new(
                ErrorKind::Cancellation,
                false,
                "the source read was cancelled",
            ));
        }
        *current = step;
        Ok(())
    }
}

impl<C: AdbcConnector> ExecutionCancel for StepCancel<C> {
    fn cancel(&self) -> Result<(), ConnectorError> {
        let mut current = self.current.lock().unwrap_or_else(PoisonError::into_inner);
        match std::mem::replace(&mut *current, Step::Cancelled) {
            Step::Query(statement) => statement.cancel(),
            Step::Idle | Step::Cancelled => Ok(()),
        }
    }
}
