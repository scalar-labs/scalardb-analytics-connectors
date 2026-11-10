//! Validated execution of connector-planned Substrait plans.

use std::{
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    task::{Context, Poll},
    time::Instant,
};

use arrow_array::RecordBatch;
use futures_core::Stream;

use crate::{ConnectorError, ErrorKind, PlannedPushdown};

/// Pull-based Arrow batch stream returned by a connector.
pub type RecordBatchStream =
    Pin<Box<dyn Stream<Item = Result<RecordBatch, ConnectorError>> + Send + 'static>>;

/// Cooperative cancellation shared with connector execution.
#[derive(Clone, Debug, Default)]
pub struct CancellationToken {
    cancelled: Arc<AtomicBool>,
}

impl CancellationToken {
    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Acquire)
    }
}

/// Non-serializable controls for one execution.
#[derive(Clone, Debug)]
pub struct ExecutionContext {
    /// Cooperative cancellation shared with Connector execution.
    pub cancellation: CancellationToken,
    /// Absolute deadline for starting and polling execution.
    pub deadline: Option<Instant>,
}

impl ExecutionContext {
    /// Creates the per-execution cancellation and deadline controls.
    pub fn new(cancellation: CancellationToken, deadline: Option<Instant>) -> Self {
        Self {
            cancellation,
            deadline,
        }
    }

    /// Fails before or during polling when cancellation or the deadline applies.
    fn check_active(&self) -> Result<(), ConnectorError> {
        if self.cancellation.is_cancelled() {
            return Err(ConnectorError::new(
                ErrorKind::Cancellation,
                false,
                "connector execution was cancelled",
            ));
        }
        if self
            .deadline
            .is_some_and(|deadline| Instant::now() >= deadline)
        {
            return Err(ConnectorError::new(
                ErrorKind::Timeout,
                true,
                "connector execution deadline expired",
            ));
        }
        Ok(())
    }
}

/// Unchecked connector output, validated by `execute_planned` before exposure.
pub struct ExecutionOutput {
    stream: RecordBatchStream,
}

impl ExecutionOutput {
    /// Wraps the Connector stream for SDK validation.
    pub fn new(stream: RecordBatchStream) -> Self {
        Self { stream }
    }
}

/// Executes plans that have already passed connector planning.
pub trait ConnectorExecutor: Send + Sync {
    /// Connector-owned instructions shared with this Connector's planner.
    type Query;

    /// Starts the already planned remote query and returns its unchecked stream.
    ///
    /// The SDK wraps the returned stream to enforce lifecycle and schema
    /// checks before batches are exposed to the caller.
    fn execute(
        &self,
        pushdown: &PlannedPushdown<Self::Query>,
        context: &ExecutionContext,
    ) -> Result<ExecutionOutput, ConnectorError>;
}

/// Starts execution and installs the shared lifecycle and schema boundary.
pub fn execute_planned<E>(
    executor: &E,
    pushdown: &PlannedPushdown<E::Query>,
    context: ExecutionContext,
) -> Result<ExecutionStream, ConnectorError>
where
    E: ConnectorExecutor,
{
    context.check_active()?;
    let output = executor.execute(pushdown, &context)?;
    Ok(ExecutionStream {
        inner: output.stream,
        expected_schema: pushdown.remote_output_schema.to_arrow(),
        context,
        terminated: false,
    })
}

/// Stream wrapper that enforces cancellation, deadline, and output schema.
pub struct ExecutionStream {
    inner: RecordBatchStream,
    expected_schema: analytics_types::arrow_schema::Schema,
    context: ExecutionContext,
    terminated: bool,
}

impl ExecutionStream {
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
        if let Err(error) = this.context.check_active() {
            this.terminated = true;
            return Poll::Ready(Some(Err(error)));
        }
        match this.inner.as_mut().poll_next(cx) {
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
