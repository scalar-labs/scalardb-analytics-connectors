//! Source-independent connector processing and connector-facing traits.

#![forbid(unsafe_code)]

mod error;
mod execution;
mod metadata;
mod planning;
mod substrait;

pub use error::{ConnectorError, ErrorCategory, RequestError};
pub use execution::{
    CancellationToken, ConnectorExecutor, ExecutionContext, ExecutionOutput, ExecutionProvenance,
    ExecutionStream, RecordBatchStream, StreamLimits, execute_planned,
};
pub use metadata::{
    CatalogReader, ConnectorLibrary, ConnectorSession, ConnectorSource, MetadataDigest,
    MetadataGap, MetadataProvenance, MetadataState, TableMetadata, TypeMapper, UnsupportedType,
};
pub use planning::{
    ConnectorPlanner, NullOrdering, PlannedSubplan, PlanningAttempt, PlanningContext,
    PlanningError, PushdownDecision, PushdownResult, PushdownRule, ResidualPolicy,
    SerializedPlannedSubplan, SourceBinding, ValidatedCandidate, ValidationFingerprint,
    plan_pushdown,
};
