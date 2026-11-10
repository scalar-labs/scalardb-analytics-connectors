//! Stoppable source reads and validated execution of connector-planned queries.

use std::{
    pin::Pin,
    sync::{Arc, OnceLock},
    task::{Context, Poll},
    time::Instant,
};

use arrow_array::RecordBatch;
use futures_core::Stream;

use crate::{ConnectorError, ErrorKind, PortableQuery, RemoteExecution};

/// Pull-based stream of items read from the Data Source by a connector.
pub type ConnectorStream<T> =
    Pin<Box<dyn Stream<Item = Result<T, ConnectorError>> + Send + 'static>>;

/// Pull-based Arrow batch stream returned by a connector.
pub type RecordBatchStream = ConnectorStream<RecordBatch>;

/// Stops one running source read: a query execution or a catalog read.
///
/// A connector returns one per read. The SDK calls it when the caller cancels
/// or the deadline expires, possibly while another thread is blocked in a
/// source call made by that read's stream.
pub trait ExecutionCancel: Send + Sync {
    /// Requests that the read stop at the source.
    ///
    /// It may be called from any thread, must not block on the read's source
    /// calls, and has no effect on a read that already finished.
    fn cancel(&self) -> Result<(), ConnectorError>;
}

/// Unchecked connector output for one source read.
pub struct ExecutionOutput<T = RecordBatch> {
    stream: ConnectorStream<T>,
    cancel: Arc<dyn ExecutionCancel>,
}

impl<T> ExecutionOutput<T> {
    /// Pairs the Connector stream with the means of stopping that read.
    pub fn new(stream: ConnectorStream<T>, cancel: Arc<dyn ExecutionCancel>) -> Self {
        Self { stream, cancel }
    }
}

/// Executes queries that have already passed connector planning.
pub trait ConnectorExecutor: Send + Sync {
    /// Connector-owned instructions shared with this Connector's planner.
    ///
    /// The query may have been planned in another process and restored with
    /// [`PortableQuery::from_bytes`].
    type Query: PortableQuery;

    /// Starts the already planned remote query and returns its unchecked stream.
    ///
    /// The SDK wraps the returned stream to enforce lifecycle and schema
    /// checks before batches are exposed to the caller, and stops the
    /// execution through the returned [`ExecutionCancel`].
    fn execute(&self, query: &Self::Query) -> Result<ExecutionOutput, ConnectorError>;
}

/// Starts one source read and installs the shared cancellation and deadline boundary.
///
/// `start` calls the connector, for example one of the `CatalogReader`
/// functions. It is not called when the deadline has already passed.
/// A deadline that passes while a pull is blocked at the source takes effect
/// only when the caller's timeout watcher calls [`ExecutionCancelHandle::expire`].
pub fn start_source_read<T>(
    deadline: Option<Instant>,
    start: impl FnOnce() -> Result<ExecutionOutput<T>, ConnectorError>,
) -> Result<SourceStream<T>, ConnectorError> {
    if deadline_passed(deadline) {
        return Err(StopReason::Expired.error());
    }
    let output = start()?;
    Ok(SourceStream {
        inner: output.stream,
        deadline,
        stop: Arc::new(ExecutionStop {
            reason: OnceLock::new(),
            cancel: output.cancel,
        }),
        terminated: false,
    })
}

/// Starts execution and installs the shared lifecycle and schema boundary.
///
/// `remote` is either [`PlannedPushdown::remote`] in the planning process or a
/// value restored with [`RemoteExecution::from_bytes`] in another process.
/// Cancellation and the deadline behave as in [`start_source_read`].
///
/// [`PlannedPushdown::remote`]: crate::PlannedPushdown::remote
pub fn execute_planned<E>(
    executor: &E,
    remote: &RemoteExecution<E::Query>,
    deadline: Option<Instant>,
) -> Result<ExecutionStream, ConnectorError>
where
    E: ConnectorExecutor,
{
    let inner = start_source_read(deadline, || executor.execute(&remote.query))?;
    Ok(ExecutionStream {
        inner,
        expected_schema: remote.output_schema.to_arrow(),
        terminated: false,
    })
}

/// Why a source read was stopped before its stream ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum StopReason {
    Cancelled,
    Expired,
}

impl StopReason {
    /// The error a pull returns once the read is stopped for this reason.
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

/// Stop state shared by one source stream and its cancel handles.
///
/// The first recorded reason wins, so a cancellation that races with the
/// deadline reports whichever was recorded first.
struct ExecutionStop {
    reason: OnceLock<StopReason>,
    cancel: Arc<dyn ExecutionCancel>,
}

impl ExecutionStop {
    /// Records the reason, if none is recorded yet, and stops the read.
    fn stop(&self, reason: StopReason) -> Result<(), ConnectorError> {
        // A later reason loses to the one already recorded; the read is
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

/// A cloneable capability for stopping one source read from any thread.
#[derive(Clone)]
pub struct ExecutionCancelHandle {
    stop: Arc<ExecutionStop>,
}

impl ExecutionCancelHandle {
    /// Stops the read; its next pull returns `cancellation`.
    pub fn cancel(&self) -> Result<(), ConnectorError> {
        self.stop.stop(StopReason::Cancelled)
    }

    /// Stops the read because its deadline passed; its next pull returns `timeout`.
    pub fn expire(&self) -> Result<(), ConnectorError> {
        self.stop.stop(StopReason::Expired)
    }
}

fn deadline_passed(deadline: Option<Instant>) -> bool {
    deadline.is_some_and(|deadline| Instant::now() >= deadline)
}

/// Stream wrapper that enforces cancellation and the deadline for one source read.
///
/// Once stopped, the stream returns the stop error once and then ends.
pub struct SourceStream<T> {
    inner: ConnectorStream<T>,
    deadline: Option<Instant>,
    stop: Arc<ExecutionStop>,
    terminated: bool,
}

impl<T> SourceStream<T> {
    /// Returns a handle that stops this read from any thread.
    pub fn cancel_handle(&self) -> ExecutionCancelHandle {
        ExecutionCancelHandle {
            stop: Arc::clone(&self.stop),
        }
    }
}

impl<T> Stream for SourceStream<T> {
    type Item = Result<T, ConnectorError>;

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
            Poll::Ready(Some(Ok(item))) => Poll::Ready(Some(Ok(item))),
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

/// Stream wrapper that enforces cancellation, deadline, and output schema.
///
/// Once stopped or failed, the stream returns that error once and then ends.
pub struct ExecutionStream {
    inner: SourceStream<RecordBatch>,
    expected_schema: analytics_types::arrow_schema::Schema,
    terminated: bool,
}

impl ExecutionStream {
    /// Returns a handle that stops this execution from any thread.
    pub fn cancel_handle(&self) -> ExecutionCancelHandle {
        self.inner.cancel_handle()
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
        match Pin::new(&mut this.inner).poll_next(cx) {
            Poll::Ready(Some(Ok(batch))) => match this.validate_batch(&batch) {
                Ok(()) => Poll::Ready(Some(Ok(batch))),
                Err(error) => {
                    this.terminated = true;
                    Poll::Ready(Some(Err(error)))
                }
            },
            polled => polled,
        }
    }
}
