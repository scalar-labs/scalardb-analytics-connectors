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

use crate::{ConnectorError, ErrorKind, PlannedSubplan, ValidationFingerprint};

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

/// Per-stream limits enforced without adding an internal buffer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StreamLimits {
    max_batch_rows: usize,
    max_batch_bytes: usize,
}

impl StreamLimits {
    pub fn new(max_batch_rows: usize, max_batch_bytes: usize) -> Result<Self, ConnectorError> {
        if max_batch_rows == 0 || max_batch_bytes == 0 {
            return Err(ConnectorError::new(
                ErrorKind::InvalidSettings,
                false,
                "stream limits must be non-zero",
            ));
        }
        Ok(Self {
            max_batch_rows,
            max_batch_bytes,
        })
    }

    pub fn max_batch_rows(self) -> usize {
        self.max_batch_rows
    }

    pub fn max_batch_bytes(self) -> usize {
        self.max_batch_bytes
    }
}

impl Default for StreamLimits {
    fn default() -> Self {
        Self {
            max_batch_rows: 1_048_576,
            max_batch_bytes: 64 * 1024 * 1024,
        }
    }
}

/// Non-serializable controls for one execution.
#[derive(Clone, Debug)]
pub struct ExecutionContext {
    cancellation: CancellationToken,
    deadline: Option<Instant>,
    limits: StreamLimits,
}

impl ExecutionContext {
    pub fn new(
        cancellation: CancellationToken,
        deadline: Option<Instant>,
        limits: StreamLimits,
    ) -> Self {
        Self {
            cancellation,
            deadline,
            limits,
        }
    }

    pub fn cancellation(&self) -> &CancellationToken {
        &self.cancellation
    }

    pub fn deadline(&self) -> Option<Instant> {
        self.deadline
    }

    pub fn limits(&self) -> StreamLimits {
        self.limits
    }

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

/// Auditable identity of one connector execution.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExecutionProvenance {
    connector: String,
    data_source_product: String,
    data_source_version: String,
    snapshot_fingerprint: Option<String>,
    validation_fingerprint: ValidationFingerprint,
}

impl ExecutionProvenance {
    pub fn new(
        connector: impl Into<String>,
        data_source_product: impl Into<String>,
        data_source_version: impl Into<String>,
        snapshot_fingerprint: Option<String>,
        validation_fingerprint: ValidationFingerprint,
    ) -> Self {
        Self {
            connector: connector.into(),
            data_source_product: data_source_product.into(),
            data_source_version: data_source_version.into(),
            snapshot_fingerprint,
            validation_fingerprint,
        }
    }

    pub fn connector(&self) -> &str {
        &self.connector
    }

    pub fn data_source_product(&self) -> &str {
        &self.data_source_product
    }

    pub fn data_source_version(&self) -> &str {
        &self.data_source_version
    }

    pub fn snapshot_fingerprint(&self) -> Option<&str> {
        self.snapshot_fingerprint.as_deref()
    }

    pub fn validation_fingerprint(&self) -> ValidationFingerprint {
        self.validation_fingerprint
    }
}

/// Unchecked connector output, validated by `execute_planned` before exposure.
pub struct ExecutionOutput {
    stream: RecordBatchStream,
    provenance: ExecutionProvenance,
}

impl ExecutionOutput {
    pub fn new(stream: RecordBatchStream, provenance: ExecutionProvenance) -> Self {
        Self { stream, provenance }
    }
}

/// Executes plans that have already passed connector planning.
pub trait ConnectorExecutor: Send + Sync {
    fn execute(
        &self,
        plan: &PlannedSubplan,
        context: &ExecutionContext,
    ) -> Result<ExecutionOutput, ConnectorError>;
}

/// Starts execution and installs the shared lifecycle and schema boundary.
pub fn execute_planned(
    executor: &impl ConnectorExecutor,
    plan: &PlannedSubplan,
    context: ExecutionContext,
) -> Result<ExecutionStream, ConnectorError> {
    context.check_active()?;
    let output = executor.execute(plan, &context)?;
    if output.provenance.validation_fingerprint() != plan.validation_fingerprint() {
        return Err(ConnectorError::new(
            ErrorKind::Internal,
            false,
            "execution provenance does not match the planned subplan",
        ));
    }
    Ok(ExecutionStream {
        inner: output.stream,
        provenance: output.provenance,
        expected_schema: plan.output_schema().to_arrow(),
        context,
        terminated: false,
    })
}

/// Stream wrapper that enforces cancellation, deadline, bounds, and output schema.
pub struct ExecutionStream {
    inner: RecordBatchStream,
    provenance: ExecutionProvenance,
    expected_schema: analytics_types::arrow_schema::Schema,
    context: ExecutionContext,
    terminated: bool,
}

impl ExecutionStream {
    pub fn provenance(&self) -> &ExecutionProvenance {
        &self.provenance
    }

    fn validate_batch(&self, batch: &RecordBatch) -> Result<(), ConnectorError> {
        if batch.num_rows() > self.context.limits.max_batch_rows {
            return Err(ConnectorError::new(
                ErrorKind::Resource,
                true,
                "connector batch exceeds the configured row limit",
            ));
        }
        if batch.get_array_memory_size() > self.context.limits.max_batch_bytes {
            return Err(ConnectorError::new(
                ErrorKind::Resource,
                true,
                "connector batch exceeds the configured byte limit",
            ));
        }
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
