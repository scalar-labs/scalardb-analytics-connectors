//! Data Source-independent connector processing and Connector-facing traits.

#![forbid(unsafe_code)]

mod error;
mod execution;
mod metadata;
mod planning;
mod substrait;

pub use error::{ConnectorError, DiagnosticEntry, DiagnosticValue, ErrorKind, RequestError};
pub use execution::{
    CancellationToken, ConnectorExecutor, ExecutionContext, ExecutionOutput, ExecutionProvenance,
    ExecutionStream, RecordBatchStream, StreamLimits, execute_planned,
};
pub use metadata::{
    CatalogReader, ConnectorLibrary, DataSource, DataSourceSession, MetadataDigest, MetadataGap,
    MetadataProvenance, MetadataState, TableMetadata, TypeMapper, UnsupportedType,
};
pub use planning::{
    ConnectorPlanner, NullOrdering, PlannedSubplan, PlanningAttempt, PlanningContext,
    PlanningError, PushdownDecision, PushdownResult, PushdownRule, ResidualPolicy,
    SerializedPlannedSubplan, TableReference, ValidatedCandidate, ValidationFingerprint,
    plan_pushdown,
};
