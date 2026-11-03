//! Data Source-independent connector processing and Connector-facing traits.

#![forbid(unsafe_code)]

mod error;
mod execution;
mod metadata;
mod planning;

pub use error::{ConnectorError, DiagnosticEntry, DiagnosticValue, ErrorKind, RequestError};
pub use execution::{
    CancellationToken, ConnectorExecutor, ExecutionContext, ExecutionOutput, ExecutionProvenance,
    ExecutionStream, RecordBatchStream, execute_planned,
};
pub use metadata::{
    CatalogReader, ConnectorLibrary, DataSource, DataSourceSession, MetadataDigest, MetadataGap,
    MetadataProvenance, MetadataState, TableMetadata, TableReference, TypeMapper, UnsupportedType,
};
pub use planning::{
    ConnectorPlanner, ExecutableQuery, ExecutableQueryResult, FunctionDeclaration, LogicalPlan,
    LogicalRelation, PlannedPushdown, PlanningError, PushdownRequest, PushdownResult, QueryPlan,
    RewriteContext, RewriteRule, RewriteRuleConnectorPlanner, RewriteRuleSet, ScalarFunction,
    TableBinding,
};
