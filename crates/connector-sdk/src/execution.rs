//! Validated execution of connector-planned Substrait plans.

use std::{
    pin::Pin,
    sync::{Arc, OnceLock},
    task::{Context, Poll},
    time::Instant,
};

use arrow_array::RecordBatch;
use futures_core::Stream;

use crate::{ConnectorError, ErrorKind, PlannedPushdown};

/// Pull-based Arrow batch stream returned by a connector.
pub type RecordBatchStream =
    Pin<Box<dyn Stream<Item = Result<RecordBatch, ConnectorError>> + Send + 'static>>;

/// Stops one running connector execution.
///
/// A connector returns one per execution. The SDK calls it when the caller
/// cancels or the deadline expires, possibly while another thread is blocked in
/// a source call made by that execution's stream.
pub trait ExecutionCancel: Send + Sync {
    /// Requests that the execution stop at the source.
    ///
    /// It may be called from any thread, must not block on the execution's
    /// source calls, and has no effect on an execution that already finished.
    fn cancel(&self) -> Result<(), ConnectorError>;
}

/// Unchecked connector output, validated by `execute_planned` before exposure.
pub struct ExecutionOutput {
    stream: RecordBatchStream,
    cancel: Arc<dyn ExecutionCancel>,
}

impl ExecutionOutput {
    /// Pairs the Connector stream with the means of stopping that execution.
    pub fn new(stream: RecordBatchStream, cancel: Arc<dyn ExecutionCancel>) -> Self {
        Self { stream, cancel }
    }
}

/// Executes plans that have already passed connector planning.
pub trait ConnectorExecutor: Send + Sync {
    /// Connector-owned instructions shared with this Connector's planner.
    type Query;

    /// Starts the already planned remote query and returns its unchecked stream.
    ///
    /// The SDK wraps the returned stream to enforce lifecycle and schema
    /// checks before batches are exposed to the caller, and stops the
    /// execution through the returned [`ExecutionCancel`].
    fn execute(
        &self,
        pushdown: &PlannedPushdown<Self::Query>,
    ) -> Result<ExecutionOutput, ConnectorError>;
}

/// Starts execution and installs the shared lifecycle and schema boundary.
///
/// `deadline` bounds pulling the stream. A deadline that passes while a pull is
/// blocked at the source takes effect only when the caller's timeout watcher
/// calls [`ExecutionCancelHandle::expire`].
pub fn execute_planned<E>(
    executor: &E,
    pushdown: &PlannedPushdown<E::Query>,
    deadline: Option<Instant>,
) -> Result<ExecutionStream, ConnectorError>
where
    E: ConnectorExecutor,
{
    if deadline_passed(deadline) {
        return Err(StopReason::Expired.error());
    }
    let output = executor.execute(pushdown)?;
    Ok(ExecutionStream {
        inner: output.stream,
        expected_schema: pushdown.remote_output_schema.to_arrow(),
        deadline,
        stop: Arc::new(ExecutionStop {
            reason: OnceLock::new(),
            cancel: output.cancel,
        }),
        terminated: false,
    })
}

/// Why an execution was stopped before its stream ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum StopReason {
    Cancelled,
    Expired,
}

impl StopReason {
    /// The error a pull returns once the execution is stopped for this reason.
    fn error(self) -> ConnectorError {
        match self {
            Self::Cancelled => ConnectorError::new(
                ErrorKind::Cancellation,
                false,
                "connector execution was cancelled",
            ),
            Self::Expired => ConnectorError::new(
                ErrorKind::Timeout,
                true,
                "connector execution deadline expired",
            ),
        }
    }
}

/// Stop state shared by one execution stream and its cancel handles.
///
/// The first recorded reason wins, so a cancellation that races with the
/// deadline reports whichever was recorded first.
struct ExecutionStop {
    reason: OnceLock<StopReason>,
    cancel: Arc<dyn ExecutionCancel>,
}

impl ExecutionStop {
    /// Records the reason, if none is recorded yet, and stops the execution.
    fn stop(&self, reason: StopReason) -> Result<(), ConnectorError> {
        // A later reason loses to the one already recorded; the execution is
        // stopped either way.
        let _ = self.reason.set(reason);
        self.cancel.cancel()
    }

    /// Returns the recorded reason, first recording expiry if the deadline passed.
    fn reason(&self, deadline: Option<Instant>) -> Option<StopReason> {
        if self.reason.get().is_none() && deadline_passed(deadline) {
            // The stream is about to end and release its source resources, so
            // expiry found during polling needs no source cancellation.
            let _ = self.reason.set(StopReason::Expired);
        }
        self.reason.get().copied()
    }
}

/// A cloneable capability for stopping one execution from any thread.
#[derive(Clone)]
pub struct ExecutionCancelHandle {
    stop: Arc<ExecutionStop>,
}

impl ExecutionCancelHandle {
    /// Stops the execution; its next pull returns `cancellation`.
    pub fn cancel(&self) -> Result<(), ConnectorError> {
        self.stop.stop(StopReason::Cancelled)
    }

    /// Stops the execution because its deadline passed; its next pull returns `timeout`.
    pub fn expire(&self) -> Result<(), ConnectorError> {
        self.stop.stop(StopReason::Expired)
    }
}

fn deadline_passed(deadline: Option<Instant>) -> bool {
    deadline.is_some_and(|deadline| Instant::now() >= deadline)
}

/// Stream wrapper that enforces cancellation, deadline, and output schema.
///
/// Once stopped, the stream returns the stop error once and then ends.
pub struct ExecutionStream {
    inner: RecordBatchStream,
    expected_schema: analytics_types::arrow_schema::Schema,
    deadline: Option<Instant>,
    stop: Arc<ExecutionStop>,
    terminated: bool,
}

impl ExecutionStream {
    /// Returns a handle that stops this execution from any thread.
    pub fn cancel_handle(&self) -> ExecutionCancelHandle {
        ExecutionCancelHandle {
            stop: Arc::clone(&self.stop),
        }
    }

    /// Checks one remote batch against the schema fixed during planning.
    fn validate_batch(&self, batch: &RecordBatch) -> Result<(), ConnectorError> {
        if batch.schema().as_ref() != &self.expected_schema {
            return Err(ConnectorError::new(
                ErrorKind::Internal,
                false,
                "connector batch schema differs from the planned output schema",
            ));
        }
        Ok(())
    }
}

impl Stream for ExecutionStream {
    type Item = Result<RecordBatch, ConnectorError>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();
        if this.terminated {
            return Poll::Ready(None);
        }
        if let Some(reason) = this.stop.reason(this.deadline) {
            this.terminated = true;
            return Poll::Ready(Some(Err(reason.error())));
        }
        let polled = this.inner.as_mut().poll_next(cx);
        if polled.is_ready() {
            // A stop requested while the pull was blocked determines its result,
            // whatever the source returned after being cancelled.
            if let Some(reason) = this.stop.reason.get() {
                this.terminated = true;
                return Poll::Ready(Some(Err(reason.error())));
            }
        }
        match polled {
            Poll::Ready(Some(Ok(batch))) => match this.validate_batch(&batch) {
                Ok(()) => Poll::Ready(Some(Ok(batch))),
                Err(error) => {
                    this.terminated = true;
                    Poll::Ready(Some(Err(error)))
                }
            },
            Poll::Ready(Some(Err(error))) => {
                this.terminated = true;
                Poll::Ready(Some(Err(error)))
            }
            Poll::Ready(None) => {
                this.terminated = true;
                Poll::Ready(None)
            }
            Poll::Pending => Poll::Pending,
        }
    }
}
